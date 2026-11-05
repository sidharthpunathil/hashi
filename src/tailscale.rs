//! Optional public route through Tailscale Serve (your tailnet) or Funnel
//! (the internet), at `https://<machine>.<tailnet>.ts.net`.
//!
//! What Hashi relies on, and where it was checked (2026-11-06):
//!
//! - `tailscale serve|funnel --bg --https=<port> http://127.0.0.1:<port>`
//!   sets a persistent listener and returns; `--https` is the default
//!   listener type and only `http://127.0.0.1` proxy targets are supported
//!   (<https://tailscale.com/kb/1242/tailscale-serve>,
//!   <https://tailscale.com/docs/reference/tailscale-cli/serve>).
//! - Turning one listener off is the command that turned it on plus `off`,
//!   with the original flags; the target is optional
//!   (`tailscale serve --https=443 off`). `reset` clears the whole Serve
//!   config, so Hashi never runs it.
//! - Funnel listens only on 443, 8443 and 10000 and needs `MagicDNS`, HTTPS
//!   certificates and the `funnel` node attribute
//!   (<https://tailscale.com/kb/1223/funnel>). When the node lacks them the
//!   CLI prints the control server's text and an approval URL on stdout and
//!   either waits for approval or exits (`enableFeatureInteractive` in
//!   `cmd/tailscale/cli/serve_legacy.go`).
//! - `tailscale serve status --json` prints the node's `ipn.ServeConfig`
//!   (`TCP` keyed by port, `Web` keyed by `"<host>:<port>"` with `Handlers`
//!   keyed by mount point, each with a `Proxy`, `AllowFunnel`, and
//!   `Foreground` for listeners held by a running `tailscale serve`).
//! - `tailscale status --json` has `BackendState` (`Running` when signed in
//!   and connected; also `NeedsLogin`, `NeedsMachineAuth`, `Stopped`,
//!   `Starting`, `NoState`) and `Self.DNSName`, which ends with a dot
//!   (`ipn/ipnstate`). The JSON format is documented as subject to change,
//!   so everything here is read defensively.
//! - The proxy sets `X-Forwarded-For` to the client's address
//!   (`ipn/ipnlocal/serve.go`), and Serve adds `Tailscale-User-*` identity
//!   headers, which Hashi ignores: a Hashi token is still required.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use crate::net::TunnelTrust;
use crate::tunnel::{StartError, Status};

/// How long the command that turns a listener on may take, including any
/// wait for the owner to approve Funnel in the browser.
pub const START_TIMEOUT: Duration = Duration::from_secs(60);
/// How long a status query or an `off` command may take.
pub const QUERY_TIMEOUT: Duration = Duration::from_secs(15);
/// How often a running route is checked, so a listener removed from
/// outside Hashi reads as down.
pub const RECHECK_EVERY: Duration = Duration::from_secs(30);
/// The default HTTPS port, also the only one whose URL needs no port.
pub const DEFAULT_PORT: u16 = 443;
/// Ports Funnel can listen on.
pub const FUNNEL_PORTS: [u16; 3] = [443, 8443, 10000];

/// Who the route is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, clap::ValueEnum)]
pub enum Mode {
    /// Devices on your tailnet, and people you share the machine with.
    Serve,
    /// Anyone on the internet.
    Funnel,
}

impl Mode {
    /// The `tailscale` subcommand, which is also how the mode is named.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Serve => "serve",
            Self::Funnel => "funnel",
        }
    }
}

impl std::fmt::Display for Mode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The arguments that turn the listener on.
#[must_use]
pub fn up_args(mode: Mode, https_port: u16, local_port: u16) -> Vec<String> {
    vec![
        mode.as_str().to_owned(),
        "--bg".to_owned(),
        format!("--https={https_port}"),
        target(local_port),
    ]
}

