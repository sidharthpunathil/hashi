//! Offline admin commands that work straight on the database: tokens,
//! invites, accounts, projects and the audit log. Every change is audited
//! with actor `cli`.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::builder::TypedValueParser;
use clap::{Args, Subcommand};
use hashi::accounts::{self as acct, AccountKind, AccountMap};
use hashi::agent::{AgentConfig, AgentKind};
use hashi::store::{NewToken, Revoke, Store};
use hashi::token::{Limits, Role, SessionMode, Token};
use hashi::util::{human_age, now_ms, rfc3339};

/// Usage caps accepted by several commands.
#[derive(Debug, Args, Clone, Default)]
pub struct LimitArgs {
    /// Prompts per UTC day.
    #[arg(long)]
    pub daily_requests: Option<u32>,
    /// Prompts per UTC month.
    #[arg(long)]
    pub monthly_requests: Option<u32>,
    /// Estimated spend per UTC day in USD (only Claude Code and opencode report cost).
    #[arg(long, value_parser = parse_budget)]
    pub daily_budget: Option<f64>,
    /// Estimated spend per UTC month in USD.
    #[arg(long, value_parser = parse_budget)]
    pub monthly_budget: Option<f64>,
    /// Prompts queued or running at once.
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..=100))]
    pub max_pending: Option<u32>,
}

impl LimitArgs {
    fn any(&self) -> bool {
        self.daily_requests.is_some()
            || self.monthly_requests.is_some()
            || self.daily_budget.is_some()
            || self.monthly_budget.is_some()
            || self.max_pending.is_some()
    }

    /// Overlays the given flags on `base`.
    fn apply(&self, base: Limits) -> Limits {
        Limits {
            daily_requests: self.daily_requests.or(base.daily_requests),
            monthly_requests: self.monthly_requests.or(base.monthly_requests),
            daily_budget_usd: self.daily_budget.or(base.daily_budget_usd),
            monthly_budget_usd: self.monthly_budget.or(base.monthly_budget_usd),
            max_pending: self.max_pending.or(base.max_pending),
        }
    }
}

/// One `--account <agent>=<name>[,<name>...]` value.
type Binding = (AgentKind, Vec<String>);

fn parse_account(s: &str) -> Result<Binding, String> {
    acct::parse_binding(s)
}

/// `base` with each `--account` binding replacing that agent's list,
/// checked against the accounts that exist.
fn bindings(store: &Store, base: AccountMap, flags: Vec<Binding>) -> Result<AccountMap> {
    let mut map = base;
    for (agent, names) in flags {
        map.insert(agent, names);
    }
    hashi::api::check_accounts(store, map).map_err(|e| anyhow::anyhow!("{e}"))
}

fn parse_budget(s: &str) -> Result<f64, String> {
    let v: f64 = s
        .trim_start_matches('$')
        .parse()
        .map_err(|_| "expected an amount like 5 or 2.50".to_owned())?;
    if v.is_finite() && v >= 0.0 {
        Ok(v)
    } else {
        Err("budget must be zero or more".to_owned())
    }
}

/// `hashi token ...`
#[derive(Debug, Subcommand)]
pub enum TokenCommand {
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
        /// Who or what the token is for.
        #[arg(long)]
        note: Option<String>,
        /// Accounts this token uses for an agent, in order, for example
        /// claude=work,personal (repeatable). Default: each agent's own login.
        #[arg(long = "account", value_name = "AGENT=NAMES", value_parser = parse_account)]
        accounts: Vec<Binding>,
        /// Bind the token to this project: it runs in the project's folder,
        /// with the agents both allow and accounts inside the project's.
        #[arg(long)]
        project: Option<String>,
        #[command(flatten)]
        limits: LimitArgs,
    },
    /// List tokens with status and today's use.
    List,
    /// Pause, resume, extend or limit a token.
    Update {
        /// Token id or display prefix.
        id: String,
        /// Stop the token from sending prompts until resumed.
        #[arg(long, conflicts_with = "resume")]
        pause: bool,
        /// Let a paused token send prompts again.
        #[arg(long)]
        resume: bool,
        /// Replace the note.
        #[arg(long)]
        note: Option<String>,
        /// New lifetime from now in days; 0 removes the expiry.
        #[arg(long, value_parser = clap::value_parser!(u32).range(0..=3650))]
        expires_in_days: Option<u32>,
        /// Remove every limit before applying the limit flags.
        #[arg(long)]
        clear_limits: bool,
        /// Set the accounts for an agent, in order, for example
        /// claude=work,personal (repeatable).
        #[arg(long = "account", value_name = "AGENT=NAMES", value_parser = parse_account)]
        accounts: Vec<Binding>,
        /// Put every agent back on its default account before applying
        /// --account.
        #[arg(long)]
        clear_accounts: bool,
        /// Bind the token to this project.
        #[arg(long, conflicts_with = "clear_project")]
        project: Option<String>,
        /// Put the token back on its own workspace.
        #[arg(long)]
        clear_project: bool,
        #[command(flatten)]
        limits: LimitArgs,
    },
    /// Revoke a token by id or prefix.
    Revoke {
        /// Token id or display prefix.
        id: String,
    },
}

