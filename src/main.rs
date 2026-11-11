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

/// Hashi: a bridge that turns your local coding-agent CLIs into an HTTP API.
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
    #[command(subcommand, after_help = TOKEN_EXAMPLES)]
    Token(admin_cli::TokenCommand),
    /// Create a one-time invite link for a person.
    #[command(after_help = SHARE_EXAMPLES)]
    Share(admin_cli::ShareArgs),
    /// Hand someone you trust one of your key accounts through a one-time
    /// link: reminders instead of limits.
    #[command(name = "share-login", after_help = SHARE_LOGIN_EXAMPLES)]
    ShareLogin(admin_cli::ShareLoginArgs),
    /// List or cancel invite links.
    #[command(subcommand)]
    Invite(admin_cli::InviteCommand),
    /// Add, sign in, list, disable and remove agent accounts.
    #[command(subcommand, after_help = ACCOUNT_EXAMPLES)]
    Account(admin_cli::AccountCommand),
    /// Point the plain `claude` (or `codex`, `gemini`, `opencode`) command at
    /// one of your accounts
    /// (see `hashi account switch`).
    Switch {
        /// claude, codex, gemini or opencode.
        agent: hashi::agent::AgentKind,
        /// A login account, or `default` for your original login.
        name: Option<String>,
    },
    /// Add, update, list and remove projects: folders to share.
    #[command(subcommand, after_help = PROJECT_EXAMPLES)]
    Project(admin_cli::ProjectCommand),
    /// Show recent sharing and access changes.
    Audit {
        /// How many entries to show.
        #[arg(long, default_value_t = 50)]
        limit: u32,
        /// Print JSON, as `GET /v1/audit` shows each entry.
        #[arg(long)]
        json: bool,
    },
    /// Send one prompt straight to an agent, without the server, or to a
    /// saved remote bridge with --remote.
    Ask(AskArgs),
    /// Redeem an invite link to someone's Hashi and save the token.
    #[command(after_help = CONNECT_EXAMPLES)]
    Connect(ConnectArgs),
    /// List or remove the bridges saved by `hashi connect`.
    #[command(subcommand)]
    Remote(RemoteCommand),
    /// Host a pool that people lend their agents to, or join one.
    #[command(subcommand, after_help = POOL_EXAMPLES)]
    Pool(admin_cli::PoolCommand),
    /// Ask someone's Hashi for access with their ask-access link, or check
    /// your requests with `hashi request status`.
    Request(RequestArgs),
    /// See and decide requests for access, and open or close the
    /// ask-access link people use to ask.
    #[command(subcommand, after_help = REQUESTS_EXAMPLES)]
    Requests(admin_cli::RequestsCommand),
    /// Check the agents, cloudflared, Tailscale, accounts and updates. Changes
    /// nothing.
    Doctor(DoctorArgs),
    /// Write a redacted diagnostic report to attach to a bug report.
    Report(ReportArgs),
    /// Download the latest release and replace this binary.
    Update(UpdateArgs),
    /// Remove the binary and everything Hashi stored on this machine.
    Uninstall(UninstallArgs),
    /// Show the official install command for a helper (cloudflared or
    /// tailscale), and run it after you confirm.
    Install(InstallArgs),
    /// Print a Markdown guide that lets an LLM agent (Claude Code, Codex,
    /// Cursor) run Hashi for you.
    #[command(
        visible_alias = "guide",
        after_help = "Examples:\n  hashi llm | pbcopy\n  hashi llm --output hashi-guide.md"
    )]
    Llm(LlmArgs),
}

const TOKEN_EXAMPLES: &str = "Examples:
  hashi token create ci-bot --agents claude --daily-requests 100
  hashi token create docs-site --project docs --expires-in-days 90
  hashi token list --json
  hashi token update ci-bot --pause
  hashi token update ci-bot --resume --monthly-budget 20
  hashi token revoke ci-bot

A token is named by its id, its prefix (the PREFIX column) or its name.";

const SHARE_EXAMPLES: &str = "Examples:
  hashi share alice
  hashi share alice --agents claude,codex --daily-requests 50 --expires-in-days 30
  hashi share bob --project web --link-hours 168 --share-usage
  hashi share carol --agents claude --stop-at 70 --expires-in-days 7

The link prints on standard output and works once. Without a public URL it
works only on this machine: start with --tunnel or --tailscale, or set
HASHI_PUBLIC_URL.

They send prompts to this Hashi, so your login never leaves this machine and
every limit holds. --stop-at 70 refuses their prompts once your account has
used 70% of its 5-hour or weekly quota. To hand over a key instead, see
`hashi share-login`.";

const POOL_EXAMPLES: &str = "Examples:
  hashi pool open --via lan          # on the host: a link for everyone on this wifi
  hashi pool open --via cloudflare   # or anyone, through the tunnel
  hashi pool join <link> --name sid --agents claude,codex --stop-at 70 --hours 4
  hashi pool status

Members never need a public URL: their Hashi asks the host for work, runs it
with their own accounts and limits, and sends the answer back. Pausing their
local \"pool\" token takes them out of rotation. Everyone in the pool sees
GET /v1/pool, and each member's own pool token uses every member's agents.";

const SHARE_LOGIN_EXAMPLES: &str = "Examples:
  claude setup-token            # makes a long-lived token for your plan
  hashi account add claude bot --kind oauth-token --key-stdin
  hashi share-login dana --account claude:bot --remind-at 70 --days 7

