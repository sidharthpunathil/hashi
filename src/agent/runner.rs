//! Spawns an agent process with hard limits on time and output size.

use std::ffi::OsString;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;

use super::AgentError;

/// Resource limits for a single run.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Wall-clock budget. The child is killed when it runs out.
    pub timeout: Duration,
    /// Maximum bytes accepted on each of stdout and stderr.
    pub max_output: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(300),
            max_output: 10 * 1024 * 1024,
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

/// Runs `bin` with `args` in `workdir`.
///
/// The child never sees a shell, gets a null stdin, and is killed if the
/// returned future is dropped, times out, or floods its pipes.
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
    let mut child = Command::new(bin)
        .args(args)
        .current_dir(workdir)
        .env("NO_COLOR", "1")
        .env("TERM", "dumb")
        .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|source| AgentError::Spawn {
            bin: bin.display().to_string(),
            source,
        })?;

    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        return Err(AgentError::Io(std::io::Error::other(
            "child pipes unavailable",
        )));
    };

    // The child is moved into the future so that dropping it (on timeout or
    // cancellation) drops the child too, which triggers `kill_on_drop`.
    let work = async move {
        let (out, err) = tokio::try_join!(
            read_capped(stdout, limits.max_output),
            read_capped(stderr, limits.max_output),
        )?;
        let status = child.wait().await?;
        Ok::<_, AgentError>((status, out, err))
    };

    let (status, stdout, stderr) = tokio::time::timeout(limits.timeout, work)
        .await
        .map_err(|_| AgentError::Timeout(limits.timeout))??;

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

    #[test]
    fn summarize_prefers_stderr() {
        assert_eq!(summarize(" boom ", "out").as_deref(), Some("boom"));
        assert_eq!(summarize("", "out").as_deref(), Some("out"));
        assert_eq!(summarize("", " "), None);
    }
}