/// `hashi share ...`: create a one-time invite link.
#[derive(Debug, Args)]
pub struct ShareArgs {
    /// Who the invite is for. Becomes the token's name.
    pub label: String,
    /// Restrict to these agents (comma separated).
    #[arg(long, value_delimiter = ',')]
    pub agents: Option<Vec<AgentKind>>,
    /// Keep a conversation per agent across prompts.
    #[arg(long)]
    pub stateful: bool,
    /// The token expires this many days after it is claimed.
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..=3650))]
    pub expires_in_days: Option<u32>,
    /// How long the link works, in hours.
    #[arg(
        long,
        default_value = "24",
        value_parser = clap::builder::PossibleValuesParser::new(["1", "24", "168"])
            .map(|s| s.parse::<u32>().unwrap_or(24)),
    )]
    pub link_hours: u32,
    /// Base URL for the link. Defaults to HASHI_PUBLIC_URL, else the public URL
    /// of the running server (from `<home>/public-url`), else this machine.
    #[arg(long, env = "HASHI_PUBLIC_URL")]
    pub public_url: Option<String>,
    /// Port of the local server, used for a local-only link.
    #[arg(long, default_value_t = 7878, env = "HASHI_PORT")]
    pub port: u16,
    /// Accounts the token uses for an agent, in order, for example
    /// claude=work,personal (repeatable).
    #[arg(long = "account", value_name = "AGENT=NAMES", value_parser = parse_account)]
    pub accounts: Vec<Binding>,
    /// Bind the token to this project. The claim page shows only its name.
    #[arg(long)]
    pub project: Option<String>,
    #[command(flatten)]
    pub limits: LimitArgs,
}

/// `hashi invite ...`
#[derive(Debug, Subcommand)]
pub enum InviteCommand {
    /// List invites.
    List,
    /// Cancel a pending invite.
    Cancel {
        /// Invite id.
        id: String,
    },
}

fn find_token(store: &Store, id_or_prefix: &str) -> Result<Token> {
    let matches: Vec<Token> = store
        .list_tokens()?
        .into_iter()
        .filter(|t| t.id == id_or_prefix || t.prefix == id_or_prefix)
        .collect();
    match matches.as_slice() {
        [t] => Ok(t.clone()),
        [] => bail!("no token matches `{id_or_prefix}`"),
        _ => bail!("`{id_or_prefix}` matches several tokens; use the full id"),
    }
}

/// Checks binding to `project` (by name), with these agents and accounts.
fn bind(
    store: &Store,
    project: Option<String>,
    agents: &[AgentKind],
    accounts: &AccountMap,
) -> Result<Option<String>> {
    let project = hashi::api::check_project_name(project).map_err(|e| anyhow::anyhow!("{e}"))?;
    hashi::api::check_binding(store, project.as_deref(), agents, accounts)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(project)
}

fn clean_label(s: &str) -> Result<String> {
    let s = s.trim();
    if s.is_empty() || s.chars().count() > 100 || s.chars().any(char::is_control) {
        bail!("name must be 1 to 100 printable characters");
    }
    Ok(s.to_owned())
}

