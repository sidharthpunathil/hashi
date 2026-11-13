//! Offline admin commands that work straight on the database: tokens,
//! invites, access requests, accounts, projects and the audit log. Every
//! change is audited with actor `cli`.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::builder::TypedValueParser;
use clap::{Args, Subcommand};
use hashi::accounts::{self as acct, AccountKind, AccountMap};
use hashi::agent::{AgentConfig, AgentKind};
use hashi::diagnostics::terminal_safe;
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
    /// Stop once the owner's account serving it has used this percent of
    /// its 5-hour or weekly quota, keeping the rest for the owner.
    #[arg(long, value_name = "PCT", value_parser = clap::value_parser!(u8).range(1..=100))]
    pub stop_at: Option<u8>,
}

impl LimitArgs {
    fn any(&self) -> bool {
        self.daily_requests.is_some()
            || self.monthly_requests.is_some()
            || self.daily_budget.is_some()
            || self.monthly_budget.is_some()
            || self.max_pending.is_some()
            || self.stop_at.is_some()
    }

    /// Overlays the given flags on `base`.
    fn apply(&self, base: Limits) -> Limits {
        Limits {
            daily_requests: self.daily_requests.or(base.daily_requests),
            monthly_requests: self.monthly_requests.or(base.monthly_requests),
            daily_budget_usd: self.daily_budget.or(base.daily_budget_usd),
            monthly_budget_usd: self.monthly_budget.or(base.monthly_budget_usd),
            max_pending: self.max_pending.or(base.max_pending),
            stop_at_percent: self.stop_at.or(base.stop_at_percent),
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
        /// Let the token see how much quota is left for each of its agents
        /// (`GET /v1/me`): headroom, next reset and state only, never
        /// account names or counts.
        #[arg(long)]
        share_usage: bool,
        #[command(flatten)]
        limits: LimitArgs,
    },
    /// List tokens with status and today's use.
    List {
        /// Print JSON, as `GET /v1/tokens` shows each token. Never the
        /// secret.
        #[arg(long)]
        json: bool,
    },
    /// Pause, resume, extend or limit a token.
    Update {
        /// Token id, display prefix or unique name.
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
        /// Show the token how much quota is left (`true`) or stop (`false`).
        #[arg(long, value_name = "BOOL")]
        share_usage: Option<bool>,
        #[command(flatten)]
        limits: LimitArgs,
    },
    /// Revoke a token by id, prefix or name. It stops its running prompts
    /// and deletes its workspace.
    Revoke {
        /// Token id, display prefix or unique name.
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
    /// Port of the local server, used for a local-only link. Default: the
    /// one saved from the dashboard in `<home>/settings.json`, else 7878.
    #[arg(long, env = "HASHI_PORT")]
    pub port: Option<u16>,
    /// Accounts the token uses for an agent, in order, for example
    /// claude=work,personal (repeatable).
    #[arg(long = "account", value_name = "AGENT=NAMES", value_parser = parse_account)]
    pub accounts: Vec<Binding>,
    /// Bind the token to this project. The claim page shows only its name.
    #[arg(long)]
    pub project: Option<String>,
    /// Let the token see how much quota is left for each of its agents
    /// (headroom, next reset and state only). The claim page says so.
    #[arg(long)]
    pub share_usage: bool,
    /// How the link reaches people: auto (whatever is public now),
    /// cloudflare, tailscale, lan (this wifi) or local.
    #[arg(long, default_value = "auto", value_parser = clap::value_parser!(hashi::links::Via))]
    pub via: hashi::links::Via,
    #[command(flatten)]
    pub limits: LimitArgs,
}

/// `hashi share-login ...`: a one-time link that hands over a key account.
#[derive(Debug, Args)]
pub struct ShareLoginArgs {
    /// Who it is for.
    pub label: String,
    /// The key account to hand over, as <agent>:<name>, for example
    /// claude:bot (an OAuth token from `claude setup-token`, or an API key).
    #[arg(long, value_name = "AGENT:NAME")]
    pub account: String,
    /// The account whose quota the key spends, read for reminders:
    /// default, or a login account of the same agent.
    #[arg(long, value_name = "NAME", default_value = "default")]
    pub usage_from: String,
    /// Remind both of you when that quota has used this percent of a
    /// 5-hour or weekly window.
    #[arg(long, value_name = "PCT", value_parser = clap::value_parser!(u8).range(1..=100))]
    pub remind_at: Option<u8>,
    /// The share ends this many days after it is claimed: their copy is
    /// switched off and the usage reading stops.
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..=3650))]
    pub days: Option<u32>,
    /// How long the link works, in hours.
    #[arg(
        long,
        default_value = "24",
        value_parser = clap::builder::PossibleValuesParser::new(["1", "24", "168"])
            .map(|s| s.parse::<u32>().unwrap_or(24)),
    )]
    pub link_hours: u32,
    /// Base URL for the link, as for `hashi share`.
    #[arg(long, env = "HASHI_PUBLIC_URL")]
    pub public_url: Option<String>,
    /// Port of the local server, used for a local-only link.
    #[arg(long, env = "HASHI_PORT")]
    pub port: Option<u16>,
    /// How the link reaches people: auto (whatever is public now),
    /// cloudflare, tailscale, lan (this wifi) or local.
    #[arg(long, default_value = "auto", value_parser = clap::value_parser!(hashi::links::Via))]
    pub via: hashi::links::Via,
}

/// `hashi share-login`: creates a login share and prints its link.
///
/// # Errors
///
/// Fails on a bad account or field, or on database errors.
pub fn share_login(store: &Store, home: &Path, args: ShareLoginArgs) -> Result<()> {
    let (agent, account) =
        hashi::share::parse_account_ref(&args.account).map_err(|e| anyhow::anyhow!(e))?;
    let new = hashi::share::NewLoginShare {
        label: clean_label(&args.label)?,
        agent,
        account,
        usage_from: args.usage_from,
        remind_at: args.remind_at,
        days: args.days,
        link_hours: args.link_hours,
    };
    let (base, local) = link_base(store, args.via, args.public_url.as_deref(), args.port)?;
    let (invite, code) = hashi::share::create_login_share(store, home, &new, "cli")
        .map_err(|e| anyhow::anyhow!(e))?;
    eprintln!(
        "login share of {agent}:{} for {} (works once, expires {}):",
        new.account,
        invite.label,
        rfc3339(invite.expires_at).unwrap_or_default()
    );
    println!("{base}/claim/{code}");
    eprintln!(
        "the link hands over the key itself: send it privately, to someone you trust. \
         They open it with `hashi connect <link>`."
    );
    if local {
        eprintln!(
            "note: this link only works on this machine. Press t in the dashboard to go public, \
             start with --tunnel or set HASHI_PUBLIC_URL to share it."
        );
    }
    Ok(())
}

/// The base URL for a link over `via`, and whether it only works on this
/// machine. A note about the route goes to standard error.
fn link_base(
    store: &Store,
    via: hashi::links::Via,
    public_url: Option<&str>,
    port: Option<u16>,
) -> Result<(String, bool)> {
    let home = store.data_dir().unwrap_or_default();
    let (base, local) =
        hashi::links::cli_base(&home, via, public_url, port).map_err(|e| anyhow::anyhow!(e))?;
    if let Some(note) = hashi::links::note(via).filter(|_| via != hashi::links::Via::Local) {
        eprintln!("note: {note}");
    }
    Ok((base, local))
}

/// `hashi invite ...`
#[derive(Debug, Subcommand)]
pub enum InviteCommand {
    /// List invites with their status.
    List {
        /// Print JSON, as `GET /v1/invites` shows each invite. Never the
        /// link.
        #[arg(long)]
        json: bool,
    },
    /// Cancel a pending invite.
    Cancel {
        /// Invite id, as `hashi invite list` shows it.
        id: String,
    },
}

/// The token with this id or display prefix, else the one with this name.
fn find_token(store: &Store, key: &str) -> Result<Token> {
    let tokens = store.list_tokens()?;
    let by_id: Vec<&Token> = tokens
        .iter()
        .filter(|t| t.id == key || t.prefix == key)
        .collect();
    let matches = if by_id.is_empty() {
        tokens.iter().filter(|t| t.name == key).collect()
    } else {
        by_id
    };
    match matches.as_slice() {
        [t] => Ok((*t).clone()),
        [] => bail!(
            "no token matches `{}`; `hashi token list` shows names and prefixes",
            terminal_safe(key)
        ),
        _ => bail!(
            "`{}` matches several tokens; use the id from `hashi token list --json`",
            terminal_safe(key)
        ),
    }
}

