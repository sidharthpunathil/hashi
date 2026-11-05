//! `hashi` command-line entry point.

mod admin_cli;

use std::collections::HashMap;
use std::io::IsTerminal;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use hashi::agent::{self, AgentConfig, AgentKind, Invocation, Limits};
use hashi::diagnostics::{self, RedactingMakeWriter};
use hashi::events::{Bus, Event};
use hashi::guard::{ResourceSettings, Size, parse_nonzero_size, parse_size};
use hashi::server::{self, ServerConfig};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// Hashi: a bridge that turns your local Claude Code and Codex CLIs into an HTTP API.
#[derive(Debug, Parser)]
#[command(name = "hashi", version, about, long_about = None)]
struct Cli {
    /// Data directory for the database, workspaces and logs.
    #[arg(long, global = true, env = "HASHI_HOME")]
    home: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Start the bridge (the default when no command is given).
    Serve(Box<ServeArgs>),
    /// Create, list, update and revoke API tokens.
    #[command(subcommand)]
    Token(admin_cli::TokenCommand),
    /// Create a one-time invite link for a person.
    Share(admin_cli::ShareArgs),
    /// List or cancel invite links.
    #[command(subcommand)]
    Invite(admin_cli::InviteCommand),
    /// Show recent sharing and access changes.
    Audit {
        /// How many entries to show.
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    /// Send one prompt straight to an agent, without the server.
    Ask(AskArgs),
    /// Check that the agents and tunnel are installed.
    Doctor(AgentArgs),
    /// Write a redacted diagnostic report to attach to a bug report.
    Report(ReportArgs),
}

#[derive(Debug, Args)]
struct ReportArgs {
    /// Where to write the report (a file, or a directory to put it in).
    /// Defaults to the current directory.
    #[arg(long, conflicts_with = "stdout")]
    out: Option<PathBuf>,
    /// Print the report instead of writing a file.
    #[arg(long)]
    stdout: bool,
    /// Include excerpts (500 characters) of the prompts that failed.
    #[arg(long)]
    include_prompts: bool,
    #[command(flatten)]
    agents: AgentArgs,
}

#[derive(Debug, Args, Clone)]
struct AgentArgs {
    /// Path to the Claude Code executable.
    #[arg(long, env = "HASHI_CLAUDE_BIN")]
    claude_bin: Option<PathBuf>,
    /// Path to the Codex executable.
    #[arg(long, env = "HASHI_CODEX_BIN")]
    codex_bin: Option<PathBuf>,
    /// Path to the Gemini CLI executable.
    #[arg(long, env = "HASHI_GEMINI_BIN")]
    gemini_bin: Option<PathBuf>,
    /// Path to the opencode executable.
    #[arg(long, env = "HASHI_OPENCODE_BIN")]
    opencode_bin: Option<PathBuf>,
    /// Path to the Qwen Code executable.
    #[arg(long, env = "HASHI_QWEN_BIN")]
    qwen_bin: Option<PathBuf>,
    /// Path to the Cursor CLI executable (`cursor-agent` or `agent`).
    #[arg(long, env = "HASHI_CURSOR_BIN")]
    cursor_bin: Option<PathBuf>,
    /// Maximum agentic turns per Claude request.
    #[arg(long, default_value_t = 10, env = "HASHI_MAX_TURNS")]
    max_turns: u32,
    /// Claude permission mode (`default`, `acceptEdits`, `plan`, `bypassPermissions`).
    #[arg(long, env = "HASHI_CLAUDE_PERMISSION_MODE")]
    claude_permission_mode: Option<String>,
    /// Codex sandbox policy.
    #[arg(long, default_value = "read-only", env = "HASHI_CODEX_SANDBOX",
          value_parser = ["read-only", "workspace-write", "danger-full-access"])]
    codex_sandbox: String,
}

impl AgentArgs {
    fn configs(&self) -> HashMap<AgentKind, AgentConfig> {
        AgentKind::ALL
            .into_iter()
            .map(|kind| {
                let mut cfg = AgentConfig::default_for(kind);
                let custom = match kind {
                    AgentKind::Claude => self.claude_bin.clone(),
                    AgentKind::Codex => self.codex_bin.clone(),
                    AgentKind::Gemini => self.gemini_bin.clone(),
                    AgentKind::Opencode => self.opencode_bin.clone(),
                    AgentKind::Qwen => self.qwen_bin.clone(),
                    AgentKind::Cursor => self.cursor_bin.clone(),
                };
                if let Some(bin) = custom {
                    cfg.bin = bin;
                }
                cfg.max_turns = self.max_turns;
                cfg.permission_mode.clone_from(&self.claude_permission_mode);
                cfg.sandbox.clone_from(&self.codex_sandbox);
                (kind, cfg)
            })
            .collect()
    }
}