/// Runs a `hashi token` command.
///
/// # Errors
///
/// Fails on invalid input or database errors.
pub fn token(store: &Store, cmd: TokenCommand) -> Result<()> {
    match cmd {
        TokenCommand::Create {
            name,
            admin,
            stateful,
            agents,
            expires_in_days,
            note,
            accounts,
            project,
            limits,
        } => {
            let accounts = bindings(store, AccountMap::new(), accounts)?;
            let agents = agents.unwrap_or_else(|| AgentKind::ALL.to_vec());
            let project = bind(store, project, &agents, &accounts)?;
            let role = if admin { Role::Admin } else { Role::User };
            let new = NewToken {
                name: clean_label(&name)?,
                role,
                session_mode: if stateful {
                    SessionMode::Stateful
                } else {
                    SessionMode::Stateless
                },
                agents,
                expires_in_days,
                note,
                limits: if limits.any() {
                    limits.apply(Limits::default())
                } else {
                    NewToken::default_limits(role)
                },
                accounts,
                project,
            };
            let (t, secret) = store.create_token(&new)?;
            store.audit("cli", "token.create", &t.name, t.role.as_str())?;
            eprintln!("created {} token `{}` ({})", t.role, t.name, t.id);
            eprintln!("store it now, it will not be shown again:");
            println!("{}", secret.raw);
        }
        TokenCommand::List => {
            let now = now_ms();
            println!(
                "{:<20} {:<16} {:<6} {:<8} {:<10} {:<13} {:<9} {:<10} {:<12} ACCOUNTS",
                "NAME",
                "PREFIX",
                "ROLE",
                "STATUS",
                "SESSION",
                "AGENTS",
                "TODAY",
                "LAST USED",
                "PROJECT",
            );
            for t in store.list_tokens()? {
                let agents = t
                    .agents
                    .iter()
                    .map(|a| a.as_str())
                    .collect::<Vec<_>>()
                    .join(",");
                let used = t
                    .last_used_at
                    .map_or_else(|| "never".to_owned(), |u| format!("{} ago", human_age(u)));
                let usage = store.token_usage(&t.id, now)?;
                let today = match t.limits.daily_requests {
                    Some(cap) => format!("{}/{cap}", usage.today_requests),
                    None => usage.today_requests.to_string(),
                };
                println!(
                    "{:<20} {:<16} {:<6} {:<8} {:<10} {:<13} {:<9} {:<10} {:<12} {}",
                    t.name,
                    t.prefix,
                    t.role.as_str(),
                    t.status(now).as_str(),
                    t.session_mode.as_str(),
                    agents,
                    today,
                    used,
                    t.project.as_deref().unwrap_or("-"),
                    acct::format_bindings(&t.accounts)
                );
            }
        }
        TokenCommand::Update {
            id,
            pause,
            resume,
            note,
            expires_in_days,
            clear_limits,
            accounts,
            clear_accounts,
            project,
            clear_project,
            limits,
        } => {
            let mut t = find_token(store, &id)?;
            let mut changes = Vec::new();
            if pause {
                t.paused = true;
                changes.push("paused".to_owned());
            }
            if resume {
                t.paused = false;
                changes.push("resumed".to_owned());
            }
            if let Some(n) = note {
                t.note = Some(n.trim().to_owned()).filter(|n| !n.is_empty());
                changes.push("note".to_owned());
            }
            if let Some(days) = expires_in_days {
                t.expires_at = (days > 0).then(|| now_ms() + i64::from(days) * 86_400_000);
                changes.push(if days == 0 {
                    "expiry removed".to_owned()
                } else {
                    format!("expires in {days}d")
                });
            }
            if clear_limits || limits.any() {
                let base = if clear_limits {
                    Limits::default()
                } else {
                    t.limits
                };
                t.limits = limits.apply(base);
                changes.push("limits".to_owned());
            }
            if clear_accounts || !accounts.is_empty() {
                let base = if clear_accounts {
                    AccountMap::new()
                } else {
                    t.accounts.clone()
                };
                t.accounts = bindings(store, base, accounts)?;
                changes.push(format!("accounts {}", acct::format_bindings(&t.accounts)));
            }
            if clear_project {
                t.project = None;
                changes.push("project default".to_owned());
            } else if project.is_some() {
                t.project =
                    hashi::api::check_project_name(project).map_err(|e| anyhow::anyhow!("{e}"))?;
                changes.push(format!(
                    "project {}",
                    t.project.as_deref().unwrap_or(hashi::projects::DEFAULT)
                ));
            }
            if changes.is_empty() {
                bail!("nothing to update; see `hashi token update --help`");
            }
            // Every change is checked, so neither a new project nor new
            // accounts can take the token outside its project.
            t.project = bind(store, t.project.clone(), &t.agents, &t.accounts)?;
            store.save_token_policy(&t)?;
            let detail = changes.join(", ");
            store.audit("cli", "token.update", &t.name, &detail)?;
            println!("updated {}: {detail}", t.name);
        }
        TokenCommand::Revoke { id } => {
            let found = find_token(store, &id).ok();
            let running = match &found {
                Some(t) => store.running_for_token(&t.id)?,
                None => 0,
            };
            let name = found.map(|t| t.name);
            match store.revoke_token(&id)? {
                Revoke::Revoked(token_id) => {
                    let target = name.unwrap_or_else(|| id.clone());
                    store.audit("cli", "token.revoke", &target, "")?;
                    println!("revoked {target}");
                    if running > 0 {
                        // A running server notices the deleted request rows
                        // within a couple of seconds and kills the runs; give
                        // it that long before deleting their workspace.
                        println!("waiting for its {running} running prompt(s) to stop");
                        std::thread::sleep(
                            hashi::queue::ROW_CHECK_EVERY + std::time::Duration::from_secs(1),
                        );
                    }
                    if let Some(home) = store.data_dir()
                        && let Err(e) = hashi::server::remove_workspace(&home, &token_id)
                    {
                        eprintln!("warning: could not delete its workspace: {e}");
                    }
                }
                Revoke::NotFound => bail!("no token matches `{id}`"),
                Revoke::LastAdmin => bail!("refusing to revoke the last admin token"),
            }
        }
    }
    Ok(())
}

/// Runs `hashi share`.
///
/// # Errors
///
/// Fails on invalid input or database errors.
pub fn share(store: &Store, args: ShareArgs) -> Result<()> {
    let mut limits = args.limits.apply(Limits::default());
    limits.max_pending = limits.max_pending.or(Some(3));
    let accounts = bindings(store, AccountMap::new(), args.accounts)?;
    let agents = args.agents.unwrap_or_else(|| AgentKind::ALL.to_vec());
    let project = bind(store, args.project, &agents, &accounts)?;
    let (invite, code) = store.create_invite(&hashi::store::NewInvite {
        label: clean_label(&args.label)?,
        session_mode: if args.stateful {
            SessionMode::Stateful
        } else {
            SessionMode::Stateless
        },
        agents,
        token_expires_in_days: args.expires_in_days,
        limits,
        link_hours: args.link_hours,
        accounts,
        project,
    })?;
    store.audit(
        "cli",
        "invite.create",
        &invite.label,
        &format!("link valid {}h", args.link_hours),
    )?;
    // A running server records its public URL (configured or tunnel) for us.
    let public = args.public_url.as_deref().map_or_else(
        || {
            store
                .data_dir()
                .and_then(|home| hashi::server::read_public_url(&home))
        },
        |u| Some(u.trim_end_matches('/').to_owned()),
    );
    let base = public
        .clone()
        .unwrap_or_else(|| format!("http://127.0.0.1:{}", args.port));
    eprintln!(
        "invite for {}{} (works once, expires {}):",
        invite.label,
        invite
            .project
            .as_deref()
            .map(|p| format!(", in project {p}"))
            .unwrap_or_default(),
        rfc3339(invite.expires_at).unwrap_or_default()
    );
    println!("{base}/claim/{code}");
    if public.is_none() {
        eprintln!(
            "note: this link only works on this machine. Press t in the dashboard to go public, \
             start with --tunnel or set HASHI_PUBLIC_URL to share it."
        );
    }
    Ok(())
}