/// Prints `rows` under `headers`, each column as wide as its widest cell,
/// so a long name does not push the columns after it out of line. The
/// last column is not padded.
fn print_table(headers: &[&str], rows: &[Vec<String>]) {
    fn width(s: &str) -> usize {
        s.chars().count()
    }
    let mut widths: Vec<usize> = headers.iter().map(|h| width(h)).collect();
    for row in rows {
        for (w, cell) in widths.iter_mut().zip(row) {
            *w = (*w).max(width(cell));
        }
    }
    let line = |cells: Vec<&str>| {
        let mut out = String::new();
        let last = cells.len().saturating_sub(1);
        for (i, cell) in cells.iter().enumerate() {
            out.push_str(cell);
            if i < last {
                out.push_str(&" ".repeat(widths[i] - width(cell) + 2));
            }
        }
        println!("{}", out.trim_end());
    };
    line(headers.to_vec());
    for row in rows {
        line(row.iter().map(String::as_str).collect());
    }
}

fn print_json(value: &serde_json::Value) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

/// A token as `GET /v1/tokens` shows it: never the secret.
fn token_json(store: &Store, t: &Token, now: i64) -> Result<serde_json::Value> {
    let conversations: Vec<serde_json::Value> = store
        .conversations(&t.id)?
        .into_iter()
        .map(|c| {
            serde_json::json!({
                "agent": c.agent,
                "account": c.account,
                "project": c.project,
                "updatedAt": rfc3339(c.updated_at),
            })
        })
        .collect();
    let mut v = serde_json::to_value(t)?;
    if let Some(obj) = v.as_object_mut() {
        obj.insert("status".into(), serde_json::json!(t.status(now)));
        obj.insert("conversations".into(), serde_json::json!(conversations));
    }
    Ok(v)
}

