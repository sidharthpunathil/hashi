//! Access requests, the owner's side and the checks on what people send.
//!
//! People ask for access in two ways: a new person through the public
//! ask-access link (off by default, opened with `hashi requests open`), and
//! an existing token asking for more from `/me`, `POST /v1/me/requests` or
//! the MCP tool `request_more`. Nothing is ever granted without the owner:
//! [`accept`] and [`decline`] are the only ways a request is decided, and the
//! HTTP API, the CLI and the dashboard all call them.
//!
//! Accepting a new person's request creates a single-use invite through the
//! same code as `hashi share`, prefilled from the request. The invite's code
//! is derived from the requester's pickup secret, so only they can collect
//! it. Accepting a "more" request changes the token as `token update` does.
//! Either way, the owner's values override the prefill, and nothing is
//! granted beyond what the owner accepts.

use axum::http::StatusCode;
use serde::{Deserialize, Serialize};

use crate::accounts::AccountMap;
use crate::agent::AgentKind;
use crate::api::{ApiError, ApiResult, CreateInviteBody};
use crate::store::{
    AccessAdmission, AccessKind, AccessLink, AccessRequest, Invite, InviteState, LINK_PREFIX,
    MoreAsk, NewAccessRequest, PICKUP_PREFIX, Store,
};
use crate::token::{Limits, SessionMode, Token};
use crate::util::now_ms;

pub use crate::store::{
    MAX_OPEN_PER_TOKEN, MAX_PENDING_PER_LINK, MAX_PENDING_TOTAL, MORE_EVERY_MS, REQUEST_DAYS,
};

/// Hours an ask-access link stays open unless given.
pub const DEFAULT_LINK_HOURS: u32 = 168;
/// Longest an ask-access link may stay open, in hours (30 days).
pub const MAX_LINK_HOURS: u32 = 720;
/// Longest requester name, in characters.
pub const MAX_NAME_CHARS: usize = 60;
/// Longest request message, in characters.
pub const MAX_MESSAGE_CHARS: usize = 500;
/// Longest note on the link, in characters.
pub const MAX_NOTE_CHARS: usize = 200;
/// Longest decline reason, in characters.
pub const MAX_REASON_CHARS: usize = 300;
/// Decided and expired requests are deleted this many days later.
pub const PURGE_DAYS: i64 = 30;
/// How long the invite an accepted request creates stays claimable, in
/// hours, unless the owner says otherwise.
pub const ACCEPTED_LINK_HOURS: u32 = 168;
/// Actor recorded in the audit log for a request sent through the link.
pub const PUBLIC_ACTOR: &str = "ask-access";

// ── cleaning ───────────────────────────────────────────────────────────────

/// Zero-width and other invisible format characters, which can hide text
/// or make two different names look the same.
fn is_invisible(c: char) -> bool {
    matches!(
        c,
        '\u{00ad}' | '\u{180e}' | '\u{200b}'..='\u{200d}' | '\u{2060}'..='\u{2064}' | '\u{feff}'
    )
}

/// `s` without control, bidi or invisible characters, trimmed. With
/// `keep_lines`, newlines stay (at most two in a row); otherwise every
/// whitespace run becomes one space. Used both when a field is stored and
/// when it is shown.
#[must_use]
pub fn clean_text(s: &str, keep_lines: bool) -> String {
    let mut out = String::with_capacity(s.len().min(4096));
    let mut newlines = 0;
    for c in s.chars() {
        let c = match c {
            '\u{2028}' | '\u{2029}' => '\n',
            '\r' => continue,
            c => c,
        };
        if c == '\n' && keep_lines {
            newlines += 1;
            if newlines <= 2 {
                out.push('\n');
            }
            continue;
        }
        if c.is_whitespace() {
            newlines = 0;
            if !out.ends_with([' ', '\n']) {
                out.push(' ');
            }
            continue;
        }
        if c.is_control() || crate::diagnostics::is_bidi_control(c) || is_invisible(c) {
            continue;
        }
        newlines = 0;
        out.push(c);
    }
    out.trim().to_owned()
}

/// A cleaned field of `min` to `max` characters.
fn field(s: &str, what: &str, min: usize, max: usize, keep_lines: bool) -> ApiResult<String> {
    // Refuse absurd input before doing any work on it.
    if s.len() > max * 8 + 64 {
        return Err(ApiError::BadRequest(format!(
            "{what} must be at most {max} characters"
        )));
    }
    let cleaned = clean_text(s, keep_lines);
    let n = cleaned.chars().count();
    if n < min || n > max {
        return Err(ApiError::BadRequest(if min > 0 {
            format!("{what} must be {min} to {max} characters")
        } else {
            format!("{what} must be at most {max} characters")
        }));
    }
    Ok(cleaned)
}