#[derive(Debug, Parser, Clone)]
struct ServeArgs {
    /// Address to listen on. Keep it on loopback and use --tunnel for remote access.
    #[arg(long, default_value = "127.0.0.1", env = "HASHI_BIND")]
    bind: std::net::IpAddr,
    /// Port to listen on.
    #[arg(short, long, default_value_t = 7878, env = "HASHI_PORT")]
    port: u16,
    /// Expose the API through a Cloudflare quick tunnel (requires `cloudflared`).
    #[arg(long, env = "HASHI_TUNNEL")]
    tunnel: bool,
    /// Stable public URL used in invite links, e.g. https://hashi.example.com.
    #[arg(long, env = "HASHI_PUBLIC_URL", value_parser = parse_public_url)]
    public_url: Option<String>,
    /// Name shown to people you share with.
    #[arg(long, default_value = "Hashi", env = "HASHI_NAME")]
    name: String,
    /// Serve HTTPS with this PEM certificate chain (needs --tls-key).
    #[arg(long, env = "HASHI_TLS_CERT", requires = "tls_key")]
    tls_cert: Option<PathBuf>,
    /// Private key for --tls-cert, in PEM.
    #[arg(long, env = "HASHI_TLS_KEY", requires = "tls_cert")]
    tls_key: Option<PathBuf>,
    /// Run a named Cloudflare tunnel with this token, for a stable hostname.
    /// Set --public-url to that hostname.
    #[arg(
        long,
        env = "HASHI_TUNNEL_TOKEN",
        requires = "public_url",
        hide_env_values = true
    )]
    tunnel_token: Option<String>,
    /// Plain logs instead of the dashboard (also used when stdout is not a terminal).
    #[arg(long)]
    headless: bool,
    /// Agent used when a request does not name one.
    #[arg(long, default_value = "claude", env = "HASHI_DEFAULT_AGENT")]
    default_agent: AgentKind,
    /// Concurrent runs per agent (the total is still capped by --max-running).
    #[arg(long, default_value_t = 1, env = "HASHI_CONCURRENCY",
          value_parser = clap::value_parser!(u16).range(1..=16))]
    concurrency: u16,
    /// Most agent runs at once, across all agents.
    /// Default: total memory in GiB / 8, between 1 and the CPU count.
    #[arg(long, env = "HASHI_MAX_RUNNING",
          value_parser = clap::value_parser!(u16).range(1..=256))]
    max_running: Option<u16>,
    /// Memory kept free for you; new runs wait while less would be left
    /// (for example 4G or 20%; 0 disables). Default: the larger of 2G and 20%.
    #[arg(long, env = "HASHI_MEMORY_RESERVE", value_parser = parse_size)]
    memory_reserve: Option<Size>,
    /// Memory one run's whole process tree may use before it is stopped
    /// (for example 4G or 40%). Default: the larger of 2G and 40%.
    #[arg(long, env = "HASHI_RUN_MEMORY_LIMIT", value_parser = parse_nonzero_size)]
    run_memory_limit: Option<Size>,
    /// Free disk space kept under the Hashi home; new runs wait below it
    /// (for example 1G or 5%; 0 disables).
    #[arg(long, default_value = "1G", env = "HASHI_DISK_FLOOR", value_parser = parse_size)]
    disk_floor: Size,
    /// Niceness for agent runs, so your own work wins (0 to 19; 0 disables).
    /// On Windows any value above 0 means below-normal priority.
    #[arg(long, default_value_t = hashi::guard::DEFAULT_NICE, env = "HASHI_NICE",
          value_parser = clap::value_parser!(u8).range(0..=19))]
    nice: u8,
    /// Per-request time limit in seconds.
    #[arg(long, default_value_t = 300, env = "HASHI_TIMEOUT")]
    timeout: u64,
    /// Authenticated requests per minute per token (0 disables).
    #[arg(long, default_value_t = 60, env = "HASHI_RATE_LIMIT")]
    rate_limit: u32,
    /// Connections open at once; further clients wait until one closes.
    #[arg(long, default_value_t = 512, env = "HASHI_MAX_CONNECTIONS",
          value_parser = clap::value_parser!(u32).range(1..))]
    max_connections: u32,
    /// Maximum pending requests across all agents.
    #[arg(long, default_value_t = 100, env = "HASHI_QUEUE_LIMIT")]
    queue_limit: u64,
    /// Days to keep finished requests.
    #[arg(long, default_value_t = 14, env = "HASHI_RETENTION_DAYS")]
    retention_days: u32,
    #[command(flatten)]
    agents: AgentArgs,
}