/// An invite as `GET /v1/invites` shows it: never its code.
fn invite_json(i: &hashi::store::Invite, now: i64) -> serde_json::Value {
    serde_json::json!({
        "id": i.id,
        "label": i.label,
        "status": i.status(now),
        "sessionMode": i.session_mode,
        "agents": i.agents,
        "tokenExpiresInDays": i.token_expires_in_days,
        "limits": i.limits,
        "expiresAt": rfc3339(i.expires_at),
        "createdAt": rfc3339(i.created_at),
        "claimedAt": i.claimed_at.and_then(rfc3339),
        "accounts": i.accounts,
        "project": i.project,
        "shareUsage": i.share_usage,
    })
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

/// The agents column: `all` when a token or invite may use every agent.
fn agents_cell(agents: &[AgentKind]) -> String {
    if AgentKind::ALL.iter().all(|a| agents.contains(a)) {
        "all".to_owned()
    } else {
        hashi::token::encode_agents(agents)
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
            accounts,
            project,
            share_usage,
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
            if share_usage {
                store.set_token_share_usage(&t.id, true)?;
            }
            store.audit("cli", "token.create", &t.name, t.role.as_str())?;
            eprintln!("created {} token `{}` ({})", t.role, t.name, t.id);
            eprintln!("store it now, it will not be shown again:");
            println!("{}", secret.raw);
        }
        TokenCommand::List { json } => {
            let now = now_ms();
            let tokens = store.list_tokens()?;
            if json {
                let list = tokens
                    .iter()
                    .map(|t| token_json(store, t, now))
                    .collect::<Result<Vec<_>>>()?;
                return print_json(&serde_json::json!({ "tokens": list }));
            }
            let mut rows = Vec::with_capacity(tokens.len());
            for t in &tokens {
                let used = t
                    .last_used_at
                    .map_or_else(|| "never".to_owned(), |u| format!("{} ago", human_age(u)));
                let usage = store.token_usage(&t.id, now)?;
                let today = match t.limits.daily_requests {
                    Some(cap) => format!("{}/{cap}", usage.today_requests),
                    None => usage.today_requests.to_string(),
                };
                rows.push(vec![
                    terminal_safe(&t.name),
                    t.prefix.clone(),
                    t.role.as_str().to_owned(),
                    t.status(now).as_str().to_owned(),
                    t.session_mode.as_str().to_owned(),
                    agents_cell(&t.agents),
                    today,
                    used,
                    t.project
                        .as_deref()
                        .map_or_else(|| "-".to_owned(), terminal_safe),
                    acct::format_bindings(&t.accounts),
                ]);
            }
            print_table(
                &[
                    "NAME",
                    "PREFIX",
                    "ROLE",
                    "STATUS",
                    "SESSION",
                    "AGENTS",
                    "TODAY",
                    "LAST USED",
                    "PROJECT",
                    "ACCOUNTS",
                ],
                &rows,
            );
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
            share_usage,
            limits,
        } => {
            let t = find_token(store, &id)?;
            let limits_patch = (clear_limits || limits.any()).then(|| {
                let base = if clear_limits {
                    Limits::default()
                } else {
                    t.limits
                };
                let full = limits.apply(base);
                hashi::api::LimitsPatch {
                    daily_requests: full.daily_requests,
                    monthly_requests: full.monthly_requests,
                    daily_budget_usd: full.daily_budget_usd,
                    monthly_budget_usd: full.monthly_budget_usd,
                    max_pending: Some(full.max_pending),
                    stop_at_percent: Some(full.stop_at_percent),
                }
            });
            let accounts = (clear_accounts || !accounts.is_empty()).then(|| {
                let mut map = if clear_accounts {
                    AccountMap::new()
                } else {
                    t.accounts.clone()
                };
                for (agent, names) in accounts {
                    map.insert(agent, names);
                }
                map
            });
            let body = hashi::api::UpdateTokenBody {
                name: None,
                note,
                paused: if pause {
                    Some(true)
                } else if resume {
                    Some(false)
                } else {
                    None
                },
                expires_in_days,
                limits: limits_patch,
                accounts,
                project: if clear_project {
                    Some(None)
                } else {
                    project.map(Some)
                },
                share_usage,
            };
            let nothing = body.note.is_none()
                && body.paused.is_none()
                && body.expires_in_days.is_none()
                && body.limits.is_none()
                && body.accounts.is_none()
                && body.project.is_none()
                && body.share_usage.is_none();
            if nothing {
                bail!("nothing to update; see `hashi token update --help`");
            }
            let (t, changes) =
                hashi::api::apply_token_update(store, t, body, None).map_err(api_err)?;
            store.save_token_policy(&t)?;
            let detail = changes.join(", ");
            store.audit("cli", "token.update", &t.name, &detail)?;
            println!("updated {}: {detail}", t.name);
        }
        TokenCommand::Revoke { id } => {
            let found = find_token(store, &id)?;
            let running = store.running_for_token(&found.id)?;
            match store.revoke_token(&found.id)? {
                Revoke::Revoked(token_id) => {
                    let target = found.name;
                    store.audit("cli", "token.revoke", &target, "")?;
                    println!("revoked {}", terminal_safe(&target));
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
                Revoke::NotFound => bail!("no token matches `{}`", terminal_safe(&id)),
                Revoke::LastAdmin => bail!(
                    "refusing to revoke the last admin token; create another with \
                     `hashi token create <name> --admin` first"
                ),
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
    let (base, local) = link_base(store, args.via, args.public_url.as_deref(), args.port)?;
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
    if args.share_usage {
        store.set_invite_share_usage(&invite.id, true)?;
    }
    store.audit(
        "cli",
        "invite.create",
        &invite.label,
        &format!("link valid {}h", args.link_hours),
    )?;
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
    if local {
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
        InviteCommand::List { json } => {
            let now = now_ms();
            let invites = store.list_invites()?;
            if json {
                let list: Vec<_> = invites.iter().map(|i| invite_json(i, now)).collect();
                return print_json(&serde_json::json!({ "invites": list }));
            }
            if invites.is_empty() {
                println!("No invites yet. Create one with `hashi share <label>`.");
                return Ok(());
            }
            let rows: Vec<Vec<String>> = invites
                .iter()
                .map(|i| {
                    vec![
                        i.id.clone(),
                        terminal_safe(&i.label),
                        i.status(now).to_string(),
                        agents_cell(&i.agents),
                        i.project
                            .as_deref()
                            .map_or_else(|| "-".to_owned(), terminal_safe),
                        format!("{} ago", human_age(i.created_at)),
                        rfc3339(i.expires_at).unwrap_or_default(),
                    ]
                })
                .collect();
            print_table(
                &[
                    "ID", "LABEL", "STATUS", "AGENTS", "PROJECT", "CREATED", "EXPIRES",
                ],
                &rows,
            );
        }
        InviteCommand::Cancel { id } => {
            let label = store
                .list_invites()?
                .into_iter()
                .find(|i| i.id == id)
                .map(|i| i.label);
            if !store.cancel_invite(&id)? {
                bail!(
                    "no pending invite with id `{}`; `hashi invite list` shows the ids",
                    terminal_safe(&id)
                );
            }
            let label = label.unwrap_or(id);
            store.audit("cli", "invite.cancel", &label, "")?;
            println!("cancelled the invite for {}", terminal_safe(&label));
        }
    }
    Ok(())
}

/// Runs `hashi audit`, as a table or, with `json`, as `GET /v1/audit`
/// shows it.
///
/// # Errors
///
/// Fails on database errors.
pub fn audit(store: &Store, limit: u32, json: bool) -> Result<()> {
    let entries = store.list_audit(limit)?;
    if json {
        let list: Vec<_> = entries
            .iter()
            .map(|e| {
                serde_json::json!({
                    "at": rfc3339(e.at),
                    "actor": e.actor,
                    "action": e.action,
                    "target": e.target,
                    "detail": e.detail,
                })
            })
            .collect();
        return print_json(&serde_json::json!({ "entries": list }));
    }
    if entries.is_empty() {
        println!("No sharing or access changes yet.");
        return Ok(());
    }
    let rows: Vec<Vec<String>> = entries
        .iter()
        .map(|e| {
            vec![
                rfc3339(e.at).unwrap_or_default(),
                terminal_safe(&e.actor),
                e.action.clone(),
                terminal_safe(&e.target),
                terminal_safe(&e.detail),
            ]
        })
        .collect();
    print_table(&["AT", "ACTOR", "ACTION", "TARGET", "DETAIL"], &rows);
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
            AgentKind::Cursor => self.cursor_bin.clone(),
        };
        custom.unwrap_or_else(|| AgentConfig::default_for(agent).bin)
    }
}

/// `hashi stats ...`
#[derive(Debug, clap::Args)]
pub struct StatsArgs {
    /// on, off or forget; leave out for the report.
    #[command(subcommand)]
    pub action: Option<StatsAction>,
    /// Days covered by the report, today included.
    #[arg(long, default_value_t = 7)]
    pub days: u32,
    /// Print JSON.
    #[arg(long)]
    pub json: bool,
}

/// What `hashi stats` does besides reporting.
#[derive(Debug, Subcommand)]
pub enum StatsAction {
    /// Start tracking your own Claude Code use (reads its session files;
    /// keeps only numbers).
    On,
    /// Stop tracking. What was collected stays until `hashi stats forget`.
    Off,
    /// Delete everything collected about your own use.
    Forget {
        /// Do not ask first.
        #[arg(long)]
        yes: bool,
    },
}

/// Runs `hashi stats`.
///
/// # Errors
///
/// Fails on database or settings errors.
pub fn stats(store: &Store, home: &Path, args: StatsArgs) -> Result<()> {
    match args.action {
        Some(StatsAction::On) => {
            hashi::stats::set_enabled(home, true).map_err(|e| anyhow::anyhow!(e))?;
            store.audit("cli", "stats.on", "", "")?;
            let r = hashi::stats::scan(store, home).map_err(|e| anyhow::anyhow!(e))?;
            println!(
                "tracking your own Claude Code use: read {} session file(s), {} message(s) so far",
                r.files, r.added
            );
            println!(
                "Only numbers are kept (tokens, model, project folder name, time), never what was \
                 said. The server keeps it up to date; see it with `hashi stats`."
            );
        }
        Some(StatsAction::Off) => {
            hashi::stats::set_enabled(home, false).map_err(|e| anyhow::anyhow!(e))?;
            store.audit("cli", "stats.off", "", "")?;
            println!("stopped tracking; `hashi stats forget` deletes what was collected");
        }
        Some(StatsAction::Forget { yes }) => {
            if !yes {
                if !std::io::stdin().is_terminal() {
                    bail!("pass --yes to delete without being asked");
                }
                if !confirm("Delete everything collected about your own use?") {
                    println!("nothing deleted");
                    return Ok(());
                }
            }
            let n = store.forget_own_usage()?;
            store.audit("cli", "stats.forget", "", &n.to_string())?;
            println!("deleted {n} message record(s)");
        }
        None => {
            if hashi::stats::enabled(home) {
                hashi::stats::scan(store, home).map_err(|e| anyhow::anyhow!(e))?;
            }
            let r = hashi::stats::report(store, home, args.days).map_err(|e| anyhow::anyhow!(e))?;
            if args.json {
                return print_json(&serde_json::to_value(&r)?);
            }
            print!("{}", hashi::stats::render(&r));
        }
    }
    Ok(())
}

/// `hashi account ...`
#[derive(Debug, Subcommand)]
pub enum AccountCommand {
    /// Add an account: a login directory Hashi owns (signed in with the
    /// agent's own command), an API key, or a friend's bridge you connected
    /// to (--remote).
    Add {
        /// Agent the account is for.
        agent: AgentKind,
        /// Name, 1 to 32 characters from a-z, 0-9 and -.
        name: String,
        /// login (the default without a key), api-key, or oauth-token (Claude
        /// Code only, from `claude setup-token`).
        #[arg(long)]
        kind: Option<AccountKind>,
        /// Use the access a friend shared with you: the remote saved by
        /// `hashi connect` (see `hashi remote list`). Runs on this account
        /// go to their bridge.
        #[arg(long, value_name = "REMOTE", conflicts_with_all = ["key_stdin", "key_env"])]
        remote: Option<String>,
        /// The agent to ask for on the remote bridge. Default: the same as
        /// this account's.
        #[arg(long, value_name = "AGENT", requires = "remote")]
        remote_agent: Option<AgentKind>,
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
        /// Print JSON, as `GET /v1/accounts` shows each account. Never a
        /// key.
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        bins: BinArgs,
    },
    /// Show how much of each account's quota is left: a bar per window
    /// with its reset time, the plan, where the figures came from and the
    /// state.
    Usage {
        /// Only this agent's accounts.
        #[arg(long)]
        agent: Option<AgentKind>,
        /// Read Claude Code and Codex quota now first (read only; tokens
        /// are never refreshed).
        #[arg(long)]
        refresh: bool,
        /// Also show each agent's `default` account (your own login, or
        /// whatever the plain command is switched to). Without it only the
        /// accounts you added are shown.
        #[arg(long)]
        all: bool,
        /// Print JSON, as `GET /v1/accounts` shows each account.
        #[arg(long)]
        json: bool,
        #[command(flatten)]
        bins: BinArgs,
    },
    /// Point the plain `claude` (or `codex`, `gemini`, `opencode`) command
    /// at an account, so it
    /// signs in as that account in any terminal or editor, or back at your
    /// own login with `default`. Without a name, shows what it uses now.
    /// Running sessions pick the change up when they restart. The Claude
    /// desktop app keeps its own sign-in and is not affected.
    Switch {
        /// claude, codex, gemini or opencode.
        agent: AgentKind,
        /// A login account, or `default` for your original login.
        name: Option<String>,
    },
    /// Undo every switch and command: plain `claude`, `codex`, `gemini`
    /// and `opencode` go back to your own logins, Hashi's copies of them are deleted, and every
    /// command Hashi wrote (such as dclaude, or the `<name>-claude` scripts
    /// of 0.4.0) is removed. Your accounts stay.
    Reset {
        /// Do not ask first.
        #[arg(long)]
        yes: bool,
    },
    /// Show or choose the shell command that starts Claude Code or Codex
    /// without permission prompts (`claude --dangerously-skip-permissions`).
    /// Nothing is set until you pick a word, such as `--name dclaude`.
    Commands {
        /// The agent whose command --name sets.
        #[arg(long, default_value = "claude")]
        agent: AgentKind,
        /// The word you type to start it, such as dclaude. Empty removes
        /// the command.
        #[arg(long, value_name = "WORD")]
        name: Option<String>,
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
    /// Clear an account's limited mark, so it takes runs again now instead
    /// of at the reset time Hashi was told.
    ClearLimit {
        /// Agent the account is for.
        agent: AgentKind,
        /// Account name (`default` for the owner's own login).
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

/// Runs the sign-in command on the owner's terminal, only after they
/// confirm and only when standard input is a terminal. Otherwise prints it.
fn sign_in(home: &Path, agent: AgentKind, name: &str, yes: bool, bins: &BinArgs) -> Result<()> {
    let bin = bins.bin(agent);
    let (Some(line), Some(sign)) = (
        acct::ops::sign_in_line(home, agent, name, &bin),
        acct::sign_in(agent),
    ) else {
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
    if !acct::ops::run_sign_in(home, agent, name, &bin)? {
        eprintln!("the sign-in command did not exit cleanly");
    }
    let state = acct::ops::signed_in(home, agent, name, AccountKind::Login, &bin, true);
    println!("signed in: {state}");
    Ok(())
}

/// The `account list` kind column: the kind, and for a remote account the
/// bridge it goes through, `remote via bob`.
fn kind_cell(a: &hashi::store::AccountState) -> String {
    match (&a.remote, a.remote_agent) {
        (Some(r), Some(ra)) if ra != a.agent => format!("remote via {r} ({ra})"),
        (Some(r), _) => format!("remote via {r}"),
        _ => match &a.shared_by {
            Some(by) => format!("{}, shared by {by}", a.kind),
            None => a.kind.as_str().to_owned(),
        },
    }
}

/// `hashi account add <agent> <name> --remote <remote>`: an account whose
/// runs go to a friend's bridge, through a remote saved by `hashi connect`.
/// Refused when the remote is this same Hashi. The dashboard's add-account
/// form does the same through [`acct::ops::add_remote`].
fn add_remote_account(
    store: &Store,
    home: &Path,
    agent: AgentKind,
    name: &str,
    remote: &str,
    remote_agent: AgentKind,
) -> Result<()> {
    let warning = acct::ops::add_remote(store, home, agent, name, remote, remote_agent, "cli")?;
    if let Some(w) = warning {
        eprintln!("warning: {w}");
    }
    println!(
        "added {} account {name} (remote via {remote}, {} on their bridge)",
        agent.display_name(),
        remote_agent.display_name()
    );
    println!("{}", acct::ops::REMOTE_PRIVACY);
    println!(
        "use it with: hashi token update <id> --account {agent}={name}  (or --account on token create and share)"
    );
    Ok(())
}

/// After an account is added or signed in: how to use it.
fn print_switch_hint(agent: AgentKind, name: &str) {
    if hashi::accounts::switch::supports(agent) {
        println!("make plain `{agent}` use it with: hashi switch {agent} {name}");
    }
}

/// `hashi switch` and `hashi account switch`.
pub fn switch(store: &Store, home: &Path, agent: AgentKind, name: Option<String>) -> Result<()> {
    let Some(name) = name else {
        let live = acct::switch::Live::from_user().ok();
        println!("{}", acct::switch::describe(home, live.as_ref(), agent));
        let mut names: Vec<String> = store
            .list_accounts()?
            .into_iter()
            .filter(|a| a.agent == agent && a.kind == AccountKind::Login && a.shared_by.is_none())
            .map(|a| a.name)
            .collect();
        names.push(acct::DEFAULT.to_owned());
        println!("switch with: hashi switch {agent} <{}>", names.join("|"));
        return Ok(());
    };
    let o = acct::ops::switch(store, home, agent, &name, "cli")?;
    let live = acct::switch::Live::from_user().ok();
    println!("{}", acct::switch::describe(home, live.as_ref(), agent));
    if o.unchanged {
        return Ok(());
    }
    println!(
        "Sessions already running keep their login until they restart. Hashi's own runs on \
         the `default` account use whatever plain {agent} signs in as. Undo with: hashi switch {agent} default"
    );
    Ok(())
}

/// `hashi account commands`: saves the chosen word when given, then syncs
/// and lists.
fn account_commands(
    home: &Path,
    store: &Store,
    agent: AgentKind,
    name: Option<String>,
) -> Result<()> {
    let Some(spec) = acct::commands::spec(agent) else {
        bail!(
            "{} has no skip-permissions command; it is for claude and codex",
            agent.display_name()
        );
    };
    if let Some(word) = name {
        let mut kw = acct::commands::Keywords::load(home);
        kw.set(agent, word.trim());
        kw.save(home).map_err(|e| anyhow::anyhow!(e))?;
        store.audit("cli", "account.commands", agent.as_str(), word.trim())?;
    }
    let kw = acct::commands::Keywords::load(home);
    let r = match acct::commands::sync(home) {
        Ok(r) => r,
        Err(why) => {
            println!("No command made: {why}.");
            return Ok(());
        }
    };
    for c in &r.written {
        println!("made {c}");
    }
    for c in &r.removed {
        println!("deleted {c}");
    }
    for (c, why) in &r.skipped {
        eprintln!("warning: no {c} command: {}", terminal_safe(why));
    }
    let mut any = false;
    for s in &acct::commands::SPECS {
        if let Some(word) = kw.get(s.agent) {
            any = true;
            println!(
                "{word}: runs {} {}  (in {})",
                s.bin,
                s.skip_flag,
                r.dir.display()
            );
        }
    }
    if !any {
        println!(
            "No skip-permissions command is set. Pick the word you want to type, for example:\n  \
             hashi account commands --agent {} --name d{}",
            spec.agent, spec.bin
        );
    }
    println!(
        "It runs the plain {}, so it follows `hashi switch`. An empty --name removes it.",
        spec.bin
    );
    Ok(())
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
            remote,
            remote_agent,
            key_stdin,
            key_env,
            yes,
            bins,
        } => {
            if let Some(remote) = remote {
                if kind.is_some_and(|k| k != AccountKind::Remote) {
                    bail!("--remote adds a remote account; drop --kind");
                }
                return add_remote_account(
                    store,
                    home,
                    agent,
                    &name,
                    &remote,
                    remote_agent.unwrap_or(agent),
                );
            }
            if kind == Some(AccountKind::Remote) {
                bail!("a remote account needs --remote <name> (see `hashi remote list`)");
            }
            let keyed = key_stdin || key_env.is_some();
            let kind = acct::ops::check_new(store, agent, &name, kind, keyed)?;
            // The key is read before anything is created, so a mistyped
            // key leaves nothing behind.
            let key = kind
                .is_key()
                .then(|| read_key(key_env.as_deref()))
                .transpose()?;
            let dir = acct::ops::create(store, home, agent, &name, kind, key.as_deref(), "cli")?;
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
            print_switch_hint(agent, &name);
        }
        AccountCommand::Login {
            agent,
            name,
            yes,
            bins,
        } => {
            acct::ops::prepare_login(store, home, agent, &name)?;
            sign_in(home, agent, &name, yes, &bins)?;
            print_switch_hint(agent, &name);
        }
        AccountCommand::Switch { agent, name } => switch(store, home, agent, name)?,
        AccountCommand::Reset { yes } => {
            if !yes {
                if !std::io::stdin().is_terminal() {
                    bail!("pass --yes to reset without being asked");
                }
                if !confirm(
                    "Put plain claude, codex, gemini and opencode back on your own logins and delete every command \
                     Hashi made?",
                ) {
                    println!("nothing changed");
                    return Ok(());
                }
            }
            let done = acct::ops::reset(store, home, "cli")?;
            if done.is_empty() {
                println!("already clean: nothing was switched and Hashi made no commands");
            } else {
                for line in done {
                    println!("{line}");
                }
                println!("Restart running agent sessions to pick it up.");
            }
        }
        AccountCommand::Commands { agent, name } => account_commands(home, store, agent, name)?,
        AccountCommand::List { json, bins } => {
            let now = now_ms();
            let rotate_at = cli_rotate_at();
            if json {
                let mut list = Vec::new();
                for a in &hashi::api::accounts_with_usage(store).map_err(api_err)? {
                    list.push(
                        hashi::api::account_json_with(store, a, now, rotate_at).map_err(api_err)?,
                    );
                }
                return print_json(&serde_json::json!({ "accounts": list }));
            }
            let accounts = store.list_accounts()?;
            if accounts.is_empty() {
                println!(
                    "No accounts yet: every agent uses your own login. Add one with \
                     `hashi account add <agent> <name>`."
                );
                return Ok(());
            }
            let mut rows = Vec::with_capacity(accounts.len());
            for a in accounts {
                let summary = store.account_summary(a.agent, &a.name, rotate_at, now)?;
                let state = match a.status(now) {
                    hashi::store::AccountStatus::Limited(until) => {
                        format!("limited until {}", rfc3339(until).unwrap_or_default())
                    }
                    hashi::store::AccountStatus::Disabled => match &a.disabled_reason {
                        Some(why) => format!("disabled: {}", terminal_safe(why)),
                        None => "disabled".to_owned(),
                    },
                    s => s.as_str().to_owned(),
                };
                let used = a
                    .last_used
                    .map_or_else(|| "never".to_owned(), |u| format!("{} ago", human_age(u)));
                let (today, _) = store.account_runs(a.agent, &a.name, now)?;
                rows.push(vec![
                    a.agent.as_str().to_owned(),
                    a.name.clone(),
                    terminal_safe(&kind_cell(&a)),
                    acct::ops::signed_in(home, a.agent, &a.name, a.kind, &bins.bin(a.agent), true),
                    state,
                    used,
                    today.to_string(),
                    store.tokens_using_account(a.agent, &a.name)?.to_string(),
                    summary_cell(&summary),
                ]);
            }
            print_table(
                &[
                    "AGENT",
                    "NAME",
                    "KIND",
                    "SIGNED IN",
                    "STATE",
                    "LAST USED",
                    "TODAY",
                    "TOKENS",
                    "USAGE",
                ],
                &rows,
            );
            println!();
            let live = acct::switch::Live::from_user().ok();
            for agent in acct::switch::AGENTS {
                let who = acct::switch::PlainLogin::read(home, live.as_ref(), agent);
                // Agents with no login here and nothing switched in say nothing.
                if who.switched() || who.email.is_some() {
                    println!(
                        "{}  (hashi switch {agent} <name>)",
                        acct::switch::describe(home, live.as_ref(), agent)
                    );
                }
            }
            let _ = acct::commands::sync(home);
        }
        AccountCommand::Usage {
            agent,
            refresh,
            all,
            json,
            bins,
        } => account_usage(store, home, agent, refresh, all, json, &bins)?,
        AccountCommand::Disable { agent, name } => {
            acct::ops::set_disabled(store, agent, &name, true, "cli")?;
            println!("disabled {agent}:{name}");
        }
        AccountCommand::Enable { agent, name } => {
            acct::ops::set_disabled(store, agent, &name, false, "cli")?;
            println!("enabled {agent}:{name}");
        }
        AccountCommand::ClearLimit { agent, name } => {
            if acct::ops::clear_limit(store, agent, &name, "cli")? {
                println!("cleared the limit on {agent}:{name}");
            } else {
                println!("{agent}:{name} was not limited");
            }
        }
        AccountCommand::Remove { agent, name, yes } => {
            let using = acct::ops::check_removable(store, home, agent, &name)?;
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
            let (removed, dir) = acct::ops::remove(store, home, agent, &name, "cli")?;
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

/// The rotate threshold the CLI shows `warning` at: `HASHI_ROTATE_AT` when
/// it is a valid one, else the default.
fn cli_rotate_at() -> u8 {
    std::env::var("HASHI_ROTATE_AT")
        .ok()
        .and_then(|v| v.trim().parse::<u8>().ok())
        .and_then(|n| hashi::usage::check_rotate_at(n).ok())
        .unwrap_or(hashi::usage::DEFAULT_ROTATE_AT)
}

/// The `account list` usage column: `62% left` or `unknown`, with the
/// state when it is not ok.
fn summary_cell(s: &hashi::usage::Summary) -> String {
    use hashi::usage::UsageState;
    match (s.state, s.headroom_pct()) {
        (UsageState::Limited, _) => match s.limited_until.and_then(rfc3339) {
            Some(t) => format!("limited until {t}"),
            None => "limited".to_owned(),
        },
        (UsageState::Warning, Some(h)) => format!("{h}% left (warning)"),
        (_, Some(h)) => format!("{h}% left"),
        (_, None) => "unknown".to_owned(),
    }
}

/// A bar of `width` cells filled to `used` percent.
fn usage_bar(used: f64, width: usize) -> String {
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss
    )]
    let filled = ((hashi::usage::clamp_percent(used) / 100.0) * width as f64).round() as usize;
    let filled = filled.min(width);
    format!("{}{}", "█".repeat(filled), "░".repeat(width - filled))
}

use hashi::util::until;

/// `hashi account usage`: each account's windows, plan, source and state,
/// after reading Claude Code and Codex quota first with `--refresh`.
fn account_usage(
    store: &Store,
    home: &Path,
    agent: Option<AgentKind>,
    refresh: bool,
    all: bool,
    json: bool,
    bins: &BinArgs,
) -> Result<()> {
    // The accounts you added; the `default` ones only with --all.
    let shown = |a: &hashi::store::AccountState| {
        agent.is_none_or(|g| a.agent == g) && (all || a.name != acct::DEFAULT)
    };
    let mut accounts: Vec<_> = hashi::api::accounts_with_usage(store)
        .map_err(api_err)?
        .into_iter()
        .filter(|a| shown(a))
        .collect();
    if refresh {
        let store = std::sync::Arc::new(server_store(home)?);
        let hub = std::sync::Arc::new(hashi::usage::Hub::default());
        let installed: Vec<AgentKind> = [AgentKind::Claude, AgentKind::Codex]
            .into_iter()
            .filter(|a| agent.is_none_or(|g| *a == g))
            .filter(|a| hashi::agent::is_executable(&bins.bin(*a)))
            .collect();
        let poller = hashi::usage::Poller::new(
            std::sync::Arc::clone(&store),
            hashi::events::Bus::default(),
            &hub,
            home.to_path_buf(),
            hashi::usage::UsageSettings::default(),
            Some(bins.bin(AgentKind::Codex)),
            &installed,
        );
        let mut targets = poller.eligible();
        targets.retain(|(a, n)| agent.is_none_or(|g| *a == g) && (all || n != acct::DEFAULT));
        let results = tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                let mut out = Vec::new();
                for (a, n) in &targets {
                    out.push(((*a, n.clone()), poller.poll(*a, n).await));
                }
                out
            })
        });
        for ((a, n), r) in results {
            if let Err(e) = r
                && !json
            {
                eprintln!("{a}:{n}: {e}");
            }
        }
        accounts = hashi::api::accounts_with_usage(store.as_ref())
            .map_err(api_err)?
            .into_iter()
            .filter(|a| shown(a))
            .collect();
    }
    let now = now_ms();
    let rotate_at = cli_rotate_at();
    if json {
        let mut list = Vec::new();
        for a in &accounts {
            list.push(hashi::api::account_json_with(store, a, now, rotate_at).map_err(api_err)?);
        }
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({ "accounts": list }))?
        );
        return Ok(());
    }
    if accounts.is_empty() {
        if all {
            println!(
                "no usage yet: Claude Code and Codex accounts are read while `hashi serve` runs \
                 (or now with --refresh)"
            );
        } else {
            println!(
                "no accounts added yet: add one with `hashi account add`, or see your own \
                 login's usage with --all"
            );
        }
        return Ok(());
    }
    for a in &accounts {
        let windows = store.usage_windows(a.agent, &a.name)?;
        let summary = hashi::usage::summarize(&windows, a.limited_until, rotate_at, now);
        let poll = store.poll_status(a.agent, &a.name)?;
        let mut head = format!("{}:{}", a.agent, a.name);
        if let Some(plan) = poll.as_ref().and_then(|p| p.plan.as_deref()) {
            head.push_str(&format!("  {plan}"));
        } else if let Some(remote) = &a.remote {
            head.push_str(&format!("  via {remote}"));
        }
        head.push_str(&format!("  {}", summary_cell(&summary)));
        if let Some(w) = windows.iter().max_by_key(|w| w.observed_at)
            && let Some(at) = w.observed_at
        {
            head.push_str(&format!("  ({}, {} ago)", w.source.as_str(), human_age(at)));
        }
        println!("{head}");
        if !windows.is_empty()
            && let Some(note) = poll.as_ref().and_then(|p| p.note.as_deref())
        {
            println!("  these figures are old; the last read failed: {note}");
        }
        if let Some(by) = &a.shared_by {
            println!(
                "  {}",
                terminal_safe(&hashi::share::share_note(
                    by,
                    a.remind_at,
                    a.share_until,
                    now
                ))
            );
        }
        if windows.is_empty() {
            let spend = store.account_spend_today(a.agent, &a.name, now)?;
            let (runs, _) = store.account_runs(a.agent, &a.name, now)?;
            let note = poll
                .as_ref()
                .and_then(|p| p.note.clone())
                .unwrap_or_else(|| "no live quota for this agent".to_owned());
            println!(
                "  {note}; {runs} run(s) today, {} tokens, ${:.2} reported",
                spend.tokens, spend.cost_usd
            );
        }
        for w in &windows {
            let used = w.used_at(now).unwrap_or(w.used_percent);
            let mut line = format!("  {:<14} {} {used:>3.0}%", w.label, usage_bar(used, 20));
            if let Some(at) = w.resets_at.filter(|r| *r > now) {
                line.push_str(&format!(
                    "  resets in {} ({})",
                    until(at, now),
                    rfc3339(at).unwrap_or_default()
                ));
            }
            println!("{line}");
        }
    }
    Ok(())
}