fn optional(
    s: Option<&str>,
    what: &str,
    max: usize,
    keep_lines: bool,
) -> ApiResult<Option<String>> {
    match s {
        None => Ok(None),
        Some(s) => Ok(Some(field(s, what, 0, max, keep_lines)?).filter(|s| !s.is_empty())),
    }
}

fn sorted(mut agents: Vec<AgentKind>) -> Vec<AgentKind> {
    agents.sort_by_key(|a| a.as_str());
    agents.dedup();
    agents
}

// ── the link ───────────────────────────────────────────────────────────────

/// What `hashi requests open` and `POST /v1/request-link` take.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OpenLink {
    /// Project new requests are for. Its agents are the ones people can pick.
    #[serde(default)]
    pub project: Option<String>,
    /// A note shown on the page, up to 200 characters.
    #[serde(default)]
    pub note: Option<String>,
    /// Hours until it closes by itself: 1 to 720, default 168.
    #[serde(default)]
    pub hours: Option<u32>,
}

/// `<base>/ask-access/<code>`.
#[must_use]
pub fn link_url(base: &str, code: &str) -> String {
    format!("{}/ask-access/{code}", base.trim_end_matches('/'))
}

/// Opens the ask-access link, closing any open one, and returns it with its
/// code. The code is shown once; only its hash is kept.
///
/// # Errors
///
/// Fails on invalid input, an unknown project or database errors.
pub fn open_link(store: &Store, actor: &str, input: OpenLink) -> ApiResult<(AccessLink, String)> {
    let hours = input.hours.unwrap_or(DEFAULT_LINK_HOURS);
    if !(1..=MAX_LINK_HOURS).contains(&hours) {
        return Err(ApiError::BadRequest(format!(
            "hours must be 1 to {MAX_LINK_HOURS}"
        )));
    }
    let note = optional(input.note.as_deref(), "note", MAX_NOTE_CHARS, false)?;
    let project = crate::api::check_project_name(input.project)?;
    if let Some(p) = &project
        && store.get_project(p)?.is_none()
    {
        return Err(ApiError::BadRequest(format!(
            "no project named `{}`; add it first with `hashi project add`",
            crate::util::truncate(p, 40)
        )));
    }
    let (link, code) = store.open_access_link(project.as_deref(), note.as_deref(), hours, actor)?;
    store.audit(
        actor,
        "access.link.open",
        link.project.as_deref().unwrap_or(crate::projects::DEFAULT),
        &format!("closes in {hours}h"),
    )?;
    Ok((link, code))
}

/// Closes the ask-access link. Returns whether one was open.
///
/// # Errors
///
/// Fails on database errors.
pub fn close_link(store: &Store, actor: &str) -> ApiResult<bool> {
    let was_open = store.close_access_links()? > 0;
    if was_open {
        store.audit(actor, "access.link.close", "ask-access link", "")?;
    }
    Ok(was_open)
}

/// The most recent link and whether it is still open.
///
/// # Errors
///
/// Fails on database errors.
pub fn current_link(store: &Store) -> ApiResult<Option<AccessLink>> {
    Ok(store.latest_access_link()?)
}

/// Agents people may ask for through `link`: its project's, or all.
///
/// # Errors
///
/// Fails on database errors, or when the project is gone.
pub fn link_agents(store: &Store, link: &AccessLink) -> ApiResult<Vec<AgentKind>> {
    match &link.project {
        None => Ok(AgentKind::ALL.to_vec()),
        Some(name) => match store.get_project(name)? {
            Some(p) => Ok(sorted(p.agents)),
            None => Err(link_closed()),
        },
    }
}

fn link_not_found() -> ApiError {
    ApiError::coded(
        StatusCode::NOT_FOUND,
        "link_not_found",
        "this link is not valid",
    )
}

fn link_closed() -> ApiError {
    ApiError::coded(
        StatusCode::GONE,
        "link_closed",
        "this link is closed; ask the owner for a new one",
    )
}

/// The open link a code belongs to.
///
/// # Errors
///
/// `link_not_found` (404) for an unknown code, `link_closed` (410) for a
/// closed or expired link.
pub fn open_link_for(store: &Store, code: &str) -> ApiResult<AccessLink> {
    if !code.starts_with(LINK_PREFIX) || code.len() > 128 {
        return Err(link_not_found());
    }
    let link = store.find_access_link(code)?.ok_or_else(link_not_found)?;
    if link.status(now_ms()) != "open" {
        return Err(link_closed());
    }
    Ok(link)
}

// ── asking ─────────────────────────────────────────────────────────────────

