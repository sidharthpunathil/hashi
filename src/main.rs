//! `hashi` command-line entry point.

use std::collections::HashMap;
use std::io::IsTerminal;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand};
use hashi::agent::{self, AgentConfig, AgentKind, Invocation, Limits};
use hashi::events::{Bus, Event};
use hashi::server::{self, ServerConfig};
use hashi::store::{NewToken, Revoke};
use hashi::token::{Role, SessionMode};
use hashi::util::human_age;
use tracing_subscriber::EnvFilter;

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
    /// Create, list and revoke API tokens.
    #[command(subcommand)]
    Token(TokenCommand),
    /// Send one prompt straight to an agent, without the server.
    Ask(AskArgs),
    /// Check that the agents and tunnel are installed.
    Doctor(AgentArgs),
}

#[derive(Debug, Args, Clone)]
struct AgentArgs {
    /// Path to the Claude Code executable.
    #[arg(long, env = "HASHI_CLAUDE_BIN")]
    claude_bin: Option<PathBuf>,
    /// Path to the Codex executable.
    #[arg(long, env = "HASHI_CODEX_BIN")]
    codex_bin: Option<PathBuf>,
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
    /// Plain logs instead of the dashboard (also used when stdout is not a terminal).
    #[arg(long)]
    headless: bool,
    /// Agent used when a request does not name one.
    #[arg(long, default_value = "claude", env = "HASHI_DEFAULT_AGENT")]
    default_agent: AgentKind,
    /// Concurrent runs per agent.
    #[arg(long, default_value_t = 1, env = "HASHI_CONCURRENCY",
          value_parser = clap::value_parser!(u16).range(1..=16))]
    concurrency: u16,
    /// Per-request time limit in seconds.
    #[arg(long, default_value_t = 300, env = "HASHI_TIMEOUT")]
    timeout: u64,
    /// Authenticated requests per minute per token (0 disables).
    #[arg(long, default_value_t = 60, env = "HASHI_RATE_LIMIT")]
    rate_limit: u32,
    /// Maximum pending requests across all agents.
    #[arg(long, default_value_t = 100, env = "HASHI_QUEUE_LIMIT")]
    queue_limit: u64,
    /// Days to keep finished requests.
    #[arg(long, default_value_t = 14, env = "HASHI_RETENTION_DAYS")]
    retention_days: u32,
    #[command(flatten)]
    agents: AgentArgs,
}

#[derive(Debug, Subcommand)]
enum TokenCommand {
    /// Create a token and print it once.
    Create {
        /// Label for the token.
        name: String,
        /// Grant admin rights.
        #[arg(long)]
        admin: bool,
        /// Keep a conversation per agent across prompts.
        #[arg(long)]
        stateful: bool,
        /// Restrict to these agents (comma separated).
        #[arg(long, value_delimiter = ',')]
        agents: Option<Vec<AgentKind>>,
        /// Expire after this many days.
        #[arg(long, value_parser = clap::value_parser!(u32).range(1..=3650))]
        expires_in_days: Option<u32>,
    },
    /// List tokens.
    List,
    /// Revoke a token by id or prefix.
    Revoke {
        /// Token id or display prefix.
        id: String,
    },
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
    match cli.command {
        None => serve(&home, ServeArgs::parse_from(["hashi"])).await,
        Some(Command::Serve(args)) => serve(&home, *args).await,
        Some(Command::Token(cmd)) => token(&home, cmd),
        Some(Command::Ask(args)) => ask(&home, args).await,
        Some(Command::Doctor(args)) => doctor(&args).await,
    }
}

fn init_logging(home: &std::path::Path, to_file: bool) -> Result<()> {
    let filter =
        EnvFilter::try_from_env("HASHI_LOG").unwrap_or_else(|_| EnvFilter::new("hashi=info,warn"));
    if to_file {
        // The dashboard owns the terminal, so logs go to a file instead.
        let dir = home.join("logs");
        std::fs::create_dir_all(&dir)?;
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("hashi.log"))?;
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_ansi(false)
            .with_writer(std::sync::Mutex::new(file))
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_target(false)
            .init();
    }
    Ok(())
}

