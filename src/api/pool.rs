//! The host's pool endpoints (see [`crate::pool`]): joining, the
//! members' workers asking for work and answering it, and one view of
//! everyone's agents and headroom.

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use serde::Deserialize;
use serde_json::{Value, json};

use super::{AnyToken, ApiError, ApiResult, AppState, ClientKey, claim_outcome};
use crate::agent::AgentKind;
use crate::pool::{Outcome, POLL_WAIT};
use crate::store::PoolMember;
use crate::util::{now_ms, rfc3339};

/// Body of `POST /v1/pool/join`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct JoinBody {
    code: String,
    name: String,
    agents: Vec<AgentKind>,
    /// Hours they stay; left out, until they leave.
    #[serde(default)]
    hours: Option<u32>,
}

/// Body of `POST /v1/pool/poll`.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct PollBody {
    /// Headroom per agent, as `/v1/me`'s `agentUsage`.
    #[serde(default)]
    usage: Option<Value>,
}

/// Body of `POST /v1/pool/result`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct ResultBody {
    job_id: String,
    /// `done`, `limited`, `busy` or `failed`.
    kind: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    reset_at: Option<String>,
    #[serde(default)]
    input_tokens: Option<u64>,
    #[serde(default)]
    output_tokens: Option<u64>,
    #[serde(default)]
    cost_usd: Option<f64>,
}

fn not_member() -> ApiError {
    ApiError::coded(
        StatusCode::UNAUTHORIZED,
        "not_in_pool",
        "this Hashi is not in the pool any more; join again with a new link",
    )
}

/// The member a request's bearer secret belongs to. Members whose time is
/// up are taken out here.
fn member(state: &AppState, headers: &HeaderMap) -> ApiResult<PoolMember> {
    let secret = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|s| s.len() <= 128)
        .ok_or_else(not_member)?;
    let m = state
        .store
        .pool_member_by_secret(secret)?
        .ok_or_else(not_member)?;
    if m.until.is_some_and(|u| u <= now_ms()) {
        leave_member(state, &m.id, "their time in the pool is up");
        return Err(not_member());
    }
    Ok(m)
}

fn leave_member(state: &AppState, id: &str, why: &str) {
    match state.store.remove_pool_member(id) {
        Ok(Some(m)) => {
            state.dispatcher.pool().forget(&m.id);
            let _ = state.store.audit(&m.name, "pool.leave", &m.name, why);
            state.bus.log(format!("{} left the pool: {why}", m.name));
        }
        Ok(None) => {}
        Err(e) => tracing::warn!(error = %e, "could not take a member out of the pool"),
    }
}

pub(super) async fn join(
    State(state): State<AppState>,
    client: ClientKey,
    body: Result<Json<JoinBody>, axum::extract::rejection::JsonRejection>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let result = join_inner(&state, body);
    claim_outcome(&state, &client, result)
}

fn join_inner(
    state: &AppState,
    body: Result<Json<JoinBody>, axum::extract::rejection::JsonRejection>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let Json(body) = body.map_err(|e| ApiError::BadRequest(e.body_text()))?;
    let gone = || {
        ApiError::coded(
            StatusCode::GONE,
            "pool_closed",
            "this pool link is closed or has expired; ask the host for a new one",
        )
    };
    if !body.code.starts_with(crate::store::POOL_LINK_PREFIX) || body.code.len() > 128 {
        return Err(gone());
    }
    let link = state
        .store
        .pool_link_by_code(&body.code)?
        .filter(|l| l.is_open(now_ms()))
        .ok_or_else(gone)?;
    let name = body.name.trim().to_ascii_lowercase();
    crate::accounts::check_name(&name, false).map_err(ApiError::BadRequest)?;
    let mut agents = body.agents;
    agents.sort();
    agents.dedup();
    if agents.is_empty() {
        return Err(ApiError::BadRequest("offer at least one agent".to_owned()));
    }
    let until = match body.hours {
        Some(h) if (1..=720).contains(&h) => Some(now_ms() + i64::from(h) * 3_600_000),
        Some(_) => {
            return Err(ApiError::BadRequest(
                "hours must be from 1 to 720".to_owned(),
            ));
        }
        None => None,
    };
    let (m, secret, token) = state
        .store
        .add_pool_member(&link, &name, &agents, until)
        .map_err(|e| match e {
            crate::store::StoreError::LimitReached(why) => {
                ApiError::coded(StatusCode::CONFLICT, "pool_full_or_taken", why)
            }
            other => other.into(),
        })?;
    state.store.audit(
        &m.name,
        "pool.join",
        &m.name,
        &crate::token::encode_agents(&m.agents),
    )?;
    state.bus.log(format!(
        "{} joined the pool with {}",
        m.name,
        crate::token::encode_agents(&m.agents)
    ));
    let base = state.runtime.base_url();
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "memberId": m.id,
            "name": m.name,
            "secret": secret,
            "token": token,
            "server": state.runtime.name,
            "baseUrl": base,
            "agents": m.agents,
            "until": m.until.and_then(rfc3339),
            "notice": "Prompts from everyone in the pool run on the agents you offer, on your \
                       machine: you can see them, and the host can see every prompt.",
        })),
    ))
}