/// A new person's request, as the page and `hashi request` send it.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NewPerson {
    /// The link code.
    pub code: String,
    /// Their name, 1 to 60 characters.
    pub name: String,
    /// What they want it for, up to 500 characters.
    #[serde(default)]
    pub message: Option<String>,
    /// Agents they want, from the link's. Default: all of them.
    #[serde(default)]
    pub agents: Option<Vec<AgentKind>>,
}

fn full(code: &'static str, message: &'static str) -> ApiError {
    ApiError::coded(StatusCode::CONFLICT, code, message)
}

/// Stores a new person's request and returns it with its pickup secret,
/// which is shown once.
///
/// # Errors
///
/// Fails for a bad or closed link, invalid fields, a full link or server,
/// or on database errors.
pub fn submit_new(store: &Store, input: NewPerson) -> ApiResult<(AccessRequest, String)> {
    let link = open_link_for(store, &input.code)?;
    let allowed = link_agents(store, &link)?;
    let name = field(&input.name, "name", 1, MAX_NAME_CHARS, false)?;
    let message =
        optional(input.message.as_deref(), "message", MAX_MESSAGE_CHARS, true)?.unwrap_or_default();
    let agents = match input.agents {
        None => allowed,
        Some(a) if a.is_empty() => allowed,
        Some(a) => {
            let a = sorted(a);
            if let Some(bad) = a.iter().find(|x| !allowed.contains(x)) {
                return Err(ApiError::BadRequest(format!(
                    "{bad} is not offered here; pick from {}",
                    crate::token::encode_agents(&allowed)
                )));
            }
            a
        }
    };
    let new = NewAccessRequest {
        kind: AccessKind::New,
        link_id: Some(link.id.clone()),
        token_id: None,
        name,
        message,
        agents,
        project: link.project,
        asked: MoreAsk::default(),
    };
    match store.insert_access_request(&new)? {
        AccessAdmission::Created(req, Some(pickup)) => {
            store.audit(
                PUBLIC_ACTOR,
                "access.request",
                &req.name,
                &format!(
                    "new person, wants {}",
                    crate::token::encode_agents(&req.agents)
                ),
            )?;
            Ok((*req, pickup))
        }
        AccessAdmission::LinkFull => Err(full(
            "link_full",
            "this link has too many requests waiting; try again later",
        )),
        AccessAdmission::TotalFull | AccessAdmission::Created(_, None) => Err(full(
            "requests_full",
            "this Hashi has too many requests waiting; try again later",
        )),
        AccessAdmission::TokenBusy | AccessAdmission::TooSoon => Err(full(
            "requests_full",
            "this Hashi cannot take a request right now",
        )),
    }
}

/// A token's request for more, as `/me`, `POST /v1/me/requests` and the
/// MCP tool `request_more` send it. Every field is optional, but at least
/// one must say something.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MoreInput {
    /// Prompts per UTC day wanted.
    #[serde(default)]
    pub daily_requests: Option<u32>,
    /// Prompts per UTC month wanted.
    #[serde(default)]
    pub monthly_requests: Option<u32>,
    /// Spend per UTC day wanted, in USD.
    #[serde(default)]
    pub daily_budget_usd: Option<f64>,
    /// Spend per UTC month wanted, in USD.
    #[serde(default)]
    pub monthly_budget_usd: Option<f64>,
    /// Agents wanted on top of the current ones.
    #[serde(default)]
    pub agents: Option<Vec<AgentKind>>,
    /// A new lifetime, in days from acceptance (1 to 3650).
    #[serde(default)]
    pub expires_in_days: Option<u32>,
    /// Anything else, up to 500 characters.
    #[serde(default)]
    pub message: Option<String>,
}