#[derive(Debug, Args)]
struct AskArgs {
    /// The prompt.
    message: String,
    /// Which agent to ask.
    #[arg(short, long, default_value = "claude")]
    agent: AgentKind,
    /// Model override.
    #[arg(short, long)]
    model: Option<String>,
    /// Time limit in seconds.
    #[arg(long, default_value_t = 300)]
    timeout: u64,
    #[command(flatten)]
    agents: AgentArgs,
}

/// Accepts `http(s)://host[:port][/path]` and strips a trailing slash.
fn parse_public_url(s: &str) -> Result<String, String> {
    let s = s.trim().trim_end_matches('/');
    let rest = s
        .strip_prefix("https://")
        .or_else(|| s.strip_prefix("http://"))
        .ok_or("public URL must start with https:// or http://")?;
    if rest.is_empty() || rest.contains(char::is_whitespace) || rest.contains(['?', '#', '@']) {
        return Err("public URL must be a plain origin like https://hashi.example.com".to_owned());
    }
    Ok(s.to_owned())
}

fn home_dir(custom: Option<PathBuf>) -> Result<PathBuf> {
    match custom {
        Some(p) => Ok(p),
        None => Ok(dirs::home_dir()
            .context("cannot determine home directory; set HASHI_HOME")?
            .join(".hashi")),
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let home = home_dir(cli.home)?;
    // Before the dashboard starts: ratatui chains its terminal restore in
    // front of this hook, so the crash note prints on a sane terminal.
    diagnostics::install_panic_hook(home.join("logs"));
    match cli.command {
        None => serve(&home, ServeArgs::parse_from(["hashi"])).await,
        Some(Command::Serve(args)) => serve(&home, *args).await,
        Some(Command::Token(cmd)) => admin_cli::token(&server::open_store(&home)?, cmd),
        Some(Command::Share(args)) => admin_cli::share(&server::open_store(&home)?, args),
        Some(Command::Invite(cmd)) => admin_cli::invite(&server::open_store(&home)?, cmd),
        Some(Command::Audit { limit }) => admin_cli::audit(&server::open_store(&home)?, limit),
        Some(Command::Ask(args)) => ask(&home, args).await,
        Some(Command::Doctor(args)) => doctor(&args).await,
        Some(Command::Report(args)) => report(&home, args).await,
    }
}

/// Logs always go to `~/.hashi/logs/hashi.log`, redacted. Without the
/// dashboard they are also printed to stderr.
fn init_logging(home: &std::path::Path, tui: bool) -> Result<()> {
    let filter =
        EnvFilter::try_from_env("HASHI_LOG").unwrap_or_else(|_| EnvFilter::new("hashi=info,warn"));
    let dir = home.join("logs");
    std::fs::create_dir_all(&dir)?;
    let rotated =
        diagnostics::rotate_logs(&dir, diagnostics::MAX_LOG_BYTES, diagnostics::KEEP_LOGS);
    let file = diagnostics::open_log_file(&dir)
        .with_context(|| format!("opening {}", dir.join(diagnostics::LOG_FILE).display()))?;
    let file_layer = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_writer(RedactingMakeWriter::new(file));
    let stderr_layer = (!tui).then(|| {
        tracing_subscriber::fmt::layer()
            .with_target(false)
            .with_ansi(std::io::stderr().is_terminal())
            .with_writer(RedactingMakeWriter::new(std::io::stderr()))
    });
    tracing_subscriber::registry()
        .with(filter)
        .with(file_layer)
        .with(stderr_layer)
        .init();
    if let Err(e) = rotated {
        tracing::warn!(error = %e, "could not rotate the log file");
    }
    Ok(())
}

async fn serve(home: &std::path::Path, args: ServeArgs) -> Result<()> {
    let tui = !args.headless && std::io::stdout().is_terminal();
    server::prepare_home(home)?;
    init_logging(home, tui)?;
    let max_connections = usize::try_from(args.max_connections).unwrap_or(usize::MAX);
    if let Some(fds) = hashi::net::raise_fd_limit()
        && fds < u64::from(args.max_connections) + 64
    {
        tracing::warn!(
            open_files = fds,
            max_connections,
            "the open-file limit is close to --max-connections; raise it with ulimit -n"
        );
    }

    if !args.bind.is_loopback() {
        if args.tls_cert.is_some() {
            eprintln!(
                "note: serving HTTPS on {}, reachable from your network",
                args.bind
            );
        } else {
            eprintln!(
                "warning: listening on {} without TLS exposes the API to your network in plain text; \
                 add --tls-cert/--tls-key, or keep 127.0.0.1 and use --tunnel",
                args.bind
            );
        }
    }

    let cfg = ServerConfig {
        bind: SocketAddr::new(args.bind, args.port),
        home: home.to_path_buf(),
        agents: args.agents.configs(),
        default_agent: args.default_agent,
        concurrency: usize::from(args.concurrency),
        resources: ResourceSettings {
            max_running: args.max_running.map(usize::from),
            memory_reserve: args.memory_reserve,
            run_memory_limit: args.run_memory_limit,
            disk_floor: Some(args.disk_floor),
            nice: Some(args.nice),
            ..ResourceSettings::default()
        },
        limits: Limits {
            timeout: Duration::from_secs(args.timeout),
            ..Limits::default()
        },
        rate_limit: args.rate_limit,
        queue_limit: args.queue_limit,
        retention_days: args.retention_days,
        tunnel: args.tunnel,
        public_url: args.public_url.clone(),
        name: args.name.clone(),
        tls: match (&args.tls_cert, &args.tls_key) {
            (Some(cert), Some(key)) => Some(hashi::tls::TlsFiles {
                cert: cert.clone(),
                key: key.clone(),
            }),
            _ => None,
        },
        tunnel_token: args.tunnel_token.clone(),
    };

    let bus = Bus::default();
    let events = bus.subscribe();
    let net = hashi::net::NetLimits {
        max_connections,
        ..hashi::net::NetLimits::default()
    };
    let running = server::start_with(cfg, net, bus).await?;

    if tui {
        let result = hashi::tui::run(
            running.state.clone(),
            events,
            running.bootstrap_token.clone(),
            args.tunnel,
        )
        .await;
        running.shutdown().await;
        return result;
    }

    print_banner(&running);
    let mut events = events;
    let printer = tokio::spawn(async move {
        while let Ok(ev) = events.recv().await {
            print_event(&ev);
        }
    });
    tokio::signal::ctrl_c()
        .await
        .context("waiting for ctrl-c")?;
    eprintln!("\nshutting down…");
    printer.abort();
    running.shutdown().await;
    Ok(())
}

fn print_banner(running: &server::Running) {
    let rt = &running.state.runtime;
    println!();
    println!("  ✦ hashi {}  the bridge", env!("CARGO_PKG_VERSION"));
    println!();
    println!("  local    {}", rt.local_url);
    for a in &rt.agents {
        let state = a.version.as_deref().unwrap_or("not found");
        println!("  {:<8} {}", a.agent.as_str(), state);
    }
    if let Some(token) = &running.bootstrap_token {
        println!();
        println!("  admin token (shown once, store it now):");
        println!("  {token}");
    }
    println!();
}

fn print_event(ev: &Event) {
    match ev {
        Event::Queued { id, agent, token } => {
            println!("  ◌ {} queued for {agent} by {token}", short(id))
        }
        Event::Started { id, agent } => println!("  ⠿ {agent} picked up {}", short(id)),
        Event::Finished {
            id,
            agent,
            ok: true,
            duration_ms,
            ..
        } => {
            println!("  ✓ {agent} answered {} in {duration_ms}ms", short(id));
        }
        Event::Finished {
            id, agent, error, ..
        } => {
            println!(
                "  ✗ {agent} failed {}: {}",
                short(id),
                error.as_deref().unwrap_or("unknown error")
            );
        }
        Event::Held { reason, detail } => println!("  ! held     {reason}: {detail}"),
        Event::TunnelReady(url) => println!("  ⌁ public   {url}"),
        Event::TunnelDown(why) => println!("  ! tunnel   {why}"),
        Event::Log(msg) => println!("  · {msg}"),
    }
}

fn short(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

async fn ask(home: &std::path::Path, args: AskArgs) -> Result<()> {
    if let Some(m) = &args.model
        && !agent::is_safe_arg(m)
    {
        bail!("model contains unsupported characters");
    }
    let configs = args.agents.configs();
    let cfg = configs.get(&args.agent).context("agent not configured")?;
    let workdir = home.join("workspaces").join("cli");
    std::fs::create_dir_all(&workdir)?;
    let inv = Invocation {
        prompt: &args.message,
        resume: None,
        model: args.model.as_deref(),
    };
    let limits = Limits {
        timeout: Duration::from_secs(args.timeout),
        ..Limits::default()
    };
    // The agent runs in its own process group, so the terminal's Ctrl-C does
    // not reach it. Dropping the run on Ctrl-C kills its whole tree instead.
    let out = tokio::select! {
        out = agent::execute(args.agent, cfg, inv, &workdir, limits) => out?,
        _ = tokio::signal::ctrl_c() => bail!("interrupted"),
    };
    println!("{}", out.text);
    if let Some(u) = out.usage {
        eprintln!(
            "\n{} in · {} out{}",
            u.input_tokens,
            u.output_tokens,
            u.cost_usd
                .map(|c| format!(" · ${c:.4}"))
                .unwrap_or_default()
        );
    }
    Ok(())
}

/// Writes the redacted diagnostic report. Nothing is uploaded.
async fn report(home: &std::path::Path, args: ReportArgs) -> Result<()> {
    let agents = server::discover_agents(&args.agents.configs()).await;
    let cloudflared = hashi::tunnel::locate();
    let db = home.join("hashi.db");
    // Opening creates the file, so only open a database that already exists.
    let store = if db.exists() {
        server::open_store(home).map_err(|e| format!("{e:#}"))
    } else {
        Err(format!("no database at {}", db.display()))
    };
    let env: Vec<(String, String)> = std::env::vars()
        .filter(|(k, _)| k.starts_with("HASHI_"))
        .collect();
    let now = time::OffsetDateTime::now_utc();
    let text = diagnostics::build_report(&diagnostics::ReportInput {
        store: store.as_ref().map_err(Clone::clone),
        logs_dir: &home.join("logs"),
        agents: &agents,
        cloudflared: cloudflared.as_deref(),
        env: &env,
        include_prompts: args.include_prompts,
        now_ms: hashi::util::now_ms(),
    });

    let name = format!("hashi-report-{}.md", diagnostics::file_timestamp(now));
    let issue = diagnostics::issue_url();
    if args.stdout {
        print!("{text}");
        eprintln!(
            "\nRead the report before you share it. To file a bug, open:\n{issue}\n\
             Nothing has been uploaded."
        );
        return Ok(());
    }
    let path = match args.out {
        Some(p) if p.is_dir() => p.join(name),
        Some(p) => p,
        None => PathBuf::from(name),
    };
    std::fs::write(&path, &text).with_context(|| format!("writing {}", path.display()))?;
    println!("\n  ✦ hashi report\n");
    let shown = std::path::absolute(&path).unwrap_or(path);
    println!("  Saved {}", shown.display());
    println!();
    println!("  Read it before you share it. Secrets are redacted and prompts are left out,");
    println!("  but it does contain error messages and log lines from this machine.");
    println!();
    println!("  To file a bug, open this link and attach the file:");
    println!("  {issue}");
    println!();
    println!("  Nothing has been uploaded.\n");
    Ok(())
}

async fn doctor(args: &AgentArgs) -> Result<()> {
    println!("\n  ✦ hashi doctor\n");
    let mut found = 0;
    for info in server::discover_agents(&args.configs()).await {
        match &info.version {
            Some(v) => {
                found += 1;
                println!("  ✓ {:<12} {v}  ({})", info.agent.display_name(), info.path);
            }
            None => println!(
                "  · {:<12} not installed (optional)",
                info.agent.display_name()
            ),
        }
    }
    match hashi::tunnel::locate() {
        Some(p) => println!("  ✓ {:<12} {}", "cloudflared", p.display()),
        None => println!(
            "  · {:<12} not installed (only needed for --tunnel)",
            "cloudflared"
        ),
    }
    println!();
    if found == 0 {
        println!(
            "  No coding agent found. Install one (for example Claude Code or Codex) and sign in.\n"
        );
    } else {
        println!(
            "  Agents that are not installed answer 503; install one any time and restart Hashi.\n"
        );
    }
    Ok(())
}
