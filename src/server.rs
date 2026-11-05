//! Wires the store, queue, API and tunnel into one running service.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Instant;

use anyhow::{Context, Result};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::agent::{self, AgentConfig, AgentKind, Limits};
use crate::api::{self, AgentInfo, AppState, RateLimiter, Runtime};
use crate::events::{Bus, Event};
use crate::guard::{ResourceGuard, ResourceSettings};
use crate::net::{LimitedListener, NetLimits};
use crate::queue::{self, Dispatcher, QueueConfig};
use crate::store::{NewToken, Store};
use crate::token::{Role, SessionMode};
use crate::tunnel::{StartError, Status as TunnelStatus};

/// Everything needed to start a server.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Listen address.
    pub bind: SocketAddr,
    /// Data directory (database, workspaces, logs).
    pub home: PathBuf,
    /// Backend settings per agent.
    pub agents: HashMap<AgentKind, AgentConfig>,
    /// Agent used when a request does not name one.
    pub default_agent: AgentKind,
    /// Workers per agent. The total running at once is still capped by
    /// [`ResourceSettings::max_running`].
    pub concurrency: usize,
    /// Machine protection: global run cap, memory reserve, per-run memory
    /// limit, disk floor and priority.
    pub resources: ResourceSettings,
    /// Per-run limits.
    pub limits: Limits,
    /// Authenticated requests per minute per token. Zero disables limiting.
    pub rate_limit: u32,
    /// Maximum pending requests.
    pub queue_limit: u64,
    /// Days to keep finished requests.
    pub retention_days: u32,
    /// Start a public tunnel.
    pub tunnel: bool,
    /// Stable public base URL for links, such as `https://hashi.example.com`.
    pub public_url: Option<String>,
    /// Name shown to people you share with.
    pub name: String,
    /// Serve HTTPS with these files instead of plain HTTP.
    pub tls: Option<crate::tls::TlsFiles>,
    /// Run a named Cloudflare tunnel with this token (needs `public_url`).
    pub tunnel_token: Option<String>,
    /// Check GitHub for a newer release in the background (once a day).
    pub update_check: bool,
    /// How long an account that hit its limit is skipped when its message
    /// gives no reset time.
    pub account_cooldown: std::time::Duration,
}

/// A running server.
#[derive(Debug)]
pub struct Running {
    /// Shared state, also used by the TUI.
    pub state: AppState,
    /// Admin secret minted on first run, shown once.
    pub bootstrap_token: Option<String>,
    /// Fires to stop everything.
    pub cancel: CancellationToken,
    tasks: JoinSet<()>,
    home: PathBuf,
}

impl Running {
    /// Stops all tasks and the tunnel, waits for them to finish and removes
    /// `<home>/public-url` and `<home>/server.pid`.
    pub async fn shutdown(mut self) {
        self.cancel.cancel();
        while self.tasks.join_next().await.is_some() {}
        self.state.runtime.tunnel.join().await;
        if let Err(e) = remove_public_url(&self.home) {
            tracing::warn!(error = %e, "could not remove {PUBLIC_URL_FILE}");
        }
        if let Err(e) = remove_server_pid(&self.home) {
            tracing::warn!(error = %e, "could not remove {PID_FILE}");
        }
    }
}

/// File in the data directory holding the running server's process id, so
/// `hashi uninstall` can refuse while a server is up.
pub const PID_FILE: &str = "server.pid";

/// Records this process as the running server, readable only by its owner.
///
/// # Errors
///
/// Fails if the file cannot be written.
pub fn write_server_pid(home: &Path) -> std::io::Result<()> {
    crate::diagnostics::write_private(&home.join(PID_FILE), &format!("{}\n", std::process::id()))
}