/// Stores a token's request for more.
///
/// # Errors
///
/// Fails for invalid fields, an admin token, too many open requests, a
/// second request within the hour, or on database errors.
pub fn submit_more(store: &Store, token: &Token, input: MoreInput) -> ApiResult<AccessRequest> {
    if token.is_admin() {
        return Err(ApiError::BadRequest(
            "admin tokens change their own access; there is nobody to ask".to_owned(),
        ));
    }
    let asked = MoreAsk {
        daily_requests: input.daily_requests,
        monthly_requests: input.monthly_requests,
        daily_budget_usd: input.daily_budget_usd,
        monthly_budget_usd: input.monthly_budget_usd,
        expires_in_days: input.expires_in_days,
    };
    if [asked.daily_budget_usd, asked.monthly_budget_usd]
        .into_iter()
        .flatten()
        .any(|b| !b.is_finite() || b < 0.0)
    {
        return Err(ApiError::BadRequest(
            "budgets must be zero or more".to_owned(),
        ));
    }
    if asked.expires_in_days.is_some_and(|d| d == 0 || d > 3_650) {
        return Err(ApiError::BadRequest(
            "expiresInDays must be between 1 and 3650".to_owned(),
        ));
    }
    let agents: Vec<AgentKind> = sorted(input.agents.unwrap_or_default())
        .into_iter()
        .filter(|a| !token.agents.contains(a))
        .collect();
    let message =
        optional(input.message.as_deref(), "message", MAX_MESSAGE_CHARS, true)?.unwrap_or_default();
    if asked.is_empty() && agents.is_empty() && message.is_empty() {
        return Err(ApiError::BadRequest(
            "say what you need: higher limits, more agents, a later expiry or a message".to_owned(),
        ));
    }
    let new = NewAccessRequest {
        kind: AccessKind::More,
        link_id: None,
        token_id: Some(token.id.clone()),
        name: clean_text(&token.name, false),
        message,
        agents,
        project: token.project.clone(),
        asked,
    };
    match store.insert_access_request(&new)? {
        AccessAdmission::Created(req, _) => {
            store.audit(
                &token.name,
                "access.request",
                &token.name,
                &asked_text(&req),
            )?;
            Ok(*req)
        }
        AccessAdmission::TokenBusy => Err(ApiError::coded(
            StatusCode::CONFLICT,
            "too_many_open_requests",
            format!(
                "you already have {MAX_OPEN_PER_TOKEN} open requests; wait for the owner to answer"
            ),
        )),
        AccessAdmission::TooSoon => Err(ApiError::coded(
            StatusCode::TOO_MANY_REQUESTS,
            "request_too_soon",
            "you can send one request an hour; try again later",
        )),
        AccessAdmission::LinkFull | AccessAdmission::TotalFull => Err(full(
            "requests_full",
            "this Hashi has too many requests waiting; try again later",
        )),
    }
}

/// A short description of what a request asks for, for the audit log and
/// lists.
#[must_use]
pub fn asked_text(r: &AccessRequest) -> String {
    let mut parts = Vec::new();
    if r.kind == AccessKind::New {
        parts.push(format!(
            "new person, wants {}",
            crate::token::encode_agents(&r.agents)
        ));
    } else {
        let a = r.asked;
        if let Some(n) = a.daily_requests {
            parts.push(format!("{n} prompts/day"));
        }
        if let Some(n) = a.monthly_requests {
            parts.push(format!("{n} prompts/month"));
        }
        if let Some(n) = a.daily_budget_usd {
            parts.push(format!("${n}/day"));
        }
        if let Some(n) = a.monthly_budget_usd {
            parts.push(format!("${n}/month"));
        }
        if !r.agents.is_empty() {
            parts.push(format!("+{}", crate::token::encode_agents(&r.agents)));
        }
        if let Some(d) = a.expires_in_days {
            parts.push(format!("expiry {d}d"));
        }
        if parts.is_empty() {
            parts.push("see message".to_owned());
        }
        parts.insert(0, "more:".to_owned());
    }
    parts.join(" ")
}

/// What a pickup secret shows.
#[derive(Debug, Clone, PartialEq)]
pub struct PickupStatus {
    /// `pending`, `accepted` (with a claim code), `claimed`, `declined` or
    /// `expired`.
    pub state: &'static str,
    /// The request.
    pub request: AccessRequest,
    /// The single-use claim code, while it can still be claimed.
    pub claim_code: Option<String>,
}

/// Looks a pickup secret up.
///
/// # Errors
///
/// `request_not_found` (404) for an unknown secret, or database errors.
pub fn pickup(store: &Store, secret: &str) -> ApiResult<PickupStatus> {
    let not_found = || {
        ApiError::coded(
            StatusCode::NOT_FOUND,
            "request_not_found",
            "no request matches this pickup secret",
        )
    };
    if !secret.starts_with(PICKUP_PREFIX) || secret.len() > 128 {
        return Err(not_found());
    }
    let p = store.pickup_access_request(secret)?.ok_or_else(not_found)?;
    let now = now_ms();
    let (state, claim_code) = match (p.request.state(now), p.claim_code) {
        ("accepted", Some(code)) => match store.preview_invite(&code)? {
            InviteState::Pending(_) => ("accepted", Some(code)),
            InviteState::Claimed => ("claimed", None),
            InviteState::Expired | InviteState::NotFound => ("expired", None),
        },
        ("accepted", None) => ("expired", None),
        (s, _) => (
            match s {
                "declined" => "declined",
                "expired" => "expired",
                _ => "pending",
            },
            None,
        ),
    };
    Ok(PickupStatus {
        state,
        request: p.request,
        claim_code,
    })
}

// ── deciding ───────────────────────────────────────────────────────────────

/// Every request, newest first.
///
/// # Errors
///
/// Fails on database errors.
pub fn list(store: &Store) -> ApiResult<Vec<AccessRequest>> {
    Ok(store.list_access_requests()?)
}

