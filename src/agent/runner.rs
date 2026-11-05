//! Spawns an agent process with hard limits on time, output size and memory.
//!
//! Every run starts in its own process group on Unix, so whatever it spawns
//! (shells, language servers, MCP servers) can be stopped with it. However a
//! run ends (normal exit, timeout, memory stop or cancellation), the group is
//! killed, and so is every process the watchdog saw the run move into another
//! group or session (see [`super::tree`]). On Windows each run is put in a Job
//! Object that kills every process in it when Hashi closes the job, with
//! `taskkill /T` as the fallback when the job cannot be set up.
//!
//! The child does not inherit Hashi's own secrets: every `HASHI_*` variable
//! and `TUNNEL_TOKEN` are removed from its environment.

use std::ffi::{OsStr, OsString};
use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, Command};

use super::AgentError;
use super::tree::RunScope;
use crate::guard::{MemoryWatch, Probe};

/// Inherited variables, besides every `HASHI_*` one, that agent children must
/// not see. `TUNNEL_TOKEN` is how cloudflared reads a named-tunnel token.
const SECRET_VARS: [&str; 1] = ["TUNNEL_TOKEN"];

/// Whether the inherited variable `name` is one of Hashi's own secrets.
/// Matched without regard to case, as Windows treats names.
#[must_use]
pub fn is_hashi_secret(name: &OsStr) -> bool {
    let name = name.to_string_lossy().to_ascii_uppercase();
    name.starts_with("HASHI_") || SECRET_VARS.contains(&name.as_str())
}

/// Whether the variable `name` matches `pattern`, without regard to case. A
/// pattern ending in `*` matches every name with that prefix (`CRUSH_*`).
#[must_use]
pub fn env_name_matches(pattern: &str, name: &str) -> bool {
    match pattern.strip_suffix('*') {
        Some(prefix) => name
            .get(..prefix.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(prefix)),
        None => pattern.eq_ignore_ascii_case(name),
    }
}

/// Variables set on every agent child, before the backend's own.
///
/// On Windows, `NoDefaultCurrentDirectoryInExePath` stops `cmd.exe` (which
/// runs npm's `.cmd` shims) from looking for `node` in the current directory,
/// the token's workspace, before `PATH`.
#[must_use]
pub fn base_env() -> Vec<(&'static str, &'static str)> {
    let mut env = vec![("NO_COLOR", "1"), ("TERM", "dumb")];
    if cfg!(windows) {
        env.push(("NoDefaultCurrentDirectoryInExePath", "1"));
    }
    env
}

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

/// A running child that takes its whole process tree down when it ends.
///
/// [`Supervised::finish`] stops the tree without blocking the runtime, and
/// `Drop` does the same synchronously if the run is cancelled first. `Drop`
/// runs before the fields are dropped, so on Windows `taskkill /T` still sees
/// the live parent and can find its descendants; `kill_on_drop` then finishes
/// the leader, and dropping the job kills everything assigned to it.
struct Supervised {
    child: Child,
    pid: Option<u32>,
    exited: bool,
    stopped: bool,
    scope: Arc<Mutex<RunScope>>,
    #[cfg(windows)]
    job: Option<win32job::Job>,
}

impl Supervised {
    fn mark_exited(&mut self) {
        self.exited = true;
    }

    /// Stops the whole tree on a blocking thread and waits for that.
    async fn finish(mut self) {
        self.stopped = true;
        // Dropping `self` closes the job, which kills everything in it.
        #[cfg(windows)]
        if self.job.is_some() {
            return;
        }
        if let Some(pid) = self.pid {
            let (exited, scope) = (self.exited, Arc::clone(&self.scope));
            let _ = tokio::task::spawn_blocking(move || kill_tree(pid, exited, &scope)).await;
        }
    }
}

impl Drop for Supervised {
    fn drop(&mut self) {
        if self.stopped {
            return;
        }
        self.stopped = true;
        #[cfg(windows)]
        if self.job.is_some() {
            return;
        }
        if let Some(pid) = self.pid {
            kill_tree(pid, self.exited, &self.scope);
        }
    }
}

/// Kills the process group led by `pid`, then every other process of the
/// run (see [`super::tree::sweep`]). After a normal exit this sweeps up
/// anything the run left behind.
#[cfg(unix)]
fn kill_tree(pid: u32, exited: bool, scope: &Mutex<RunScope>) {
    use nix::sys::signal::{Signal, killpg};
    use nix::unistd::Pid;
    if let Ok(raw) = i32::try_from(pid)
        && raw > 1
    {
        // ESRCH (nothing left in the group) is the normal case after an exit.
        let _ = killpg(Pid::from_raw(raw), Signal::SIGKILL);
    }
    let mut scope = scope.lock().unwrap_or_else(PoisonError::into_inner).clone();
    super::tree::sweep(pid, exited, &mut scope);
}