/// The store again, shared, for the poller.
fn server_store(home: &Path) -> Result<Store> {
    hashi::server::open_store(home)
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
    /// How runs choose among an agent's accounts: in-order, most-left or
    /// soonest-reset. Default: the server's --account-strategy.
    #[arg(long, value_parser = parse_strategy)]
    account_strategy: Option<hashi::usage::Strategy>,
    /// Percent used (50 to 99) at which runs move off an account. Default:
    /// the server's --rotate-at.
    #[arg(long, value_name = "PCT",
          value_parser = clap::value_parser!(u8).range(50..=99))]
    rotate_at: Option<u8>,
}

fn parse_strategy(s: &str) -> Result<hashi::usage::Strategy, String> {
    s.parse()
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
        /// Go back to the server's account strategy.
        #[arg(long, conflicts_with = "account_strategy")]
        clear_account_strategy: bool,
        /// Go back to the server's rotate threshold.
        #[arg(long, conflicts_with = "rotate_at")]
        clear_rotate_at: bool,
        #[command(flatten)]
        settings: ProjectArgs,
    },
    /// List projects with their folder, settings and use.
    List {
        /// Print JSON, as `GET /v1/projects` shows each project.
        #[arg(long)]
        json: bool,
    },
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
fn installed_agents(bins: &BinArgs) -> Vec<AgentKind> {
    let found: Vec<AgentKind> = AgentKind::ALL
        .into_iter()
        .filter(|a| hashi::agent::is_executable(&bins.bin(*a)))
        .collect();
    if found.is_empty() {
        AgentKind::ALL.to_vec()
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
                account_strategy: settings.account_strategy,
                rotate_at: settings.rotate_at,
                created_at: now_ms(),
            };
            if store.list_projects()?.len() >= hashi::projects::MAX_PROJECTS {
                bail!(
                    "the project limit ({}) is reached; remove one first with \
                     `hashi project remove <name>`",
                    hashi::projects::MAX_PROJECTS
                );
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
            clear_account_strategy,
            clear_rotate_at,
            settings,
        } => {
            let current = store.get_project(&name)?.with_context(|| {
                format!("no project named {name}; `hashi project list` shows them")
            })?;
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
                account_strategy: if clear_account_strategy {
                    Some(None)
                } else {
                    settings.account_strategy.map(Some)
                },
                rotate_at: if clear_rotate_at {
                    Some(None)
                } else {
                    settings.rotate_at.map(Some)
                },
            };
            let (p, changes) =
                hashi::api::apply_update(store, Some(home), current, body).map_err(api_err)?;
            store.save_project(&p)?;
            let detail = changes.join(", ");
            store.audit("cli", "project.update", &p.name, &detail)?;
            println!("updated {}: {detail}", p.name);
        }
        ProjectCommand::List { json } => {
            let projects = store.list_projects()?;
            let now = now_ms();
            if json {
                let mut list = Vec::with_capacity(projects.len());
                for p in &projects {
                    list.push(hashi::api::project_json(store, p, now).map_err(api_err)?);
                }
                return print_json(&serde_json::json!({ "projects": list }));
            }
            if projects.is_empty() {
                println!(
                    "No projects yet: every token runs in its own workspace. Add one with \
                     `hashi project add <name> <path>`."
                );
                return Ok(());
            }
            let mut rows = Vec::with_capacity(projects.len());
            for p in projects {
                let st = store.project_stats(&p.name, now)?;
                let agents = match p.default_agent {
                    Some(d) => format!("{} ({d})", hashi::token::encode_agents(&p.agents)),
                    None => hashi::token::encode_agents(&p.agents),
                };
                let strategy = match (p.account_strategy, p.rotate_at) {
                    (None, None) => "server default".to_owned(),
                    (s, r) => format!(
                        "{} at {}",
                        s.map_or("default", hashi::usage::Strategy::as_str),
                        r.map_or_else(|| "default".to_owned(), |n| format!("{n}%"))
                    ),
                };
                rows.push(vec![
                    p.name.clone(),
                    st.tokens.to_string(),
                    st.today_requests.to_string(),
                    st.month_requests.to_string(),
                    format!("${:.2}", st.month_cost_usd),
                    agents,
                    acct::format_bindings(&p.accounts),
                    hashi::projects::format_models(&p.models),
                    strategy,
                    terminal_safe(&p.path),
                ]);
            }
            print_table(
                &[
                    "NAME", "TOKENS", "TODAY", "MONTH", "SPEND", "AGENTS", "ACCOUNTS", "MODELS",
                    "STRATEGY", "PATH",
                ],
                &rows,
            );
        }
        ProjectCommand::Remove { name, unbind } => match store.remove_project(&name, unbind)? {
            hashi::store::RemovedProject::NotFound => {
                bail!("no project named {name}; `hashi project list` shows them")
            }
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

// ── access requests ────────────────────────────────────────────────────────

/// `hashi requests ...`
#[derive(Debug, Subcommand)]
pub enum RequestsCommand {
    /// List requests for access and for more (pending ones unless --all).
    List {
        /// Show decided and expired requests too.
        #[arg(long)]
        all: bool,
        /// Print JSON, as `GET /v1/access-requests` shows each request,
        /// with the ask-access link's state. Never a secret.
        #[arg(long)]
        json: bool,
    },
    /// Accept a request. A new person gets a single-use invite that only
    /// they can collect; a token gets the asked values. Flags override what
    /// was asked for.
    Accept {
        /// Request id, or a unique start of it.
        id: String,
        /// Token name for a new person. Default: the name they gave.
        #[arg(long)]
        label: Option<String>,
        /// Agents (comma separated). Default: what they asked for, within
        /// the link's project, or the token's agents plus the asked ones.
        #[arg(long, value_delimiter = ',')]
        agents: Option<Vec<AgentKind>>,
        /// Keep a conversation per agent across prompts (new people).
        #[arg(long)]
        stateful: bool,
        /// Token lifetime in days. For a token, 0 removes the expiry.
        #[arg(long, value_parser = clap::value_parser!(u32).range(0..=3650))]
        expires_in_days: Option<u32>,
        /// How long the invite stays claimable, in hours (new people).
        #[arg(
            long,
            value_parser = clap::builder::PossibleValuesParser::new(["1", "24", "168"])
                .map(|s| s.parse::<u32>().unwrap_or(168)),
        )]
        link_hours: Option<u32>,
        /// Accounts for an agent, in order, for example claude=work
        /// (repeatable, new people).
        #[arg(long = "account", value_name = "AGENT=NAMES", value_parser = parse_account)]
        accounts: Vec<Binding>,
        /// Project for a new person, instead of the link's.
        #[arg(long)]
        project: Option<String>,
        #[command(flatten)]
        limits: LimitArgs,
    },
    /// Decline a request. The requester sees the reason.
    Decline {
        /// Request id, or a unique start of it.
        id: String,
        /// Why, up to 300 characters.
        #[arg(long)]
        reason: Option<String>,
    },
    /// Open the ask-access link, so people can ask for access. Opening a
    /// new one closes the old one.
    Open {
        /// The project new requests are for.
        #[arg(long)]
        project: Option<String>,
        /// A note shown on the page.
        #[arg(long)]
        note: Option<String>,
        /// Hours until it closes by itself (1 to 720).
        #[arg(long, default_value_t = hashi::access::DEFAULT_LINK_HOURS,
              value_parser = clap::value_parser!(u32).range(1..=720))]
        hours: u32,
        /// Base URL for the link, as for `hashi share`.
        #[arg(long, env = "HASHI_PUBLIC_URL")]
        public_url: Option<String>,
        /// Port of the local server, used for a local-only link. Default: the
        /// one saved from the dashboard in `<home>/settings.json`, else 7878.
        #[arg(long, env = "HASHI_PORT")]
        port: Option<u16>,
    },
    /// Close the ask-access link.
    Close,
}

