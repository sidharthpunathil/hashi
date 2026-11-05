//! Prompts: one submit path shared by HTTP and MCP, results, and `/v1/me`.

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
use crate::store::{NewRequest, Request, RequestStatus, Store};
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

/// A prompt to submit, from any front end.
#[derive(Debug, Clone)]
pub struct SubmitInput {
    /// The prompt.
    pub message: String,
    /// Agent name; `None` uses the server default.
    pub agent: Option<String>,
    /// Model override.
    pub model: Option<String>,
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
    created_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    completed_at: Option<String>,
    poll: String,
}

/// Renders a request as API JSON, with 200 when finished and 202 otherwise.
///
/// # Errors
///
/// Fails on database errors.
pub fn request_json(store: &Store, r: Request) -> ApiResult<(StatusCode, Value)> {
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

fn limit_error(limit: &'static str, message: String, reset_at: i64) -> ApiError {
    ApiError::Coded {
        status: StatusCode::TOO_MANY_REQUESTS,
        code: "limit_reached",
        message,
        extra: Some(json!({ "limit": limit, "resetAt": rfc3339(reset_at) })),
    }
}

/// Validates a prompt against the token's policy and queues it.
///
/// Checks run in this order: pause, input, agent scope and availability,
/// in-flight cap, request caps, budget caps, global queue limit.
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
                "this token is paused by the owner",
            ));
        }
        TokenStatus::Expired => return Err(ApiError::Unauthorized("invalid or expired token")),
    }

    let message = input.message;
    if message.trim().is_empty() {
        return Err(ApiError::BadRequest("message must not be empty".to_owned()));
    }
    if message.len() > MAX_MESSAGE_BYTES {
        return Err(ApiError::BadRequest(format!(
            "message exceeds {MAX_MESSAGE_BYTES} bytes"
        )));
    }
    if message.contains('\0') {
        return Err(ApiError::BadRequest(
            "message must not contain NUL bytes".to_owned(),
        ));
    }
    let agent = match input.agent.as_deref() {
        Some(name) => name
            .parse::<AgentKind>()
            .map_err(|e| ApiError::BadRequest(e.to_string()))?,
        None => state.default_agent,
    };
    if !token.allows(agent) {
        return Err(ApiError::Forbidden(format!(
            "this token may not use {agent}"
        )));
    }
    if let Some(model) = &input.model
        && !agent::is_safe_arg(model)
    {
        return Err(ApiError::BadRequest(
            "model contains unsupported characters".to_owned(),
        ));
    }
    if !state
        .runtime
        .agents
        .iter()
        .any(|a| a.agent == agent && a.available)
    {
        return Err(ApiError::Unavailable(format!(
            "{} is not installed on this host",
            agent.display_name()
        )));
    }

    let usage = state.store.token_usage(&token.id, now)?;
    let l = token.limits;
    if let Some(max) = l.max_pending
        && usage.pending >= u64::from(max)
    {
        return Err(ApiError::coded(
            StatusCode::TOO_MANY_REQUESTS,
            "too_many_pending",
            format!(
                "this token already has {} prompt(s) in flight (max {max})",
                usage.pending
            ),
        ));
    }
    if let Some(max) = l.daily_requests
        && usage.today_requests >= u64::from(max)
    {
        return Err(limit_error(
            "dailyRequests",
            format!("daily limit of {max} prompts reached"),
            next_day_utc(now),
        ));
    }
    if let Some(max) = l.monthly_requests
        && usage.month_requests >= u64::from(max)
    {
        return Err(limit_error(
            "monthlyRequests",
            format!("monthly limit of {max} prompts reached"),
            next_month_utc(now),
        ));
    }
    if let Some(max) = l.daily_budget_usd
        && usage.today_cost_usd >= max
    {
        return Err(limit_error(
            "dailyBudgetUsd",
            format!("daily budget of ${max:.2} reached"),
            next_day_utc(now),
        ));
    }
    if let Some(max) = l.monthly_budget_usd
        && usage.month_cost_usd >= max
    {
        return Err(limit_error(
            "monthlyBudgetUsd",
            format!("monthly budget of ${max:.2} reached"),
            next_month_utc(now),
        ));
    }
    if state.store.pending_count()? >= state.queue_limit {
        return Err(ApiError::TooManyRequests(
            "queue is full, try again shortly",
        ));
    }

    let id = uuid::Uuid::new_v4().to_string();
    state.store.insert_request(NewRequest {
        id: &id,
        token,
        agent,
        model: input.model.as_deref(),
        message: &message,
    })?;
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

/// Waits up to `secs` (capped at the server maximum) for a request to finish,
/// then returns its latest state.
///
/// # Errors
///
/// Fails on database errors or if the request does not exist.
pub async fn wait_until_done(
    state: &AppState,
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

pub(super) async fn ask(
    State(state): State<AppState>,
    Auth(token): Auth,
    body: Result<Json<AskBody>, axum::extract::rejection::JsonRejection>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let Json(body) = body.map_err(|e| ApiError::BadRequest(e.body_text()))?;
    let wait = body.wait;
    let req = submit(
        &state,
        &token,
        SubmitInput {
            message: body.message,
            agent: body.agent,
            model: body.model,
        },
    )
    .await?;
    let req = match wait {
        Some(secs) => wait_until_done(&state, &req.id, secs, MAX_WAIT_SECS).await?,
        None => req,
    };
    let (code, v) = request_json(&state.store, req)?;
    Ok((code, Json(v)))
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
    if req.token_id != token.id && !token.is_admin() {
        // Same answer as a missing id, so ids cannot be probed across tokens.
        return Err(ApiError::NotFound("request not found"));
    }
    let req = match q.wait {
        Some(secs) if !req.status.is_terminal() => {
            wait_until_done(&state, &id, secs, MAX_WAIT_SECS).await?
        }
        _ => req,
    };
    let (code, v) = request_json(&state.store, req)?;
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

/// A token's own view of its access: status, scope, limits and usage.
///
/// # Errors
///
/// Fails on database errors.
pub async fn me_json(state: &AppState, token: &Token) -> ApiResult<Value> {
    let now = now_ms();
    let usage = state.store.token_usage(&token.id, now)?;
    Ok(json!({
        "name": token.name,
        "note": token.note,
        "role": token.role,
        "status": token.status(now),
        "sessionMode": token.session_mode,
        "agents": token.agents,
        "expiresAt": token.expires_at.and_then(rfc3339),
        "limits": token.limits,
        "usage": usage,
        "resets": { "day": rfc3339(next_day_utc(now)), "month": rfc3339(next_month_utc(now)) },
        "server": { "name": state.runtime.name, "version": env!("CARGO_PKG_VERSION") },
    }))
}

pub(super) async fn me(
    State(state): State<AppState>,
    AnyToken(token): AnyToken,
) -> ApiResult<Json<Value>> {
    Ok(Json(me_json(&state, &token).await?))
}