/// Keeps a member's reported headroom as the usage of their accounts.
fn record_usage(state: &AppState, m: &PoolMember, usage: &Value) {
    let me = json!({ "status": "active", "agentUsage": usage });
    for agent in &m.agents {
        let reading = crate::forward::me_windows(&me, *agent, &m.name);
        if !reading.shared {
            continue;
        }
        let store = &state.store;
        let saved = store
            .replace_usage(*agent, &m.name, &reading.windows)
            .and_then(|()| store.set_poll_status(*agent, &m.name, true, Some("pool"), None));
        if let Err(e) = saved {
            tracing::warn!(error = %e, "could not keep a pool member's usage");
        }
        if let Some(until) = reading.limited_until.filter(|u| *u > now_ms()) {
            let _ = store.set_account_limited(*agent, &m.name, until);
        }
        crate::usage::publish(store, &state.bus, state.dispatcher.usage(), *agent, &m.name);
    }
}

pub(super) async fn poll(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<PollBody>, axum::extract::rejection::JsonRejection>,
) -> ApiResult<Json<Value>> {
    let m = member(&state, &headers)?;
    let body = body.map(|Json(b)| b).unwrap_or_default();
    state.store.touch_pool_member(&m.id)?;
    state.dispatcher.pool().seen(&m.id);
    if let Some(usage) = body.usage.as_ref().filter(|u| u.is_object()) {
        record_usage(&state, &m, usage);
    }
    let job = state.dispatcher.pool().next(&m.id, POLL_WAIT).await;
    Ok(Json(json!({ "job": job })))
}

pub(super) async fn result(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<ResultBody>, axum::extract::rejection::JsonRejection>,
) -> ApiResult<Json<Value>> {
    let m = member(&state, &headers)?;
    let Json(b) = body.map_err(|e| ApiError::BadRequest(e.body_text()))?;
    let message = b.message.unwrap_or_default();
    let outcome = match b.kind.as_str() {
        "done" => Outcome::Done {
            text: b.text.unwrap_or_default(),
            input_tokens: b.input_tokens,
            output_tokens: b.output_tokens,
            cost_usd: b.cost_usd.filter(|c| c.is_finite() && *c >= 0.0),
        },
        "limited" => Outcome::Limited {
            reset_at: b.reset_at.as_deref().and_then(crate::util::parse_rfc3339),
            message,
        },
        "busy" => Outcome::Busy(message),
        "failed" => Outcome::Failed(message),
        _ => {
            return Err(ApiError::BadRequest(
                "kind must be done, limited, busy or failed".to_owned(),
            ));
        }
    };
    let taken = state.dispatcher.pool().answer(&m.id, &b.job_id, outcome);
    Ok(Json(json!({ "ok": taken })))
}

pub(super) async fn leave(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    let m = member(&state, &headers)?;
    leave_member(&state, &m.id, "they left");
    Ok(Json(json!({ "ok": true })))
}

/// Everyone in the pool and the headroom of each agent they offer.
///
/// # Errors
///
/// Fails on database errors.
pub fn view(state: &AppState) -> ApiResult<Value> {
    let hub = state.dispatcher.pool();
    Ok(view_with(
        &state.store,
        state.dispatcher.usage().policy().rotate_at,
        &state.runtime.name,
        |m| hub.online(&m.id),
    )?)
}

/// [`view`] from the store alone, with `online` saying who is online.
///
/// # Errors
///
/// Fails on database errors.
pub fn view_with(
    store: &crate::store::Store,
    rotate_at: u8,
    server: &str,
    online: impl Fn(&PoolMember) -> bool,
) -> Result<Value, crate::store::StoreError> {
    let now = now_ms();
    let mut members = Vec::new();
    for m in store.list_pool_members()? {
        let mut agents = serde_json::Map::new();
        for agent in &m.agents {
            let s = store.account_summary(*agent, &m.name, rotate_at, now)?;
            let disabled = store
                .get_account(*agent, &m.name)?
                .is_some_and(|a| a.disabled);
            agents.insert(
                agent.as_str().to_owned(),
                json!({
                    "headroom": s.headroom_pct(),
                    "nextReset": s.next_reset.and_then(rfc3339),
                    "state": if disabled { json!("disabled") } else { json!(s.state) },
                }),
            );
        }
        members.push(json!({
            "name": m.name,
            "online": online(&m),
            "joinedAt": rfc3339(m.joined_at),
            "until": m.until.and_then(rfc3339),
            "agents": agents,
        }));
    }
    let link = store.pool_link()?;
    Ok(json!({
        "server": server,
        "open": link.as_ref().is_some_and(|l| l.is_open(now)),
        "joinClosesAt": link.filter(|l| l.is_open(now)).and_then(|l| rfc3339(l.expires_at)),
        "members": members,
    }))
}

pub(super) async fn status(
    State(state): State<AppState>,
    AnyToken(token): AnyToken,
) -> ApiResult<Json<Value>> {
    if !token.is_admin() && !state.store.token_in_pool(&token)? {
        return Err(ApiError::Forbidden(
            "this token is not in the pool".to_owned(),
        ));
    }
    Ok(Json(view(&state)?))
}