fn find_request(store: &Store, id: &str) -> Result<hashi::store::AccessRequest> {
    let matches: Vec<_> = store
        .list_access_requests()?
        .into_iter()
        .filter(|r| r.id == id || (id.len() >= 4 && r.id.starts_with(id)))
        .collect();
    match matches.as_slice() {
        [r] => Ok(r.clone()),
        [] => bail!(
            "no access request matches `{}`; `hashi requests list --all` shows them",
            terminal_safe(id)
        ),
        _ => bail!(
            "`{}` matches several requests; use more of the id",
            terminal_safe(id)
        ),
    }
}

fn limits_text(l: &Limits) -> String {
    let mut parts = Vec::new();
    if let Some(n) = l.daily_requests {
        parts.push(format!("{n}/day"));
    }
    if let Some(n) = l.monthly_requests {
        parts.push(format!("{n}/month"));
    }
    if let Some(n) = l.daily_budget_usd {
        parts.push(format!("${n}/day"));
    }
    if let Some(n) = l.monthly_budget_usd {
        parts.push(format!("${n}/month"));
    }
    if let Some(n) = l.max_pending {
        parts.push(format!("{n} at a time"));
    }
    if let Some(n) = l.stop_at_percent {
        parts.push(format!("stops at {n}% of your quota"));
    }
    if parts.is_empty() {
        "no limits".to_owned()
    } else {
        parts.join(", ")
    }
}