/// Requests waiting for the owner, newest first. Its length is the
/// dashboard's badge count.
///
/// # Errors
///
/// Fails on database errors.
pub fn pending(store: &Store) -> ApiResult<Vec<AccessRequest>> {
    let now = now_ms();
    Ok(store
        .list_access_requests()?
        .into_iter()
        .filter(|r| r.is_pending(now))
        .collect())
}

/// A token's access now, shown next to what it asked for.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Current {
    /// Its limits.
    pub limits: Limits,
    /// Its agents.
    pub agents: Vec<AgentKind>,
    /// When it expires, Unix ms.
    pub expires_at: Option<i64>,
}

/// The values the accept form starts from.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Prefill {
    /// Token name (new people).
    pub label: String,
    /// Agents: the wanted ones the link's project allows (new), or the
    /// current ones plus the asked ones (more).
    pub agents: Vec<AgentKind>,
    /// Project.
    pub project: Option<String>,
    /// Limits: invite defaults (new), or current limits with the asked ones
    /// over them (more).
    pub limits: Limits,
    /// Token lifetime in days.
    pub expires_in_days: Option<u32>,
    /// The token's current access (more).
    pub current: Option<Current>,
}

/// The accept form's starting values for `r`.
///
/// # Errors
///
/// Fails when a "more" request's token is gone, or on database errors.
pub fn prefill(store: &Store, r: &AccessRequest) -> ApiResult<Prefill> {
    match r.kind {
        AccessKind::New => {
            let allowed = match &r.project {
                Some(name) => store.get_project(name)?.map_or_else(Vec::new, |p| p.agents),
                None => AgentKind::ALL.to_vec(),
            };
            let mut agents: Vec<AgentKind> = r
                .agents
                .iter()
                .copied()
                .filter(|a| allowed.contains(a))
                .collect();
            if agents.is_empty() {
                agents = allowed;
            }
            Ok(Prefill {
                label: r.name.clone(),
                agents: sorted(agents),
                project: r.project.clone(),
                limits: Limits {
                    max_pending: Some(crate::store::DEFAULT_MAX_PENDING),
                    ..Limits::default()
                },
                expires_in_days: None,
                current: None,
            })
        }
        AccessKind::More => {
            let token = token_of(store, r)?;
            let a = r.asked;
            let limits = Limits {
                daily_requests: a.daily_requests.or(token.limits.daily_requests),
                monthly_requests: a.monthly_requests.or(token.limits.monthly_requests),
                daily_budget_usd: a.daily_budget_usd.or(token.limits.daily_budget_usd),
                monthly_budget_usd: a.monthly_budget_usd.or(token.limits.monthly_budget_usd),
                max_pending: token.limits.max_pending,
            };
            let mut agents = token.agents.clone();
            agents.extend(r.agents.iter().copied());
            Ok(Prefill {
                label: token.name.clone(),
                agents: sorted(agents),
                project: token.project.clone(),
                limits,
                expires_in_days: a.expires_in_days,
                current: Some(Current {
                    limits: token.limits,
                    agents: token.agents.clone(),
                    expires_at: token.expires_at,
                }),
            })
        }
    }
}

fn token_of(store: &Store, r: &AccessRequest) -> ApiResult<Token> {
    let id = r
        .token_id
        .as_deref()
        .ok_or(ApiError::NotFound("the token that asked no longer exists"))?;
    store
        .get_token(id)?
        .ok_or(ApiError::NotFound("the token that asked no longer exists"))
}

/// The owner's values when accepting. Every field is optional: a field
/// left out takes the prefilled value (see [`prefill`]). `label`,
/// `sessionMode`, `linkHours`, `accounts` and `project` apply to new people
/// only.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AcceptInput {
    /// Token name.
    #[serde(default)]
    pub label: Option<String>,
    /// `stateless` (default) or `stateful`.
    #[serde(default)]
    pub session_mode: Option<SessionMode>,
    /// Agents, replacing the prefilled ones.
    #[serde(default)]
    pub agents: Option<Vec<AgentKind>>,
    /// Token lifetime in days. For a "more" request, `0` removes the expiry.
    #[serde(default)]
    pub expires_in_days: Option<u32>,
    /// Limits. Each field given replaces the prefilled one.
    #[serde(default)]
    pub limits: Option<Limits>,
    /// How long the invite stays claimable: 1, 24 or 168 (default) hours.
    #[serde(default)]
    pub link_hours: Option<u32>,
    /// Accounts per agent, as on `POST /v1/invites`.
    #[serde(default)]
    pub accounts: Option<AccountMap>,
    /// Project, replacing the link's; an empty string means none.
    #[serde(default)]
    pub project: Option<String>,
}