The link hands over the key itself, so dana runs Claude Code on their own
machine. Nothing can be enforced after that; instead both of you are
reminded at --remind-at percent, their Hashi reads your usage through the
link, and their copy is switched off when --days ends. They open it with
`hashi connect <link>` or the dashboard's Connect. Revoke the token the link
created to stop the usage reading; revoke the key where it was made to take
it back.";

const ACCOUNT_EXAMPLES: &str = "Examples:
  hashi account add claude work
  hashi account add codex ci --kind api-key --key-env OPENAI_API_KEY
  hashi account add claude via-alice --remote alice
  hashi account list --json
  hashi account usage --refresh
  hashi token update ci-bot --account claude=work,default";

const PROJECT_EXAMPLES: &str = "Examples:
  hashi project add web /home/me/src/web --agents claude,codex --account claude=work
  hashi project update web --account-strategy most-left --rotate-at 85
  hashi project list --json
  hashi share bob --project web
  hashi project remove web --unbind";

const REQUESTS_EXAMPLES: &str = "Examples:
  hashi requests open --project web --hours 72
  hashi requests list
  hashi requests list --all --json
  hashi requests accept 3f2a --daily-requests 30
  hashi requests decline 3f2a --reason \"not this week\"
  hashi requests close

A request is named by its id or a unique start of it (4 characters or more).";

const CONNECT_EXAMPLES: &str = "Examples:
  hashi connect https://their-host/claim/hashi_inv_<code> --name alice
  hashi connect <invite link> --setup claude
  hashi ask --remote alice \"Summarise the README\"

The token is saved in the data directory and never printed.";

#[derive(Debug, Args)]
struct LlmArgs {
    /// Write the guide to this file, readable only by you, instead of
    /// printing it.
    #[arg(long, short = 'o', value_name = "FILE")]
    output: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct UninstallArgs {
    /// Keep the data directory; remove only the binary and the PATH entry.
    #[arg(long)]
    keep_data: bool,
    /// Do not ask for confirmation.
    #[arg(long, short = 'y')]
    yes: bool,
    /// List what would be removed and stop.
    #[arg(long)]
    dry_run: bool,
}

#[derive(Debug, Args)]
struct InstallArgs {
    /// Which helper: cloudflared or tailscale.
    #[arg(value_parser = parse_helper)]
    helper: hashi::install::Helper,
    /// Run the command without asking first.
    #[arg(long, short = 'y')]
    yes: bool,
}

fn parse_helper(s: &str) -> Result<hashi::install::Helper, String> {
    hashi::install::Helper::parse(s).ok_or_else(|| "choose cloudflared or tailscale".to_owned())
}

#[derive(Debug, Args, Clone)]
struct HelperArgs {
    /// Path to the cloudflared executable. Default: looked up on PATH.
    #[arg(long, env = "HASHI_CLOUDFLARED_BIN")]
    cloudflared_bin: Option<PathBuf>,
    /// Path to the tailscale executable. Default: looked up on PATH.
    #[arg(long, env = "HASHI_TAILSCALE_BIN")]
    tailscale_bin: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct DoctorArgs {
    #[command(flatten)]
    agents: AgentArgs,
    #[command(flatten)]
    helpers: HelperArgs,
}

#[derive(Debug, Args)]
struct UpdateArgs {
    /// Only report whether an update is available.
    #[arg(long)]
    check: bool,
    /// Install this release (for example v0.3.0) instead of the latest.
    #[arg(long, conflicts_with = "check")]
    version: Option<String>,
    /// Let --version install a release older than this one. Its database
    /// support may be older too: back up the Hashi home first.
    #[arg(long, requires = "version")]
    allow_downgrade: bool,
    /// Do not ask for confirmation. Required without a terminal.
    #[arg(long, short = 'y')]
    yes: bool,
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
    /// Include excerpts (500 characters) of the prompts that failed, and their
    /// error text, which can contain agent output (stdout, stderr or the
    /// model's own reply). Without it, failures are listed by kind only.
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
    /// Address to listen on. Keep it on loopback and use --tunnel for remote
    /// access. Default: the one saved from the dashboard in
    /// `<home>/settings.json`, else 127.0.0.1.
    #[arg(long, env = "HASHI_BIND")]
    bind: Option<std::net::IpAddr>,
    /// Port to listen on. Default: the one saved from the dashboard in
    /// `<home>/settings.json`, else 7878.
    #[arg(short, long, env = "HASHI_PORT")]
    port: Option<u16>,
    /// Expose the API through a Cloudflare quick tunnel (requires `cloudflared`).
    #[arg(long, env = "HASHI_TUNNEL")]
    tunnel: bool,
    /// Go public through Tailscale: `serve` (your tailnet) or `funnel` (the
    /// internet). Needs `tailscale`, signed in.
    #[arg(long, env = "HASHI_TAILSCALE", value_enum,
          conflicts_with_all = ["tunnel", "tunnel_token"])]
    tailscale: Option<hashi::tailscale::Mode>,
    /// HTTPS port for --tailscale (Funnel allows 443, 8443 or 10000).
    #[arg(long, default_value_t = hashi::tailscale::DEFAULT_PORT, env = "HASHI_TAILSCALE_PORT",
          value_parser = clap::value_parser!(u16).range(1..))]
    tailscale_port: u16,
    #[command(flatten)]
    helpers: HelperArgs,
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
    /// Do not check GitHub for a newer release (checked at most once a day).
    #[arg(long, env = "HASHI_NO_UPDATE_CHECK")]
    no_update_check: bool,
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
    /// Behind a reverse proxy on this host (Caddy, nginx): key rate limits by
    /// the last X-Forwarded-For hop of requests from loopback.
    #[arg(long, env = "HASHI_TRUST_PROXY")]
    trust_proxy: bool,
    /// Web origin allowed to call the API from a browser, such as
    /// https://app.example.com (repeatable). Default: none.
    #[arg(long = "cors-origin", env = "HASHI_CORS_ORIGINS", value_delimiter = ',',
          value_parser = hashi::net::parse_origin)]
    cors_origins: Vec<String>,
    /// Extra host name this server answers to, besides IP addresses,
    /// localhost and the public URL's host (repeatable).
    #[arg(long = "allowed-host", env = "HASHI_ALLOWED_HOSTS", value_delimiter = ',',
          value_parser = hashi::net::parse_allowed_host)]
    allowed_hosts: Vec<String>,
    /// Maximum pending requests across all agents.
    #[arg(long, default_value_t = 100, env = "HASHI_QUEUE_LIMIT")]
    queue_limit: u64,
    /// Days to keep finished requests.
    #[arg(long, default_value_t = 14, env = "HASHI_RETENTION_DAYS")]
    retention_days: u32,
    /// Minutes an account that hit its limit is skipped when the agent's
    /// message does not say when the limit resets.
    #[arg(long, default_value_t = 60, env = "HASHI_ACCOUNT_COOLDOWN",
          value_parser = clap::value_parser!(u32).range(1..=10_080))]
    account_cooldown: u32,
    /// Do not read Claude Code and Codex quota in the background. Runs and
    /// their limit errors are still recorded. `HASHI_USAGE_POLL=0` does the
    /// same.
    #[arg(long)]
    no_usage_poll: bool,
    /// How runs choose among an agent's accounts when the project sets
    /// nothing: in-order, most-left or soonest-reset.
    #[arg(long, default_value = "in-order", env = "HASHI_ACCOUNT_STRATEGY",
          value_parser = parse_strategy)]
    account_strategy: hashi::usage::Strategy,
    /// Percent used (50 to 99) at which runs move off an account, when the
    /// project sets nothing.
    #[arg(long, value_name = "PCT", default_value_t = hashi::usage::DEFAULT_ROTATE_AT,
          env = "HASHI_ROTATE_AT", value_parser = clap::value_parser!(u8).range(50..=99))]
    rotate_at: u8,
    #[command(flatten)]
    agents: AgentArgs,
}