/// Runs a `hashi invite` command.
///
/// # Errors
///
/// Fails on database errors or an unknown invite.
pub fn invite(store: &Store, cmd: InviteCommand) -> Result<()> {
    match cmd {
        InviteCommand::List => {
            let now = now_ms();
            println!(
                "{:<36} {:<20} {:<8} {:<10}",
                "ID", "LABEL", "STATUS", "CREATED"
            );
            for i in store.list_invites()? {
                println!(
                    "{:<36} {:<20} {:<8} {:<10}",
                    i.id,
                    i.label,
                    i.status(now),
                    format!("{} ago", human_age(i.created_at))
                );
            }
        }
        InviteCommand::Cancel { id } => {
            let label = store
                .list_invites()?
                .into_iter()
                .find(|i| i.id == id)
                .map(|i| i.label);
            if !store.cancel_invite(&id)? {
                bail!("no pending invite with id `{id}`");
            }
            let label = label.unwrap_or(id);
            store.audit("cli", "invite.cancel", &label, "")?;
            println!("cancelled invite for {label}");
        }
    }
    Ok(())
}

/// Runs `hashi audit`.
///
/// # Errors
///
/// Fails on database errors.
pub fn audit(store: &Store, limit: u32) -> Result<()> {
    for e in store.list_audit(limit)? {
        println!(
            "{:<20} {:<10} {:<14} {:<20} {}",
            rfc3339(e.at).unwrap_or_default(),
            e.actor,
            e.action,
            e.target,
            e.detail
        );
    }
    Ok(())
}

// ── accounts ───────────────────────────────────────────────────────────────

/// Agent executables used to sign accounts in and check them, with the
/// same flags and variables as `hashi serve`.
#[derive(Debug, Args, Clone, Default)]
pub struct BinArgs {
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
    /// Path to the Crush executable.
    #[arg(long, env = "HASHI_CRUSH_BIN")]
    crush_bin: Option<PathBuf>,
    /// Path to the Cursor CLI executable.
    #[arg(long, env = "HASHI_CURSOR_BIN")]
    cursor_bin: Option<PathBuf>,
}

impl BinArgs {
    fn bin(&self, agent: AgentKind) -> PathBuf {
        let custom = match agent {
            AgentKind::Claude => self.claude_bin.clone(),
            AgentKind::Codex => self.codex_bin.clone(),
            AgentKind::Gemini => self.gemini_bin.clone(),
            AgentKind::Opencode => self.opencode_bin.clone(),
            AgentKind::Qwen => self.qwen_bin.clone(),
            AgentKind::Crush => self.crush_bin.clone(),
            AgentKind::Cursor => self.cursor_bin.clone(),
        };
        custom.unwrap_or_else(|| AgentConfig::default_for(agent).bin)
    }
}

/// `hashi account ...`
#[derive(Debug, Subcommand)]
pub enum AccountCommand {
    /// Add an account: a login directory Hashi owns (signed in with the
    /// agent's own command), or an API key.
    Add {
        /// Agent the account is for.
        agent: AgentKind,
        /// Name, 1 to 32 characters from a-z, 0-9 and -.
        name: String,
        /// login (the default without a key), api-key, or oauth-token (Claude
        /// Code only, from `claude setup-token`).
        #[arg(long)]
        kind: Option<AccountKind>,
        /// Read the key from standard input; on a terminal it is not shown.
        #[arg(long, conflicts_with = "key_env")]
        key_stdin: bool,
        /// Read the key from this environment variable.
        #[arg(long, value_name = "VAR")]
        key_env: Option<String>,
        /// Run the sign-in command without asking first (still only on a
        /// terminal).
        #[arg(long, short = 'y')]
        yes: bool,
        #[command(flatten)]
        bins: BinArgs,
    },
    /// Sign an existing login account in again.
    Login {
        /// Agent the account is for.
        agent: AgentKind,
        /// Account name.
        name: String,
        /// Run the sign-in command without asking first.
        #[arg(long, short = 'y')]
        yes: bool,
        #[command(flatten)]
        bins: BinArgs,
    },
    /// List accounts with their state and use.
    List {
        #[command(flatten)]
        bins: BinArgs,
    },
    /// Stop using an account until it is enabled again.
    Disable {
        /// Agent the account is for.
        agent: AgentKind,
        /// Account name.
        name: String,
    },
    /// Use a disabled account again.
    Enable {
        /// Agent the account is for.
        agent: AgentKind,
        /// Account name.
        name: String,
    },
    /// Delete an account, its directory and its key, and take it off every
    /// token and invite.
    Remove {
        /// Agent the account is for.
        agent: AgentKind,
        /// Account name.
        name: String,
        /// Do not ask for confirmation.
        #[arg(long, short = 'y')]
        yes: bool,
    },
}

/// Asks a yes or no question on the terminal. No means no.
fn confirm(question: &str) -> bool {
    eprint!("{question} [y/N] ");
    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer).is_ok()
        && matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

/// Reads a key from `--key-env`, or from standard input: without echo on a
/// terminal, as one line otherwise. The key is never printed or logged.
fn read_key(key_env: Option<&str>) -> Result<String> {
    let raw = match key_env {
        Some(var) => std::env::var(var)
            .ok()
            .filter(|v| !v.trim().is_empty())
            .with_context(|| format!("{var} is not set"))?,
        None if std::io::stdin().is_terminal() => {
            eprint!("Paste the key (it is not shown), then press Enter: ");
            let key = read_hidden();
            eprintln!();
            key?
        }
        None => {
            let mut line = String::new();
            std::io::stdin()
                .read_line(&mut line)
                .context("reading the key from standard input")?;
            line
        }
    };
    acct::clean_key(&raw).map_err(|e| anyhow::anyhow!(e))
}