/// The arguments that turn exactly that listener off again: the same
/// command and flags plus `off`. Never `reset`.
#[must_use]
pub fn off_args(mode: Mode, https_port: u16) -> Vec<String> {
    vec![
        mode.as_str().to_owned(),
        format!("--https={https_port}"),
        "off".to_owned(),
    ]
}

/// The full `off` command, as the owner would type it.
#[must_use]
pub fn off_command(mode: Mode, https_port: u16) -> String {
    format!("tailscale {}", off_args(mode, https_port).join(" "))
}

/// The proxy target for Hashi's local port. Tailscale proxies only to
/// `http://127.0.0.1`.
#[must_use]
pub fn target(local_port: u16) -> String {
    format!("http://127.0.0.1:{local_port}")
}

/// The public URL for a device name and HTTPS port.
#[must_use]
pub fn url_for(dns_name: &str, https_port: u16) -> String {
    if https_port == DEFAULT_PORT {
        format!("https://{dns_name}")
    } else {
        format!("https://{dns_name}:{https_port}")
    }
}

/// How to sign in on this platform.
#[must_use]
pub fn sign_in_hint() -> &'static str {
    if cfg!(any(target_os = "macos", windows)) {
        "sign in from the Tailscale app, or run: tailscale up"
    } else {
        "run: sudo tailscale up"
    }
}

/// This device as `tailscale status --json` describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelfStatus {
    /// `BackendState`, such as `Running` or `NeedsLogin`.
    pub backend_state: String,
    /// `Self.DNSName` without its trailing dot, when the device has one.
    pub dns_name: Option<String>,
}

impl SelfStatus {
    /// Signed in and connected.
    #[must_use]
    pub fn running(&self) -> bool {
        self.backend_state == "Running"
    }
}

/// Reads `tailscale status --json`.
///
/// # Errors
///
/// Fails with a readable reason when the text is not JSON or has no
/// `BackendState`.
pub fn parse_status(json: &str) -> Result<SelfStatus, String> {
    let v: Value = serde_json::from_str(json)
        .map_err(|e| format!("tailscale status --json gave unreadable output ({e})"))?;
    let backend_state = v
        .get("BackendState")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or("tailscale status --json has no BackendState; is this Tailscale too old or new?")?
        .to_owned();
    let dns_name = v
        .get("Self")
        .and_then(|s| s.get("DNSName"))
        .and_then(Value::as_str)
        .map(|n| n.trim().trim_end_matches('.').to_ascii_lowercase())
        .filter(|n| {
            !n.is_empty()
                && n.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
        });
    Ok(SelfStatus {
        backend_state,
        dns_name,
    })
}

/// What is on an HTTPS port, according to `tailscale serve status --json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortUse {
    /// Nothing.
    Free,
    /// Exactly Hashi's proxy to this server and nothing else, for example
    /// left by a run that did not shut down cleanly. Hashi may use it and
    /// turn it off.
    Ours,
    /// Something else, described for the owner.
    Foreign(String),
}

fn proxy_matches(proxy: &str, target: &str) -> bool {
    proxy.trim().trim_end_matches('/') == target
}