/// Puts the child in a Job Object that kills every process in it when the
/// job's last handle is closed, which happens when the run is dropped.
/// Descendants join the job automatically. Returns `None` (and the runner
/// falls back to `taskkill`) if the job cannot be created or assigned.
#[cfg(windows)]
fn contain(child: &Child) -> Option<win32job::Job> {
    let handle = child.raw_handle()?;
    let mut info = win32job::ExtendedLimitInfo::new();
    info.limit_kill_on_job_close();
    let job = win32job::Job::create_with_limit_info(&info).ok()?;
    job.assign_process(handle as isize).ok()?;
    Some(job)
}

/// Ends the process tree rooted at `pid` with `taskkill /T /F`.
#[cfg(windows)]
fn kill_tree(pid: u32, exited: bool, _scope: &Mutex<RunScope>) {
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
fn kill_tree(_pid: u32, _exited: bool, _scope: &Mutex<RunScope>) {}

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

/// Samples the run's processes, recording their groups and sessions in
/// `scope`, and resolves when they grow past the memory limit, with the
/// measured and allowed sizes.
///
/// Without a memory watch the run is still sampled on Unix (with no limit),
/// so that whatever it detaches is known when it ends. Elsewhere, or without
/// a pid, this never resolves.
async fn watch_memory(
    pid: Option<u32>,
    watch: Option<MemoryWatch>,
    scope: Arc<Mutex<RunScope>>,
) -> (u64, u64) {
    let Some(pid) = pid else {
        return std::future::pending().await;
    };
    let (probe, limit, every) = match watch {
        Some(w) => (w.probe, w.limit, w.every),
        None if cfg!(unix) => (tracking_probe(), u64::MAX, crate::guard::SAMPLE_EVERY),
        None => return std::future::pending().await,
    };
    let mut tick = tokio::time::interval(every.max(Duration::from_millis(10)));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        let probe = Arc::clone(&probe);
        let scope = Arc::clone(&scope);
        let used = tokio::task::spawn_blocking(move || {
            let mut scope = scope.lock().unwrap_or_else(PoisonError::into_inner);
            probe.run_memory(pid, &mut scope)
        })
        .await
        .ok()
        .flatten();
        if let Some(used) = used
            && used > limit
        {
            return (used, limit);
        }
    }
}

/// Probe used to follow runs that have no memory watch.
fn tracking_probe() -> Arc<dyn Probe> {
    static PROBE: std::sync::OnceLock<Arc<crate::guard::SystemProbe>> = std::sync::OnceLock::new();
    let probe: Arc<dyn Probe> = PROBE.get_or_init(Arc::default).clone();
    probe
}

/// Runs `bin` with `args` in `workdir`.
///
/// The child never sees a shell, gets a null stdin, runs in its own process
/// group, and does not inherit Hashi's secrets (see [`is_hashi_secret`]).
/// However the run ends (exit, timeout, overflowing its pipes, its memory
/// limit, or the returned future being dropped), every process of the run is
/// killed before this returns.
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
    run_with(bin, args, env, &[], workdir, limits).await
}