/// What accepting did.
#[derive(Debug, Clone)]
pub enum Accepted {
    /// A new person: their single-use invite, which only their pickup
    /// secret can collect.
    Invite {
        /// The request, now accepted.
        request: Box<AccessRequest>,
        /// The invite created for it.
        invite: Box<Invite>,
    },
    /// More for a token: the token as saved, and what changed.
    Token {
        /// The request, now accepted.
        request: Box<AccessRequest>,
        /// The token after the change.
        token: Box<Token>,
        /// What changed, as in the audit log.
        changes: String,
    },
}

fn not_pending(state: &str) -> ApiError {
    ApiError::coded(
        StatusCode::CONFLICT,
        "not_pending",
        format!("this request is {state}, not pending"),
    )
}

fn pending_request(store: &Store, id: &str) -> ApiResult<AccessRequest> {
    let r = store
        .get_access_request(id)?
        .ok_or(ApiError::NotFound("no access request with that id"))?;
    let state = r.state(now_ms());
    if state != "pending" {
        return Err(not_pending(state));
    }
    Ok(r)
}

/// Accepts a pending request with the owner's values over the prefill.
///
/// # Errors
///
/// Fails for an unknown or decided request, invalid values, a binding the
/// project does not allow, too many pending invites, or database errors.
pub fn accept(store: &Store, actor: &str, id: &str, input: AcceptInput) -> ApiResult<Accepted> {
    let r = pending_request(store, id)?;
    match r.kind {
        AccessKind::New => accept_new(store, actor, r, input),
        AccessKind::More => accept_more(store, actor, r, input),
    }
}

fn accept_new(
    store: &Store,
    actor: &str,
    r: AccessRequest,
    input: AcceptInput,
) -> ApiResult<Accepted> {
    let pre = prefill(store, &r)?;
    let mut new = crate::api::validate_new_invite(CreateInviteBody {
        label: input.label.unwrap_or(pre.label),
        session_mode: input.session_mode,
        agents: Some(input.agents.unwrap_or(pre.agents)),
        expires_in_days: input.expires_in_days,
        limits: input.limits,
        link_hours: Some(input.link_hours.unwrap_or(ACCEPTED_LINK_HOURS)),
        accounts: input.accounts,
        project: input.project.or(pre.project),
    })?;
    new.accounts = crate::api::check_accounts(store, new.accounts)?;
    crate::api::check_binding(store, new.project.as_deref(), &new.agents, &new.accounts)?;
    let now = now_ms();
    let pending = store
        .list_invites()?
        .iter()
        .filter(|i| i.status(now) == "pending")
        .count();
    if pending >= crate::api::MAX_PENDING_INVITES {
        return Err(ApiError::Conflict(
            "too many pending invites, cancel some first",
        ));
    }
    let hash = store
        .access_claim_hash(&r.id)?
        .ok_or(ApiError::Conflict("this request cannot be accepted"))?;
    let invite = store.create_invite_with_hash(&new, &hash)?;
    if !store.decide_access_request(&r.id, true, None, Some(&invite.id), actor)? {
        store.cancel_invite(&invite.id)?;
        return Err(not_pending("no longer pending"));
    }
    store.audit(
        actor,
        "access.accept",
        &invite.label,
        &format!("new person, invite valid {}h", new.link_hours),
    )?;
    let request = store.get_access_request(&r.id)?.unwrap_or(r);
    Ok(Accepted::Invite {
        request: Box::new(request),
        invite: Box::new(invite),
    })
}

