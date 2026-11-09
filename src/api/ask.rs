//! Prompts: one submit path shared by HTTP and MCP, results, and `/v1/me`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::broadcast::error::RecvError;

use super::{AnyToken, ApiError, ApiResult, AppState, Auth, MAX_MESSAGE_BYTES, MAX_WAIT_SECS};
use crate::agent::{self, AgentKind, Usage};
use crate::events::Event;
use crate::store::{Admission, Cap, NewRequest, Request, RequestStatus, Store, admin_reserve};
use crate::token::{Token, TokenStatus};
use crate::util::{next_day_utc, next_month_utc, now_ms, rfc3339};

/// Body of `POST /v1/ask`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AskBody {
    /// The prompt.
    pub message: String,
    /// Target agent. Defaults to the server's default agent.
    #[serde(default)]
    pub agent: Option<String>,
    /// Model override, passed through to the agent.
    #[serde(default)]
    pub model: Option<String>,
    /// Seconds to wait for the result before answering 202.
    #[serde(default)]
    pub wait: Option<u64>,
}

/// Query string of `GET /v1/ask/{id}`.
#[derive(Debug, Deserialize)]
pub struct WaitQuery {
    /// Seconds to long-poll for completion.
    #[serde(default)]
    pub wait: Option<u64>,
}

/// Long polls one token may hold open at once, across `POST /v1/ask`,
/// `GET /v1/ask/{id}` and the MCP tools. Further waits answer at once.
pub const MAX_WAITS_PER_TOKEN: usize = 4;

/// Counts the long polls each token holds open.
#[derive(Debug, Default)]
pub struct WaitSlots {
    held: Mutex<HashMap<String, usize>>,
}

/// One held long poll. Dropping it frees the slot.
#[derive(Debug)]
pub struct WaitSlot {
    slots: Arc<WaitSlots>,
    token_id: String,
}

impl WaitSlots {
    /// Takes a slot for `token_id`, or `None` when it already holds
    /// [`MAX_WAITS_PER_TOKEN`].
    #[must_use]
    pub fn try_acquire(self: &Arc<Self>, token_id: &str) -> Option<WaitSlot> {
        let mut held = self.held.lock().unwrap_or_else(PoisonError::into_inner);
        let n = held.entry(token_id.to_owned()).or_insert(0);
        if *n >= MAX_WAITS_PER_TOKEN {
            return None;
        }
        *n += 1;
        Some(WaitSlot {
            slots: Arc::clone(self),
            token_id: token_id.to_owned(),
        })
    }

    /// Long polls `token_id` holds right now.
    #[must_use]
    pub fn held(&self, token_id: &str) -> usize {
        self.held
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(token_id)
            .copied()
            .unwrap_or(0)
    }
}

impl Drop for WaitSlot {
    fn drop(&mut self) {
        let mut held = self
            .slots
            .held
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(n) = held.get_mut(&self.token_id) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                held.remove(&self.token_id);
            }
        }
    }
}

