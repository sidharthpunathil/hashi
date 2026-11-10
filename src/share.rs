//! Two ways to share an agent with someone, both through a one-time link.
//!
//! - **Bridge share** (`hashi share`, untrusted): the person gets a token
//!   on this Hashi and sends prompts here; the login never leaves this
//!   machine. Its limits are enforced, including `stopAtPercent`, which
//!   holds the token back once the owner's account has used that share of
//!   a quota window.
//! - **Login share** (`hashi share-login`, trusted): the link hands over
//!   the key of one of the owner's key accounts (an API key, or a Claude
//!   Code OAuth token from `claude setup-token`), so the person runs the
//!   agent on their own machine. Nothing can be enforced once the key is
//!   theirs, so both sides are only reminded: the token the link also
//!   creates reads the owner's usage, the person's Hashi shows it and warns
//!   at `remindAt` percent, the owner is told the same, and when the share
//!   ends the person's copy is switched off (they can switch it back on).

use std::path::Path;

use serde_json::{Value, json};
use zeroize::Zeroizing;

use crate::accounts::{self as acct, AccountKind, AccountMap};
use crate::agent::AgentKind;
use crate::store::{Invite, LoginShare, NewInvite, Store};
use crate::token::{Limits, SessionMode};

/// Link lifetimes a share may have, in hours.
pub const LINK_HOURS: [u32; 3] = [1, 24, 168];

/// Prompts the token of a login share may have in flight. The token is
/// there to read usage, but a trusted person may ask through it too.
const LOGIN_MAX_PENDING: u32 = 3;

/// A login share to create.
#[derive(Debug, Clone)]
pub struct NewLoginShare {
    /// Who it is for.
    pub label: String,
    /// The agent of the account whose key is handed over.
    pub agent: AgentKind,
    /// The key account handed over.
    pub account: String,
    /// The account whose quota the key spends, read for reminders:
    /// `default` or a login account of the same agent.
    pub usage_from: String,
    /// Remind both sides at this percent of a quota window.
    pub remind_at: Option<u8>,
    /// Days the share lasts, from the moment it is claimed. `None` keeps
    /// it until the owner revokes the token.
    pub days: Option<u32>,
    /// How long the link stays valid, in hours: 1, 24 or 168.
    pub link_hours: u32,
}

/// The key accounts that can be handed over: enabled API-key and OAuth
/// token accounts, as `(agent, name, kind)`.
#[must_use]
pub fn shareable_logins(store: &Store) -> Vec<(AgentKind, String, AccountKind)> {
    store
        .list_accounts()
        .unwrap_or_default()
        .into_iter()
        .filter(|a| a.kind.is_key() && !a.disabled && a.shared_by.is_none())
        .map(|a| (a.agent, a.name, a.kind))
        .collect()
}

/// The accounts of `agent` whose usage Hashi reads: `default` and its
/// login accounts. A login share names one as where its key's quota is.
#[must_use]
pub fn usage_sources(store: &Store, agent: AgentKind) -> Vec<String> {
    let mut out = vec![acct::DEFAULT.to_owned()];
    out.extend(
        store
            .list_accounts()
            .unwrap_or_default()
            .into_iter()
            .filter(|a| a.agent == agent && a.kind == AccountKind::Login && !a.disabled)
            .map(|a| a.name),
    );
    out
}

/// `<agent>:<name>`.
#[must_use]
pub fn account_ref(agent: AgentKind, name: &str) -> String {
    format!("{agent}:{name}")
}

/// Splits `<agent>:<name>`.
///
/// # Errors
///
/// Returns a message for anything else.
pub fn parse_account_ref(s: &str) -> Result<(AgentKind, String), String> {
    let (agent, name) = s
        .split_once(':')
        .ok_or_else(|| format!("`{s}` is not <agent>:<account>, for example claude:bot"))?;
    let agent: AgentKind = agent.trim().parse().map_err(|e| format!("{e}"))?;
    let name = name.trim();
    acct::check_name(name, false)?;
    Ok((agent, name.to_owned()))
}