/// Like [`run`], but also removes the inherited variables named in
/// `remove` (matched without regard to case) before `env` is applied. A run
/// on a non-default account uses this to drop the owner's credentials.
///
/// # Errors
///
/// See [`AgentError`].
pub async fn run_with(
    bin: &Path,
    args: &[OsString],
    env: &[(String, String)],
    remove: &[&str],
    workdir: &Path,
    limits: Limits,
) -> Result<RawOutput, AgentError> {
    let mut cmd = command(bin, args, limits.nice);
    for (key, _) in std::env::vars_os() {
        let upper = key.to_string_lossy().to_ascii_uppercase();
        if is_hashi_secret(&key) || remove.iter().any(|r| env_name_matches(r, &upper)) {
            cmd.env_remove(&key);
        }
    }
    cmd.current_dir(workdir)
        .envs(base_env())
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
    #[cfg(unix)]
    if let Some(pid) = pid {
        super::tree::register_leader(pid);
    }
    #[cfg(windows)]
    let job = contain(&child);
    let scope = Arc::new(Mutex::new(RunScope::new(pid.unwrap_or(0))));
    let mut proc_ = Supervised {
        child,
        pid,
        exited: false,
        stopped: false,
        scope: Arc::clone(&scope),
        #[cfg(windows)]
        job,
    };

    let (Some(stdout), Some(stderr)) = (proc_.child.stdout.take(), proc_.child.stderr.take())
    else {
        proc_.finish().await;
        return Err(AgentError::Io(std::io::Error::other(
            "child pipes unavailable",
        )));
    };

    // `proc_` stays outside the future, so after a timeout or memory stop the
    // tree is stopped by `finish` below. If the whole `run` future is dropped
    // instead, dropping `proc_` stops it.
    let max_output = limits.max_output;
    let outcome = {
        let child = &mut proc_.child;
        let work = async move {
            let (out, err) = tokio::try_join!(
                read_capped(stdout, max_output),
                read_capped(stderr, max_output),
            )?;
            let status = child.wait().await?;
            Ok::<_, AgentError>((status, out, err))
        };
        tokio::select! {
            done = tokio::time::timeout(limits.timeout, work) => {
                done.unwrap_or(Err(AgentError::Timeout(limits.timeout)))
            }
            (used, limit) = watch_memory(pid, limits.memory, scope) => {
                Err(AgentError::MemoryLimit { used, limit })
            }
        }
    };
    if outcome.is_ok() {
        proc_.mark_exited();
    }
    proc_.finish().await;
    let (status, stdout, stderr) = outcome?;

    Ok(RawOutput {
        success: status.success(),
        status: status.to_string(),
        stdout: into_text(stdout),
        stderr: into_text(stderr),
    })
}

