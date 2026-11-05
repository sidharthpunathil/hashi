//! Wires the store, queue, API and tunnel into one running service.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Instant;

use anyhow::{Context, Result};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::agent::{self, AgentConfig, AgentKind, Limits};
use crate::api::{self, AgentInfo, AppState, RateLimiter, Runtime};
use crate::events::{Bus, Event};
use crate::queue::{self, Dispatcher, QueueConfig};
use crate::store::{NewToken, Store};
use crate::token::{Role, SessionMode};

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
    /// Workers per agent.
    pub concurrency: usize,
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
}

impl Running {
    /// Stops all tasks and waits for them to finish.
    pub async fn shutdown(mut self) {
        self.cancel.cancel();
        while self.tasks.join_next().await.is_some() {}
    }
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
    Ok(())
}

/// Opens the database in `home`, restricting the file to its owner.
///
/// # Errors
///
/// Fails if the database cannot be opened.
pub fn open_store(home: &Path) -> Result<Store> {
    prepare_home(home)?;
    let path = home.join("hashi.db");
    let store = Store::open(&path).with_context(|| format!("opening {}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(store)
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

/// Starts the API, workers and (optionally) the tunnel.
///
/// # Errors
///
/// Fails if the database cannot be opened or the port cannot be bound.
pub async fn start(cfg: ServerConfig, bus: Bus) -> Result<Running> {
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

    let runtime = Arc::new(Runtime {
        started: Instant::now(),
        local_url: format!("{scheme}://{local}"),
        public_url: RwLock::new(cfg.public_url.clone()),
        agents,
        name: cfg.name.clone(),
    });
    let dispatcher = Dispatcher::new();
    let state = AppState {
        store: Arc::clone(&store),
        bus: bus.clone(),
        dispatcher: dispatcher.clone(),
        runtime: Arc::clone(&runtime),
        limiter: Arc::new(RateLimiter::new(cfg.rate_limit)),
        default_agent: cfg.default_agent,
        queue_limit: cfg.queue_limit,
        run_timeout_secs: cfg.limits.timeout.as_secs(),
    };

    let cancel = CancellationToken::new();
    let queue_cfg = Arc::new(QueueConfig {
        agents: cfg.agents.clone(),
        concurrency: cfg.concurrency,
        limits: cfg.limits,
        workspace: cfg.home.join("workspaces"),
    });
    let mut tasks = queue::spawn_workers(
        Arc::clone(&store),
        bus.clone(),
        &dispatcher,
        queue_cfg,
        cancel.clone(),
    );

    let mut app = api::router(state.clone());
    let shutdown = cancel.clone();
    let local_tls = tls_config.is_some();
    match tls_config {
        Some(config) => {
            // Browsers should keep using HTTPS for this host from now on.
            app = app.layer(axum::middleware::map_response(hsts));
            let listener = crate::tls::TlsListener::new(listener, config)?;
            tasks.spawn(async move {
                let serve =
                    axum::serve(listener, app).with_graceful_shutdown(shutdown.cancelled_owned());
                if let Err(e) = serve.await {
                    warn!(error = %e, "https server stopped");
                }
            });
        }
        None => {
            tasks.spawn(async move {
                let serve =
                    axum::serve(listener, app).with_graceful_shutdown(shutdown.cancelled_owned());
                if let Err(e) = serve.await {
                    warn!(error = %e, "http server stopped");
                }
            });
        }
    }
    bus.log(format!("listening on {scheme}://{local}"));

    if cfg.tunnel || cfg.tunnel_token.is_some() {
        let bus = bus.clone();
        let runtime = Arc::clone(&runtime);
        let cancel = cancel.clone();
        let port = local.port();
        let tunnel_token = cfg.tunnel_token.clone();
        let home = cfg.home.clone();
        let named_url = cfg.public_url.clone();
        tasks.spawn(async move {
            let started = match (&tunnel_token, &named_url) {
                (Some(token), Some(url)) => crate::tunnel::start_named(token, url, &home).await,
                _ => crate::tunnel::start(port, local_tls, &home).await,
            };
            match started {
                Ok(mut tunnel) => {
                    // A configured public URL wins: invite links must not change.
                    if let Ok(mut url) = runtime.public_url.write()
                        && url.is_none()
                    {
                        *url = Some(tunnel.url.clone());
                    }
                    bus.emit(Event::TunnelReady(tunnel.url.clone()));
                    tokio::select! {
                        () = cancel.cancelled() => {}
                        status = tunnel.child.wait() => {
                            bus.emit(Event::TunnelDown(format!("cloudflared exited: {status:?}")));
                        }
                    }
                }
                Err(e) => bus.emit(Event::TunnelDown(e.to_string())),
            }
        });
    }

    Ok(Running {
        state,
        bootstrap_token,
        cancel,
        tasks,
    })
}