/// Checks one Serve config (the top level, or one foreground session).
fn port_use_in(config: &Value, port: u16, target: &str) -> PortUse {
    let key = port.to_string();
    if let Some(tcp) = config.get("TCP").and_then(|t| t.get(&key)) {
        if let Some(fwd) = tcp
            .get("TCPForward")
            .and_then(Value::as_str)
            .filter(|f| !f.is_empty())
        {
            return PortUse::Foreign(format!("a TCP forward to {fwd}"));
        }
        if tcp.get("HTTP").and_then(Value::as_bool) == Some(true) {
            return PortUse::Foreign("a plain HTTP listener".to_owned());
        }
    }
    let suffix = format!(":{port}");
    let mut ours = false;
    if let Some(web) = config.get("Web").and_then(Value::as_object) {
        for (host_port, server) in web {
            if !host_port.ends_with(&suffix) {
                continue;
            }
            let handlers = server.get("Handlers").and_then(Value::as_object);
            let mut found = Vec::new();
            for (mount, h) in handlers.into_iter().flatten() {
                let proxy = h.get("Proxy").and_then(Value::as_str);
                if mount == "/" && proxy.is_some_and(|p| proxy_matches(p, target)) {
                    ours = true;
                } else {
                    let what = proxy
                        .map(str::to_owned)
                        .or_else(|| h.get("Path").and_then(Value::as_str).map(str::to_owned))
                        .unwrap_or_else(|| "a handler".to_owned());
                    found.push(format!("{mount} -> {what}"));
                }
            }
            if !found.is_empty() {
                return PortUse::Foreign(found.join(", "));
            }
        }
    }
    if ours {
        return PortUse::Ours;
    }
    if config.get("TCP").and_then(|t| t.get(&key)).is_some() {
        return PortUse::Foreign("an HTTPS listener".to_owned());
    }
    PortUse::Free
}

/// Reads `tailscale serve status --json` for `port`. Listeners held by a
/// `tailscale serve` running in another terminal (`Foreground`) count as
/// foreign too.
///
/// # Errors
///
/// Fails when the text is not a JSON object.
pub fn port_use(json: &str, port: u16, local_port: u16) -> Result<PortUse, String> {
    let trimmed = json.trim();
    if trimmed.is_empty() {
        return Ok(PortUse::Free);
    }
    let v: Value = serde_json::from_str(trimmed)
        .map_err(|e| format!("tailscale serve status --json gave unreadable output ({e})"))?;
    if !v.is_object() {
        return Err("tailscale serve status --json did not give an object".to_owned());
    }
    let target = target(local_port);
    if let Some(sessions) = v.get("Foreground").and_then(Value::as_object) {
        for session in sessions.values() {
            match port_use_in(session, port, &target) {
                PortUse::Free => {}
                PortUse::Ours => {
                    return Ok(PortUse::Foreign(
                        "a foreground `tailscale serve` in another terminal".to_owned(),
                    ));
                }
                foreign @ PortUse::Foreign(_) => return Ok(foreign),
            }
        }
    }
    Ok(port_use_in(&v, port, &target))
}

/// Pulls the first `https://` URL out of a line of CLI output.
#[must_use]
pub fn find_url(line: &str) -> Option<String> {
    let start = line.find("https://")?;
    let rest = &line[start..];
    let end = rest
        .find(|c: char| c.is_whitespace() || c == '"' || c == '\'' || c == '<' || c == '>')
        .unwrap_or(rest.len());
    let url = rest[..end].trim_end_matches(['.', ',', ')']);
    (url.len() > "https://".len()).then(|| url.to_owned())
}

/// Why a Tailscale route could not be started or stopped.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TsError {
    /// No `tailscale` executable.
    #[error("tailscale is not installed; run hashi install tailscale")]
    NotInstalled,
    /// Signed out, stopped or still starting.
    #[error("Tailscale is not connected ({0}); {hint}", hint = sign_in_hint())]
    SignedOut(String),
    /// Signed in, but the device has no `MagicDNS` name.
    #[error("this device has no Tailscale DNS name; turn on MagicDNS for your tailnet")]
    NoDnsName,
    /// The port already serves something Hashi did not set up.
    #[error(
        "Tailscale already serves {what} on HTTPS port {port}, which Hashi did not set up; \
         choose another port with --tailscale-port"
    )]
    PortTaken {
        /// The HTTPS port.
        port: u16,
        /// What is there.
        what: String,
    },
    /// Funnel cannot use that port.
    #[error("Funnel only listens on ports 443, 8443 and 10000, not {0}")]
    FunnelPort(u16),
    /// The CLI did not finish in time.
    #[error("tailscale {mode} did not finish within {secs} s{}", approval_note(.approval.as_deref()))]
    Timeout {
        /// Serve or Funnel.
        mode: Mode,
        /// The limit.
        secs: u64,
        /// An approval URL it printed, if any.
        approval: Option<String>,
    },
    /// The CLI failed or did not set the listener up.
    #[error("tailscale {mode} failed: {detail}{}", approval_note(.approval.as_deref()))]
    Failed {
        /// Serve or Funnel.
        mode: Mode,
        /// What it said.
        detail: String,
        /// An approval URL it printed, if any.
        approval: Option<String>,
    },
    /// The CLI could not be run, or its output could not be read.
    #[error("{0}")]
    Cli(String),
    /// The owner stopped it while it was starting.
    #[error("stopped before Tailscale finished")]
    Cancelled,
}