/// Reads one line from the terminal in raw mode, so nothing is echoed.
fn read_hidden() -> Result<String> {
    use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
    crossterm::terminal::enable_raw_mode().context("switching the terminal to raw mode")?;
    let mut key = String::new();
    let result = loop {
        match event::read() {
            Ok(Event::Key(k)) if k.kind != KeyEventKind::Release => match k.code {
                KeyCode::Enter => break Ok(()),
                KeyCode::Esc => break Err(anyhow::anyhow!("cancelled")),
                KeyCode::Char('c' | 'd') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                    break Err(anyhow::anyhow!("cancelled"));
                }
                KeyCode::Backspace => {
                    key.pop();
                }
                KeyCode::Char(c) => key.push(c),
                _ => {}
            },
            Ok(Event::Paste(text)) => key.push_str(&text),
            Ok(_) => {}
            Err(e) => break Err(e.into()),
        }
    };
    let _ = crossterm::terminal::disable_raw_mode();
    result.map(|()| key)
}

/// The account variable and directory, and the sign-in command line, as
/// shown to the owner.
fn sign_in_line(home: &Path, agent: AgentKind, name: &str, bin: &Path) -> Option<String> {
    let sign = acct::sign_in(agent)?;
    acct::dir_var(agent)?;
    let dir = acct::account_dir(home, agent, name);
    let mut parts: Vec<String> = acct::account_vars(agent, &dir)
        .iter()
        .map(|(k, v)| format!("{k}={}", shell_quote(v)))
        .collect();
    parts.extend(sign.env.iter().map(|(k, v)| format!("{k}={v}")));
    parts.push(shell_quote(&bin.to_string_lossy()));
    parts.extend(sign.args.iter().map(|a| (*a).to_owned()));
    Some(parts.join(" "))
}