/// Decodes captured output like [`String::from_utf8_lossy`], but reuses the
/// buffer when it is valid UTF-8 (the usual case), so a large output is not
/// held twice.
fn into_text(bytes: Vec<u8>) -> String {
    String::from_utf8(bytes).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
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

#[cfg(test)]
mod env_tests {
    use super::*;

    #[test]
    fn hashi_secrets_are_recognised() {
        for name in [
            "HASHI_TUNNEL_TOKEN",
            "HASHI_HOME",
            "hashi_admin_token",
            "TUNNEL_TOKEN",
            "tunnel_token",
        ] {
            assert!(is_hashi_secret(OsStr::new(name)), "{name}");
        }
        for name in [
            "PATH",
            "HOME",
            "ANTHROPIC_API_KEY",
            "OPENAI_API_KEY",
            "LANG",
            "MY_HASHI_THING",
            "TUNNEL_TOKENS",
        ] {
            assert!(!is_hashi_secret(OsStr::new(name)), "{name}");
        }
    }

    #[test]
    fn output_is_decoded_like_from_utf8_lossy() {
        for bytes in [
            b"plain ascii".to_vec(),
            "caf\u{e9} \u{1f980}".as_bytes().to_vec(),
            b"bad \xff byte \xc3".to_vec(),
            Vec::new(),
        ] {
            let expected = String::from_utf8_lossy(&bytes).into_owned();
            assert_eq!(into_text(bytes), expected);
        }
    }

    #[test]
    fn valid_output_keeps_its_buffer() {
        let bytes = b"{\"type\":\"result\"}".to_vec();
        let ptr = bytes.as_ptr();
        let text = into_text(bytes);
        assert_eq!(text.as_ptr(), ptr);
    }

    #[test]
    fn base_env_disables_cwd_lookup_on_windows() {
        let env = base_env();
        assert!(env.contains(&("NO_COLOR", "1")));
        assert!(env.contains(&("TERM", "dumb")));
        assert_eq!(
            env.contains(&("NoDefaultCurrentDirectoryInExePath", "1")),
            cfg!(windows)
        );
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// Forks an intermediate that calls `setsid` and forks again, the usual
    /// way to daemonise. Arguments: the file the daemon writes its pid to,
    /// how long the intermediate and the leader live (seconds), and when and
    /// how many MiB the daemon allocates (0 for none). The daemon itself
    /// lives for a minute unless something kills it.
    const DAEMONIZE: &str = r#"
use POSIX ();
my ($pidfile, $mid, $lead, $hog_after, $hog_mib) = @ARGV;
my $a = fork;
die "fork: $!" unless defined $a;
if ($a == 0) {
    POSIX::setsid() or die "setsid: $!";
    open STDOUT, '>', '/dev/null';
    open STDERR, '>', '/dev/null';
    my $d = fork;
    die "fork: $!" unless defined $d;
    if ($d == 0) {
        open STDIN, '<', '/dev/null';
        open my $f, '>', "$pidfile.tmp" or die;
        print $f $$;
        close $f;
        rename "$pidfile.tmp", $pidfile;
        if ($hog_mib > 0) {
            select(undef, undef, undef, $hog_after);
            my $x = 'a' x ($hog_mib * 1024 * 1024);
            sleep 60;
        }
        sleep 60;
        POSIX::_exit(0);
    }
    select(undef, undef, undef, $mid);
    POSIX::_exit(0);
}
select(undef, undef, undef, $lead);
exit 0;
"#;

    /// Kills the daemon whatever the test outcome, so no test leaks one.
    struct Daemon {
        pidfile: std::path::PathBuf,
        _dir: tempfile::TempDir,
    }

    // Test setup: failing loudly is the point.
    #[allow(clippy::unwrap_used, clippy::panic)]
    impl Daemon {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            Self {
                pidfile: dir.path().join("daemon.pid"),
                _dir: dir,
            }
        }

        fn args(&self, mid: f64, lead: f64, hog_after: f64, hog_mib: u32) -> Vec<OsString> {
            vec![
                "-e".into(),
                DAEMONIZE.into(),
                self.pidfile.clone().into(),
                mid.to_string().into(),
                lead.to_string().into(),
                hog_after.to_string().into(),
                hog_mib.to_string().into(),
            ]
        }

        async fn pid(&self) -> String {
            for _ in 0..50 {
                if let Ok(pid) = std::fs::read_to_string(&self.pidfile) {
                    return pid.trim().to_owned();
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            panic!("the daemon never wrote its pid");
        }
    }

    impl Drop for Daemon {
        fn drop(&mut self) {
            if let Ok(pid) = std::fs::read_to_string(&self.pidfile)
                && let Ok(pid) = pid.trim().parse::<i32>()
                && pid > 1
            {
                let _ = nix::sys::signal::kill(
                    nix::unistd::Pid::from_raw(pid),
                    nix::sys::signal::Signal::SIGKILL,
                );
            }
        }
    }

    fn perl() -> Option<PathBuf> {
        super::super::locate("perl")
    }

    fn sampled(every_ms: u64, limit: u64) -> Limits {
        Limits {
            timeout: Duration::from_secs(20),
            memory: Some(MemoryWatch {
                limit,
                every: Duration::from_millis(every_ms),
                probe: Arc::new(crate::guard::SystemProbe::new()),
            }),
            ..Limits::default()
        }
    }

    #[tokio::test]
    async fn detached_daemon_dies_when_the_run_exits() {
        let Some(perl) = perl() else { return };
        let daemon = Daemon::new();
        let out = run(
            &perl,
            &daemon.args(1.0, 1.5, 0.0, 0),
            &[],
            Path::new("."),
            sampled(50, u64::MAX),
        )
        .await
        .unwrap();
        assert!(out.success, "{out:?}");
        let pid = daemon.pid().await;
        assert!(wait_gone(&pid).await, "detached daemon {pid} survived");
    }

    #[tokio::test]
    async fn detached_daemon_dies_when_the_run_times_out() {
        let Some(perl) = perl() else { return };
        let daemon = Daemon::new();
        // Long enough that a slow CI machine has started the daemon (and it
        // has written its pid) before the timeout kills the run; otherwise
        // the test cannot tell "killed" from "never started".
        let limits = Limits {
            timeout: Duration::from_millis(4_000),
            ..sampled(50, u64::MAX)
        };
        let err = run(
            &perl,
            &daemon.args(0.5, 30.0, 0.0, 0),
            &[],
            Path::new("."),
            limits,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, AgentError::Timeout(_)), "{err}");
        let pid = daemon.pid().await;
        assert!(wait_gone(&pid).await, "detached daemon {pid} survived");
    }

    #[tokio::test]
    async fn detached_memory_hog_trips_the_limit() {
        let Some(perl) = perl() else { return };
        let daemon = Daemon::new();
        // The intermediate exits at 0.5 s and the daemon only grows at 1.5 s,
        // so by then nothing links it to the leader but its session.
        let started = std::time::Instant::now();
        let err = run(
            &perl,
            &daemon.args(0.5, 30.0, 1.5, 300),
            &[],
            Path::new("."),
            sampled(50, 100 * crate::guard::MIB),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, AgentError::MemoryLimit { .. }), "{err}");
        assert!(started.elapsed() < Duration::from_secs(15));
        let pid = daemon.pid().await;
        assert!(wait_gone(&pid).await, "detached daemon {pid} survived");
    }

    /// With no sample at all between the double fork and the exit, only the
    /// subreaper links the daemon to the run.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn unsampled_detached_daemon_dies_on_linux() {
        let Some(perl) = perl() else { return };
        let daemon = Daemon::new();
        let out = run(
            &perl,
            &daemon.args(0.0, 0.5, 0.0, 0),
            &[],
            Path::new("."),
            Limits::default(),
        )
        .await
        .unwrap();
        assert!(out.success, "{out:?}");
        let pid = daemon.pid().await;
        // Other tests' runs may still be in progress, and an orphan nobody
        // recorded is only claimed once a run is alone, so allow for them.
        assert!(
            wait_gone_within(&pid, Duration::from_secs(60)).await,
            "detached daemon {pid} survived"
        );
    }

    /// An orphan that no run recorded must not be charged to, or killed by,
    /// a different run that happens to be in progress at the same time.
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn concurrent_runs_are_not_charged_for_each_others_orphans() {
        let Some(perl) = perl() else { return };
        let daemon = Daemon::new();
        let hog_args = daemon.args(0.0, 3.0, 0.2, 300);
        let quiet_args: [OsString; 2] = ["-c".into(), "sleep 2".into()];
        let hog = run(&perl, &hog_args, &[], Path::new("."), Limits::default());
        let quiet = run(
            Path::new("/bin/sh"),
            &quiet_args,
            &[],
            Path::new("."),
            sampled(50, 100 * crate::guard::MIB),
        );
        let (hog, quiet) = tokio::join!(hog, quiet);
        assert!(hog.is_ok(), "{hog:?}");
        let quiet = quiet.unwrap();
        assert!(quiet.success, "{quiet:?}");
        let pid = daemon.pid().await;
        assert!(
            wait_gone_within(&pid, Duration::from_secs(60)).await,
            "detached daemon {pid} survived"
        );
    }

    /// Runs itself again with secrets in the environment and checks what an
    /// agent child sees.
    #[tokio::test]
    async fn run_hides_hashi_secrets_from_the_child() {
        const MARK: &str = "HASHI_RUNNER_SECRET_PROBE";
        if std::env::var_os(MARK).is_some() {
            let out = run(
                Path::new("/bin/sh"),
                &sh(
                    "printf 'seen=[%s|%s|%s|%s] kept=[%s]' \"$HASHI_TUNNEL_TOKEN\" \
                     \"$TUNNEL_TOKEN\" \"$HASHI_HOME\" \"$HASHI_RUNNER_SECRET_PROBE\" \"$KEEP_ME\"",
                ),
                &[],
                Path::new("."),
                Limits::default(),
            )
            .await
            .unwrap();
            println!("{}", out.stdout);
            return;
        }
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "agent::runner::tests::run_hides_hashi_secrets_from_the_child",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(MARK, "1")
            .env("HASHI_TUNNEL_TOKEN", "named-tunnel-secret")
            .env("TUNNEL_TOKEN", "cloudflared-secret")
            .env("HASHI_HOME", "/srv/hashi")
            .env("KEEP_ME", "visible")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "{stdout}");
        assert!(stdout.contains("seen=[|||] kept=[visible]"), "{stdout}");
    }

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
        // `nice -n` is relative to Hashi's own niceness, which is not always
        // 0 (CI runners can start tests at a negative value), so compare with
        // a run at the same base.
        let niceness = |nice| async move {
            let limits = Limits {
                nice,
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
            out.stdout.trim().parse::<i32>().unwrap()
        };
        let base = niceness(0).await;
        let lowered = niceness(7).await;
        assert_eq!(
            lowered,
            (base + 7).min(19),
            "base {base}, lowered {lowered}"
        );
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
        wait_gone_within(pid, Duration::from_secs(5)).await
    }

    pub(crate) async fn wait_gone_within(pid: &str, limit: Duration) -> bool {
        let started = std::time::Instant::now();
        while started.elapsed() < limit {
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
    fn env_patterns_match_names_and_prefixes() {
        assert!(env_name_matches("OPENAI_API_KEY", "openai_api_key"));
        assert!(!env_name_matches("OPENAI_API_KEY", "OPENAI_API_KEY_2"));
        assert!(env_name_matches("CRUSH_*", "CRUSH_HOME"));
        assert!(env_name_matches("CRUSH_*", "crush_anthropic_api_key"));
        assert!(!env_name_matches("CRUSH_*", "CRUSH"));
        assert!(!env_name_matches("CRUSH_*", "XCRUSH_HOME"));
    }

    #[test]
    fn summarize_prefers_stderr() {
        assert_eq!(summarize(" boom ", "out").as_deref(), Some("boom"));
        assert_eq!(summarize("", "out").as_deref(), Some("out"));
        assert_eq!(summarize("", " "), None);
    }
}