fn approval_note(url: Option<&str>) -> String {
    url.map(|u| format!("; approve it at {u} and try again"))
        .unwrap_or_default()
}

/// Runs `tailscale <args>` to completion and returns its stdout.
async fn query(bin: &Path, args: &[&str]) -> Result<String, TsError> {
    let mut cmd = Command::new(bin);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let shown = args.join(" ");
    let out = tokio::time::timeout(QUERY_TIMEOUT, cmd.output())
        .await
        .map_err(|_| {
            TsError::Cli(format!(
                "tailscale {shown} did not answer within {} s",
                QUERY_TIMEOUT.as_secs()
            ))
        })?
        .map_err(|e| TsError::Cli(format!("could not run tailscale: {e}")))?;
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let why = crate::util::one_line(err.trim());
        // `tailscale status --json` exits non-zero while signed out on some
        // versions but still prints the JSON.
        if stdout.trim_start().starts_with('{') {
            return Ok(stdout);
        }
        return Err(TsError::Cli(format!(
            "tailscale {shown} failed ({}): {}",
            out.status,
            crate::util::truncate(&why, 200)
        )));
    }
    Ok(stdout)
}

/// Asks `tailscale status --json` whether this device is signed in.
///
/// # Errors
///
/// Fails when the CLI cannot be run or its output cannot be read.
pub async fn self_status(bin: &Path) -> Result<SelfStatus, TsError> {
    let json = query(bin, &["status", "--json"]).await?;
    parse_status(&json).map_err(TsError::Cli)
}

/// What is on `port` now.
async fn current_use(bin: &Path, port: u16, local_port: u16) -> Result<PortUse, TsError> {
    let json = query(bin, &["serve", "status", "--json"]).await?;
    port_use(&json, port, local_port).map_err(TsError::Cli)
}

/// A route Hashi turned on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Route {
    /// Serve or Funnel.
    pub mode: Mode,
    /// The HTTPS port on the tailnet side.
    pub https_port: u16,
    /// The device's DNS name, which requests carry as their `Host`.
    pub host: String,
    /// `https://<host>[:port]`.
    pub url: String,
}