/// The key of the account `<agent>:<name>`, with its kind, checked to be
/// a key account Hashi can read.
///
/// # Errors
///
/// Returns a message for an unknown account, one that is not a key
/// account, or a key that cannot be read.
pub fn read_login_key(
    store: &Store,
    home: &Path,
    agent: AgentKind,
    name: &str,
) -> Result<(AccountKind, Zeroizing<String>), String> {
    let state = store
        .get_account(agent, name)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("there is no {agent} account named {name}"))?;
    if !state.kind.is_key() {
        return Err(format!(
            "{agent}:{name} is a {} account; only an API key or an OAuth token can be handed \
             over (add one with `hashi account add {agent} <name> --kind oauth-token`)",
            state.kind
        ));
    }
    if state.shared_by.is_some() {
        return Err(format!(
            "{agent}:{name} was shared with you; only its owner can share it"
        ));
    }
    let key = acct::read_key(home, agent, name)
        .map_err(|e| format!("the key of {agent}:{name} could not be read ({})", e.kind()))?;
    Ok((state.kind, Zeroizing::new(key)))
}

fn check_percent(p: Option<u8>, what: &str) -> Result<Option<u8>, String> {
    match p {
        Some(n) if !(1..=100).contains(&n) => Err(format!("{what} must be from 1 to 100")),
        other => Ok(other),
    }
}

/// Creates a login share and returns its invite and one-time code.
///
/// # Errors
///
/// Returns a message for a bad field, an account that cannot be handed
/// over, or a database error.
pub fn create_login_share(
    store: &Store,
    home: &Path,
    new: &NewLoginShare,
    actor: &str,
) -> Result<(Invite, String), String> {
    let label = new.label.trim();
    if label.is_empty() || label.chars().count() > 100 || label.chars().any(char::is_control) {
        return Err("the label must be 1 to 100 printable characters".to_owned());
    }
    read_login_key(store, home, new.agent, &new.account)?;
    let remind_at = check_percent(new.remind_at, "remind at")?;
    if !LINK_HOURS.contains(&new.link_hours) {
        return Err("the link must be valid for 1, 24 or 168 hours".to_owned());
    }
    if new.days.is_some_and(|d| !(1..=3650).contains(&d)) {
        return Err("a share lasts 1 to 3650 days".to_owned());
    }
    if !usage_sources(store, new.agent).contains(&new.usage_from) {
        return Err(format!(
            "usage must come from {}'s default account or one of its login accounts",
            new.agent.display_name()
        ));
    }
    let mut accounts = AccountMap::new();
    if new.usage_from != acct::DEFAULT {
        accounts.insert(new.agent, vec![new.usage_from.clone()]);
    }
    let invite = NewInvite {
        label: label.to_owned(),
        session_mode: SessionMode::Stateless,
        agents: vec![new.agent],
        token_expires_in_days: new.days,
        limits: Limits {
            max_pending: Some(LOGIN_MAX_PENDING),
            ..Limits::default()
        },
        link_hours: new.link_hours,
        accounts,
        project: None,
    };
    let (mut invite, code) = store.create_invite(&invite).map_err(|e| e.to_string())?;
    let login = LoginShare {
        account: account_ref(new.agent, &new.account),
        remind_at,
    };
    let saved = store
        .set_invite_share_usage(&invite.id, true)
        .and_then(|_| store.set_invite_login(&invite.id, &login));
    if let Err(e) = saved {
        let _ = store.cancel_invite(&invite.id);
        return Err(e.to_string());
    }
    invite.share_usage = true;
    invite.login_account = Some(login.account.clone());
    invite.remind_at = remind_at;
    let _ = store.audit(
        actor,
        "invite.create",
        &invite.label,
        &format!(
            "login share of {}, link valid {}h",
            login.account, new.link_hours
        ),
    );
    Ok((invite, code))
}