fn accept_more(
    store: &Store,
    actor: &str,
    r: AccessRequest,
    input: AcceptInput,
) -> ApiResult<Accepted> {
    if input.label.is_some()
        || input.session_mode.is_some()
        || input.link_hours.is_some()
        || input.accounts.is_some()
        || input.project.is_some()
    {
        return Err(ApiError::BadRequest(
            "label, sessionMode, linkHours, accounts and project apply to new people only; \
             use token update for the rest"
                .to_owned(),
        ));
    }
    let pre = prefill(store, &r)?;
    let mut token = token_of(store, &r)?;
    let before = token.clone();
    let over = input.limits.unwrap_or_default();
    token.limits = crate::api::validate_limits(Limits {
        daily_requests: over.daily_requests.or(pre.limits.daily_requests),
        monthly_requests: over.monthly_requests.or(pre.limits.monthly_requests),
        daily_budget_usd: over.daily_budget_usd.or(pre.limits.daily_budget_usd),
        monthly_budget_usd: over.monthly_budget_usd.or(pre.limits.monthly_budget_usd),
        max_pending: over.max_pending.or(pre.limits.max_pending),
    })?;
    token.agents = sorted(input.agents.unwrap_or(pre.agents));
    if token.agents.is_empty() {
        return Err(ApiError::BadRequest(
            "agents must name at least one agent".to_owned(),
        ));
    }
    let mut changes = Vec::new();
    if let Some(days) = input.expires_in_days.or(pre.expires_in_days) {
        if days > 3_650 {
            return Err(ApiError::BadRequest(
                "expiresInDays must be between 0 and 3650".to_owned(),
            ));
        }
        token.expires_at = (days > 0).then(|| now_ms() + i64::from(days) * 86_400_000);
        changes.push(if days == 0 {
            "expiry removed".to_owned()
        } else {
            format!("expires in {days}d")
        });
    }
    if token.limits != before.limits {
        changes.push("limits".to_owned());
    }
    if token.agents != sorted(before.agents) {
        changes.push(format!(
            "agents {}",
            crate::token::encode_agents(&token.agents)
        ));
    }
    crate::api::check_binding(
        store,
        token.project.as_deref(),
        &token.agents,
        &token.accounts,
    )?;
    if !store.decide_access_request(&r.id, true, None, None, actor)? {
        return Err(not_pending("no longer pending"));
    }
    store.save_token_policy(&token)?;
    let changes = if changes.is_empty() {
        "no change".to_owned()
    } else {
        changes.join(", ")
    };
    store.audit(
        actor,
        "access.accept",
        &token.name,
        &format!("more: {changes}"),
    )?;
    let request = store.get_access_request(&r.id)?.unwrap_or(r);
    Ok(Accepted::Token {
        request: Box::new(request),
        token: Box::new(token),
        changes,
    })
}

/// Declines a pending request, with an optional reason the requester sees.
///
/// # Errors
///
/// Fails for an unknown or decided request, a reason that is too long, or
/// database errors.
pub fn decline(
    store: &Store,
    actor: &str,
    id: &str,
    reason: Option<&str>,
) -> ApiResult<AccessRequest> {
    let r = pending_request(store, id)?;
    let reason = optional(reason, "reason", MAX_REASON_CHARS, true)?;
    if !store.decide_access_request(&r.id, false, reason.as_deref(), None, actor)? {
        return Err(not_pending("no longer pending"));
    }
    store.audit(
        actor,
        "access.decline",
        &r.name,
        &reason
            .as_deref()
            .map(|s| crate::util::truncate(&crate::util::one_line(s), 80))
            .unwrap_or_default(),
    )?;
    Ok(store.get_access_request(&r.id)?.unwrap_or(r))
}

