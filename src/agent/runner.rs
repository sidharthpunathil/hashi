//! Spawns an agent process with hard limits on time, output size and memory.
//!
//! Every run starts in its own process group on Unix, so whatever it spawns
//! (shells, language servers, MCP servers) can be stopped with it. However a
//! run ends, the whole group is killed; on Windows the process tree is ended
//! with `taskkill /T`.

use std::ffi::OsString;
use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, Command};

use super::AgentError;
use crate::guard::MemoryWatch;

/// Resource limits for a single run.
#[derive(Debug, Clone)]
pub struct Limits {
    /// Wall-clock budget. The child is killed when it runs out.
    pub timeout: Duration,
    /// Maximum bytes accepted on each of stdout and stderr.
    pub max_output: usize,
    /// Niceness for the run (Unix, via `nice`). On Windows any value above
    /// zero starts the run with below-normal priority. Zero leaves it alone.
    pub nice: u8,
    /// Memory ceiling for the run's whole process tree, if any.
    pub memory: Option<MemoryWatch>,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(300),
            max_output: 10 * 1024 * 1024,
            nice: 0,
            memory: None,
        }
    }
}

/// Captured output of a finished process.
#[derive(Debug)]
pub struct RawOutput {
    /// Whether the process exited with status zero.
    pub success: bool,
    /// Description of the exit status.
    pub status: String,
    /// Captured stdout, lossily decoded.
    pub stdout: String,
    /// Captured stderr, lossily decoded.
    pub stderr: String,
}

/// A running child that takes its whole process tree down when dropped.
///
/// `Drop` runs before the fields are dropped, so on Windows `taskkill /T`
/// still sees the live parent and can find its descendants; `kill_on_drop`
/// then finishes the leader.
struct Supervised {
    child: Child,
    pid: Option<u32>,
    exited: bool,
}

impl Supervised {
    fn mark_exited(&mut self) {
        self.exited = true;
    }
}

impl Drop for Supervised {
    fn drop(&mut self) {
        if let Some(pid) = self.pid {
            kill_tree(pid, self.exited);
        }
    }
}

/// Kills the process group led by `pid`. After a normal exit this sweeps up
/// anything the run left behind in its group.
#[cfg(unix)]
fn kill_tree(pid: u32, _exited: bool) {
    use nix::sys::signal::{Signal, killpg};
    use nix::unistd::Pid;
    if let Ok(raw) = i32::try_from(pid)
        && raw > 1
    {
        // ESRCH (nothing left in the group) is the normal case after an exit.
        let _ = killpg(Pid::from_raw(raw), Signal::SIGKILL);
    }
}

/// Ends the process tree rooted at `pid` with `taskkill /T /F`.
#[cfg(windows)]
fn kill_tree(pid: u32, exited: bool) {
    use std::os::windows::process::CommandExt;
    // Once the parent has exited the tree can no longer be walked from it.
    if exited {
        return;
    }
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let _ = std::process::Command::new("taskkill")
        .args(["/T", "/F", "/PID", &pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .status();
}

#[cfg(not(any(unix, windows)))]
fn kill_tree(_pid: u32, _exited: bool) {}

/// The `nice` executable, if this system has one.
#[cfg(unix)]
fn nice_bin() -> Option<&'static Path> {
    static NICE: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    NICE.get_or_init(|| {
        ["/usr/bin/nice", "/bin/nice"]
            .iter()
            .map(PathBuf::from)
            .find(|p| p.is_file())
            .or_else(|| super::locate("nice"))
    })
    .as_deref()
}

/// Resolves `bin` the way the spawn would, so it can be handed to `nice`.
#[cfg(unix)]
fn resolve(bin: &Path) -> Option<PathBuf> {
    if bin.components().count() > 1 {
        return bin.is_file().then(|| bin.to_path_buf());
    }
    super::locate(bin.to_str()?)
}

/// Builds the command, lowering its priority when asked.
fn command(bin: &Path, args: &[OsString], nice: u8) -> Command {
    #[cfg(unix)]
    if nice > 0
        && let Some(nice_bin) = nice_bin()
        && let Some(target) = resolve(bin)
    {
        // `nice` execs the agent, so the pid (and process group) stay the same.
        // A missing binary is spawned directly instead, to keep a clear error.
        let mut cmd = Command::new(nice_bin);
        cmd.arg("-n")
            .arg(nice.to_string())
            .arg("--")
            .arg(target)
            .args(args);
        return cmd;
    }
    let mut cmd = Command::new(bin);
    cmd.args(args);
    #[cfg(windows)]
    if nice > 0 {
        const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x0000_4000;
        cmd.creation_flags(BELOW_NORMAL_PRIORITY_CLASS);
    }
    cmd
}

/// Resolves when the run's process tree grows past its limit, with the
/// measured and allowed sizes. Never resolves without a watch.
async fn watch_memory(pid: Option<u32>, watch: Option<MemoryWatch>) -> (u64, u64) {
    let (Some(pid), Some(watch)) = (pid, watch) else {
        return std::future::pending().await;
    };
    let mut tick = tokio::time::interval(watch.every.max(Duration::from_millis(10)));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        let probe = Arc::clone(&watch.probe);
        let used = tokio::task::spawn_blocking(move || probe.tree_memory(pid))
            .await
            .ok()
            .flatten();
        if let Some(used) = used
            && used > watch.limit
        {
            return (used, watch.limit);
        }
    }
}