fn parse_strategy(s: &str) -> Result<hashi::usage::Strategy, String> {
    s.parse()
}

/// Whether `HASHI_USAGE_POLL` turns background usage polling off (`0`,
/// `false`, `no` or `off`).
fn usage_poll_env_off() -> bool {
    std::env::var("HASHI_USAGE_POLL").is_ok_and(|v| {
        matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "off"
        )
    })
}

#[derive(Debug, Args)]
struct AskArgs {
    /// The prompt.
    #[arg(required_unless_present = "remote")]
    message: Option<String>,
    /// Send the prompt to a bridge saved by `hashi connect` instead of a
    /// local agent. The name can be left out when only one is saved.
    #[arg(long, value_name = "NAME", num_args = 0..=1, default_missing_value = "")]
    remote: Option<String>,
    /// Which agent to ask. Default: claude, or the server's default with --remote.
    #[arg(short, long)]
    agent: Option<AgentKind>,
    /// Model override.
    #[arg(short, long)]
    model: Option<String>,
    /// Time limit in seconds.
    #[arg(long, default_value_t = 300)]
    timeout: u64,
    #[command(flatten)]
    agents: AgentArgs,
}

#[derive(Debug, Args)]
struct ConnectArgs {
    /// The invite link, https://<host>/claim/hashi_inv_<code>.
    url: String,
    /// Name to save the bridge under. Default: the server's name.
    #[arg(long)]
    name: Option<String>,
    /// Also add the bridge to this tool as an MCP server.
    #[arg(long, value_parser = ["claude"])]
    setup: Option<String>,
    /// Path to the Claude Code executable, for --setup claude.
    #[arg(long, env = "HASHI_CLAUDE_BIN")]
    claude_bin: Option<PathBuf>,
}

#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
struct RequestArgs {
    #[command(subcommand)]
    command: Option<RequestCommand>,
    /// The ask-access link, https://<host>/ask-access/hashi_ask_<code>.
    url: Option<String>,
    /// Your name, as the owner will see it. Default: your login name.
    #[arg(long)]
    name: Option<String>,
    /// What you want access for.
    #[arg(long)]
    message: Option<String>,
    /// Agents you want (comma separated). Default: all the link offers.
    #[arg(long, value_delimiter = ',')]
    agents: Option<Vec<String>>,
    /// Name to save the bridge under once accepted. Default: the server's
    /// name.
    #[arg(long)]
    remote_name: Option<String>,
}

#[derive(Debug, Subcommand)]
enum RequestCommand {
    /// Check your pending requests. Accepted ones are saved as remotes,
    /// as `hashi connect` saves them.
    Status,
}

