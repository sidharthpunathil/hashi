//! Optional public HTTPS endpoint via a Cloudflare quick tunnel.
//!
//! Hashi never downloads `cloudflared` on its own: running an unverified binary
//! fetched at runtime is a supply-chain risk. Install it with your package
//! manager (`brew install cloudflared`) and Hashi will find it.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::net::TunnelTrust;

/// Cloudflare's download page for `cloudflared`.
pub const DOWNLOADS_URL: &str =
    "https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/downloads/";

/// How to install `cloudflared` on this platform.
#[must_use]
pub fn install_hint() -> String {
    if cfg!(target_os = "macos") {
        "brew install cloudflared".to_owned()
    } else {
        format!("download it from {DOWNLOADS_URL}")
    }
}

/// Tunnel failures.
#[derive(Debug, thiserror::Error)]
pub enum TunnelError {
    /// `cloudflared` is not installed.
    #[error(
        "cloudflared not found; install it ({}) or run without --tunnel",
        install_hint()
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

/// Where the public tunnel is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Status {
    /// None was asked for, or it has not been started yet.
    #[default]
    Off,
    /// Waiting for cloudflared.
    Starting,
    /// Serving at this URL.
    Up(String),
    /// Failed or closed, for this reason.
    Down(String),
}

/// Why a tunnel could not be started on request.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StartError {
    /// One is already on its way up.
    #[error("the tunnel is already starting")]
    Starting,
    /// One is already serving.
    #[error("the tunnel is already live at {0}")]
    Running(String),
    /// `--public-url` is set, so invite links already use it.
    #[error("a public URL is configured ({0}); invite links already use it")]
    Configured(String),
    /// `cloudflared` is not installed.
    #[error("cloudflared is not installed; {}", install_hint())]
    NotInstalled,
    /// This server has nothing a tunnel could point at.
    #[error("this server cannot start a tunnel")]
    Unavailable,
}

/// What a tunnel started by [`Control`] points at.
#[derive(Debug, Clone)]
pub struct Settings {
    /// Local listen port.
    pub port: u16,
    /// Whether Hashi serves HTTPS itself.
    pub local_tls: bool,
    /// Data directory, for cloudflared's isolated config.
    pub home: PathBuf,
    /// The `--public-url`, when set. It wins over any tunnel URL.
    pub configured_url: Option<String>,
    /// Named tunnel token (`--tunnel-token`), used only for the tunnel
    /// configured at startup.
    pub named_token: Option<String>,
    /// A tunnel was configured at startup (`--tunnel` or `--tunnel-token`):
    /// `CF-Connecting-IP` stays trusted for the server's whole life, as it
    /// always has.
    pub configured: bool,
    /// Stops the tunnel on shutdown.
    pub cancel: CancellationToken,
}

#[derive(Debug, Default)]
struct Slot {
    status: Status,
    task: Option<JoinHandle<()>>,
}

/// One server's tunnel: its status, the `CF-Connecting-IP` trust switch and
/// the task running cloudflared. At most one tunnel runs at a time.
///
/// The default control has no [`Settings`] and never starts anything.
#[derive(Debug, Default)]
pub struct Control {
    settings: Option<Settings>,
    trust: TunnelTrust,
    slot: Mutex<Slot>,
}

impl Control {
    /// A control for a server. When a tunnel is configured, trust is turned
    /// on right away.
    #[must_use]
    pub fn new(settings: Settings, trust: TunnelTrust) -> Self {
        if settings.configured {
            trust.set(true);
        }
        Self {
            settings: Some(settings),
            trust,
            slot: Mutex::default(),
        }
    }

    fn slot(&self) -> MutexGuard<'_, Slot> {
        self.slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Current status.
    #[must_use]
    pub fn status(&self) -> Status {
        self.slot().status.clone()
    }

    /// The `CF-Connecting-IP` trust switch the server reads per request.
    #[must_use]
    pub fn trust(&self) -> &TunnelTrust {
        &self.trust
    }

    /// The settings, when this control can start a tunnel.
    #[must_use]
    pub fn settings(&self) -> Option<&Settings> {
        self.settings.as_ref()
    }

    /// Claims the right to start a tunnel: fails when one is starting or
    /// up, then marks it starting and trusts `CF-Connecting-IP`.
    ///
    /// `on_demand` is set for a tunnel the owner asks for on a running
    /// server. It is refused when a public URL is configured or
    /// `cloudflared` is missing; the startup tunnel reports those as
    /// [`crate::events::Event::TunnelDown`] instead, as before.
    ///
    /// # Errors
    ///
    /// See [`StartError`].
    pub fn begin(&self, on_demand: bool) -> Result<&Settings, StartError> {
        let settings = self.settings.as_ref().ok_or(StartError::Unavailable)?;
        let mut slot = self.slot();
        match &slot.status {
            Status::Starting => return Err(StartError::Starting),
            Status::Up(url) => return Err(StartError::Running(url.clone())),
            Status::Off | Status::Down(_) => {}
        }
        if on_demand {
            if let Some(url) = &settings.configured_url {
                return Err(StartError::Configured(url.clone()));
            }
            if locate().is_none() {
                return Err(StartError::NotInstalled);
            }
        }
        slot.status = Status::Starting;
        self.trust.set(true);
        Ok(settings)
    }