/// Turns a listener on: checks that Tailscale is signed in, that the port
/// is free (or already Hashi's), runs the `--bg` command within
/// [`START_TIMEOUT`] and confirms the listener is there. Every URL the CLI
/// prints that is not the route itself is passed to `approval` as it
/// appears.
///
/// When `cancel` fires first, the command is stopped and, if the listener
/// made it into the config anyway, turned off again.
///
/// # Errors
///
/// See [`TsError`].
pub async fn start(
    bin: &Path,
    mode: Mode,
    https_port: u16,
    local_port: u16,
    cancel: &CancellationToken,
    mut approval: impl FnMut(String),
) -> Result<Route, TsError> {
    if mode == Mode::Funnel && !FUNNEL_PORTS.contains(&https_port) {
        return Err(TsError::FunnelPort(https_port));
    }
    let me = self_status(bin).await?;
    if !me.running() {
        return Err(TsError::SignedOut(me.backend_state));
    }
    let host = me.dns_name.ok_or(TsError::NoDnsName)?;
    if let PortUse::Foreign(what) = current_use(bin, https_port, local_port).await? {
        return Err(TsError::PortTaken {
            port: https_port,
            what,
        });
    }

    let mut child = Command::new(bin)
        .args(up_args(mode, https_port, local_port))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| TsError::Cli(format!("could not run tailscale: {e}")))?;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    if let Some(out) = child.stdout.take() {
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(out).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                let _ = tx.send(l);
            }
        });
    }
    if let Some(err) = child.stderr.take() {
        let tx = tx.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(err).lines();
            while let Ok(Some(l)) = lines.next_line().await {
                let _ = tx.send(l);
            }
        });
    }
    drop(tx);

    let url = url_for(&host, https_port);
    let mut seen: Option<String> = None;
    let mut tail: Vec<String> = Vec::new();
    let mut note = |line: String, seen: &mut Option<String>, tail: &mut Vec<String>| {
        if let Some(u) = find_url(&line)
            && crate::net::url_host(&u).as_deref() != Some(host.as_str())
            && seen.as_deref() != Some(u.as_str())
        {
            *seen = Some(u.clone());
            approval(u);
        }
        let line = crate::util::one_line(line.trim());
        if !line.is_empty() {
            tail.push(line);
            if tail.len() > 4 {
                tail.remove(0);
            }
        }
    };
    let deadline = tokio::time::sleep(START_TIMEOUT);
    tokio::pin!(deadline);
    let mut open = true;
    let status = loop {
        tokio::select! {
            () = cancel.cancelled() => {
                let _ = child.kill().await;
                if current_use(bin, https_port, local_port).await == Ok(PortUse::Ours) {
                    let _ = stop(bin, mode, https_port).await;
                }
                return Err(TsError::Cancelled);
            }
            () = &mut deadline => {
                let _ = child.kill().await;
                return Err(TsError::Timeout { mode, secs: START_TIMEOUT.as_secs(), approval: seen });
            }
            line = rx.recv(), if open => match line {
                Some(l) => note(l, &mut seen, &mut tail),
                None => open = false,
            },
            status = child.wait(), if !open => {
                break status.map_err(|e| TsError::Cli(format!("waiting for tailscale: {e}")))?;
            }
        }
    };
    if !status.success() {
        return Err(TsError::Failed {
            mode,
            detail: if tail.is_empty() {
                status.to_string()
            } else {
                tail.join(" / ")
            },
            approval: seen,
        });
    }
    // The CLI exits 0 after printing an approval URL it will not wait for,
    // so success means the listener is really there.
    match current_use(bin, https_port, local_port).await? {
        PortUse::Ours => Ok(Route {
            mode,
            https_port,
            host,
            url,
        }),
        _ => Err(TsError::Failed {
            mode,
            detail: "the listener is not in Tailscale's Serve config".to_owned(),
            approval: seen,
        }),
    }
}

/// Turns the listener off with the matching `off` command. Only that
/// listener is touched.
///
/// # Errors
///
/// Fails when the command cannot be run or fails.
pub async fn stop(bin: &Path, mode: Mode, https_port: u16) -> Result<(), TsError> {
    let args = off_args(mode, https_port);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    query(bin, &args).await.map(drop)
}

/// Whether the route is still in Tailscale's config. `None` when that
/// could not be read, which is not taken as down.
pub async fn still_up(bin: &Path, route: &Route, local_port: u16) -> Option<bool> {
    current_use(bin, route.https_port, local_port)
        .await
        .ok()
        .map(|u| u == PortUse::Ours)
}