fn public_base(store: &Store, public_url: Option<&str>, port: u16) -> (String, bool) {
    let public = public_url.map_or_else(
        || {
            store
                .data_dir()
                .and_then(|home| hashi::server::read_public_url(&home))
        },
        |u| Some(u.trim_end_matches('/').to_owned()),
    );
    let local = public.is_none();
    (
        public.unwrap_or_else(|| format!("http://127.0.0.1:{port}")),
        local,
    )
}

/// An access request as `GET /v1/access-requests` shows it.
fn request_json(store: &Store, r: &hashi::store::AccessRequest, now: i64) -> serde_json::Value {
    let status = r.state(now);
    let prefill = (status == "pending")
        .then(|| hashi::access::prefill(store, r).ok())
        .flatten();
    serde_json::json!({
        "id": r.id,
        "kind": r.kind,
        "status": status,
        "name": r.name,
        "message": r.message,
        "agents": r.agents,
        "project": r.project,
        "asked": r.asked,
        "summary": hashi::access::asked_text(r),
        "tokenId": r.token_id,
        "reason": r.reason,
        "inviteId": r.invite_id,
        "createdAt": rfc3339(r.created_at),
        "expiresAt": rfc3339(r.expires_at),
        "decidedAt": r.decided_at.and_then(rfc3339),
        "decidedBy": r.decided_by,
        "prefill": prefill,
    })
}

