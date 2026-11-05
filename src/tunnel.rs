//! Optional public HTTPS endpoint via a Cloudflare quick tunnel.
//!
//! Hashi never downloads `cloudflared` on its own: running an unverified binary
//! fetched at runtime is a supply-chain risk. Install it with your package
//! manager (`brew install cloudflared`) and Hashi will find it.

use std::path::PathBuf;
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

/// Starts a quick tunnel pointing at `http://127.0.0.1:{port}`.
///
/// # Errors
///
/// See [`TunnelError`].
pub async fn start(port: u16) -> Result<Tunnel, TunnelError> {
    let bin = locate().ok_or(TunnelError::NotInstalled)?;
    let mut child = Command::new(bin)
        .args([
            "tunnel",
            "--no-autoupdate",
            "--url",
            &format!("http://127.0.0.1:{port}"),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;

    let stderr = child.stderr.take().ok_or(TunnelError::Exited)?;
    let mut lines = BufReader::new(stderr).lines();
    let limit = Duration::from_secs(30);

    let url = tokio::time::timeout(limit, async {
        while let Ok(Some(line)) = lines.next_line().await {
            if let Some(url) = extract_url(&line) {
                return Some(url);
            }
        }
        None
    })
    .await
    .map_err(|_| TunnelError::Timeout(limit))?
    .ok_or(TunnelError::Exited)?;

    // Keep draining stderr so cloudflared never blocks on a full pipe.
    tokio::spawn(async move { while let Ok(Some(_)) = lines.next_line().await {} });

    Ok(Tunnel { url, child })
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
    fn extract_url_ignores_other_urls() {
        assert_eq!(
            extract_url("see https://developers.cloudflare.com/docs"),
            None
        );
    }
}