/// Deletes requests decided or expired more than [`PURGE_DAYS`] ago, and
/// links nothing refers to any more. Called by the hourly maintenance.
///
/// # Errors
///
/// Fails on database errors.
pub fn purge(store: &Store) -> Result<usize, crate::store::StoreError> {
    store.prune_access(now_ms() - PURGE_DAYS * 86_400_000)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::NewToken;
    use crate::token::Role;

    #[test]
    fn clean_text_strips_controls_bidi_and_invisibles() {
        assert_eq!(
            clean_text("  al\u{202e}ice\u{200b}\u{1b}[2J \u{7}bob  ", false),
            "alice[2J bob"
        );
        assert_eq!(clean_text("a\r\nb\n\n\n\nc\td", true), "a\nb\n\nc d");
        assert_eq!(clean_text("a\nb", false), "a b");
        assert_eq!(clean_text("\u{2066}\u{2069}\u{feff}", false), "");
        assert_eq!(
            clean_text("caf\u{e9} \u{1f600}", false),
            "caf\u{e9} \u{1f600}"
        );
    }

    #[test]
    fn fields_are_length_limited_after_cleaning() {
        assert!(field("", "name", 1, 60, false).is_err());
        assert!(field("\u{202e}\u{200b}", "name", 1, 60, false).is_err());
        assert!(field(&"a".repeat(61), "name", 1, 60, false).is_err());
        assert_eq!(
            field(&"é".repeat(60), "name", 1, 60, false)
                .unwrap()
                .chars()
                .count(),
            60
        );
        assert!(field(&"x".repeat(100_000), "message", 0, 500, true).is_err());
    }

    fn user(store: &Store) -> Token {
        store
            .create_token(&NewToken {
                name: "bob".into(),
                role: Role::User,
                session_mode: SessionMode::Stateless,
                agents: vec![AgentKind::Claude],
                expires_in_days: None,
                note: None,
                limits: Limits {
                    daily_requests: Some(10),
                    max_pending: Some(3),
                    ..Limits::default()
                },
                accounts: AccountMap::new(),
                project: None,
            })
            .unwrap()
            .0
    }

    #[test]
    fn new_person_flow_creates_an_invite_only_the_requester_can_collect() {
        let s = Store::open_in_memory().unwrap();
        let (_, code) = open_link(&s, "cli", OpenLink::default()).unwrap();
        let (req, secret) = submit_new(
            &s,
            NewPerson {
                code,
                name: "Al\u{202e}ice".into(),
                message: Some("hi".into()),
                agents: Some(vec![AgentKind::Codex]),
            },
        )
        .unwrap();
        assert_eq!(req.name, "Alice");
        assert_eq!(pickup(&s, &secret).unwrap().state, "pending");
        let Accepted::Invite { invite, .. } = accept(
            &s,
            "cli",
            &req.id,
            AcceptInput {
                limits: Some(Limits {
                    daily_requests: Some(5),
                    ..Limits::default()
                }),
                ..AcceptInput::default()
            },
        )
        .unwrap() else {
            panic!("expected an invite");
        };
        assert_eq!(invite.label, "Alice");
        assert_eq!(invite.agents, vec![AgentKind::Codex]);
        assert_eq!(invite.limits.daily_requests, Some(5));
        assert_eq!(invite.limits.max_pending, Some(3));
        let p = pickup(&s, &secret).unwrap();
        assert_eq!(p.state, "accepted");
        let claim = p.claim_code.unwrap();
        assert!(matches!(
            s.redeem_invite(&claim).unwrap(),
            crate::store::Redeemed::Token(..)
        ));
        assert_eq!(pickup(&s, &secret).unwrap().state, "claimed");
        assert!(accept(&s, "cli", &req.id, AcceptInput::default()).is_err());
    }

    #[test]
    fn more_requests_apply_the_owners_values_over_the_asked_ones() {
        let s = Store::open_in_memory().unwrap();
        let t = user(&s);
        let r = submit_more(
            &s,
            &t,
            MoreInput {
                daily_requests: Some(100),
                monthly_requests: Some(1000),
                agents: Some(vec![AgentKind::Codex, AgentKind::Claude]),
                ..MoreInput::default()
            },
        )
        .unwrap();
        // Claude is already allowed, so only Codex is asked for.
        assert_eq!(r.agents, vec![AgentKind::Codex]);
        let pre = prefill(&s, &r).unwrap();
        assert_eq!(pre.limits.daily_requests, Some(100));
        assert_eq!(pre.current.unwrap().limits.daily_requests, Some(10));
        let Accepted::Token { token, .. } = accept(
            &s,
            "cli",
            &r.id,
            AcceptInput {
                limits: Some(Limits {
                    daily_requests: Some(50),
                    ..Limits::default()
                }),
                agents: Some(vec![AgentKind::Claude]),
                ..AcceptInput::default()
            },
        )
        .unwrap() else {
            panic!("expected a token");
        };
        assert_eq!(token.limits.daily_requests, Some(50));
        assert_eq!(token.limits.monthly_requests, Some(1000));
        assert_eq!(token.agents, vec![AgentKind::Claude]);
        let saved = s.get_token(&t.id).unwrap().unwrap();
        assert_eq!(saved.limits, token.limits);
    }

    #[test]
    fn more_requests_are_capped_per_token() {
        let s = Store::open_in_memory().unwrap();
        let t = user(&s);
        let ask = || MoreInput {
            message: Some("please".into()),
            ..MoreInput::default()
        };
        assert!(submit_more(&s, &t, MoreInput::default()).is_err());
        submit_more(&s, &t, ask()).unwrap();
        let e = submit_more(&s, &t, ask()).unwrap_err();
        assert!(e.to_string().contains("one request an hour"), "{e}");
    }

    #[test]
    fn decline_keeps_a_cleaned_reason() {
        let s = Store::open_in_memory().unwrap();
        let (_, code) = open_link(&s, "cli", OpenLink::default()).unwrap();
        let (req, secret) = submit_new(
            &s,
            NewPerson {
                code,
                name: "x".into(),
                message: None,
                agents: None,
            },
        )
        .unwrap();
        let r = decline(&s, "cli", &req.id, Some("not now\u{1b}[31m")).unwrap();
        assert_eq!(r.reason.as_deref(), Some("not now[31m"));
        let p = pickup(&s, &secret).unwrap();
        assert_eq!((p.state, p.claim_code), ("declined", None));
    }

    #[test]
    fn closed_links_take_no_requests() {
        let s = Store::open_in_memory().unwrap();
        let (_, code) = open_link(&s, "cli", OpenLink::default()).unwrap();
        assert!(close_link(&s, "cli").unwrap());
        let e = submit_new(
            &s,
            NewPerson {
                code,
                name: "x".into(),
                message: None,
                agents: None,
            },
        )
        .unwrap_err();
        assert!(e.to_string().contains("closed"), "{e}");
        assert!(
            open_link(
                &s,
                "cli",
                OpenLink {
                    hours: Some(MAX_LINK_HOURS + 1),
                    ..OpenLink::default()
                }
            )
            .is_err()
        );
    }
}