#[derive(Debug, Subcommand)]
enum RemoteCommand {
    /// Show saved bridges (names and URLs, never tokens).
    List,
    /// Forget a saved bridge.
    Remove {
        /// Its name, as `hashi remote list` shows it.
        name: String,
    },
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
    hashi::llm::set_command(<Cli as clap::CommandFactory>::command);
    let cli = Cli::parse();
    let home = home_dir(cli.home)?;
    // Before the dashboard starts: ratatui chains its terminal restore in
    // front of this hook, so the crash note prints on a sane terminal.
    diagnostics::install_panic_hook(home.join("logs"));
    // A Windows update leaves the old binary beside the new one.
    if let Ok(exe) = std::env::current_exe() {
        hashi::update::cleanup_old_exe(&exe);
    }
    match cli.command {
        None => serve(&home, ServeArgs::parse_from(["hashi"])).await,
        Some(Command::Serve(args)) => serve(&home, *args).await,
        Some(Command::Token(cmd)) => admin_cli::token(&server::open_store(&home)?, cmd),
        Some(Command::Share(args)) => admin_cli::share(&server::open_store(&home)?, args),
        Some(Command::ShareLogin(args)) => {
            admin_cli::share_login(&server::open_store(&home)?, &home, args)
        }
        Some(Command::Invite(cmd)) => admin_cli::invite(&server::open_store(&home)?, cmd),
        Some(Command::Account(cmd)) => admin_cli::account(&server::open_store(&home)?, &home, cmd),
        Some(Command::Switch { agent, name }) => {
            admin_cli::switch(&server::open_store(&home)?, &home, agent, name)
        }
        Some(Command::Project(cmd)) => admin_cli::project(&server::open_store(&home)?, &home, cmd),
        Some(Command::Audit { limit, json }) => {
            admin_cli::audit(&server::open_store(&home)?, limit, json)
        }
        Some(Command::Ask(args)) if args.remote.is_some() => ask_remote(home, args).await,
        Some(Command::Ask(args)) => ask(&home, args).await,
        Some(Command::Connect(args)) => connect(home, args).await,
        Some(Command::Remote(RemoteCommand::List)) => hashi::remote::list(&home),
        Some(Command::Pool(cmd)) => {
            let store = server::open_store(&home)?;
            tokio::task::spawn_blocking(move || admin_cli::pool(&store, &home, cmd))
                .await
                .context("the pool command stopped")?
        }
        Some(Command::Remote(RemoteCommand::Remove { name })) => {
            hashi::remote::remove(&home, &name)
        }
        Some(Command::Request(args)) => request(home, args).await,
        Some(Command::Requests(cmd)) => admin_cli::requests(&server::open_store(&home)?, cmd),
        Some(Command::Doctor(args)) => doctor(&home, &args).await,
        Some(Command::Install(args)) => tokio::task::spawn_blocking(move || install(&args)).await?,
        Some(Command::Report(args)) => report(&home, args).await,
        Some(Command::Update(args)) => {
            tokio::task::spawn_blocking(move || update(&home, &args)).await?
        }
        Some(Command::Llm(args)) => llm(&args),
        Some(Command::Uninstall(args)) => hashi::uninstall::run(
            &home,
            hashi::uninstall::Options {
                keep_data: args.keep_data,
                yes: args.yes,
                dry_run: args.dry_run,
            },
        ),
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
    let file =
        diagnostics::RotatingLog::open(&dir, diagnostics::MAX_LOG_BYTES, diagnostics::KEEP_LOGS)
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
    if args.tunnel_token.is_some() && hashi::tunnel::token_in_argv(std::env::args_os()) {
        eprintln!("{}", hashi::tunnel::ARGV_TOKEN_WARNING);
    }
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

    // A flag or variable wins over the dashboard's saved choice.
    let saved = hashi::settings::load(home).unwrap_or_else(|why| {
        tracing::warn!("{why}");
        hashi::settings::Settings::default()
    });
    let (bind, bind_from) =
        hashi::settings::resolve(args.bind, saved.bind, hashi::settings::DEFAULT_BIND);
    let (port, port_from) =
        hashi::settings::resolve(args.port, saved.port, hashi::settings::DEFAULT_PORT);
    if let Some(warning) = server::plain_text_warning(bind, args.tls_cert.is_some()) {
        eprintln!("{warning}");
    }

    let cfg = ServerConfig {
        bind: SocketAddr::new(bind, port),
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
        update_check: !args.no_update_check,
        tailscale: args.tailscale,
        helpers: server::Helpers {
            tailscale_port: args.tailscale_port,
            tailscale_bin: args.helpers.tailscale_bin.clone(),
            cloudflared_bin: args.helpers.cloudflared_bin.clone(),
        },
        account_cooldown: Duration::from_secs(u64::from(args.account_cooldown) * 60),
        usage: hashi::usage::UsageSettings {
            poll: !args.no_usage_poll && !usage_poll_env_off(),
            policy: hashi::usage::Policy {
                strategy: args.account_strategy,
                rotate_at: args.rotate_at,
            },
            ..hashi::usage::UsageSettings::default()
        },
    };

    let bus = Bus::default();
    let events = bus.subscribe();
    let net = hashi::net::NetLimits {
        max_connections,
        trust_proxy: args.trust_proxy,
        cors_origins: args.cors_origins.clone(),
        allowed_hosts: args.allowed_hosts.clone(),
        ..hashi::net::NetLimits::default()
    };
    let running = server::start_with(cfg, net, bus).await?;
    running
        .state
        .runtime
        .listen
        .set_overrides(hashi::net::Overrides {
            port: port_from == hashi::settings::Source::Override,
            bind: bind_from == hashi::settings::Source::Override,
        });
    // Ctrl-C, SIGTERM and SIGHUP (or the Windows console closing) all stop
    // the server the same graceful way; a second one exits at once.
    let mut signals = hashi::shutdown::Signals::install().context("installing signal handlers")?;

    if tui {
        let result = hashi::tui::run_until(
            running.state.clone(),
            events,
            running.bootstrap_token.clone(),
            args.tunnel || args.tailscale.is_some(),
            // While an install runs from the dashboard, Ctrl-C is meant
            // for it (the terminal sends it to both), not for Hashi.
            hashi::tui::stop_unless_attached(&mut signals),
        )
        .await;
        let _force = hashi::shutdown::exit_on_next(signals);
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
    let why = signals.recv().await;
    eprintln!("\nshutting down ({why}), press Ctrl-C again to exit at once…");
    printer.abort();
    let _force = hashi::shutdown::exit_on_next(signals);
    running.shutdown().await;
    Ok(())
}

fn print_banner(running: &server::Running) {
    let rt = &running.state.runtime;
    println!();
    println!("  ✦ hashi {}  the bridge", env!("CARGO_PKG_VERSION"));
    println!();
    println!("  local    {}", rt.local_url());
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

/// Agent output, token names and tunnel messages reach this line, so it is
/// sanitised and redacted by [`diagnostics::headless_line`].
fn print_event(ev: &Event) {
    println!("{}", diagnostics::headless_line(ev));
}

async fn ask(home: &std::path::Path, args: AskArgs) -> Result<()> {
    if let Some(m) = &args.model
        && !agent::is_safe_arg(m)
    {
        bail!("model contains unsupported characters");
    }
    let kind = args.agent.unwrap_or(AgentKind::Claude);
    let message = args.message.context("a prompt is required")?;
    let configs = args.agents.configs();
    let Some(cfg) = configs.get(&kind) else {
        bail!("{}", agent::unavailable_message(kind));
    };
    let workdir = home.join("workspaces").join("cli");
    std::fs::create_dir_all(&workdir)?;
    let inv = Invocation {
        prompt: &message,
        resume: None,
        model: args.model.as_deref(),
    };
    let limits = Limits {
        timeout: Duration::from_secs(args.timeout),
        ..Limits::default()
    };
    // The agent runs in its own process group, so the terminal's Ctrl-C does
    // not reach it. Dropping the run on Ctrl-C kills its whole tree instead.
    // The default account: the agent's own environment, unchanged.
    let account = hashi::accounts::run_env(
        home,
        kind,
        hashi::accounts::DEFAULT,
        hashi::accounts::AccountKind::Default,
    )?;
    let out = tokio::select! {
        out = agent::execute_as(kind, cfg, inv, &workdir, limits, account.as_ref()) => out?,
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

/// `hashi ask --remote [<name>] "<prompt>"`. With one value after
/// `--remote` and no prompt, that value is the prompt.
async fn ask_remote(home: PathBuf, args: AskArgs) -> Result<()> {
    let remote = args.remote.unwrap_or_default();
    let (name, message) = match args.message {
        Some(m) => ((!remote.is_empty()).then_some(remote), m),
        None => (None, remote),
    };
    if message.trim().is_empty() {
        bail!("a prompt is required: hashi ask --remote [<name>] \"<prompt>\"");
    }
    let opts = hashi::remote::AskOptions {
        name,
        message,
        agent: args.agent.map(|a| a.as_str().to_owned()),
        model: args.model,
    };
    let run = tokio::task::spawn_blocking(move || hashi::remote::ask(&home, &opts));
    let out = tokio::select! {
        out = run => out.context("the request thread stopped")??,
        _ = tokio::signal::ctrl_c() => {
            // The blocking request cannot be cancelled; leave without
            // waiting for it. The prompt may still finish on the server.
            eprintln!("interrupted");
            std::process::exit(130);
        }
    };
    println!("{}", out.response);
    if let Some(u) = out.usage {
        eprintln!("\n{u}");
    }
    Ok(())
}

async fn connect(home: PathBuf, args: ConnectArgs) -> Result<()> {
    let setup_claude = args.setup.is_some().then(|| {
        args.claude_bin
            .clone()
            .unwrap_or_else(|| AgentConfig::default_for(AgentKind::Claude).bin)
    });
    let opts = hashi::remote::ConnectOptions {
        url: args.url,
        name: args.name,
        setup_claude,
    };
    tokio::task::spawn_blocking(move || hashi::remote::connect(&home, &opts).map(|_| ()))
        .await
        .context("the connect thread stopped")?
}

async fn request(home: PathBuf, args: RequestArgs) -> Result<()> {
    let job = move || -> Result<()> {
        if matches!(args.command, Some(RequestCommand::Status)) {
            return hashi::access_client::status(&home);
        }
        let Some(url) = args.url else {
            bail!("give the ask-access link: hashi request <link>, or hashi request status");
        };
        let name = args
            .name
            .or_else(|| std::env::var("USER").ok())
            .or_else(|| std::env::var("USERNAME").ok())
            .filter(|n| !n.trim().is_empty())
            .context("give your name with --name")?;
        let opts = hashi::access_client::RequestOptions {
            url,
            name,
            message: args.message,
            agents: args.agents,
            remote_name: args.remote_name,
        };
        hashi::access_client::request(&home, &opts).map(|_| ())
    };
    tokio::task::spawn_blocking(job)
        .await
        .context("the request thread stopped")?
}

/// `hashi llm`: prints the guide, or writes it owner-only with `--output`.
fn llm(args: &LlmArgs) -> Result<()> {
    let guide = hashi::llm::guide();
    match &args.output {
        None => print!("{guide}"),
        Some(path) => {
            diagnostics::write_private(path, &guide)
                .with_context(|| format!("writing {}", path.display()))?;
            eprintln!(
                "wrote the guide ({} lines) to {}",
                guide.lines().count(),
                path.display()
            );
        }
    }
    Ok(())
}

/// Writes the redacted diagnostic report. Nothing is uploaded.
async fn report(home: &std::path::Path, args: ReportArgs) -> Result<()> {
    let agents = server::discover_agents(&args.agents.configs()).await;
    let db = home.join("hashi.db");
    // Opening creates the file, so only open a database that already exists.
    let store = if db.exists() {
        server::open_store(home).map_err(|e| format!("{e:#}"))
    } else {
        Err(format!("no database at {}", db.display()))
    };
    let text = diagnostics::report_text(
        home,
        store.as_ref().map_err(Clone::clone),
        &agents,
        args.include_prompts,
    );
    let issue = diagnostics::issue_url();
    if args.stdout {
        print!("{text}");
        eprintln!(
            "\nRead the report before you share it. To file a bug, open:\n{issue}\n\
             Nothing has been uploaded."
        );
        return Ok(());
    }
    let shown =
        diagnostics::save_report(args.out.as_deref(), &text).context("writing the report")?;
    println!("\n  ✦ hashi report\n");
    println!("  Saved {}", shown.display());
    println!();
    if args.include_prompts {
        println!("  Read it before you share it. Secrets are redacted, but it includes prompt");
        println!("  excerpts, agent error output and log lines from this machine.");
    } else {
        println!("  Read it before you share it. Secrets are redacted and prompts, responses and");
        println!("  agent error output are left out, but it has log lines from this machine.");
    }
    println!();
    println!("  To file a bug, open this link and attach the file:");
    println!("  {issue}");
    println!();
    println!("  Nothing has been uploaded.\n");
    Ok(())
}

/// `hashi update`: report, or download, verify and install a release.
fn update(home: &std::path::Path, args: &UpdateArgs) -> Result<()> {
    use hashi::update::{self, Outcome, Sources};
    let current = env!("CARGO_PKG_VERSION");
    let sources = Sources::default();
    if args.check {
        let latest = update::check(home, &sources.api, hashi::util::now_ms(), true)?;
        if update::is_newer(&latest.version, &update::current_version()) {
            println!(
                "hashi v{current}: v{} is available, run hashi update",
                latest.version
            );
        } else {
            println!("hashi v{current} is up to date");
        }
        return Ok(());
    }
    let exe = std::env::current_exe().context("finding the running binary")?;
    server::prepare_home(home)?;
    let ask = update::needs_prompt(args.yes, std::io::stdin().is_terminal())?;
    let mut confirm = |plan: &update::Plan| {
        if let Some(warning) = update::downgrade_warning(plan) {
            eprintln!("{warning}");
        }
        if !ask {
            return true;
        }
        eprint!(
            "Update hashi v{current} to {} at {}? [y/N] ",
            plan.tag,
            plan.exe.display()
        );
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer).is_ok()
            && matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
    };
    match update::install(
        home,
        &sources,
        &exe,
        args.version.as_deref(),
        args.allow_downgrade,
        &mut confirm,
    )? {
        Outcome::UpToDate(tag) => {
            println!("hashi v{current} is already the latest release ({tag})");
        }
        Outcome::Declined => println!("not updated"),
        Outcome::Updated(tag) => println!("updated to {tag}; restart hashi to use it"),
    }
    Ok(())
}

/// `hashi install cloudflared|tailscale`: shows the vendor's install
/// command for this machine and runs it only after a yes.
fn install(args: &InstallArgs) -> Result<()> {
    use hashi::install::{self, Plan};
    let helper = args.helper;
    println!("\n  ✦ hashi install {helper}\n");
    if let Some(path) = install::locate(helper, None) {
        println!("  {helper} is already installed at {}\n", path.display());
        return Ok(());
    }
    let plan = install::plan_here(helper);
    match &plan {
        Plan::Download { url, why, .. } => {
            println!("  No command to run here: {why}.");
            println!("  Download it from:\n\n    {url}\n");
            return Ok(());
        }
        Plan::Run {
            via,
            steps,
            source,
            note,
            ..
        } => {
            println!("  The official install ({via}), from {source}:\n");
            for step in steps {
                println!("    {}", step.shown);
            }
            println!();
            if let Some(note) = note {
                println!("  note: {note}\n");
            }
        }
    }
    if !args.yes {
        if !std::io::stdin().is_terminal() {
            bail!("not installing without confirmation; rerun with --yes to run it");
        }
        eprint!("  Run it now? [y/N] ");
        let mut answer = String::new();
        std::io::stdin().read_line(&mut answer)?;
        if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
            println!("  not installed");
            return Ok(());
        }
    }
    install::run(&plan)?;
    match install::locate(helper, None) {
        Some(path) => println!("\n  ✓ {helper} installed at {}\n", path.display()),
        None => println!(
            "\n  The install finished, but {helper} is not on PATH yet; open a new terminal.\n"
        ),
    }
    if helper == install::Helper::Tailscale {
        println!("  Sign in next: {}\n", hashi::tailscale::sign_in_hint());
    }
    Ok(())
}

/// Doctor's update state: always asks GitHub (ignoring the cache's age)
/// unless `HASHI_NO_UPDATE_CHECK` is set.
async fn doctor_update(home: &std::path::Path) -> hashi::doctor::UpdateState {
    use hashi::doctor::UpdateState;
    if hashi::update::opted_out() {
        return UpdateState::OptedOut;
    }
    let home = home.to_path_buf();
    let latest = tokio::task::spawn_blocking(move || {
        hashi::update::check(
            &home,
            hashi::update::LATEST_URL,
            hashi::util::now_ms(),
            true,
        )
    })
    .await;
    match latest {
        Ok(Ok(r)) => UpdateState::Latest(Ok(r.version)),
        Ok(Err(e)) => UpdateState::Latest(Err(format!("{e:#}"))),
        Err(_) => UpdateState::Unknown,
    }
}

async fn doctor(home: &std::path::Path, args: &DoctorArgs) -> Result<()> {
    use hashi::doctor;
    use hashi::install::{self, Helper};
    println!("\n  ✦ hashi doctor\n");
    let found_agents = server::discover_agents(&args.agents.configs()).await;
    let (mut checks, found) = doctor::agents(&found_agents);
    let cloudflared = install::locate(Helper::Cloudflared, args.helpers.cloudflared_bin.as_deref());
    checks.push(doctor::cloudflared(cloudflared.as_deref()));
    // Tailscale's sign-in state is read only when it is installed.
    let tailscale = install::locate(Helper::Tailscale, args.helpers.tailscale_bin.as_deref());
    let status = match &tailscale {
        Some(bin) => Some(
            hashi::tailscale::self_status(bin)
                .await
                .map_err(|e| e.to_string()),
        ),
        None => None,
    };
    checks.push(doctor::tailscale(tailscale.as_deref(), status));
    // The database is read only when it exists: opening creates it.
    match home
        .join("hashi.db")
        .exists()
        .then(|| server::open_store(home))
    {
        Some(Err(_)) => checks.push(doctor::Check {
            mark: doctor::Mark::Warn,
            name: "accounts".to_owned(),
            detail: "could not open the database".to_owned(),
        }),
        Some(Ok(store)) => checks.extend(doctor::accounts(Some(&store))),
        None => checks.extend(doctor::accounts(None)),
    }
    checks.push(doctor::update(&doctor_update(home).await));
    for check in &checks {
        println!("  {}", check.line());
    }
    println!();
    println!("  {}\n", doctor::advice(found));
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;

    fn configs(args: &[&str]) -> HashMap<AgentKind, AgentConfig> {
        let mut argv = vec!["hashi", "serve"];
        argv.extend_from_slice(args);
        match Cli::try_parse_from(argv).map(|c| c.command) {
            Ok(Some(Command::Serve(serve))) => serve.agents.configs(),
            other => panic!("unexpected parse: {other:?}"),
        }
    }

    #[test]
    fn every_agent_is_configured() {
        let all = configs(&["--cursor-bin", "/opt/cursor-agent"]);
        assert_eq!(all.len(), AgentKind::ALL.len());
        assert_eq!(
            all[&AgentKind::Cursor].bin,
            PathBuf::from("/opt/cursor-agent")
        );
    }

    /// Every leaf subcommand path, such as `token update`, and each
    /// visible alias of a leaf, such as `guide` for `llm`.
    fn leaf_paths(cmd: &clap::Command, prefix: &str, out: &mut Vec<String>) {
        let join = |name: &str| {
            if prefix.is_empty() {
                name.to_owned()
            } else {
                format!("{prefix} {name}")
            }
        };
        for sub in cmd.get_subcommands().filter(|s| s.get_name() != "help") {
            let path = join(sub.get_name());
            if sub.has_subcommands() {
                leaf_paths(sub, &path, out);
            } else {
                out.push(path);
                out.extend(sub.get_visible_aliases().map(join));
            }
        }
    }

    #[test]
    fn the_llm_guide_names_every_subcommand_path() {
        use clap::CommandFactory;
        let cmd = Cli::command();
        let guide = hashi::llm::guide_for(&cmd);
        let paths = hashi::llm::command_paths(&cmd);
        assert!(paths.len() > 30, "{paths:?}");
        for path in &paths {
            assert!(
                guide.contains(&format!("### `hashi {path}`")),
                "the guide has no section for `hashi {path}`"
            );
        }
        // Flags come with their help and say whether they take a value.
        assert!(guide.contains("`--expires-in-days <EXPIRES_IN_DAYS>`"));
        assert!(guide.contains("`--admin`: Grant admin rights. Switch, takes no value."));
        assert!(guide.contains("Also: `guide`."));
        let lines = guide.lines().count();
        assert!((300..=1_200).contains(&lines), "{lines} lines");
    }

    #[test]
    fn the_llm_guide_has_no_secrets_or_dash_substitutes() {
        use clap::CommandFactory;
        let guide = hashi::llm::guide_for(&Cli::command());
        // `hashi_` then 8 or more hex digits is a token, a prefix or a code.
        for (i, _) in guide.match_indices("hashi_") {
            let rest = &guide[i + "hashi_".len()..];
            let rest = ["inv_", "ask_", "pick_"]
                .iter()
                .find_map(|p| rest.strip_prefix(p))
                .unwrap_or(rest);
            let hex = rest.chars().take_while(char::is_ascii_hexdigit).count();
            assert!(hex < 8, "token-like text at byte {i}");
        }
        for word in ["Bearer hashi", "sk-ant-", "sk-proj-", "sk_live_"] {
            assert!(!guide.contains(word), "{word}");
        }
        for c in [
            '\u{2014}', '\u{2012}', '\u{2015}', '\u{2212}', '\u{FE58}', '\u{FF0D}',
        ] {
            assert!(!guide.contains(c), "U+{:04X}", u32::from(c));
        }
        assert!(!guide.contains(" \u{2013} "));
    }

    #[cfg(unix)]
    #[test]
    fn llm_output_writes_the_guide_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("guide.md");
        std::fs::write(&path, "old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        let args = match Cli::try_parse_from(["hashi", "guide", "--output", path.to_str().unwrap()])
            .map(|c| c.command)
        {
            Ok(Some(Command::Llm(args))) => args,
            other => panic!("unexpected parse: {other:?}"),
        };
        llm(&args).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# Running Hashi for your owner"));
        assert!(text.contains("## Rules"));
    }

    #[test]
    fn every_subcommand_is_in_the_dashboard_or_excluded() {
        use clap::CommandFactory;
        use hashi::tui::parity::{self, Entry};
        let mut paths = Vec::new();
        leaf_paths(&Cli::command(), "", &mut paths);
        assert!(paths.len() > 20, "{paths:?}");
        let missing: Vec<&String> = paths
            .iter()
            .filter(|p| parity::entry(p).is_none())
            .collect();
        assert!(
            missing.is_empty(),
            "add these subcommands to src/tui/parity.rs: {missing:?}"
        );
        let stale: Vec<&str> = parity::TABLE
            .iter()
            .map(|(p, _)| *p)
            .filter(|p| !paths.iter().any(|q| q == p))
            .collect();
        assert!(stale.is_empty(), "no such subcommands: {stale:?}");
        // Only serve and uninstall stay out, and every row is a real row.
        for (path, entry) in parity::TABLE {
            match entry {
                Entry::Excluded(_) => assert!(
                    matches!(*path, "serve" | "uninstall"),
                    "{path} must be in the dashboard"
                ),
                Entry::Row(action) | Entry::InPopup { row: action, .. } => {
                    assert!(hashi::tui::app::Action::ALL.contains(action), "{path}");
                }
                Entry::Key { view, key } => {
                    assert!(
                        hashi::tui::app::Tab::ALL.iter().any(|t| t.title() == *view),
                        "{path}: no view {view}"
                    );
                    assert!(!key.is_empty());
                }
            }
        }
        let mut seen: Vec<&str> = parity::TABLE.iter().map(|(p, _)| *p).collect();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), parity::TABLE.len(), "a path is listed twice");
    }

    #[test]
    fn port_and_bind_are_unset_without_a_flag_so_the_saved_value_can_apply() {
        // HASHI_PORT or HASHI_BIND in the test's own environment would set them.
        if std::env::var_os("HASHI_PORT").is_some() || std::env::var_os("HASHI_BIND").is_some() {
            return;
        }
        let serve = |args: &[&str]| {
            let mut argv = vec!["hashi", "serve"];
            argv.extend_from_slice(args);
            match Cli::try_parse_from(argv).map(|c| c.command) {
                Ok(Some(Command::Serve(serve))) => (serve.port, serve.bind),
                other => panic!("unexpected parse: {other:?}"),
            }
        };
        assert_eq!(serve(&[]), (None, None));
        assert_eq!(
            serve(&["--port", "9000", "--bind", "0.0.0.0"]),
            (Some(9000), Some("0.0.0.0".parse().unwrap()))
        );
        let home = tempfile::tempdir().unwrap();
        hashi::settings::save(
            home.path(),
            &hashi::settings::Settings {
                port: Some(8000),
                ..Default::default()
            },
        )
        .unwrap();
        let saved = hashi::settings::load(home.path()).unwrap();
        let (port, from) = hashi::settings::resolve(serve(&[]).0, saved.port, 7878);
        assert_eq!((port, from), (8000, hashi::settings::Source::Saved));
        let (port, from) = hashi::settings::resolve(serve(&["-p", "9001"]).0, saved.port, 7878);
        assert_eq!((port, from), (9001, hashi::settings::Source::Override));
    }

    #[test]
    fn allow_downgrade_needs_a_version() {
        let parse = |args: &[&str]| {
            let mut argv = vec!["hashi", "update"];
            argv.extend_from_slice(args);
            Cli::try_parse_from(argv).map(|c| match c.command {
                Some(Command::Update(u)) => (u.version, u.allow_downgrade),
                other => panic!("unexpected parse: {other:?}"),
            })
        };
        assert!(parse(&["--allow-downgrade"]).is_err());
        assert_eq!(
            parse(&["--version", "v0.1.0", "--allow-downgrade"]).unwrap(),
            (Some("v0.1.0".to_owned()), true)
        );
        assert_eq!(parse(&[]).unwrap(), (None, false));
    }
}