async fn serve(home: &std::path::Path, args: ServeArgs) -> Result<()> {
    let tui = !args.headless && std::io::stdout().is_terminal();
    server::prepare_home(home)?;
    init_logging(home, tui)?;

    if !args.bind.is_loopback() {
        eprintln!(
            "warning: listening on {} exposes the API to your network; prefer 127.0.0.1 with --tunnel",
            args.bind
        );
    }

    let cfg = ServerConfig {
        bind: SocketAddr::new(args.bind, args.port),
        home: home.to_path_buf(),
        agents: args.agents.configs(),
        default_agent: args.default_agent,
        concurrency: usize::from(args.concurrency),
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
    };

    let bus = Bus::default();
    let events = bus.subscribe();
    let running = server::start(cfg, bus).await?;

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
        Event::TunnelReady(url) => println!("  ⌁ public   {url}"),
        Event::TunnelDown(why) => println!("  ! tunnel   {why}"),
        Event::Log(msg) => println!("  · {msg}"),
    }
}

fn short(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

fn token(home: &std::path::Path, cmd: TokenCommand) -> Result<()> {
    let store = server::open_store(home)?;
    match cmd {
        TokenCommand::Create {
            name,
            admin,
            stateful,
            agents,
            expires_in_days,
        } => {
            let name = name.trim().to_owned();
            if name.is_empty() || name.chars().count() > 100 {
                bail!("name must be 1 to 100 characters");
            }
            let new = NewToken {
                name,
                role: if admin { Role::Admin } else { Role::User },
                session_mode: if stateful {
                    SessionMode::Stateful
                } else {
                    SessionMode::Stateless
                },
                agents: agents.unwrap_or_else(|| AgentKind::ALL.to_vec()),
                expires_in_days,
                note: None,
                limits: hashi::token::Limits::default(),
            };
            let (t, secret) = store.create_token(&new)?;
            eprintln!("created {} token `{}` ({})", t.role, t.name, t.id);
            eprintln!("store it now, it will not be shown again:");
            println!("{}", secret.raw);
        }
        TokenCommand::List => {
            let tokens = store.list_tokens()?;
            println!(
                "{:<20} {:<16} {:<6} {:<10} {:<13} {:<10}",
                "NAME", "PREFIX", "ROLE", "SESSION", "AGENTS", "LAST USED"
            );
            for t in tokens {
                let agents = t
                    .agents
                    .iter()
                    .map(|a| a.as_str())
                    .collect::<Vec<_>>()
                    .join(",");
                let used = t
                    .last_used_at
                    .map_or_else(|| "never".to_owned(), |u| format!("{} ago", human_age(u)));
                println!(
                    "{:<20} {:<16} {:<6} {:<10} {:<13} {:<10}",
                    t.name, t.prefix, t.role, t.session_mode, agents, used
                );
            }
        }
        TokenCommand::Revoke { id } => match store.revoke_token(&id)? {
            Revoke::Revoked => println!("revoked {id}"),
            Revoke::NotFound => bail!("no token matches `{id}`"),
            Revoke::LastAdmin => bail!("refusing to revoke the last admin token"),
        },
    }
    Ok(())
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
    let out = agent::execute(args.agent, cfg, inv, &workdir, limits).await?;
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

async fn doctor(args: &AgentArgs) -> Result<()> {
    println!("\n  ✦ hashi doctor\n");
    let mut ok = true;
    for info in server::discover_agents(&args.configs()).await {
        match &info.version {
            Some(v) => println!("  ✓ {:<12} {v}  ({})", info.agent.display_name(), info.path),
            None => {
                ok = false;
                println!(
                    "  ✗ {:<12} not found at {}",
                    info.agent.display_name(),
                    info.path
                );
            }
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
    if !ok {
        println!("  At least one agent is missing. Hashi still runs; requests to it return 503.\n");
    }
    Ok(())
}