/// What a claimed login share hands over, as the claim answer's `login`.
#[must_use]
pub fn grant_json(
    agent: AgentKind,
    kind: AccountKind,
    key: &str,
    remind_at: Option<u8>,
    until: Option<i64>,
) -> Value {
    json!({
        "agent": agent,
        "kind": kind,
        "key": key,
        "remindAt": remind_at,
        "until": until.and_then(crate::util::rfc3339),
    })
}

/// A login share as the person's Hashi received it.
#[derive(Clone)]
pub struct LoginGrant {
    /// The agent.
    pub agent: AgentKind,
    /// `api-key` or `oauth-token`.
    pub kind: AccountKind,
    /// The key.
    pub key: Zeroizing<String>,
    /// Remind at this percent of a window.
    pub remind_at: Option<u8>,
    /// When the share ends, Unix ms.
    pub until: Option<i64>,
}

impl std::fmt::Debug for LoginGrant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoginGrant")
            .field("agent", &self.agent)
            .field("kind", &self.kind)
            .field("key", &"<redacted>")
            .field("remind_at", &self.remind_at)
            .field("until", &self.until)
            .finish()
    }
}

impl LoginGrant {
    /// Reads a claim answer's `login`.
    ///
    /// # Errors
    ///
    /// Returns a message when it is missing a usable agent, kind or key.
    pub fn from_json(v: &Value) -> Result<Self, String> {
        let agent: AgentKind = v["agent"]
            .as_str()
            .ok_or("the share names no agent")?
            .parse()
            .map_err(|_| "the share names an agent this Hashi does not know".to_owned())?;
        let kind: AccountKind = v["kind"]
            .as_str()
            .ok_or("the share names no account kind")?
            .parse()?;
        if !kind.is_key() {
            return Err("the share does not carry a key".to_owned());
        }
        let key = acct::clean_key(v["key"].as_str().unwrap_or_default())?;
        let remind_at = v["remindAt"]
            .as_u64()
            .and_then(|n| u8::try_from(n).ok())
            .filter(|n| (1..=100).contains(n));
        let until = v["until"].as_str().and_then(crate::util::parse_rfc3339);
        Ok(Self {
            agent,
            kind,
            key: Zeroizing::new(key),
            remind_at,
            until,
        })
    }
}

/// An account name for a login shared through the remote `remote`, free
/// for `agent`: the remote's name in account form, numbered if taken.
fn free_account_name(store: &Store, agent: AgentKind, remote: &str) -> String {
    let mut base: String = remote
        .chars()
        .map(|c| match c {
            'a'..='z' | '0'..='9' | '-' => c,
            'A'..='Z' => c.to_ascii_lowercase(),
            _ => '-',
        })
        .collect::<String>()
        .trim_matches('-')
        .chars()
        .take(28)
        .collect();
    if acct::check_name(&base, false).is_err() {
        base = "shared".to_owned();
    }
    let taken = |n: &str| store.get_account(agent, n).ok().flatten().is_some();
    if !taken(&base) {
        return base;
    }
    (2..1000)
        .map(|i| format!("{base}-{i}"))
        .find(|n| !taken(n))
        .unwrap_or_else(|| format!("{base}-x"))
}

/// Adds a received login share as a key account named after `remote`,
/// whose usage is read through that remote. Returns the account's name.
///
/// # Errors
///
/// Fails when the account cannot be written or on database errors.
pub fn add_shared_login(
    store: &Store,
    home: &Path,
    grant: &LoginGrant,
    remote: &str,
    actor: &str,
) -> anyhow::Result<String> {
    let name = free_account_name(store, grant.agent, remote);
    let kind = acct::ops::check_new(store, grant.agent, &name, Some(grant.kind), true)?;
    acct::ops::create(
        store,
        home,
        grant.agent,
        &name,
        kind,
        Some(&grant.key),
        actor,
    )?;
    let shared = store.set_account_share(grant.agent, &name, remote, grant.remind_at, grant.until);
    // `create` synced before the share was recorded; a shared login gets
    // no `<name>-claude` command, so its use goes through Hashi.
    acct::ops::sync_commands(store, home);
    shared?;
    Ok(name)
}