/// The ask-access link as `GET /v1/request-link` shows it, never its code.
fn link_json(l: &hashi::store::AccessLink, now: i64) -> serde_json::Value {
    serde_json::json!({
        "id": l.id,
        "status": l.status(now),
        "project": l.project,
        "note": l.note,
        "createdAt": rfc3339(l.created_at),
        "expiresAt": rfc3339(l.expires_at),
        "closedAt": l.closed_at.and_then(rfc3339),
        "createdBy": l.created_by,
    })
}

fn print_requests(store: &Store, all: bool, json: bool) -> Result<()> {
    let now = now_ms();
    let every = hashi::access::list(store).map_err(api_err)?;
    let pending = every.iter().filter(|r| r.is_pending(now)).count();
    let list: Vec<_> = every
        .into_iter()
        .filter(|r| all || r.is_pending(now))
        .collect();
    if json {
        let requests: Vec<_> = list.iter().map(|r| request_json(store, r, now)).collect();
        let link = store.latest_access_link()?.map(|l| link_json(&l, now));
        return print_json(&serde_json::json!({
            "requests": requests,
            "pending": pending,
            "link": link,
        }));
    }
    match store.latest_access_link()? {
        Some(link) if link.status(now) == "open" => println!(
            "ask-access link: open, closes {}",
            rfc3339(link.expires_at).unwrap_or_default()
        ),
        Some(link) => println!("ask-access link: {}", link.status(now)),
        None => println!("ask-access link: never opened (hashi requests open)"),
    }
    if list.is_empty() {
        println!("No {}requests.", if all { "" } else { "pending " });
        return Ok(());
    }
    println!(
        "{:<8}  {:<4}  {:<8}  {:<20}  {:<9}  WHAT",
        "ID", "KIND", "STATUS", "NAME", "AGE"
    );
    for r in &list {
        println!(
            "{:<8}  {:<4}  {:<8}  {:<20}  {:<9}  {}",
            r.id.get(..8).unwrap_or(&r.id),
            r.kind.as_str(),
            r.state(now),
            terminal_safe(&hashi::util::truncate(&r.name, 20)),
            format!("{} ago", human_age(r.created_at)),
            terminal_safe(&hashi::access::asked_text(r)),
        );
        if !r.message.is_empty() {
            println!(
                "          \"{}\"",
                terminal_safe(&hashi::util::truncate(&r.message, 200))
            );
        }
        if r.is_pending(now)
            && let Ok(pre) = hashi::access::prefill(store, r)
            && let Some(cur) = &pre.current
        {
            println!(
                "          now: {}, agents {}; accepting gives: {}, agents {}",
                limits_text(&cur.limits),
                hashi::token::encode_agents(&cur.agents),
                limits_text(&pre.limits),
                hashi::token::encode_agents(&pre.agents),
            );
        }
        if let Some(reason) = &r.reason {
            println!("          reason: {}", terminal_safe(reason));
        }
    }
    Ok(())
}

/// Runs a `hashi requests` command.
///
/// # Errors
///
/// Fails on invalid input, an unknown or decided request, or database errors.
pub fn requests(store: &Store, cmd: RequestsCommand) -> Result<()> {
    match cmd {
        RequestsCommand::List { all, json } => print_requests(store, all, json)?,
        RequestsCommand::Accept {
            id,
            label,
            agents,
            stateful,
            expires_in_days,
            link_hours,
            accounts,
            project,
            limits,
        } => {
            let r = find_request(store, &id)?;
            let accounts = if accounts.is_empty() {
                None
            } else {
                Some(bindings(store, AccountMap::new(), accounts)?)
            };
            let input = hashi::access::AcceptInput {
                label: label.map(|l| clean_label(&l)).transpose()?,
                session_mode: stateful.then_some(SessionMode::Stateful),
                agents,
                expires_in_days,
                limits: limits.any().then(|| limits.apply(Limits::default())),
                link_hours,
                accounts,
                project,
            };
            match hashi::access::accept(store, "cli", &r.id, input).map_err(api_err)? {
                hashi::access::Accepted::Invite { invite, .. } => {
                    println!(
                        "accepted {}: a single-use invite ({}, {}) waits for them until {}",
                        terminal_safe(&invite.label),
                        hashi::token::encode_agents(&invite.agents),
                        limits_text(&invite.limits),
                        rfc3339(invite.expires_at).unwrap_or_default()
                    );
                    println!(
                        "they collect it from their request page or `hashi request status`; \
                         it is never shown here"
                    );
                }
                hashi::access::Accepted::Token { token, changes, .. } => {
                    println!(
                        "accepted {}: {changes} (now {}, agents {})",
                        terminal_safe(&token.name),
                        limits_text(&token.limits),
                        hashi::token::encode_agents(&token.agents)
                    );
                }
            }
        }
        RequestsCommand::Decline { id, reason } => {
            let r = find_request(store, &id)?;
            let r =
                hashi::access::decline(store, "cli", &r.id, reason.as_deref()).map_err(api_err)?;
            println!("declined the request from {}", terminal_safe(&r.name));
        }
        RequestsCommand::Open {
            project,
            note,
            hours,
            public_url,
            port,
        } => {
            let (link, code) = hashi::access::open_link(
                store,
                "cli",
                hashi::access::OpenLink {
                    project,
                    note,
                    hours: Some(hours),
                },
            )
            .map_err(api_err)?;
            let saved = store
                .data_dir()
                .and_then(|home| hashi::settings::load(&home).ok())
                .and_then(|s| s.port);
            let (port, _) = hashi::settings::resolve(port, saved, hashi::settings::DEFAULT_PORT);
            let (base, local) = public_base(store, public_url.as_deref(), port);
            eprintln!(
                "ask-access link{} (anyone with it can ask; closes {}):",
                link.project
                    .as_deref()
                    .map(|p| format!(" for project {p}"))
                    .unwrap_or_default(),
                rfc3339(link.expires_at).unwrap_or_default()
            );
            println!("{}", hashi::access::link_url(&base, &code));
            if local {
                eprintln!(
                    "note: this link only works on this machine. Press t in the dashboard to go \
                     public, start with --tunnel or set HASHI_PUBLIC_URL to share it."
                );
            }
        }
        RequestsCommand::Close => {
            if hashi::access::close_link(store, "cli").map_err(api_err)? {
                println!("closed the ask-access link");
            } else {
                println!("no ask-access link was open");
            }
        }
    }
    Ok(())
}