    /// Records the tunnel's new status. Once it is down, trust goes back
    /// off unless a tunnel was configured at startup.
    pub fn set_status(&self, status: Status) {
        let down = matches!(status, Status::Down(_) | Status::Off);
        self.slot().status = status;
        if down && !self.settings.as_ref().is_some_and(|s| s.configured) {
            self.trust.set(false);
        }
    }

    /// Keeps the task running the tunnel, so shutdown can wait for it.
    pub fn set_task(&self, task: JoinHandle<()>) {
        // An earlier attempt has already reported itself down; let it finish.
        drop(self.slot().task.replace(task));
    }

    /// Waits for the tunnel task to stop. Call after cancelling.
    pub async fn join(&self) {
        let task = self.slot().task.take();
        if let Some(task) = task {
            let _ = task.await;
        }
    }
}

/// The warning printed when [`token_in_argv`] is true.
pub const ARGV_TOKEN_WARNING: &str = "warning: --tunnel-token on the command line is visible to \
     other users of this machine in the process list; set HASHI_TUNNEL_TOKEN in the environment \
     instead";

/// Whether the tunnel token was given as a `--tunnel-token` flag rather than
/// through `HASHI_TUNNEL_TOKEN`. Arguments are readable by every local user
/// (`ps`), so the flag form leaks the token.
pub fn token_in_argv(args: impl IntoIterator<Item = std::ffi::OsString>) -> bool {
    args.into_iter().take_while(|a| a != "--").any(|a| {
        a.to_str()
            .is_some_and(|a| a == "--tunnel-token" || a.starts_with("--tunnel-token="))
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

    #[test]
    fn spots_a_tunnel_token_on_the_command_line() {
        let argv = |a: &[&str]| a.iter().map(std::ffi::OsString::from).collect::<Vec<_>>();
        assert!(token_in_argv(argv(&[
            "hashi",
            "serve",
            "--tunnel-token",
            "x"
        ])));
        assert!(token_in_argv(argv(&["hashi", "serve", "--tunnel-token=x"])));
        assert!(!token_in_argv(argv(&["hashi", "serve", "--tunnel"])));
        assert!(!token_in_argv(argv(&[
            "hashi",
            "serve",
            "--public-url",
            "https://x"
        ])));
        // After `--` it is a value, not a flag.
        assert!(!token_in_argv(argv(&[
            "hashi",
            "ask",
            "--",
            "--tunnel-token"
        ])));
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

    fn settings(configured: bool, configured_url: Option<&str>) -> Settings {
        Settings {
            port: 7878,
            local_tls: false,
            home: std::env::temp_dir(),
            configured_url: configured_url.map(str::to_owned),
            named_token: None,
            configured,
            cancel: CancellationToken::new(),
        }
    }

    #[test]
    fn trust_turns_on_only_once_a_tunnel_is_configured_or_started() {
        // No tunnel configured: as strict as ever.
        let control = Control::new(settings(false, None), TunnelTrust::default());
        assert!(!control.trust().get());
        assert_eq!(control.status(), Status::Off);
        // Starting one (the startup path, which never looks for cloudflared).
        control.begin(false).unwrap();
        assert!(control.trust().get());
        assert_eq!(control.status(), Status::Starting);
        // Once it is down, trust goes back off.
        control.set_status(Status::Down("cloudflared exited".into()));
        assert!(!control.trust().get());

        // Configured at startup: trusted from the start and for good.
        let control = Control::new(settings(true, None), TunnelTrust::default());
        assert!(control.trust().get());
        control.set_status(Status::Down("cloudflared exited".into()));
        assert!(control.trust().get());
    }

    #[test]
    fn a_second_tunnel_is_refused_while_one_is_starting_or_up() {
        let control = Control::new(settings(false, None), TunnelTrust::default());
        control.begin(false).unwrap();
        assert_eq!(control.begin(false).unwrap_err(), StartError::Starting);
        assert_eq!(control.begin(true).unwrap_err(), StartError::Starting);
        let url = "https://calm-river-bridge.trycloudflare.com".to_owned();
        control.set_status(Status::Up(url.clone()));
        assert_eq!(control.begin(false).unwrap_err(), StartError::Running(url));
        // After it goes down it may be started again.
        control.set_status(Status::Down("gone".into()));
        assert!(control.begin(false).is_ok());
    }

    #[test]
    fn on_demand_defers_to_a_configured_public_url() {
        let url = "https://hashi.example.com";
        let control = Control::new(settings(false, Some(url)), TunnelTrust::default());
        assert_eq!(
            control.begin(true).unwrap_err(),
            StartError::Configured(url.to_owned())
        );
        assert!(!control.trust().get());
        assert_eq!(control.status(), Status::Off);
    }

    #[test]
    fn a_default_control_never_starts_anything() {
        let control = Control::default();
        assert_eq!(control.begin(true).unwrap_err(), StartError::Unavailable);
        assert!(!control.trust().get());
    }

    #[test]
    fn install_hint_names_a_command_or_the_download_page() {
        let hint = install_hint();
        if cfg!(target_os = "macos") {
            assert_eq!(hint, "brew install cloudflared");
        } else {
            assert!(hint.contains(DOWNLOADS_URL));
        }
        assert!(TunnelError::NotInstalled.to_string().contains(&hint));
    }

    #[test]
    fn extract_url_ignores_other_urls() {
        assert_eq!(
            extract_url("see https://developers.cloudflare.com/docs"),
            None
        );
    }
}