/// Where a route started by [`Control`] points, and what it may use.
#[derive(Debug, Clone)]
pub struct Settings {
    /// `--tailscale-bin`, when set. Otherwise `tailscale` is looked up.
    pub bin: Option<PathBuf>,
    /// Hashi's local port.
    pub local_port: u16,
    /// The HTTPS port to use (`--tailscale-port`).
    pub https_port: u16,
    /// Why Tailscale cannot reach this server, if it cannot (Hashi serving
    /// HTTPS itself, or listening somewhere `127.0.0.1` does not reach).
    pub unreachable: Option<String>,
    /// Data directory, for the public-url file.
    pub home: PathBuf,
    /// The server-wide shutdown token. Each route gets a child of it.
    pub cancel: CancellationToken,
}

impl Settings {
    /// The `tailscale` executable to run, if there is one.
    #[must_use]
    pub fn locate(&self) -> Option<PathBuf> {
        crate::install::locate(crate::install::Helper::Tailscale, self.bin.as_deref())
    }
}

#[derive(Debug, Default)]
struct Slot {
    status: Status,
    mode: Option<Mode>,
    host: Option<String>,
    approval: Option<String>,
    stop: Option<CancellationToken>,
    task: Option<JoinHandle<()>>,
}

/// One server's Tailscale route: its status, the `X-Forwarded-For` trust
/// switch and the task that owns the listener. At most one runs at a time.
///
/// The default control has no [`Settings`] and never starts anything.
#[derive(Debug, Default)]
pub struct Control {
    settings: Option<Settings>,
    trust: TunnelTrust,
    slot: Mutex<Slot>,
}