/// `hashi pool ...`: host a pool, or lend this machine's agents to one.
#[derive(Debug, Subcommand)]
pub enum PoolCommand {
    /// Open the join link for your pool (closes any open one).
    Open {
        /// Hours the link lets people join.
        #[arg(long, default_value_t = 6, value_parser = clap::value_parser!(u32).range(1..=720))]
        hours: u32,
        /// How many people may join.
        #[arg(long, default_value_t = 20, value_parser = clap::value_parser!(u32).range(1..=50))]
        max_members: u32,
        /// How the link reaches people: auto, cloudflare, tailscale, lan
        /// (this wifi) or local.
        #[arg(long, default_value = "auto", value_parser = clap::value_parser!(hashi::links::Via))]
        via: hashi::links::Via,
        /// Base URL for the link, as for `hashi share`.
        #[arg(long, env = "HASHI_PUBLIC_URL")]
        public_url: Option<String>,
        /// Port of the local server.
        #[arg(long, env = "HASHI_PORT")]
        port: Option<u16>,
    },
    /// Stop letting people join. Those in the pool stay.
    Close,
    /// Who is in your pool, online or not, and each agent's headroom.
    Status {
        /// Print JSON, as `GET /v1/pool` shows it.
        #[arg(long)]
        json: bool,
    },
    /// Take someone out of your pool.
    Kick {
        /// Their name.
        name: String,
    },
    /// Lend this machine's agents to someone's pool. `hashi serve` must run
    /// for the prompts to be served.
    Join {
        /// The pool link, https://<host>/pool/hashi_pool_<code>.
        link: String,
        /// Your name in the pool.
        #[arg(long)]
        name: String,
        /// The agents to offer (comma separated). Default: claude.
        #[arg(long, value_delimiter = ',', default_value = "claude")]
        agents: Vec<AgentKind>,
        /// The accounts to run them on, for example claude=work
        /// (repeatable). Default: each agent's default account.
        #[arg(long = "account", value_name = "AGENT=NAMES", value_parser = parse_account)]
        accounts: Vec<Binding>,
        /// Stop serving once your account has used this percent of its
        /// 5-hour or weekly quota.
        #[arg(long, value_name = "PCT", value_parser = clap::value_parser!(u8).range(1..=100))]
        stop_at: Option<u8>,
        /// Leave by yourself after this many hours.
        #[arg(long, value_parser = clap::value_parser!(u32).range(1..=720))]
        hours: Option<u32>,
    },
    /// Leave a pool you joined.
    Leave {
        /// The host's name (default: the only pool joined).
        host: Option<String>,
    },
    /// The pools this machine joined.
    List,
}

/// Runs a `hashi pool` command.
///
/// # Errors
///
/// Fails on a bad link or name, a refusal from the host, or database errors.
pub fn pool(store: &Store, home: &Path, cmd: PoolCommand) -> Result<()> {
    match cmd {
        PoolCommand::Open {
            hours,
            max_members,
            via,
            public_url,
            port,
        } => {
            let (base, local) = link_base(store, via, public_url.as_deref(), port)?;
            let (link, code) = store.open_pool_link(hours, max_members)?;
            store.audit("cli", "pool.open", "pool", &format!("{hours}h"))?;
            eprintln!(
                "pool link (up to {} people, joins close {}):",
                link.max_members,
                rfc3339(link.expires_at).unwrap_or_default()
            );
            println!("{base}/pool/{code}");
            eprintln!(
                "people join with: hashi pool join <link> --name <their name> --stop-at 70\n\
                 they keep `hashi serve` running; everyone in the pool sees `hashi pool status`."
            );
            if local {
                eprintln!(
                    "note: this link only works on this machine; pass --via lan, cloudflare or tailscale."
                );
            }
        }
        PoolCommand::Close => {
            if store.close_pool_link()? {
                store.audit("cli", "pool.close", "pool", "")?;
                println!("the pool link is closed; members stay until they leave or you kick them");
            } else {
                println!("no pool link is open");
            }
        }
        PoolCommand::Status { json } => {
            let rotate_at = cli_rotate_at();
            let online = |m: &hashi::store::PoolMember| {
                m.last_seen.is_some_and(|s| {
                    now_ms().saturating_sub(s)
                        < i64::try_from(hashi::pool::ONLINE_FOR.as_millis()).unwrap_or(i64::MAX)
                })
            };
            let v = hashi::api::pool::view_with(store, rotate_at, "this Hashi", online)?;
            if json {
                return print_json(&v);
            }
            println!(
                "pool link: {}",
                match v["joinClosesAt"].as_str() {
                    Some(t) => format!("open until {t}"),
                    None => "closed (hashi pool open)".to_owned(),
                }
            );
            let members = v["members"].as_array().cloned().unwrap_or_default();
            if members.is_empty() {
                println!("nobody has joined yet");
            }
            for m in members {
                let agents: Vec<String> = m["agents"]
                    .as_object()
                    .map(|o| {
                        o.iter()
                            .map(|(a, u)| match u["headroom"].as_u64() {
                                Some(h) => format!("{a} {h}% left"),
                                None => format!("{a} {}", u["state"].as_str().unwrap_or("unknown")),
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                println!(
                    "{}  {}  {}",
                    terminal_safe(m["name"].as_str().unwrap_or_default()),
                    if m["online"] == true {
                        "online"
                    } else {
                        "offline"
                    },
                    agents.join(", ")
                );
            }
        }
        PoolCommand::Kick { name } => match store.remove_pool_member(&name)? {
            Some(m) => {
                store.audit("cli", "pool.kick", &m.name, "")?;
                println!("{} is out of the pool", terminal_safe(&m.name));
            }
            None => bail!("nobody called {} is in the pool", terminal_safe(&name)),
        },
        PoolCommand::Join {
            link,
            name,
            agents,
            accounts,
            stop_at,
            hours,
        } => {
            let accounts = bindings(store, AccountMap::new(), accounts)?;
            let joined = hashi::pool_client::join(
                store,
                home,
                &hashi::pool_client::JoinOptions {
                    link,
                    name,
                    agents,
                    accounts,
                    stop_at,
                    hours,
                },
            )?;
            println!(
                "joined {}'s pool as {}, offering {}",
                terminal_safe(&joined.host),
                joined.name,
                agents_cell(&joined.agents)
            );
            println!(
                "  prompts from everyone in the pool run here, with a local token \"pool {}\":",
                hashi::remote::slug(&joined.host)
            );
            println!("  pause it to stop serving for a while, edit it to change limits");
            println!("  keep `hashi serve` running; leave with `hashi pool leave`");
            if let Some(remote) = &joined.remote {
                println!("  use the pool yourself: hashi ask --remote {remote} \"hello\"");
                println!(
                    "  everyone's agents and headroom: {}/v1/pool",
                    joined.base_url
                );
            }
        }
        PoolCommand::Leave { host } => {
            let p = hashi::pool_client::leave(store, home, host.as_deref())?;
            println!("left {}'s pool", terminal_safe(&p.host));
        }
        PoolCommand::List => {
            let pools = hashi::pool_client::load(home)?;
            if pools.is_empty() {
                println!("this machine is in no pool; join one with `hashi pool join <link>`");
            }
            for p in pools {
                println!(
                    "{}  as {}  {}  {}{}",
                    terminal_safe(&p.host),
                    p.name,
                    p.base_url,
                    agents_cell(&p.agents),
                    p.until
                        .and_then(rfc3339)
                        .map(|t| format!("  until {t}"))
                        .unwrap_or_default()
                );
            }
        }
    }
    Ok(())
}