/// The process id a server recorded, if the file holds one.
#[must_use]
pub fn read_server_pid(home: &Path) -> Option<u32> {
    std::fs::read_to_string(home.join(PID_FILE))
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// Removes the recorded process id. A missing file is not an error.
///
/// # Errors
///
/// Fails if the file exists but cannot be removed.
pub fn remove_server_pid(home: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(home.join(PID_FILE)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// File in the data directory holding the running server's public base URL,
/// so `hashi share` in another terminal can print a public link.
pub const PUBLIC_URL_FILE: &str = "public-url";

/// Records `url` as the running server's public base URL, readable only by
/// its owner.
///
/// # Errors
///
/// Fails if the file cannot be written.
pub fn write_public_url(home: &Path, url: &str) -> std::io::Result<()> {
    crate::diagnostics::write_private(&home.join(PUBLIC_URL_FILE), &format!("{url}\n"))
}

/// The public base URL a running server recorded, if any. Anything that is
/// not a single `http(s)://` URL is ignored.
#[must_use]
pub fn read_public_url(home: &Path) -> Option<String> {
    let text = std::fs::read_to_string(home.join(PUBLIC_URL_FILE)).ok()?;
    let url = text.trim().trim_end_matches('/');
    let plain = (url.starts_with("https://") || url.starts_with("http://"))
        && url.len() > "https://".len()
        && !url.chars().any(|c| c.is_whitespace() || c.is_control());
    plain.then(|| url.to_owned())
}

/// Removes the recorded public base URL. A missing file is not an error.
///
/// # Errors
///
/// Fails if the file exists but cannot be removed.
pub fn remove_public_url(home: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(home.join(PUBLIC_URL_FILE)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Starts a quick tunnel on a running server, as `--tunnel` does at startup.
/// The dashboard calls this when the owner presses `t`. Progress arrives on
/// the bus as [`Event::TunnelReady`] or [`Event::TunnelDown`].
///
/// # Errors
///
/// Fails at once, without starting anything, when a tunnel is already
/// starting or up, a public URL is configured, `cloudflared` is missing or
/// there is no Tokio runtime.
pub fn go_public(runtime: &Arc<Runtime>, bus: &Bus) -> Result<(), StartError> {
    launch_tunnel(runtime, bus, true)
}

/// The one path every tunnel takes: claims the [`crate::tunnel::Control`],
/// runs cloudflared until shutdown, publishes the URL unless one is
/// configured, and reports on the bus.
fn launch_tunnel(runtime: &Arc<Runtime>, bus: &Bus, on_demand: bool) -> Result<(), StartError> {
    let settings = runtime.tunnel.begin(on_demand)?.clone();
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        runtime.tunnel.set_status(TunnelStatus::Off);
        return Err(StartError::Unavailable);
    };
    let runtime_task = Arc::clone(runtime);
    let bus = bus.clone();
    let task = handle.spawn(async move {
        let runtime = runtime_task;
        let s = settings;
        let named = s
            .named_token
            .as_ref()
            .zip(s.configured_url.as_ref())
            .filter(|_| !on_demand);
        let starting = async {
            match named {
                Some((token, url)) => crate::tunnel::start_named(token, url, &s.home).await,
                None => crate::tunnel::start(s.port, s.local_tls, &s.home).await,
            }
        };
        let started = tokio::select! {
            () = s.cancel.cancelled() => return,
            started = starting => started,
        };
        let mut tunnel = match started {
            Ok(tunnel) => tunnel,
            Err(e) => {
                runtime.tunnel.set_status(TunnelStatus::Down(e.to_string()));
                bus.emit(Event::TunnelDown(e.to_string()));
                return;
            }
        };
        // A configured public URL wins: invite links must not change.
        let ours = match runtime.public_url.write() {
            Ok(mut url) if url.is_none() => {
                *url = Some(tunnel.url.clone());
                true
            }
            _ => false,
        };
        if ours && let Err(e) = write_public_url(&s.home, &tunnel.url) {
            bus.log(format!("could not record the public URL: {e}"));
        }
        runtime
            .tunnel
            .set_status(TunnelStatus::Up(tunnel.url.clone()));
        bus.emit(Event::TunnelReady(tunnel.url.clone()));
        tokio::select! {
            () = s.cancel.cancelled() => {}
            status = tunnel.child.wait() => {
                // The URL is dead now: links go back to local until the next tunnel.
                if ours {
                    if let Ok(mut url) = runtime.public_url.write()
                        && url.as_deref() == Some(tunnel.url.as_str())
                    {
                        *url = None;
                    }
                    let _ = remove_public_url(&s.home);
                }
                let why = format!("cloudflared exited: {status:?}");
                runtime.tunnel.set_status(TunnelStatus::Down(why.clone()));
                bus.emit(Event::TunnelDown(why));
            }
        }
    });
    runtime.tunnel.set_task(task);
    Ok(())
}

async fn hsts(mut res: axum::response::Response) -> axum::response::Response {
    res.headers_mut().insert(
        axum::http::header::STRICT_TRANSPORT_SECURITY,
        axum::http::HeaderValue::from_static("max-age=31536000"),
    );
    res
}

/// Creates the data directory with owner-only permissions.
///
/// # Errors
///
/// Fails if the directory cannot be created or secured.
pub fn prepare_home(home: &Path) -> Result<()> {
    std::fs::create_dir_all(home).with_context(|| format!("creating {}", home.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(home, std::fs::Permissions::from_mode(0o700))
            .with_context(|| format!("securing {}", home.display()))?;
    }
    #[cfg(windows)]
    win_acl::secure_home(home)?;
    Ok(())
}

/// Owner-only access on Windows, which has no mode bits. The home's ACL is
/// replaced with full control for the current user, SYSTEM and
/// Administrators, inherited by everything created inside it. `icacls` and
/// `whoami` are run by full path with no shell.
#[cfg(any(windows, test))]
mod win_acl {
    use std::ffi::{OsStr, OsString};
    use std::path::Path;

    /// Where the home is relative to the user's profile.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(super) enum Location {
        /// Inside the profile, whose ACL already keeps other users out.
        InProfile,
        /// The profile directory itself: its ACL is left alone.
        Profile,
        /// Anywhere else, where the parent's ACL may let other users in.
        Outside,
        /// A drive root, never a valid home.
        Root,
    }

    fn norm(p: &Path) -> String {
        let s = p.display().to_string();
        let s = s.trim_end_matches(['/', '\\']);
        if cfg!(windows) {
            s.to_lowercase()
        } else {
            s.to_owned()
        }
    }

    pub(super) fn location(home: &Path, profile: Option<&Path>) -> Location {
        if home.parent().is_none() {
            return Location::Root;
        }
        let Some(profile) = profile else {
            return Location::Outside;
        };
        let (home, profile) = (norm(home), norm(profile));
        if home == profile {
            Location::Profile
        } else if home
            .strip_prefix(&profile)
            .is_some_and(|rest| rest.starts_with(['/', '\\']))
        {
            Location::InProfile
        } else {
            Location::Outside
        }
    }

    /// The SID in `whoami /user /fo csv /nh` output, such as
    /// `"host\user","S-1-5-21-..."`. Anything that is not a plain SID is
    /// rejected, so it cannot add arguments to `icacls`.
    pub(super) fn parse_sid(output: &str) -> Option<String> {
        let field = output.trim().rsplit(',').next()?.trim().trim_matches('"');
        let rest = field.strip_prefix("S-1-")?;
        let plain = !rest.is_empty()
            && rest
                .split('-')
                .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()));
        plain.then(|| field.to_owned())
    }

    /// `icacls` arguments that drop inherited entries and grant full control
    /// to `sid`, SYSTEM (S-1-5-18) and Administrators (S-1-5-32-544) only.
    /// SIDs, not names, so it works whatever the system language.
    pub(super) fn grant_args(dir: &OsStr, sid: &str) -> Vec<OsString> {
        let mut args = vec![dir.to_owned()];
        for a in [
            "/inheritance:r".to_owned(),
            "/grant:r".to_owned(),
            format!("*{sid}:(OI)(CI)F"),
            "*S-1-5-18:(OI)(CI)F".to_owned(),
            "*S-1-5-32-544:(OI)(CI)F".to_owned(),
            "/Q".to_owned(),
        ] {
            args.push(a.into());
        }
        args
    }

    #[cfg(windows)]
    fn system_tool(name: &str) -> std::path::PathBuf {
        let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
        Path::new(&root).join("System32").join(name)
    }

    #[cfg(windows)]
    fn run(tool: &str, args: &[OsString]) -> anyhow::Result<String> {
        let out = std::process::Command::new(system_tool(tool))
            .args(args)
            .stdin(std::process::Stdio::null())
            .output()?;
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        if !out.status.success() {
            anyhow::bail!(
                "{tool} failed ({}): {}{}",
                out.status,
                stdout.trim(),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(stdout)
    }

    #[cfg(windows)]
    fn restrict(home: &Path) -> anyhow::Result<()> {
        let who = run(
            "whoami.exe",
            &["/user".into(), "/fo".into(), "csv".into(), "/nh".into()],
        )?;
        let sid = parse_sid(&who).ok_or_else(|| anyhow::anyhow!("whoami gave no SID"))?;
        let dir = std::path::absolute(home)?.into_os_string();
        // Drop explicit entries someone else may have added, then inherited
        // ones, keeping only ours.
        run("icacls.exe", &[dir.clone(), "/reset".into(), "/Q".into()])?;
        run("icacls.exe", &grant_args(&dir, &sid))?;
        Ok(())
    }

    #[cfg(windows)]
    pub(super) fn secure_home(home: &Path) -> anyhow::Result<()> {
        let profile = dirs::home_dir();
        let shown = home.display();
        match location(home, profile.as_deref()) {
            Location::Root => {
                anyhow::bail!("{shown} is a drive root; choose a folder for HASHI_HOME")
            }
            Location::Profile => {
                eprintln!(
                    "warning: HASHI_HOME is your profile folder ({shown}); its permissions are left \
                     as they are. Use a folder inside it, such as the default .hashi."
                );
                Ok(())
            }
            Location::InProfile => {
                if let Err(e) = restrict(home) {
                    eprintln!("warning: could not restrict {shown} to your account: {e:#}");
                }
                Ok(())
            }
            Location::Outside => {
                eprintln!(
                    "warning: HASHI_HOME ({shown}) is outside your user profile. Hashi limits it \
                     to your account, SYSTEM and Administrators, but cannot check who owns it. If \
                     another account created this folder, it can still read and change your \
                     tokens and history. Prefer a folder inside your profile."
                );
                restrict(home).map_err(|e| {
                    e.context(format!(
                        "could not restrict {shown} to your account; refusing to use it"
                    ))
                })
            }
        }
    }
}

/// Opens the database in `home`, restricting it and its WAL and SHM files to
/// their owner.
///
/// # Errors
///
/// Fails if the database cannot be opened or secured.
pub fn open_store(home: &Path) -> Result<Store> {
    prepare_home(home)?;
    let path = home.join("hashi.db");
    // Before SQLite opens it: SQLite creates `-wal` and `-shm` with the mode
    // of the database file, so they start out 0600 too.
    #[cfg(unix)]
    owner_only_file(&path, true)?;
    let store = Store::open(&path).with_context(|| format!("opening {}", path.display()))?;
    // Left over from an earlier run that did not close cleanly.
    #[cfg(unix)]
    for suffix in ["-wal", "-shm"] {
        owner_only_file(&home.join(format!("hashi.db{suffix}")), false)?;
    }
    Ok(store)
}

/// Sets a regular file's mode to 0600, creating it empty first when `create`
/// is set. A missing file is skipped otherwise.
#[cfg(unix)]
fn owner_only_file(path: &Path, create: bool) -> Result<()> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(create)
        .mode(0o600)
        .open(path);
    let file = match file {
        Ok(f) => f,
        Err(e) if !create && e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e).with_context(|| format!("opening {}", path.display())),
    };
    if file.metadata()?.is_file() {
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .with_context(|| format!("securing {}", path.display()))?;
    }
    Ok(())
}

/// Deletes a revoked token's workspace, `<home>/workspaces/<token_id>/`,
/// with every agent's directory in it. Symlinks are removed, never followed,
/// so nothing outside the workspace root is touched. Returns `false` when
/// there was nothing to delete.
///
/// # Errors
///
/// Fails if `token_id` is not a plain id, if the workspace root is not a real
/// directory, or if deleting fails.
pub fn remove_workspace(home: &Path, token_id: &str) -> std::io::Result<bool> {
    remove_token_workspace(&home.join("workspaces"), token_id)
}

/// Like [`remove_workspace`], given the workspace root itself
/// (`<home>/workspaces`) instead of the data directory.
///
/// # Errors
///
/// As for [`remove_workspace`].
pub fn remove_token_workspace(root: &Path, token_id: &str) -> std::io::Result<bool> {
    use std::io::{Error, ErrorKind};
    if token_id.is_empty()
        || !token_id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        return Err(Error::new(ErrorKind::InvalidInput, "not a token id"));
    }
    match std::fs::symlink_metadata(root) {
        Ok(m) if m.is_dir() => {}
        Ok(_) => return Err(Error::other("the workspace root is not a directory")),
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    }
    let dir = root.join(token_id);
    match std::fs::symlink_metadata(&dir) {
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
        // `remove_dir_all` does not follow symlinks inside the tree.
        Ok(m) if m.is_dir() => std::fs::remove_dir_all(&dir).map(|()| true),
        // A link or file in place of the directory: remove the entry itself.
        Ok(_) => std::fs::remove_file(&dir)
            .or_else(|_| std::fs::remove_dir(&dir))
            .map(|()| true),
    }
}

/// Mints an admin token if none exists. Returns the raw secret when it did.
///
/// # Errors
///
/// Fails on database errors.
pub fn ensure_admin(store: &Store) -> Result<Option<String>> {
    if store.admin_count()? > 0 {
        return Ok(None);
    }
    let (_, secret) = store.create_token(&NewToken {
        name: "admin".to_owned(),
        role: Role::Admin,
        session_mode: SessionMode::Stateless,
        agents: AgentKind::ALL.to_vec(),
        expires_in_days: None,
        note: Some("created on first start".to_owned()),
        limits: crate::token::Limits::default(),
        accounts: crate::accounts::AccountMap::new(),
    })?;
    Ok(Some(secret.raw))
}

/// Probes each configured agent for its version.
pub async fn discover_agents(agents: &HashMap<AgentKind, AgentConfig>) -> Vec<AgentInfo> {
    let mut out = Vec::new();
    for kind in AgentKind::ALL {
        let Some(cfg) = agents.get(&kind) else {
            continue;
        };
        let version = agent::probe_version(&cfg.bin).await;
        out.push(AgentInfo {
            agent: kind,
            available: version.is_some(),
            version,
            path: cfg.bin.display().to_string(),
        });
    }
    out
}

/// Starts the API, workers and (optionally) the tunnel, with the default
/// [`NetLimits`].
///
/// # Errors
///
/// Fails if the database cannot be opened or the port cannot be bound.
pub async fn start(cfg: ServerConfig, bus: Bus) -> Result<Running> {
    start_with(cfg, NetLimits::default(), bus).await
}

/// Starts the API, workers and (optionally) the tunnel, with explicit
/// connection limits. `CF-Connecting-IP` is trusted from loopback peers
/// while a tunnel is configured or started ([`NetLimits::tunnel_trust`]), or
/// always when `net.trust_forwarded` is set.
///
/// # Errors
///
/// Fails if the database cannot be opened or the port cannot be bound.
pub async fn start_with(cfg: ServerConfig, net: NetLimits, bus: Bus) -> Result<Running> {
    let tunnel_configured = cfg.tunnel || cfg.tunnel_token.is_some();
    let store = Arc::new(open_store(&cfg.home)?);
    let recovered = store.recover_interrupted()?;
    if recovered > 0 {
        bus.log(format!("recovered {recovered} interrupted request(s)"));
    }
    let pruned = store.prune(cfg.retention_days)?;
    if pruned > 0 {
        info!(pruned, "pruned old requests");
    }
    let bootstrap_token = ensure_admin(&store)?;

    let agents = discover_agents(&cfg.agents).await;
    for a in &agents {
        match &a.version {
            Some(v) => bus.log(format!("{} ready ({v})", a.agent.display_name())),
            None => bus.log(format!(
                "{} not found at {}",
                a.agent.display_name(),
                a.path
            )),
        }
    }

    // Load certificates before binding, so a bad file fails fast and clearly.
    let tls_config = cfg.tls.as_ref().map(crate::tls::load).transpose()?;
    if cfg.tunnel_token.is_some() && cfg.public_url.is_none() {
        anyhow::bail!("a named tunnel needs --public-url set to its hostname");
    }
    let listener = tokio::net::TcpListener::bind(cfg.bind)
        .await
        .with_context(|| format!("binding {}", cfg.bind))?;
    let local = listener.local_addr()?;
    let scheme = if tls_config.is_some() {
        "https"
    } else {
        "http"
    };

    let cancel = CancellationToken::new();
    let tunnel = crate::tunnel::Control::new(
        crate::tunnel::Settings {
            port: local.port(),
            local_tls: tls_config.is_some(),
            home: cfg.home.clone(),
            configured_url: cfg.public_url.clone(),
            named_token: cfg.tunnel_token.clone(),
            configured: tunnel_configured,
            cancel: cancel.clone(),
        },
        net.tunnel_trust.clone(),
    );
    let runtime = Arc::new(Runtime {
        started: Instant::now(),
        local_url: format!("{scheme}://{local}"),
        public_url: RwLock::new(cfg.public_url.clone()),
        agents,
        name: cfg.name.clone(),
        tunnel,
    });
    // A file left by a run that did not shut down cleanly is stale.
    let recorded = match &cfg.public_url {
        Some(url) => write_public_url(&cfg.home, url),
        None => remove_public_url(&cfg.home),
    };
    if let Err(e) = recorded {
        bus.log(format!("could not record the public URL: {e}"));
    }
    if let Err(e) = write_server_pid(&cfg.home) {
        bus.log(format!("could not record the server pid: {e}"));
    }
    let dispatcher = Dispatcher::new();
    let guard = Arc::new(ResourceGuard::new(
        &cfg.resources,
        cfg.home.clone(),
        bus.clone(),
    ));
    let summary = guard.describe();
    info!("{summary}");
    bus.log(summary);
    let state = AppState {
        guard: Arc::clone(&guard),
        store: Arc::clone(&store),
        bus: bus.clone(),
        dispatcher: dispatcher.clone(),
        runtime: Arc::clone(&runtime),
        limiter: Arc::new(RateLimiter::new(cfg.rate_limit)),
        default_agent: cfg.default_agent,
        queue_limit: cfg.queue_limit,
        run_timeout_secs: cfg.limits.timeout.as_secs(),
        waits: Arc::default(),
    };

    let queue_cfg = Arc::new(QueueConfig {
        agents: cfg.agents.clone(),
        concurrency: cfg.concurrency,
        limits: cfg.limits.clone(),
        workspace: cfg.home.join("workspaces"),
        home: cfg.home.clone(),
        account_cooldown: cfg.account_cooldown,
    });
    let mut tasks = queue::spawn_workers(
        Arc::clone(&store),
        bus.clone(),
        &dispatcher,
        queue_cfg,
        guard,
        cancel.clone(),
    );
    crate::maintenance::spawn(
        &mut tasks,
        Arc::clone(&store),
        cfg.retention_days,
        cancel.clone(),
    );

    let mut app = api::router(state.clone(), &net);
    let shutdown = cancel.clone();
    match tls_config {
        Some(config) => {
            // Browsers should keep using HTTPS for this host from now on.
            app = app.layer(axum::middleware::map_response(hsts));
            let listener = crate::tls::TlsListener::new(listener, config, net.max_connections)?;
            tasks.spawn(async move { crate::net::serve(listener, app, &net, shutdown).await });
        }
        None => {
            let listener = LimitedListener::new(listener, net.max_connections);
            tasks.spawn(async move { crate::net::serve(listener, app, &net, shutdown).await });
        }
    }
    bus.log(format!("listening on {scheme}://{local}"));

    if tunnel_configured {
        launch_tunnel(&runtime, &bus, false)?;
    }

    if cfg.update_check {
        crate::update::spawn_check(
            cfg.home.clone(),
            crate::update::LATEST_URL.to_owned(),
            bus.clone(),
        );
    }

    Ok(Running {
        state,
        bootstrap_token,
        cancel,
        tasks,
        home: cfg.home.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[cfg(unix)]
    #[test]
    fn database_wal_and_shm_are_owner_only() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let store = open_store(&home).unwrap();
        ensure_admin(&store).unwrap();
        // The connection is still open, so the WAL and SHM files exist.
        for name in ["hashi.db", "hashi.db-wal", "hashi.db-shm"] {
            let path = home.join(name);
            assert!(path.exists(), "{name} missing");
            assert_eq!(mode(&path), 0o600, "{name}");
        }
        assert_eq!(mode(&home), 0o700);
    }

    #[test]
    fn public_url_file_round_trips_and_is_removed() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        assert_eq!(read_public_url(home), None);
        write_public_url(home, "https://calm-river-bridge.trycloudflare.com").unwrap();
        assert_eq!(
            read_public_url(home).as_deref(),
            Some("https://calm-river-bridge.trycloudflare.com")
        );
        #[cfg(unix)]
        assert_eq!(mode(&home.join(PUBLIC_URL_FILE)), 0o600);
        // Overwriting replaces the old URL.
        write_public_url(home, "https://hashi.example.com/").unwrap();
        assert_eq!(
            read_public_url(home).as_deref(),
            Some("https://hashi.example.com")
        );
        remove_public_url(home).unwrap();
        assert_eq!(read_public_url(home), None);
        // Removing twice is fine.
        remove_public_url(home).unwrap();
    }

    #[test]
    fn a_garbled_public_url_file_is_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        for bad in [
            "",
            "hashi.example.com",
            "https://",
            "https://a b",
            "ftp://x",
        ] {
            std::fs::write(home.join(PUBLIC_URL_FILE), bad).unwrap();
            assert_eq!(read_public_url(home), None, "{bad:?}");
        }
    }

    #[test]
    fn parses_the_current_sid_from_whoami() {
        use win_acl::parse_sid;
        assert_eq!(
            parse_sid(
                "\"desktop-1\\\\alice\",\"S-1-5-21-1004336348-1177238915-682003330-1001\"\r\n"
            )
            .as_deref(),
            Some("S-1-5-21-1004336348-1177238915-682003330-1001")
        );
        for bad in [
            "",
            "\"desktop-1\\\\alice\"",
            "\"a\",\"S-1-5-21-1 /grant Everyone:F\"",
            "\"a\",\"Everyone\"",
            "\"a\",\"S-1-\"",
        ] {
            assert_eq!(parse_sid(bad), None, "{bad}");
        }
    }

    #[test]
    fn icacls_grants_only_the_user_system_and_administrators() {
        let args = win_acl::grant_args(std::ffi::OsStr::new("C:\\hashi"), "S-1-5-21-1-2-3-1001");
        let args: Vec<&str> = args.iter().map(|a| a.to_str().unwrap()).collect();
        assert_eq!(
            args,
            [
                "C:\\hashi",
                "/inheritance:r",
                "/grant:r",
                "*S-1-5-21-1-2-3-1001:(OI)(CI)F",
                "*S-1-5-18:(OI)(CI)F",
                "*S-1-5-32-544:(OI)(CI)F",
                "/Q",
            ]
        );
    }

    #[test]
    fn classifies_where_the_home_is() {
        use win_acl::{Location, location};
        let profile = Some(Path::new("/Users/alice"));
        assert_eq!(
            location(Path::new("/Users/alice/.hashi"), profile),
            Location::InProfile
        );
        assert_eq!(
            location(Path::new("/Users/alice"), profile),
            Location::Profile
        );
        assert_eq!(
            location(Path::new("/srv/hashi"), profile),
            Location::Outside
        );
        assert_eq!(
            location(Path::new("/Users/alicecooper/h"), profile),
            Location::Outside
        );
        assert_eq!(location(Path::new("/srv/hashi"), None), Location::Outside);
        assert_eq!(location(Path::new("/"), profile), Location::Root);
    }

    #[cfg(unix)]
    #[test]
    fn reopening_tightens_an_existing_database() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        drop(open_store(&home).unwrap());
        let db = home.join("hashi.db");
        std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o644)).unwrap();
        let store = open_store(&home).unwrap();
        ensure_admin(&store).unwrap();
        for name in ["hashi.db", "hashi.db-wal", "hashi.db-shm"] {
            assert_eq!(mode(&home.join(name)), 0o600, "{name}");
        }
    }
}