impl Control {
    /// A control for a server.
    #[must_use]
    pub fn new(settings: Settings, trust: TunnelTrust) -> Self {
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

    /// The settings, when this control can start a route.
    #[must_use]
    pub fn settings(&self) -> Option<&Settings> {
        self.settings.as_ref()
    }

    /// Current status.
    #[must_use]
    pub fn status(&self) -> Status {
        self.slot().status.clone()
    }

    /// The mode of the route starting or up, or of the last attempt.
    #[must_use]
    pub fn mode(&self) -> Option<Mode> {
        self.slot().mode
    }

    /// The device name requests arrive with while the route is up.
    #[must_use]
    pub fn host(&self) -> Option<String> {
        let slot = self.slot();
        matches!(slot.status, Status::Up(_))
            .then(|| slot.host.clone())
            .flatten()
    }

    /// The last approval URL the CLI printed for the current attempt.
    #[must_use]
    pub fn approval(&self) -> Option<String> {
        self.slot().approval.clone()
    }

    /// Whether a route is starting or up.
    #[must_use]
    pub fn active(&self) -> bool {
        matches!(self.slot().status, Status::Starting | Status::Up(_))
    }

    /// The `X-Forwarded-For` trust switch, on while the route is up.
    #[must_use]
    pub fn trust(&self) -> &TunnelTrust {
        &self.trust
    }

    /// Claims the right to start a route in `mode`. Fails when one is
    /// starting or up, Tailscale cannot reach this server, or `tailscale`
    /// is missing. Returns the settings, the executable and the token that
    /// stops this route.
    ///
    /// # Errors
    ///
    /// See [`StartError`].
    pub fn begin(&self, mode: Mode) -> Result<(Settings, PathBuf, CancellationToken), StartError> {
        let settings = self.settings.as_ref().ok_or(StartError::Unavailable)?;
        if let Some(why) = &settings.unreachable {
            return Err(StartError::Unsupported(why.clone()));
        }
        let mut slot = self.slot();
        match &slot.status {
            Status::Starting => return Err(StartError::Starting),
            Status::Up(url) => return Err(StartError::Running(url.clone())),
            Status::Off | Status::Down(_) => {}
        }
        let bin = settings.locate().ok_or(StartError::TailscaleNotInstalled)?;
        let stop = settings.cancel.child_token();
        slot.status = Status::Starting;
        slot.mode = Some(mode);
        slot.host = None;
        slot.approval = None;
        slot.stop = Some(stop.clone());
        Ok((settings.clone(), bin, stop))
    }

    /// Records an approval URL the CLI printed.
    pub fn set_approval(&self, url: String) {
        self.slot().approval = Some(url);
    }

    /// Records that the route is up at `url`, reached as `host`, and starts
    /// trusting `X-Forwarded-For` from loopback.
    pub fn set_up(&self, url: String, host: String) {
        let mut slot = self.slot();
        slot.status = Status::Up(url);
        slot.host = Some(host);
        slot.approval = None;
        self.trust.set(true);
    }

    /// Records that the route is off or down, and stops trusting the header.
    pub fn set_down(&self, status: Status) {
        let mut slot = self.slot();
        slot.status = status;
        slot.host = None;
        slot.stop = None;
        self.trust.set(false);
    }

    /// Asks the route to stop. Returns whether one was starting or up.
    pub fn stop(&self) -> bool {
        let slot = self.slot();
        match (&slot.status, &slot.stop) {
            (Status::Starting | Status::Up(_), Some(stop)) => {
                stop.cancel();
                true
            }
            _ => false,
        }
    }

    /// Keeps the task owning the route, so shutdown can wait for it.
    pub fn set_task(&self, task: JoinHandle<()>) {
        drop(self.slot().task.replace(task));
    }

    /// Waits for the route's task to finish (after it turned the listener
    /// off). Call after [`Control::stop`] or cancelling the server.
    pub async fn join(&self) {
        let task = self.slot().task.take();
        if let Some(task) = task {
            let _ = task.await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_device_name_without_its_trailing_dot() {
        let me = parse_status(
            r#"{"Version":"1.90.1","BackendState":"Running",
                "Self":{"DNSName":"Laptop.tail1234.ts.net.","HostName":"laptop"}}"#,
        )
        .unwrap();
        assert!(me.running());
        assert_eq!(me.dns_name.as_deref(), Some("laptop.tail1234.ts.net"));
    }

    #[test]
    fn status_parsing_is_defensive() {
        let me = parse_status(r#"{"BackendState":"NeedsLogin","Self":null}"#).unwrap();
        assert!(!me.running());
        assert_eq!(me.dns_name, None);
        let me = parse_status(r#"{"BackendState":"Running","Self":{"DNSName":"a b;c."}}"#).unwrap();
        assert_eq!(me.dns_name, None, "a name with odd characters is refused");
        assert!(parse_status("not json").is_err());
        assert!(
            parse_status(r#"{"Self":{}}"#)
                .unwrap_err()
                .contains("BackendState")
        );
        assert!(parse_status("[]").is_err());
    }

    #[test]
    fn argv_turns_on_and_off_the_same_listener() {
        assert_eq!(
            up_args(Mode::Serve, 443, 7878),
            ["serve", "--bg", "--https=443", "http://127.0.0.1:7878"]
        );
        assert_eq!(
            up_args(Mode::Funnel, 8443, 9000),
            ["funnel", "--bg", "--https=8443", "http://127.0.0.1:9000"]
        );
        assert_eq!(off_args(Mode::Serve, 443), ["serve", "--https=443", "off"]);
        assert_eq!(
            off_command(Mode::Funnel, 10000),
            "tailscale funnel --https=10000 off"
        );
        for args in [off_args(Mode::Serve, 443), off_args(Mode::Funnel, 443)] {
            assert!(!args.iter().any(|a| a == "reset"));
        }
    }

    #[test]
    fn urls_leave_out_the_default_port() {
        assert_eq!(url_for("a.ts.net", 443), "https://a.ts.net");
        assert_eq!(url_for("a.ts.net", 8443), "https://a.ts.net:8443");
    }

    const OURS: &str = r#"{"TCP":{"443":{"HTTPS":true}},
        "Web":{"laptop.tail1234.ts.net:443":{"Handlers":{"/":{"Proxy":"http://127.0.0.1:7878"}}}}}"#;

    #[test]
    fn tells_free_ours_and_foreign_ports_apart() {
        assert_eq!(port_use("{}", 443, 7878), Ok(PortUse::Free));
        assert_eq!(port_use("", 443, 7878), Ok(PortUse::Free));
        assert_eq!(port_use(OURS, 443, 7878), Ok(PortUse::Ours));
        assert_eq!(port_use(OURS, 8443, 7878), Ok(PortUse::Free));
        // Another local port is someone else's proxy.
        assert!(matches!(port_use(OURS, 443, 9000), Ok(PortUse::Foreign(_))));
        let blog = r#"{"TCP":{"443":{"HTTPS":true}},
            "Web":{"laptop.tail1234.ts.net:443":{"Handlers":{"/":{"Proxy":"http://127.0.0.1:3000"}}}}}"#;
        assert_eq!(
            port_use(blog, 443, 7878),
            Ok(PortUse::Foreign("/ -> http://127.0.0.1:3000".to_owned()))
        );
        let extra = r#"{"Web":{"x.ts.net:443":{"Handlers":{
            "/":{"Proxy":"http://127.0.0.1:7878"},"/blog":{"Path":"/srv/blog"}}}}}"#;
        assert!(matches!(
            port_use(extra, 443, 7878),
            Ok(PortUse::Foreign(_))
        ));
        let tcp = r#"{"TCP":{"443":{"TCPForward":"127.0.0.1:22"}}}"#;
        assert_eq!(
            port_use(tcp, 443, 7878),
            Ok(PortUse::Foreign("a TCP forward to 127.0.0.1:22".to_owned()))
        );
        let fg = format!(r#"{{"Foreground":{{"abc":{OURS}}}}}"#);
        assert!(matches!(port_use(&fg, 443, 7878), Ok(PortUse::Foreign(_))));
        assert!(port_use("nope", 443, 7878).is_err());
        assert!(port_use("[1]", 443, 7878).is_err());
    }