/// What the person is told about a shared login: who shared it, the
/// reminder and the end, for `hashi account list`, `usage` and the
/// dashboard.
#[must_use]
pub fn share_note(shared_by: &str, remind_at: Option<u8>, until: Option<i64>, now: i64) -> String {
    let mut s = format!("shared by {shared_by}");
    if let Some(p) = remind_at {
        s.push_str(&format!(", asked to stop at {p}%"));
    }
    match until {
        Some(t) if t <= now => s.push_str(", the share has ended"),
        Some(t) => s.push_str(&format!(", ends in {}", crate::util::until(t, now))),
        None => {}
    }
    s
}

/// Whether a reading moved from under `pct` to at or over it.
#[must_use]
pub fn crossed(before: Option<f64>, after: Option<f64>, pct: u8) -> bool {
    let pct = f64::from(pct);
    after.is_some_and(|a| a >= pct) && before.is_none_or(|b| b < pct)
}

/// Switches off every shared login whose share has ended, with a reason
/// that says how to keep or drop it. Returns what to tell the person.
///
/// # Errors
///
/// Fails on database errors.
pub fn end_expired_shares(
    store: &Store,
    now: i64,
) -> Result<Vec<String>, crate::store::StoreError> {
    let mut out = Vec::new();
    for a in store.list_accounts()? {
        let (Some(by), Some(until)) = (&a.shared_by, a.share_until) else {
            continue;
        };
        if a.disabled || until > now {
            continue;
        }
        let reason = format!(
            "{by}'s share ended on {}; `hashi account enable {} {}` keeps using it, \
             `hashi account remove {} {}` drops it",
            crate::util::rfc3339(until).unwrap_or_default(),
            a.agent,
            a.name,
            a.agent,
            a.name,
        );
        if store.disable_account_because(a.agent, &a.name, &reason)? {
            out.push(format!(
                "{}:{} switched off: {by}'s share ended",
                a.agent, a.name
            ));
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crossing_counts_only_the_way_up() {
        assert!(crossed(Some(60.0), Some(71.0), 70));
        assert!(crossed(None, Some(70.0), 70));
        assert!(!crossed(Some(72.0), Some(75.0), 70));
        assert!(!crossed(Some(60.0), Some(65.0), 70));
        assert!(!crossed(Some(60.0), None, 70));
    }

    #[test]
    fn account_refs_parse() {
        assert_eq!(
            parse_account_ref("claude:bot").unwrap(),
            (AgentKind::Claude, "bot".to_owned())
        );
        assert!(parse_account_ref("claude").is_err());
        assert!(parse_account_ref("nope:bot").is_err());
    }

    #[test]
    fn grants_round_trip_and_hide_the_key_in_debug() {
        let v = grant_json(
            AgentKind::Claude,
            AccountKind::OauthToken,
            "sk-ant-oat01-abc",
            Some(70),
            Some(1_800_000_000_000),
        );
        let g = LoginGrant::from_json(&v).unwrap();
        assert_eq!(g.agent, AgentKind::Claude);
        assert_eq!(g.kind, AccountKind::OauthToken);
        assert_eq!(g.key.as_str(), "sk-ant-oat01-abc");
        assert_eq!(g.remind_at, Some(70));
        assert_eq!(g.until, Some(1_800_000_000_000));
        assert!(!format!("{g:?}").contains("sk-ant"));
        let mut bad = v;
        bad["kind"] = json!("login");
        assert!(LoginGrant::from_json(&bad).is_err());
    }

    #[test]
    fn share_notes_read_plainly() {
        let now = 1_000_000;
        assert_eq!(
            share_note("alice", Some(70), None, now),
            "shared by alice, asked to stop at 70%"
        );
        assert_eq!(
            share_note("alice", None, Some(now - 1), now),
            "shared by alice, the share has ended"
        );
    }
}