fn shell_quote(s: &str) -> String {
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '/' | '.' | '-' | '_' | ':' | '~'))
    {
        s.to_owned()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

/// A command run with the account's directory and none of the owner's
/// credential variables or Hashi's own secrets.
fn account_command(home: &Path, agent: AgentKind, name: &str, bin: &Path) -> std::process::Command {
    let mut cmd = std::process::Command::new(bin);
    for (key, _) in std::env::vars_os() {
        let upper = key.to_string_lossy().to_ascii_uppercase();
        let removed = acct::credential_vars(agent)
            .iter()
            .chain(acct::always_removed(agent))
            .any(|p| hashi::agent::runner::env_name_matches(p, &upper));
        if hashi::agent::runner::is_hashi_secret(&key) || removed {
            cmd.env_remove(&key);
        }
    }
    cmd.envs(acct::account_vars(
        agent,
        &acct::account_dir(home, agent, name),
    ));
    cmd
}

/// Runs the sign-in command on the owner's terminal, only after they
/// confirm and only when standard input is a terminal. Otherwise prints it.
fn sign_in(home: &Path, agent: AgentKind, name: &str, yes: bool, bins: &BinArgs) -> Result<()> {
    let bin = bins.bin(agent);
    let Some(line) = sign_in_line(home, agent, name, &bin) else {
        bail!(
            "{} has no sign-in command Hashi knows",
            agent.display_name()
        );
    };
    let Some(sign) = acct::sign_in(agent) else {
        bail!(
            "{} has no sign-in command Hashi knows",
            agent.display_name()
        );
    };
    println!("sign-in command:\n  {line}");
    if let Some(hint) = sign.hint {
        println!("  ({hint})");
    }
    if !std::io::stdin().is_terminal() {
        println!("run it in a terminal, then check with `hashi account list`");
        return Ok(());
    }
    if !yes && !confirm("Run it now?") {
        println!("not run; run the command above when you are ready");
        return Ok(());
    }
    let status = account_command(home, agent, name, &bin)
        .args(sign.args)
        .envs(sign.env.iter().copied())
        .status()
        .with_context(|| format!("starting {}", bin.display()))?;
    if !status.success() {
        eprintln!("the sign-in command exited with {status}");
    }
    let state = signed_in(home, agent, name, AccountKind::Login, &bin);
    println!("signed in: {state}");
    Ok(())
}

/// Whether an account is signed in, as far as can be checked: Claude Code
/// answers `claude auth status --json`; for the other CLIs, whether they
/// wrote anything into the account directory; for a key account, whether
/// its key file is there.
fn signed_in(home: &Path, agent: AgentKind, name: &str, kind: AccountKind, bin: &Path) -> String {
    match kind {
        AccountKind::Default => "-".to_owned(),
        AccountKind::ApiKey | AccountKind::OauthToken => {
            if acct::read_key(home, agent, name).is_ok() {
                "key set".to_owned()
            } else {
                "no key".to_owned()
            }
        }
        AccountKind::Login => match acct::status_args(agent) {
            Some(args) => match run_status(account_command(home, agent, name, bin).args(args)) {
                Some(true) => "yes".to_owned(),
                Some(false) => "no".to_owned(),
                None => "unknown".to_owned(),
            },
            None if acct::has_files(&acct::account_dir(home, agent, name)) => {
                "files present".to_owned()
            }
            None => "no".to_owned(),
        },
    }
}

/// Runs a status command with a ten second limit. `Some(true)` when it
/// reports a login: a `loggedIn` field when its output is JSON with one,
/// else a zero exit.
fn run_status(cmd: &mut std::process::Command) -> Option<bool> {
    use std::process::Stdio;
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let out = child.wait_with_output().ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let field = serde_json::from_str::<serde_json::Value>(text.trim())
        .ok()
        .and_then(|v| v.get("loggedIn").and_then(serde_json::Value::as_bool));
    Some(field.unwrap_or(out.status.success()))
}

fn account_name(name: &str) -> Result<()> {
    acct::check_name(name, false).map_err(|e| anyhow::anyhow!(e))
}

/// Runs a `hashi account` command.
///
/// # Errors
///
/// Fails on invalid input, file system or database errors.
pub fn account(store: &Store, home: &Path, cmd: AccountCommand) -> Result<()> {
    match cmd {
        AccountCommand::Add {
            agent,
            name,
            kind,
            key_stdin,
            key_env,
            yes,
            bins,
        } => {
            account_name(&name)?;
            let keyed = key_stdin || key_env.is_some();
            let kind = match kind {
                Some(AccountKind::Default) => bail!("the default account is always there"),
                Some(AccountKind::Login) if keyed => {
                    bail!("a login account has no key; drop --key-stdin and --key-env")
                }
                Some(k) => k,
                None if keyed => AccountKind::ApiKey,
                None => AccountKind::Login,
            };
            if !acct::supports(agent, kind) {
                bail!(
                    "{} does not support {kind} accounts{}",
                    agent.display_name(),
                    match agent {
                        AgentKind::Cursor => "; add an API key with --kind api-key",
                        AgentKind::Opencode | AgentKind::Qwen | AgentKind::Crush =>
                            "; add a login account",
                        _ => "",
                    }
                );
            }
            if store
                .get_account(agent, &name)?
                .is_some_and(|a| a.kind != AccountKind::Default)
            {
                bail!("{agent} already has an account named {name}");
            }
            // The key is read before anything is created, so a mistyped
            // key leaves nothing behind.
            let key = kind
                .is_key()
                .then(|| read_key(key_env.as_deref()))
                .transpose()?;
            // A row left by a limit mark on a since removed account.
            store.remove_account(agent, &name)?;
            store.add_account(agent, &name, kind)?;
            let made = match &key {
                Some(key) => acct::write_key(home, agent, &name, key)
                    .map(|()| acct::account_dir(home, agent, &name)),
                None => acct::create_dir(home, agent, &name),
            };
            let dir = match made {
                Ok(dir) => dir,
                Err(e) => {
                    store.remove_account(agent, &name)?;
                    return Err(e).context("creating the account directory");
                }
            };
            store.audit(
                "cli",
                "account.add",
                &format!("{agent}:{name}"),
                kind.as_str(),
            )?;
            println!(
                "added {} account {name} ({kind}) in {}",
                agent.display_name(),
                dir.display()
            );
            if kind == AccountKind::Login {
                sign_in(home, agent, &name, yes, &bins)?;
            }
            println!(
                "use it with: hashi token update <id> --account {agent}={name}  (or --account on token create and share)"
            );
        }
        AccountCommand::Login {
            agent,
            name,
            yes,
            bins,
        } => {
            // Crush's default account has a Hashi-owned directory too, so
            // the owner's own Crush config never loads; sign it in here.
            if agent == AgentKind::Crush && name == acct::DEFAULT {
                acct::crush_default_dir(home).context("preparing the account directory")?;
                sign_in(home, agent, &name, yes, &bins)?;
                return Ok(());
            }
            account_name(&name)?;
            match store.get_account(agent, &name)? {
                Some(a) if a.kind == AccountKind::Login => {}
                Some(a) if a.kind != AccountKind::Default => {
                    bail!("{agent}:{name} is a {} account, not a login", a.kind)
                }
                _ => bail!("no {agent} account named {name}"),
            }
            acct::create_dir(home, agent, &name).context("preparing the account directory")?;
            sign_in(home, agent, &name, yes, &bins)?;
        }
        AccountCommand::List { bins } => {
            let now = now_ms();
            let accounts = store.list_accounts()?;
            if accounts.is_empty() {
                println!(
                    "no accounts yet; every agent uses your own login (add one with hashi account add)"
                );
                return Ok(());
            }
            println!(
                "{:<9} {:<16} {:<12} {:<14} {:<34} {:<10} {:<6} TOKENS",
                "AGENT", "NAME", "KIND", "SIGNED IN", "STATE", "LAST USED", "TODAY"
            );
            for a in accounts {
                let state = match a.status(now) {
                    hashi::store::AccountStatus::Limited(until) => {
                        format!("limited until {}", rfc3339(until).unwrap_or_default())
                    }
                    s => s.as_str().to_owned(),
                };
                let used = a
                    .last_used
                    .map_or_else(|| "never".to_owned(), |u| format!("{} ago", human_age(u)));
                let (today, _) = store.account_runs(a.agent, &a.name, now)?;
                println!(
                    "{:<9} {:<16} {:<12} {:<14} {:<34} {:<10} {:<6} {}",
                    a.agent.as_str(),
                    a.name,
                    a.kind.as_str(),
                    signed_in(home, a.agent, &a.name, a.kind, &bins.bin(a.agent)),
                    state,
                    used,
                    today,
                    store.tokens_using_account(a.agent, &a.name)?
                );
            }
        }
        AccountCommand::Disable { agent, name } => set_disabled(store, agent, &name, true)?,
        AccountCommand::Enable { agent, name } => set_disabled(store, agent, &name, false)?,
        AccountCommand::Remove { agent, name, yes } => {
            account_name(&name)?;
            if store
                .get_account(agent, &name)?
                .is_none_or(|a| a.kind == AccountKind::Default)
            {
                bail!("no {agent} account named {name}");
            }
            let using = store.tokens_using_account(agent, &name)?;
            if !yes {
                if !std::io::stdin().is_terminal() {
                    bail!("pass --yes to remove {agent}:{name} without being asked");
                }
                if !confirm(&format!(
                    "Remove {agent}:{name}, its directory and key, and take it off {using} token(s)?"
                )) {
                    println!("not removed");
                    return Ok(());
                }
            }
            let removed = store.remove_account(agent, &name)?;
            let dir = acct::remove_dir(home, agent, &name);
            store.audit("cli", "account.remove", &format!("{agent}:{name}"), "")?;
            println!(
                "removed {agent}:{name}; taken off {} token(s) and {} invite(s)",
                removed.tokens, removed.invites
            );
            if let Err(e) = dir {
                eprintln!("warning: could not delete its directory: {e}");
            }
        }
    }
    Ok(())
}

fn set_disabled(store: &Store, agent: AgentKind, name: &str, disabled: bool) -> Result<()> {
    if name == acct::DEFAULT {
        bail!("the default account cannot be disabled; leave it out of a token's list instead");
    }
    if store
        .get_account(agent, name)?
        .is_none_or(|a| a.kind == AccountKind::Default)
    {
        bail!("no {agent} account named {name}");
    }
    store.set_account_disabled(agent, name, disabled)?;
    let action = if disabled { "disabled" } else { "enabled" };
    store.audit(
        "cli",
        &format!("account.{}", &action[..action.len() - 1]),
        &format!("{agent}:{name}"),
        "",
    )?;
    println!("{action} {agent}:{name}");
    Ok(())
}

// ── projects ───────────────────────────────────────────────────────────────

/// One `--model <agent>=<model>` value.
type ModelBinding = (AgentKind, String);

fn parse_model(s: &str) -> Result<ModelBinding, String> {
    hashi::projects::parse_model(s)
}

/// The settings `project add` and `project update` share.
#[derive(Debug, Args, Clone, Default)]
pub struct ProjectArgs {
    /// Agents the project allows (comma separated). Default for a new
    /// project: the installed ones.
    #[arg(long, value_delimiter = ',')]
    agents: Option<Vec<AgentKind>>,
    /// Accounts for an agent, in order, for example claude=work,personal
    /// (repeatable). An agent left out uses its default account.
    #[arg(long = "account", value_name = "AGENT=NAMES", value_parser = parse_account)]
    accounts: Vec<Binding>,
    /// Agent used when a request names none.
    #[arg(long)]
    default_agent: Option<AgentKind>,
    /// Model for an agent when a request names none, for example
    /// claude=sonnet (repeatable).
    #[arg(long = "model", value_name = "AGENT=MODEL", value_parser = parse_model)]
    models: Vec<ModelBinding>,
    /// Who or what the project is for.
    #[arg(long)]
    note: Option<String>,
}

/// `hashi project ...`
#[derive(Debug, Subcommand)]
pub enum ProjectCommand {
    /// Add a project: a folder that tokens and invites can be bound to.
    Add {
        /// Name, 1 to 32 characters from a-z, 0-9 and -.
        name: String,
        /// Absolute path of an existing folder. Symlinks are resolved.
        path: PathBuf,
        #[command(flatten)]
        settings: ProjectArgs,
        #[command(flatten)]
        bins: BinArgs,
    },
    /// Change a project. Bound tokens must stay inside it.
    Update {
        /// Project name.
        name: String,
        /// New folder.
        #[arg(long)]
        path: Option<PathBuf>,
        /// Put every agent back on its default account before applying
        /// --account.
        #[arg(long)]
        clear_accounts: bool,
        /// Remove the default agent.
        #[arg(long, conflicts_with = "default_agent")]
        clear_default_agent: bool,
        /// Remove every model before applying --model.
        #[arg(long)]
        clear_models: bool,
        #[command(flatten)]
        settings: ProjectArgs,
    },
    /// List projects with their folder, settings and use.
    List,
    /// Remove a project. Its folder is never touched.
    Remove {
        /// Project name.
        name: String,
        /// Put bound tokens and invites back on their own workspace first.
        #[arg(long)]
        unbind: bool,
    },
}

/// The agents whose executable is there, or every agent when none is.
/// Crush is opt-in, so it is never part of a project's default agents; list
/// it with `--agents` to allow it.
fn installed_agents(bins: &BinArgs) -> Vec<AgentKind> {
    let candidates = AgentKind::ALL.into_iter().filter(|a| *a != AgentKind::Crush);
    let found: Vec<AgentKind> = candidates
        .clone()
        .filter(|a| hashi::agent::is_executable(&bins.bin(*a)))
        .collect();
    if found.is_empty() {
        candidates.collect()
    } else {
        found
    }
}

fn api_err(e: hashi::api::ApiError) -> anyhow::Error {
    anyhow::anyhow!("{e}")
}

/// Runs a `hashi project` command.
///
/// # Errors
///
/// Fails on invalid input or database errors.
pub fn project(store: &Store, home: &Path, cmd: ProjectCommand) -> Result<()> {
    match cmd {
        ProjectCommand::Add {
            name,
            path,
            settings,
            bins,
        } => {
            let mut accounts = AccountMap::new();
            for (agent, names) in settings.accounts {
                accounts.insert(agent, names);
            }
            let raw = hashi::projects::Project {
                name,
                path: String::new(),
                agents: settings.agents.unwrap_or_else(|| installed_agents(&bins)),
                accounts,
                default_agent: settings.default_agent,
                models: settings.models.into_iter().collect(),
                note: settings.note,
                created_at: now_ms(),
            };
            if store.list_projects()?.len() >= hashi::projects::MAX_PROJECTS {
                bail!("project limit reached, remove one first");
            }
            let p = hashi::api::validate_project(store, Some(home), raw, &path).map_err(api_err)?;
            if !store.add_project(&p)? {
                bail!("a project named {} already exists", p.name);
            }
            store.audit("cli", "project.add", &p.name, &p.path)?;
            println!(
                "added project {} in {} (agents {})",
                p.name,
                p.path,
                hashi::token::encode_agents(&p.agents)
            );
            println!(
                "share it with: hashi share <name> --project {}  (or --project on token create)",
                p.name
            );
        }
        ProjectCommand::Update {
            name,
            path,
            clear_accounts,
            clear_default_agent,
            clear_models,
            settings,
        } => {
            let current = store
                .get_project(&name)?
                .with_context(|| format!("no project named {name}"))?;
            let accounts = (clear_accounts || !settings.accounts.is_empty()).then(|| {
                let mut map = if clear_accounts {
                    AccountMap::new()
                } else {
                    current.accounts.clone()
                };
                for (agent, names) in settings.accounts {
                    map.insert(agent, names);
                }
                map
            });
            let models = (clear_models || !settings.models.is_empty()).then(|| {
                let mut map = if clear_models {
                    hashi::projects::ModelMap::new()
                } else {
                    current.models.clone()
                };
                map.extend(settings.models);
                map
            });
            let body = hashi::api::UpdateProjectBody {
                path: path.map(|p| p.to_string_lossy().into_owned()),
                agents: settings.agents,
                accounts,
                default_agent: if clear_default_agent {
                    Some(None)
                } else {
                    settings.default_agent.map(Some)
                },
                models,
                note: settings.note,
            };
            let (p, changes) =
                hashi::api::apply_update(store, Some(home), current, body).map_err(api_err)?;
            store.save_project(&p)?;
            let detail = changes.join(", ");
            store.audit("cli", "project.update", &p.name, &detail)?;
            println!("updated {}: {detail}", p.name);
        }
        ProjectCommand::List => {
            let projects = store.list_projects()?;
            if projects.is_empty() {
                println!(
                    "no projects yet; every token runs in its own workspace (add one with hashi project add)"
                );
                return Ok(());
            }
            let now = now_ms();
            println!(
                "{:<16} {:<6} {:<8} {:<8} {:<10} {:<24} {:<20} {:<16} PATH",
                "NAME", "TOKENS", "TODAY", "MONTH", "SPEND", "AGENTS", "ACCOUNTS", "MODELS"
            );
            for p in projects {
                let st = store.project_stats(&p.name, now)?;
                let agents = match p.default_agent {
                    Some(d) => format!("{} ({d})", hashi::token::encode_agents(&p.agents)),
                    None => hashi::token::encode_agents(&p.agents),
                };
                println!(
                    "{:<16} {:<6} {:<8} {:<8} {:<10} {:<24} {:<20} {:<16} {}",
                    p.name,
                    st.tokens,
                    st.today_requests,
                    st.month_requests,
                    format!("${:.2}", st.month_cost_usd),
                    agents,
                    acct::format_bindings(&p.accounts),
                    hashi::projects::format_models(&p.models),
                    p.path
                );
            }
        }
        ProjectCommand::Remove { name, unbind } => match store.remove_project(&name, unbind)? {
            hashi::store::RemovedProject::NotFound => bail!("no project named {name}"),
            hashi::store::RemovedProject::Bound { tokens, invites } => bail!(
                "{tokens} token(s) and {invites} pending invite(s) are bound to {name}; pass \
                 --unbind to put them back on their own workspace"
            ),
            hashi::store::RemovedProject::Removed { tokens, invites } => {
                store.audit(
                    "cli",
                    "project.remove",
                    &name,
                    &format!("unbound {tokens} token(s) and {invites} invite(s)"),
                )?;
                println!(
                    "removed project {name}; unbound {tokens} token(s) and {invites} invite(s). \
                     Its folder was not touched."
                );
            }
        },
    }
    Ok(())
}

/// `hashi doctor`'s account lines: the accounts each agent has besides the
/// default. Reads the database only when it exists.
pub fn doctor_accounts(home: &Path) {
    let none = || {
        println!(
            "  · {:<12} none: every agent uses your own login (hashi account add)",
            "accounts"
        );
    };
    if !home.join("hashi.db").exists() {
        none();
        return;
    }
    let Ok(store) = hashi::server::open_store(home) else {
        println!("  ! {:<12} could not open the database", "accounts");
        return;
    };
    let now = now_ms();
    let list: Vec<_> = store
        .list_accounts()
        .unwrap_or_default()
        .into_iter()
        .filter(|a| a.kind != AccountKind::Default)
        .collect();
    if list.is_empty() {
        none();
        return;
    }
    for agent in AgentKind::ALL {
        let names: Vec<String> = list
            .iter()
            .filter(|a| a.agent == agent)
            .map(|a| format!("{} ({}, {})", a.name, a.kind, a.status(now).as_str()))
            .collect();
        if !names.is_empty() {
            println!(
                "  ✓ {:<12} {}: {}",
                "accounts",
                agent.as_str(),
                names.join(", ")
            );
        }
    }
}