    #[test]
    fn finds_urls_in_cli_output() {
        assert_eq!(
            find_url("         https://login.tailscale.com/f/funnel?node=abc").as_deref(),
            Some("https://login.tailscale.com/f/funnel?node=abc")
        );
        assert_eq!(
            find_url("Visit https://example.com/x.").as_deref(),
            Some("https://example.com/x")
        );
        assert_eq!(find_url("no url here"), None);
        assert_eq!(find_url("https://"), None);
    }

    fn settings(bin: Option<PathBuf>, unreachable: Option<&str>) -> Settings {
        Settings {
            bin,
            local_port: 7878,
            https_port: 443,
            unreachable: unreachable.map(str::to_owned),
            home: std::env::temp_dir(),
            cancel: CancellationToken::new(),
        }
    }

    #[test]
    fn begin_refuses_what_cannot_work() {
        assert_eq!(
            Control::default().begin(Mode::Serve).unwrap_err(),
            StartError::Unavailable
        );
        let c = Control::new(
            settings(None, Some("Hashi serves HTTPS itself")),
            TunnelTrust::default(),
        );
        assert!(matches!(
            c.begin(Mode::Serve).unwrap_err(),
            StartError::Unsupported(_)
        ));
        let dir = tempfile::tempdir().unwrap();
        let c = Control::new(
            settings(Some(dir.path().join("missing")), None),
            TunnelTrust::default(),
        );
        assert_eq!(
            c.begin(Mode::Funnel).unwrap_err(),
            StartError::TailscaleNotInstalled
        );
        assert_eq!(c.status(), Status::Off);
    }

    #[test]
    fn trust_follows_the_route() {
        let c = Control::default();
        c.set_up("https://a.ts.net".into(), "a.ts.net".into());
        assert!(c.trust().get());
        assert_eq!(c.host().as_deref(), Some("a.ts.net"));
        c.set_down(Status::Off);
        assert!(!c.trust().get());
        assert_eq!(c.host(), None);
    }
}