/// A prompt to submit, from any front end.
#[derive(Debug, Clone, Default)]
pub struct SubmitInput {
    /// The prompt.
    pub message: String,
    /// Agent name; `None` uses the server default.
    pub agent: Option<String>,
    /// Model override.
    pub model: Option<String>,
    /// Run on this account of the agent only, instead of the token's list.
    /// Admin tokens only; the dashboard's prompt popup sets it.
    pub account: Option<String>,
    /// How many bridges already forwarded it (`X-Hashi-Hops`), 0 for a
    /// prompt sent straight here.
    pub hops: u8,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RequestView {
    id: String,
    agent: AgentKind,
    status: RequestStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    position: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    usage: Option<Usage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    duration_ms: Option<i64>,
    /// The account that ran it, for admins only.
    #[serde(skip_serializing_if = "Option::is_none")]
    account: Option<String>,
    /// The project it ran in, by name.
    #[serde(skip_serializing_if = "Option::is_none")]
    project: Option<String>,
    created_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    completed_at: Option<String>,
    poll: String,
}

/// Renders a request as API JSON, with 200 when finished and 202 otherwise.
/// The account that ran it is included only when `admin` is set: token
/// holders never see account names.
///
/// # Errors
///
/// Fails on database errors.
pub fn request_json(store: &Store, r: Request, admin: bool) -> ApiResult<(StatusCode, Value)> {
    let position = (r.status == RequestStatus::Queued)
        .then(|| store.queue_position(&r))
        .transpose()?;
    let code = if r.status.is_terminal() {
        StatusCode::OK
    } else {
        StatusCode::ACCEPTED
    };
    let view = RequestView {
        poll: format!("/v1/ask/{}", r.id),
        duration_ms: r.duration_ms(),
        account: if admin { r.account } else { None },
        project: r.project,
        created_at: rfc3339(r.created_at),
        completed_at: r.completed_at.and_then(rfc3339),
        id: r.id,
        agent: r.agent,
        status: r.status,
        model: r.model,
        position,
        response: r.response,
        error: r.error,
        usage: r.usage,
    };
    Ok((code, serde_json::to_value(view).unwrap_or(Value::Null)))
}

/// "1 prompt" or "N prompts".
fn prompts(n: impl Into<u64>) -> String {
    match n.into() {
        1 => "1 prompt".to_owned(),
        n => format!("{n} prompts"),
    }
}

/// `429 limit_reached` for a cap that resets at `reset_at`: the body names
/// the `limit`, its `resetAt` and the seconds until then as `retryAfter`,
/// which the response also sends as `Retry-After`.
fn limit_error(limit: &'static str, message: String, reset_at: i64, now: i64) -> ApiError {
    let wait = u64::try_from(reset_at.saturating_sub(now)).unwrap_or(0);
    let retry_after = super::retry_secs(Duration::from_millis(wait));
    let reset = rfc3339(reset_at);
    let message = match &reset {
        Some(at) => format!("{message}; it resets at {at}"),
        None => message,
    };
    ApiError::Coded {
        status: StatusCode::TOO_MANY_REQUESTS,
        code: "limit_reached",
        message,
        extra: Some(json!({ "limit": limit, "resetAt": reset, "retryAfter": retry_after })),
    }
}

/// `429 limit_reached` for a token past its stop on the owner's quota.
/// The message already says when it opens again.
fn stop_error(message: String, opens_at: i64, now: i64) -> ApiError {
    let wait = u64::try_from(opens_at.saturating_sub(now)).unwrap_or(0);
    let retry_after = super::retry_secs(Duration::from_millis(wait));
    ApiError::Coded {
        status: StatusCode::TOO_MANY_REQUESTS,
        code: "limit_reached",
        message,
        extra: Some(json!({
            "limit": "stopAtPercent",
            "resetAt": rfc3339(opens_at),
            "retryAfter": retry_after,
        })),
    }
}

/// Whether a run on `agent` for this token could go to an enabled remote
/// account, which runs on a friend's bridge, so the agent need not be
/// installed here.
fn has_remote_account(
    state: &AppState,
    accounts: &crate::accounts::AccountMap,
    agent: AgentKind,
) -> bool {
    accounts.get(&agent).is_some_and(|list| {
        list.iter().any(|n| {
            state
                .store
                .get_account(agent, n)
                .ok()
                .flatten()
                .is_some_and(|a| {
                    matches!(
                        a.kind,
                        crate::accounts::AccountKind::Remote | crate::accounts::AccountKind::Pool
                    ) && !a.disabled
                })
        })
    })
}

/// The agents `token` may use that this server can run for it now:
/// installed here (and, for Crush, enabled), or served by a remote
/// account. `GET /v1/models` and the MCP `ask_<agent>` tools offer these,
/// so a client is never shown an agent that can only answer `503`.
///
/// # Errors
///
/// Fails on database errors.
pub fn offered_agents(state: &AppState, token: &Token) -> ApiResult<Vec<AgentKind>> {
    let project = match &token.project {
        Some(name) => state.store.get_project(name)?,
        None => None,
    };
    let eff = crate::projects::effective(token, project.as_ref());
    let usable = super::usable_agents(&state.store, token);
    Ok(AgentKind::ALL
        .into_iter()
        .filter(|a| usable.contains(a))
        .filter(|a| {
            state
                .runtime
                .agents
                .iter()
                .any(|i| i.agent == *a && i.available)
                || has_remote_account(state, &eff.accounts, *a)
        })
        .collect())
}

/// Validates a prompt against the token's policy and queues it.
///
/// Checks run in this order: pause, input, agent scope and availability,
/// then request caps, budget caps, the in-flight cap and the global queue
/// limit, all in the same transaction as the insert so a concurrent burst
/// cannot overshoot them. Non-admin tokens cannot take the last
/// [`admin_reserve`] queue slots.
///
/// # Errors
///
/// Returns the [`ApiError`] that explains the first failed check.
pub async fn submit(state: &AppState, token: &Token, input: SubmitInput) -> ApiResult<Request> {
    let now = now_ms();
    match token.status(now) {
        TokenStatus::Active => {}
        TokenStatus::Paused => {
            return Err(ApiError::coded(
                StatusCode::FORBIDDEN,
                "token_paused",
                super::TOKEN_PAUSED,
            ));
        }
        TokenStatus::Expired => return Err(ApiError::Unauthorized(super::INVALID_TOKEN)),
    }

    let message = input.message;
    if message.trim().is_empty() {
        return Err(ApiError::BadRequest("message must not be empty".to_owned()));
    }
    if message.len() > MAX_MESSAGE_BYTES {
        return Err(ApiError::BadRequest(format!(
            "message is {} bytes, over the limit of {MAX_MESSAGE_BYTES} (32 KB); shorten it",
            message.len()
        )));
    }
    if message.contains('\0') {
        return Err(ApiError::BadRequest(
            "message must not contain NUL bytes".to_owned(),
        ));
    }
    // A project-bound token gets its project's effective settings: the
    // agents both allow, the project's default agent and models.
    let project = match &token.project {
        Some(name) => Some(state.store.get_project(name)?.ok_or_else(|| {
            ApiError::Forbidden("this token's project no longer exists; ask the owner".to_owned())
        })?),
        None => None,
    };
    let eff = crate::projects::effective(token, project.as_ref());
    let agent = match input.agent.as_deref() {
        Some(name) => name
            .parse::<AgentKind>()
            .map_err(|e| ApiError::BadRequest(e.to_string()))?,
        None => eff.default_agent.unwrap_or(state.default_agent),
    };
    if !token.allows(agent) || !eff.allows(agent) {
        let allowed: Vec<&str> = AgentKind::ALL
            .into_iter()
            .filter(|a| token.allows(*a) && eff.allows(*a))
            .map(AgentKind::as_str)
            .collect();
        let who = if token.allows(agent) {
            "this token's project does not allow"
        } else {
            "this token may not use"
        };
        return Err(ApiError::Forbidden(if allowed.is_empty() {
            format!("{who} {agent}")
        } else {
            format!("{who} {agent}; it may use {}", allowed.join(", "))
        }));
    }
    if let Some(model) = &input.model
        && !agent::is_safe_arg(model)
    {
        return Err(ApiError::BadRequest(
            "model must be up to 128 characters, start with a letter or digit and use only \
             letters, digits and - _ . : / @ [ ]"
                .to_owned(),
        ));
    }
    let model = eff.model(agent, input.model.as_deref());
    // A remote account runs on a friend's bridge, so the agent need not be
    // installed here.
    let remote = has_remote_account(state, &eff.accounts, agent);
    // Refused here, as any body over the cap is, rather than failing on
    // the friend's bridge with an error that would reveal it.
    if remote && !crate::forward::fits_remote(&message, model.as_deref()) {
        return Err(ApiError::coded(
            StatusCode::PAYLOAD_TOO_LARGE,
            "too_large",
            format!(
                "the prompt is too large for this token; shorten it to under {} KB",
                super::MAX_BODY_BYTES / 1024
            ),
        ));
    }
    if !remote
        && !state
            .runtime
            .agents
            .iter()
            .any(|a| a.agent == agent && a.available)
    {
        return Err(ApiError::Unavailable(agent::unavailable_message(agent)));
    }

    if let Some(name) = &input.account {
        check_pinned_account(state, token, agent, name)?;
    }
    if let Some(stop) = eff.stop_at {
        let names = match &input.account {
            Some(name) => vec![name.clone()],
            None => crate::accounts::for_agent(&eff.accounts, agent),
        };
        if let Some(held) = state.store.all_past_stop(agent, &names, stop, now)? {
            let message = crate::usage::stop_message(agent, stop, &held);
            // Without a known reset, a client is told to come back in an
            // hour, when the usage will have been read again.
            let opens = held.opens_at.unwrap_or(now + 3_600_000);
            return Err(stop_error(message, opens, now));
        }
    }
    let id = uuid::Uuid::new_v4().to_string();
    // Pinned before the row exists, so no worker can claim it first.
    if let Some(name) = &input.account {
        state.dispatcher.pin_account(&id, name);
    }
    let admission = state.store.insert_request_forwarded(
        NewRequest {
            id: &id,
            token,
            agent,
            model: model.as_deref(),
            message: &message,
        },
        &token.limits,
        state.queue_limit,
        admin_reserve(state.queue_limit),
        input.hops,
    );
    let l = token.limits;
    if !matches!(admission, Ok(Admission::Accepted)) {
        state.dispatcher.unpin(&id);
    }
    let admission = admission?;
    match admission {
        Admission::Accepted => {}
        Admission::LimitReached(cap) => {
            return Err(match cap {
                Cap::DailyRequests => limit_error(
                    "dailyRequests",
                    format!(
                        "daily limit of {} reached",
                        prompts(l.daily_requests.unwrap_or(0))
                    ),
                    next_day_utc(now),
                    now,
                ),
                Cap::MonthlyRequests => limit_error(
                    "monthlyRequests",
                    format!(
                        "monthly limit of {} reached",
                        prompts(l.monthly_requests.unwrap_or(0))
                    ),
                    next_month_utc(now),
                    now,
                ),
                Cap::DailyBudgetUsd => limit_error(
                    "dailyBudgetUsd",
                    format!(
                        "daily budget of ${:.2} reached",
                        l.daily_budget_usd.unwrap_or(0.0)
                    ),
                    next_day_utc(now),
                    now,
                ),
                Cap::MonthlyBudgetUsd => limit_error(
                    "monthlyBudgetUsd",
                    format!(
                        "monthly budget of ${:.2} reached",
                        l.monthly_budget_usd.unwrap_or(0.0)
                    ),
                    next_month_utc(now),
                    now,
                ),
            });
        }
        Admission::TokenBusy { pending, max } => {
            return Err(ApiError::coded(
                StatusCode::TOO_MANY_REQUESTS,
                "too_many_pending",
                format!(
                    "this token already has {} queued or running, its limit of {max}; \
                     wait for one to finish or cancel one",
                    prompts(pending)
                ),
            ));
        }
        Admission::QueueFull => {
            return Err(ApiError::TooManyRequests(
                "the queue on this Hashi is full; try again in a minute",
            ));
        }
    }
    state.bus.emit(Event::Queued {
        id: id.clone(),
        agent,
        token: token.name.clone(),
    });
    state.dispatcher.wake(agent);
    state
        .store
        .get_request(&id)?
        .ok_or(ApiError::NotFound("request vanished"))
}

/// Checks that an admin may pin a request on `agent` to account `name`:
/// the default account, or one that exists, is enabled and not limited.
fn check_pinned_account(
    state: &AppState,
    token: &Token,
    agent: AgentKind,
    name: &str,
) -> ApiResult<()> {
    if !token.is_admin() {
        return Err(ApiError::Forbidden(
            "only an admin token may pick the account".to_owned(),
        ));
    }
    crate::accounts::check_name(name, true).map_err(ApiError::BadRequest)?;
    if name == crate::accounts::DEFAULT {
        return Ok(());
    }
    match state.store.get_account(agent, name)? {
        Some(a) if a.kind != crate::accounts::AccountKind::Default => match a.status(now_ms()) {
            crate::store::AccountStatus::Ready => Ok(()),
            s => Err(ApiError::Conflict(
                if s == crate::store::AccountStatus::Disabled {
                    "that account is disabled"
                } else {
                    "that account is at its limit"
                },
            )),
        },
        _ => Err(ApiError::BadRequest(format!(
            "{agent} has no account named {name}"
        ))),
    }
}

/// Waits up to `secs` (capped at `cap`) for a request to finish, then
/// returns its latest state. The wait counts against `waiter`, the token
/// asking; one that already holds [`MAX_WAITS_PER_TOKEN`] waits gets the
/// current state at once instead.
///
/// # Errors
///
/// Fails on database errors or if the request does not exist.
pub async fn wait_until_done(
    state: &AppState,
    waiter: &str,
    id: &str,
    secs: u64,
    cap: u64,
) -> ApiResult<Request> {
    // Subscribe before reading, so a completion between the two is not missed.
    let mut rx = state.bus.subscribe();
    let current = state
        .store
        .get_request(id)?
        .ok_or(ApiError::NotFound("request not found"))?;
    if current.status.is_terminal() || secs == 0 {
        return Ok(current);
    }
    let Some(_slot) = state.waits.try_acquire(waiter) else {
        return Ok(current);
    };
    // The request is read again when the wait ends. Holding this copy (and
    // its prompt) for the whole wait would only cost memory per connection.
    drop(current);
    let deadline = Duration::from_secs(secs.min(cap));
    let _ = tokio::time::timeout(deadline, async {
        loop {
            match rx.recv().await {
                Ok(Event::Finished { id: done, .. }) if done == id => return,
                Err(RecvError::Closed) => return,
                // On lag the caller re-reads the store anyway.
                Ok(_) | Err(RecvError::Lagged(_)) => {}
            }
        }
    })
    .await;
    state
        .store
        .get_request(id)?
        .ok_or(ApiError::NotFound("request not found"))
}

/// The hop count of a prompt another bridge forwarded here, from
/// [`crate::forward::HOPS_HEADER`]. A prompt past
/// [`crate::forward::MAX_HOPS`] hops, or one whose sender
/// ([`crate::forward::FROM_HEADER`]) is this bridge itself, is refused, so
/// a chain of bridges cannot loop.
///
/// # Errors
///
/// `508 hop_limit` or `508 loop`, or `400` for a malformed header.
pub fn forwarded_hops(state: &AppState, headers: &axum::http::HeaderMap) -> ApiResult<u8> {
    let hops = match headers.get(crate::forward::HOPS_HEADER) {
        None => 0,
        Some(v) => v
            .to_str()
            .ok()
            .and_then(|s| s.trim().parse::<u8>().ok())
            .ok_or_else(|| {
                ApiError::BadRequest(format!(
                    "{} must be a small number",
                    crate::forward::HOPS_HEADER
                ))
            })?,
    };
    if hops > crate::forward::MAX_HOPS {
        return Err(ApiError::coded(
            StatusCode::LOOP_DETECTED,
            "hop_limit",
            format!(
                "this prompt was already forwarded through {hops} bridges (at most {})",
                crate::forward::MAX_HOPS
            ),
        ));
    }
    if let Some(from) = headers
        .get(crate::forward::FROM_HEADER)
        .and_then(|v| v.to_str().ok())
        && state.store.instance_id()? == from.trim()
    {
        return Err(ApiError::coded(
            StatusCode::LOOP_DETECTED,
            "loop",
            "this prompt was forwarded by this same bridge",
        ));
    }
    Ok(hops)
}

pub(super) async fn ask(
    State(state): State<AppState>,
    Auth(token): Auth,
    headers: axum::http::HeaderMap,
    body: Result<Json<AskBody>, axum::extract::rejection::JsonRejection>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let Json(body) = body.map_err(|e| ApiError::BadRequest(e.body_text()))?;
    let hops = forwarded_hops(&state, &headers)?;
    let wait = body.wait;
    let req = submit(
        &state,
        &token,
        SubmitInput {
            message: body.message,
            agent: body.agent,
            model: body.model,
            account: None,
            hops,
        },
    )
    .await?;
    let req = match wait {
        Some(secs) => {
            // Only the id is needed while waiting; the result is read again.
            let id = req.id.clone();
            drop(req);
            wait_until_done(&state, &token.id, &id, secs, MAX_WAIT_SECS).await?
        }
        None => req,
    };
    let (code, v) = request_json(&state.store, req, reads_any_request(&token))?;
    Ok((code, Json(v)))
}

/// Whether `token` may read requests that other tokens sent: only an admin
/// token that is active. A paused admin has lost its admin powers, as the
/// `Admin` extractor treats it, so it reads only its own requests.
#[must_use]
pub fn reads_any_request(token: &Token) -> bool {
    token.is_admin() && token.status(now_ms()) == TokenStatus::Active
}

pub(super) async fn poll(
    State(state): State<AppState>,
    Auth(token): Auth,
    Path(id): Path<String>,
    Query(q): Query<WaitQuery>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let req = state
        .store
        .get_request(&id)?
        .ok_or(ApiError::NotFound("request not found"))?;
    if req.token_id != token.id && !reads_any_request(&token) {
        // Same answer as a missing id, so ids cannot be probed across tokens.
        return Err(ApiError::NotFound("request not found"));
    }
    let req = match q.wait {
        Some(secs) if !req.status.is_terminal() => {
            // Read again when the wait ends; do not hold this copy meanwhile.
            drop(req);
            wait_until_done(&state, &token.id, &id, secs, MAX_WAIT_SECS).await?
        }
        _ => req,
    };
    let (code, v) = request_json(&state.store, req, reads_any_request(&token))?;
    Ok((code, Json(v)))
}

pub(super) async fn reset_session(
    State(state): State<AppState>,
    Auth(token): Auth,
    Path(agent): Path<String>,
) -> ApiResult<Json<Value>> {
    let agent: AgentKind = agent
        .parse()
        .map_err(|e: agent::UnknownAgent| ApiError::BadRequest(e.to_string()))?;
    state.store.clear_session(&token.id, agent)?;
    state
        .store
        .audit(&token.name, "session.reset", &token.name, agent.as_str())?;
    Ok(Json(json!({ "ok": true, "agent": agent })))
}

/// The headroom a token with `shareUsage` sees for each agent it may use:
/// across the accounts serving that agent for it, the most headroom left
/// on an account that can take a run, that account's next reset and its
/// state; or, when every one is at its limit or off, `limited` with the
/// soonest time one comes back. Never account names, how many accounts
/// there are, or their windows.
///
/// # Errors
///
/// Fails on database errors.
pub fn shared_usage(state: &AppState, token: &Token, now: i64) -> ApiResult<Value> {
    let project = match &token.project {
        Some(name) => state.store.get_project(name)?,
        None => None,
    };
    let eff = crate::projects::effective(token, project.as_ref());
    let rotate_at = eff
        .rotate_at
        .unwrap_or_else(|| state.dispatcher.usage().policy().rotate_at);
    let mut out = serde_json::Map::new();
    for agent in super::usable_agents(&state.store, token) {
        let mut best: Option<crate::usage::Summary> = None;
        let mut back: Option<i64> = None;
        for name in crate::accounts::for_agent(&eff.accounts, agent) {
            let row = state.store.get_account(agent, &name)?;
            if row.as_ref().is_some_and(|a| a.disabled) {
                continue;
            }
            let summary = state.store.account_summary(agent, &name, rotate_at, now)?;
            if summary.state == crate::usage::UsageState::Limited {
                if let Some(u) = summary.limited_until {
                    back = Some(back.map_or(u, |b| b.min(u)));
                }
                continue;
            }
            let better = match &best {
                None => true,
                Some(b) => summary.headroom.unwrap_or(-1.0) > b.headroom.unwrap_or(-1.0),
            };
            if better {
                best = Some(summary);
            }
        }
        let view = match best {
            Some(s) => json!({
                "headroom": s.headroom_pct(),
                "nextReset": s.next_reset.and_then(rfc3339),
                "state": s.state,
            }),
            None => json!({
                "headroom": 0,
                "nextReset": back.and_then(rfc3339),
                "state": crate::usage::UsageState::Limited,
            }),
        };
        out.insert(agent.as_str().to_owned(), view);
    }
    Ok(Value::Object(out))
}

/// A token's own view of its access: status, scope, limits and usage, and
/// with `shareUsage` the headroom left per agent ([`shared_usage`]).
///
/// # Errors
///
/// Fails on database errors.
pub async fn me_json(state: &AppState, token: &Token) -> ApiResult<Value> {
    let now = now_ms();
    let usage = state.store.token_usage(&token.id, now)?;
    let mut v = json!({
        "name": token.name,
        "note": token.note,
        "role": token.role,
        "status": token.status(now),
        "sessionMode": token.session_mode,
        "agents": super::runnable_agents(state, &super::usable_agents(&state.store, token)),
        // The project's name only: never its folder or accounts.
        "project": token.project,
        "expiresAt": token.expires_at.and_then(rfc3339),
        "limits": token.limits,
        "usage": usage,
        "resets": { "day": rfc3339(next_day_utc(now)), "month": rfc3339(next_month_utc(now)) },
        "server": { "name": state.runtime.name, "version": env!("CARGO_PKG_VERSION") },
    });
    if token.share_usage
        && let Some(obj) = v.as_object_mut()
    {
        obj.insert("agentUsage".into(), shared_usage(state, token, now)?);
    }
    Ok(v)
}

pub(super) async fn me(
    State(state): State<AppState>,
    AnyToken(token): AnyToken,
) -> ApiResult<Json<Value>> {
    Ok(Json(me_json(&state, &token).await?))
}