/// Runs `bin` with `args` in `workdir`.
///
/// The child never sees a shell, gets a null stdin, and runs in its own
/// process group. The whole group is killed if the returned future is
/// dropped, times out, floods its pipes or exceeds its memory limit.
///
/// # Errors
///
/// See [`AgentError`].
pub async fn run(
    bin: &Path,
    args: &[OsString],
    env: &[(String, String)],
    workdir: &Path,
    limits: Limits,
) -> Result<RawOutput, AgentError> {
    let mut cmd = command(bin, args, limits.nice);
    cmd.current_dir(workdir)
        .env("NO_COLOR", "1")
        .env("TERM", "dumb")
        .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);
    let child = cmd.spawn().map_err(|source| AgentError::Spawn {
        bin: bin.display().to_string(),
        source,
    })?;
    let pid = child.id();
    let mut proc_ = Supervised {
        child,
        pid,
        exited: false,
    };

    let (Some(stdout), Some(stderr)) = (proc_.child.stdout.take(), proc_.child.stderr.take())
    else {
        return Err(AgentError::Io(std::io::Error::other(
            "child pipes unavailable",
        )));
    };

    // The child is moved into the future so that dropping it (on timeout,
    // memory stop or cancellation) drops the `Supervised` too, which kills
    // the whole tree.
    let max_output = limits.max_output;
    let work = async move {
        let (out, err) = tokio::try_join!(
            read_capped(stdout, max_output),
            read_capped(stderr, max_output),
        )?;
        let status = proc_.child.wait().await?;
        proc_.mark_exited();
        Ok::<_, AgentError>((status, out, err))
    };

    let (status, stdout, stderr) = tokio::select! {
        done = tokio::time::timeout(limits.timeout, work) => {
            done.map_err(|_| AgentError::Timeout(limits.timeout))??
        }
        (used, limit) = watch_memory(pid, limits.memory) => {
            return Err(AgentError::MemoryLimit { used, limit });
        }
    };

    Ok(RawOutput {
        success: status.success(),
        status: status.to_string(),
        stdout: String::from_utf8_lossy(&stdout).into_owned(),
        stderr: String::from_utf8_lossy(&stderr).into_owned(),
    })
}

async fn read_capped<R: AsyncRead + Unpin>(reader: R, max: usize) -> Result<Vec<u8>, AgentError> {
    let mut buf = Vec::new();
    // Read one byte past the cap so overflow is detectable.
    let cap = u64::try_from(max).unwrap_or(u64::MAX).saturating_add(1);
    reader.take(cap).read_to_end(&mut buf).await?;
    if buf.len() > max {
        return Err(AgentError::OutputTooLarge(max));
    }
    Ok(buf)
}

