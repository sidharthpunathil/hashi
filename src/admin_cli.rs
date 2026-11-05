//! Offline admin commands that work straight on the database: tokens,
//! invites and the audit log. Every change is audited with actor `cli`.

use anyhow::{Result, bail};
use clap::builder::TypedValueParser;
use clap::{Args, Subcommand};
use hashi::agent::AgentKind;
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
    /// Estimated spend per UTC day in USD (Claude reports cost; Codex does not).
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
    /// Base URL for the link. Defaults to HASHI_PUBLIC_URL, else this machine.
    #[arg(long, env = "HASHI_PUBLIC_URL")]
    pub public_url: Option<String>,
    /// Port of the local server, used for a local-only link.
    #[arg(long, default_value_t = 7878, env = "HASHI_PORT")]
    pub port: u16,
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
            limits,
        } => {
            let role = if admin { Role::Admin } else { Role::User };
            let new = NewToken {
                name: clean_label(&name)?,
                role,
                session_mode: if stateful {
                    SessionMode::Stateful
                } else {
                    SessionMode::Stateless
                },
                agents: agents.unwrap_or_else(|| AgentKind::ALL.to_vec()),
                expires_in_days,
                note,
                limits: if limits.any() {
                    limits.apply(Limits::default())
                } else {
                    NewToken::default_limits(role)
                },
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
                "{:<20} {:<16} {:<6} {:<8} {:<10} {:<13} {:<9} {:<10}",
                "NAME", "PREFIX", "ROLE", "STATUS", "SESSION", "AGENTS", "TODAY", "LAST USED"
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
                    "{:<20} {:<16} {:<6} {:<8} {:<10} {:<13} {:<9} {:<10}",
                    t.name,
                    t.prefix,
                    t.role.as_str(),
                    t.status(now).as_str(),
                    t.session_mode.as_str(),
                    agents,
                    today,
                    used
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
            if changes.is_empty() {
                bail!("nothing to update; see `hashi token update --help`");
            }
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
    let (invite, code) = store.create_invite(&hashi::store::NewInvite {
        label: clean_label(&args.label)?,
        session_mode: if args.stateful {
            SessionMode::Stateful
        } else {
            SessionMode::Stateless
        },
        agents: args.agents.unwrap_or_else(|| AgentKind::ALL.to_vec()),
        token_expires_in_days: args.expires_in_days,
        limits,
        link_hours: args.link_hours,
    })?;
    store.audit(
        "cli",
        "invite.create",
        &invite.label,
        &format!("link valid {}h", args.link_hours),
    )?;
    let base = match &args.public_url {
        Some(u) => u.trim_end_matches('/').to_owned(),
        None => format!("http://127.0.0.1:{}", args.port),
    };
    eprintln!(
        "invite for {} (works once, expires {}):",
        invite.label,
        rfc3339(invite.expires_at).unwrap_or_default()
    );
    println!("{base}/claim/{code}");
    if args.public_url.is_none() {
        eprintln!(
            "note: this link only works on this machine. Start with --tunnel or set HASHI_PUBLIC_URL to share it."
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
