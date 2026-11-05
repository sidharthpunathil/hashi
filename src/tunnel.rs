//! Optional public HTTPS endpoint via a Cloudflare quick tunnel.
//!
//! Hashi never downloads `cloudflared` on its own: running an unverified binary
//! fetched at runtime is a supply-chain risk. Install it with your package
//! manager (`brew install cloudflared`) and Hashi will find it.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

/// Tunnel failures.
#[derive(Debug, thiserror::Error)]
pub enum TunnelError {
    /// `cloudflared` is not installed.
    #[error(
        "cloudflared not found; install it (e.g. `brew install cloudflared`) or run without --tunnel"
    )]
    NotInstalled,
    /// It could not be started.
    #[error("failed to start cloudflared: {0}")]
    Spawn(#[from] std::io::Error),
    /// It never printed a public URL.
    #[error("cloudflared did not report a public URL within {0:?}")]
    Timeout(Duration),
    /// It exited before printing a URL.
    #[error("cloudflared exited before the tunnel came up")]
    Exited,
}

/// A running tunnel. Dropping it kills `cloudflared`.
#[derive(Debug)]
pub struct Tunnel {
    /// Public HTTPS URL.
    pub url: String,
    /// Keeps the child alive; `kill_on_drop` cleans it up.
    pub child: Child,
}

/// Finds `cloudflared`.
#[must_use]
pub fn locate() -> Option<PathBuf> {
    crate::agent::locate("cloudflared")
}

/// Starts a quick tunnel to the local server and returns its random public URL.
///
/// When Hashi serves HTTPS itself, cloudflared connects over HTTPS and skips
/// certificate checks for that loopback hop only (the certificate is usually
/// issued for a public name, not 127.0.0.1).
///
/// # Errors
///
/// See [`TunnelError`].
pub async fn start(port: u16, local_tls: bool, home: &Path) -> Result<Tunnel, TunnelError> {
    let bin = locate().ok_or(TunnelError::NotInstalled)?;
    let scheme = if local_tls { "https" } else { "http" };
    let mut cmd = Command::new(bin);
    cmd.arg("tunnel")
        .arg("--config")
        .arg(isolated_config(home)?);
    cmd.args([
        "--no-autoupdate",
        "--url",
        &format!("{scheme}://127.0.0.1:{port}"),
    ]);
    if local_tls {
        cmd.arg("--no-tls-verify");
    }
    let mut child = spawn(cmd)?;
    let lines = stderr_lines(&mut child)?;
    let (url, lines) = wait_for(lines, extract_url).await?;
    drain(lines);
    Ok(Tunnel { url, child })
}

/// Runs a named tunnel from the Cloudflare dashboard, which gives a stable
/// hostname. Its ingress (hostname to `http://localhost:<port>`) is set in the
/// dashboard, so the public URL must be configured too.
///
/// The token is passed through the `TUNNEL_TOKEN` environment variable rather
/// than the command line, so other users on the machine cannot read it from
/// the process list.
///
/// # Errors
///
/// See [`TunnelError`].
pub async fn start_named(
    token: &str,
    public_url: &str,
    home: &Path,
) -> Result<Tunnel, TunnelError> {
    let bin = locate().ok_or(TunnelError::NotInstalled)?;
    let mut cmd = Command::new(bin);
    cmd.arg("tunnel")
        .arg("--config")
        .arg(isolated_config(home)?);
    cmd.args(["--no-autoupdate", "run"])
        .env("TUNNEL_TOKEN", token);
    let mut child = spawn(cmd)?;
    let lines = stderr_lines(&mut child)?;
    let ((), lines) = wait_for(lines, |l| {
        l.to_ascii_lowercase()
            .contains("registered tunnel connection")
            .then_some(())
    })
    .await?;
    drain(lines);
    Ok(Tunnel {
        url: public_url.to_owned(),
        child,
    })
}

/// An empty cloudflared config owned by Hashi.
///
/// Without `--config`, cloudflared also loads `~/.cloudflared/config.yml`.
/// If that file belongs to another project, its ingress rules (often a
/// catch-all `http_status:404`) silently override Hashi's tunnel target.
fn isolated_config(home: &Path) -> Result<PathBuf, TunnelError> {
    let path = home.join("cloudflared.yml");
    crate::diagnostics::write_private(
        &path,
        "# Managed by Hashi. Keeps ~/.cloudflared/config.yml out of Hashi's tunnel.\n",
    )?;
    Ok(path)
}

type Lines = tokio::io::Lines<BufReader<tokio::process::ChildStderr>>;

fn spawn(mut cmd: Command) -> Result<Child, TunnelError> {
    Ok(cmd
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?)
}

fn stderr_lines(child: &mut Child) -> Result<Lines, TunnelError> {
    let stderr = child.stderr.take().ok_or(TunnelError::Exited)?;
    Ok(BufReader::new(stderr).lines())
}

async fn wait_for<T>(
    mut lines: Lines,
    mut found: impl FnMut(&str) -> Option<T>,
) -> Result<(T, Lines), TunnelError> {
    let limit = Duration::from_secs(30);
    let hit = tokio::time::timeout(limit, async {
        while let Ok(Some(line)) = lines.next_line().await {
            if let Some(v) = found(&line) {
                return Some(v);
            }
        }
        None
    })
    .await
    .map_err(|_| TunnelError::Timeout(limit))?
    .ok_or(TunnelError::Exited)?;
    Ok((hit, lines))
}

/// Keeps reading stderr so cloudflared never blocks on a full pipe.
fn drain(mut lines: Lines) {
    tokio::spawn(async move { while let Ok(Some(_)) = lines.next_line().await {} });
}

/// Pulls a `https://*.trycloudflare.com` URL out of a log line.
#[must_use]
pub fn extract_url(line: &str) -> Option<String> {
    let start = line.find("https://")?;
    let rest = &line[start..];
    let end = rest
        .find(|c: char| c.is_whitespace() || c == '|' || c == '"')
        .unwrap_or(rest.len());
    let url = &rest[..end];
    url.ends_with(".trycloudflare.com").then(|| url.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_url_finds_quick_tunnel_url() {
        let line = "2026-11-05 INF |  https://calm-river-bridge.trycloudflare.com  |";
        assert_eq!(
            extract_url(line).as_deref(),
            Some("https://calm-river-bridge.trycloudflare.com")
        );
    }

    #[test]
    fn isolated_config_is_an_empty_hashi_owned_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = isolated_config(dir.path()).unwrap();
        let body = std::fs::read_to_string(&path).unwrap();
        // Only a comment: no tunnel, credentials or ingress rules.
        assert!(body.lines().all(|l| l.starts_with('#')));
    }

    #[cfg(unix)]
    #[test]
    fn isolated_config_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = isolated_config(dir.path()).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn extract_url_ignores_other_urls() {
        assert_eq!(
            extract_url("see https://developers.cloudflare.com/docs"),
            None
        );
    }
}