/// Picks the most informative tail of stderr (or stdout), capped for display.
#[must_use]
pub fn summarize(stderr: &str, stdout: &str) -> Option<String> {
    let source = if stderr.trim().is_empty() {
        stdout
    } else {
        stderr
    };
    let trimmed = source.trim();
    if trimmed.is_empty() {
        return None;
    }
    const MAX: usize = 2_000;
    let start = trimmed.len().saturating_sub(MAX);
    let start = (start..trimmed.len())
        .find(|i| trimmed.is_char_boundary(*i))
        .unwrap_or(0);
    Some(trimmed[start..].to_owned())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn sh(script: &str) -> Vec<OsString> {
        vec!["-c".into(), script.into()]
    }

    #[tokio::test]
    async fn run_captures_stdout_and_status() {
        let out = run(
            Path::new("/bin/sh"),
            &sh("echo hi; echo oops >&2"),
            &[],
            Path::new("."),
            Limits::default(),
        )
        .await
        .unwrap();
        assert!(out.success);
        assert_eq!(out.stdout.trim(), "hi");
        assert_eq!(out.stderr.trim(), "oops");
    }

    #[tokio::test]
    async fn run_passes_extra_environment() {
        let env = [("HASHI_TEST_VAR".to_owned(), "bridged".to_owned())];
        let out = run(
            Path::new("/bin/sh"),
            &sh("printf %s \"$HASHI_TEST_VAR\""),
            &env,
            Path::new("."),
            Limits::default(),
        )
        .await
        .unwrap();
        assert_eq!(out.stdout, "bridged");
    }

    #[tokio::test]
    async fn run_kills_child_after_timeout() {
        let limits = Limits {
            timeout: Duration::from_millis(100),
            ..Limits::default()
        };
        let err = run(
            Path::new("/bin/sh"),
            &sh("sleep 5"),
            &[],
            Path::new("."),
            limits,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, AgentError::Timeout(_)));
    }

    #[tokio::test]
    async fn run_rejects_oversized_output() {
        let limits = Limits {
            max_output: 16,
            ..Limits::default()
        };
        let err = run(
            Path::new("/bin/sh"),
            &sh("printf '%064d' 0"),
            &[],
            Path::new("."),
            limits,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, AgentError::OutputTooLarge(16)));
    }

    #[tokio::test]
    async fn run_reports_missing_binary() {
        let err = run(
            Path::new("/nonexistent/agent"),
            &[],
            &[],
            Path::new("."),
            Limits::default(),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, AgentError::Spawn { .. }));
    }

    #[tokio::test]
    async fn run_stops_a_tree_over_its_memory_limit() {
        use crate::guard::{GIB, ManualProbe, Probe};
        let probe = Arc::new(ManualProbe::roomy());
        probe.set_tree_memory(Some(3 * GIB));
        let probe: Arc<dyn Probe> = probe;
        let limits = Limits {
            memory: Some(MemoryWatch {
                limit: 2 * GIB,
                every: Duration::from_millis(50),
                probe,
            }),
            ..Limits::default()
        };
        let started = std::time::Instant::now();
        let err = run(
            Path::new("/bin/sh"),
            &sh("sleep 5"),
            &[],
            Path::new("."),
            limits,
        )
        .await
        .unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(4));
        assert!(
            matches!(err, AgentError::MemoryLimit { used, limit } if used == 3 * GIB && limit == 2 * GIB)
        );
        assert_eq!(
            err.to_string(),
            "run exceeded its memory limit (3.0 GiB of 2.0 GiB)"
        );
    }

    #[tokio::test]
    async fn run_lowers_priority_with_nice() {
        if nice_bin().is_none() {
            return;
        }
        let limits = Limits {
            nice: 7,
            ..Limits::default()
        };
        let out = run(
            Path::new("/bin/sh"),
            &sh("ps -o nice= -p $$"),
            &[],
            Path::new("."),
            limits,
        )
        .await
        .unwrap();
        assert_eq!(out.stdout.trim(), "7", "{out:?}");
    }

    #[tokio::test]
    async fn run_kills_leftover_group_members_after_exit() {
        let dir = tempfile::tempdir().unwrap();
        let pidfile = dir.path().join("pid");
        let script = format!(
            "sleep 30 >/dev/null 2>&1 & echo $! > '{}'",
            pidfile.display()
        );
        let out = run(
            Path::new("/bin/sh"),
            &sh(&script),
            &[],
            Path::new("."),
            Limits::default(),
        )
        .await
        .unwrap();
        assert!(out.success);
        let pid = std::fs::read_to_string(&pidfile).unwrap();
        assert!(wait_gone(pid.trim()).await, "background child survived");
    }

    /// Polls `kill -0 <pid>` until the process is gone, up to five seconds.
    pub(crate) async fn wait_gone(pid: &str) -> bool {
        for _ in 0..50 {
            let alive = std::process::Command::new("kill")
                .args(["-0", pid])
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success());
            if !alive {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        false
    }

    #[test]
    fn summarize_prefers_stderr() {
        assert_eq!(summarize(" boom ", "out").as_deref(), Some("boom"));
        assert_eq!(summarize("", "out").as_deref(), Some("out"));
        assert_eq!(summarize("", " "), None);
    }
}
