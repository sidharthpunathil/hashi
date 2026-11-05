//! Access requests over HTTP. The logic lives in [`crate::access`]; these
//! handlers add authentication, the per-address limit and the event that
//! tells the dashboard a request arrived.

use axum::Json;
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use super::{Admin, AnyToken, ApiError, ApiResult, AppState, ClientKey, claim_outcome};
use crate::access::{self, AcceptInput, Accepted, MoreInput, NewPerson, OpenLink};
use crate::events::Event;
use crate::store::{AccessLink, AccessRequest};
use crate::util::{now_ms, rfc3339};

/// A JSON body that may also be empty, meaning all defaults.
fn optional_body<T: DeserializeOwned + Default>(body: &Bytes) -> ApiResult<T> {
    if body.iter().all(u8::is_ascii_whitespace) {
        return Ok(T::default());
    }
    serde_json::from_slice(body).map_err(|e| ApiError::BadRequest(format!("invalid body: {e}")))
}

fn body<T: DeserializeOwned>(body: &Bytes) -> ApiResult<T> {
    serde_json::from_slice(body).map_err(|e| ApiError::BadRequest(format!("invalid body: {e}")))
}

fn link_json(l: &AccessLink, now: i64) -> Value {
    json!({
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

/// A request as the owner sees it. Never a secret, and never an address.
fn admin_json(state: &AppState, r: &AccessRequest, now: i64) -> Value {
    let status = r.state(now);
    let prefill = (status == "pending")
        .then(|| access::prefill(&state.store, r).ok())
        .flatten();
    json!({
        "id": r.id,
        "kind": r.kind,
        "status": status,
        "name": r.name,
        "message": r.message,
        "agents": r.agents,
        "project": r.project,
        "asked": r.asked,
        "summary": access::asked_text(r),
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

/// A request as the token that made it sees it.
fn own_json(r: &AccessRequest, now: i64) -> Value {
    json!({
        "id": r.id,
        "status": r.state(now),
        "asked": r.asked,
        "agents": r.agents,
        "message": r.message,
        "reason": r.reason,
        "createdAt": rfc3339(r.created_at),
        "expiresAt": rfc3339(r.expires_at),
        "decidedAt": r.decided_at.and_then(rfc3339),
    })
}

fn announce(state: &AppState, r: &AccessRequest) {
    state.bus.emit(Event::AccessRequest {
        id: r.id.clone(),
        kind: r.kind,
        name: r.name.clone(),
    });
}

// ── owner ──────────────────────────────────────────────────────────────────

pub(super) async fn link_state(
    State(state): State<AppState>,
    Admin(_): Admin,
) -> ApiResult<Json<Value>> {
    let now = now_ms();
    let link = access::current_link(&state.store)?;
    Ok(Json(json!({
        "open": link.as_ref().is_some_and(|l| l.status(now) == "open"),
        "link": link.as_ref().map(|l| link_json(l, now)),
        "pending": state.store.pending_access_count()?,
    })))
}

pub(super) async fn open_link(
    State(state): State<AppState>,
    Admin(admin): Admin,
    raw: Bytes,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let input: OpenLink = optional_body(&raw)?;
    let (link, code) = access::open_link(&state.store, &admin.name, input)?;
    state.bus.log("ask-access link opened");
    let public = state.runtime.public_url();
    let base = public.clone().unwrap_or_else(|| state.runtime.local_url());
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "url": access::link_url(&base, &code),
            "localOnly": public.is_none(),
            "warning": "Anyone with this link can ask for access until it closes. It cannot be shown again.",
            "link": link_json(&link, now_ms()),
        })),
    ))
}

pub(super) async fn close_link(
    State(state): State<AppState>,
    Admin(admin): Admin,
) -> ApiResult<Json<Value>> {
    let closed = access::close_link(&state.store, &admin.name)?;
    if closed {
        state.bus.log("ask-access link closed");
    }
    Ok(Json(json!({ "ok": true, "closed": closed })))
}

pub(super) async fn list(State(state): State<AppState>, Admin(_): Admin) -> ApiResult<Json<Value>> {
    let now = now_ms();
    let all = access::list(&state.store)?;
    let pending = all.iter().filter(|r| r.is_pending(now)).count();
    let requests: Vec<Value> = all.iter().map(|r| admin_json(&state, r, now)).collect();
    Ok(Json(json!({ "requests": requests, "pending": pending })))
}

pub(super) async fn accept(
    State(state): State<AppState>,
    Admin(admin): Admin,
    Path(id): Path<String>,
    raw: Bytes,
) -> ApiResult<Json<Value>> {
    let input: AcceptInput = optional_body(&raw)?;
    let now = now_ms();
    match access::accept(&state.store, &admin.name, &id, input)? {
        Accepted::Invite { request, invite } => {
            state
                .bus
                .log(format!("access request accepted: {}", invite.label));
            Ok(Json(json!({
                "ok": true,
                "request": admin_json(&state, &request, now),
                "invite": {
                    "id": invite.id,
                    "label": invite.label,
                    "agents": invite.agents,
                    "sessionMode": invite.session_mode,
                    "limits": invite.limits,
                    "tokenExpiresInDays": invite.token_expires_in_days,
                    "project": invite.project,
                    "expiresAt": rfc3339(invite.expires_at),
                },
                "delivery": "The requester collects their single-use link the next time they check. It is never shown here.",
            })))
        }
        Accepted::Token {
            request,
            token,
            changes,
        } => {
            // Raised limits can let queued prompts run.
            state.dispatcher.wake_all();
            state.bus.log(format!(
                "access request accepted: {} ({changes})",
                token.name
            ));
            let status = token.status(now);
            let mut record = serde_json::to_value(&*token).unwrap_or(Value::Null);
            if let Some(obj) = record.as_object_mut() {
                obj.insert("status".into(), json!(status));
            }
            Ok(Json(json!({
                "ok": true,
                "request": admin_json(&state, &request, now),
                "record": record,
                "changes": changes,
            })))
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeclineBody {
    #[serde(default)]
    reason: Option<String>,
}

pub(super) async fn decline(
    State(state): State<AppState>,
    Admin(admin): Admin,
    Path(id): Path<String>,
    raw: Bytes,
) -> ApiResult<Json<Value>> {
    let b: DeclineBody = optional_body(&raw)?;
    let r = access::decline(&state.store, &admin.name, &id, b.reason.as_deref())?;
    state
        .bus
        .log(format!("access request declined: {}", r.name));
    Ok(Json(
        json!({ "ok": true, "request": admin_json(&state, &r, now_ms()) }),
    ))
}

// ── the public page ────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CodeBody {
    code: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PickupBody {
    pickup: String,
}

pub(super) async fn preview(
    State(state): State<AppState>,
    client: ClientKey,
    raw: Bytes,
) -> ApiResult<Json<Value>> {
    let result = preview_code(&state, &raw);
    claim_outcome(&state, &client, result)
}

fn preview_code(state: &AppState, raw: &Bytes) -> ApiResult<Json<Value>> {
    let b: CodeBody = body(raw)?;
    let link = access::open_link_for(&state.store, &b.code)?;
    let allowed = access::link_agents(&state.store, &link)?;
    // Offer what this machine runs, when it runs any of them.
    let installed: Vec<_> = allowed
        .iter()
        .copied()
        .filter(|a| {
            state
                .runtime
                .agents
                .iter()
                .any(|i| i.agent == *a && i.available)
        })
        .collect();
    let agents = if installed.is_empty() {
        allowed
    } else {
        installed
    };
    Ok(Json(json!({
        "server": state.runtime.name,
        "note": link.note,
        // The project's name only: never its folder or accounts.
        "project": link.project,
        "agents": agents,
        "closesAt": rfc3339(link.expires_at),
        "maxName": access::MAX_NAME_CHARS,
        "maxMessage": access::MAX_MESSAGE_CHARS,
        "notice": "The owner sees your name and message, and decides. If they accept, the owner of this Hashi can see the prompts you send and the answers you get.",
    })))
}

pub(super) async fn submit(
    State(state): State<AppState>,
    client: ClientKey,
    raw: Bytes,
) -> ApiResult<(StatusCode, Json<Value>)> {
    // Every submission counts, accepted or not.
    if !state.limiter.check(&format!("askaccess:{}", client.0)) {
        return Err(ApiError::TooManyRequests(
            "too many requests from this address, slow down",
        ));
    }
    let result = submit_request(&state, &raw);
    claim_outcome(&state, &client, result)
}

fn submit_request(state: &AppState, raw: &Bytes) -> ApiResult<(StatusCode, Json<Value>)> {
    let input: NewPerson = body(raw)?;
    let (req, pickup) = access::submit_new(&state.store, input)?;
    announce(state, &req);
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "id": req.id,
            "status": "pending",
            "pickup": pickup,
            "expiresAt": rfc3339(req.expires_at),
            "warning": "Keep this pickup secret. It is shown once, and it is the only way to collect your access.",
        })),
    ))
}

pub(super) async fn status(
    State(state): State<AppState>,
    client: ClientKey,
    raw: Bytes,
) -> ApiResult<Json<Value>> {
    let result = pickup_status(&state, &raw);
    claim_outcome(&state, &client, result)
}

fn pickup_status(state: &AppState, raw: &Bytes) -> ApiResult<Json<Value>> {
    let b: PickupBody = body(raw)?;
    let p = access::pickup(&state.store, &b.pickup)?;
    let base = state.runtime.base_url();
    Ok(Json(json!({
        "status": p.state,
        "name": p.request.name,
        "server": state.runtime.name,
        "reason": (p.state == "declined").then_some(p.request.reason.clone()).flatten(),
        "expiresAt": rfc3339(p.request.expires_at),
        "claimCode": p.claim_code,
        "claimUrl": p.claim_code.as_ref().map(|c| format!("{base}/claim/{c}")),
    })))
}

// ── tokens asking for more ─────────────────────────────────────────────────

pub(super) async fn my_requests(
    State(state): State<AppState>,
    AnyToken(token): AnyToken,
) -> ApiResult<Json<Value>> {
    let now = now_ms();
    let list: Vec<Value> = state
        .store
        .access_requests_for_token(&token.id)?
        .iter()
        .map(|r| own_json(r, now))
        .collect();
    Ok(Json(json!({ "requests": list })))
}

pub(super) async fn ask_more(
    State(state): State<AppState>,
    AnyToken(token): AnyToken,
    raw: Bytes,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let input: MoreInput = body(&raw)?;
    let r = ask_more_for(&state, &token, input)?;
    Ok((
        StatusCode::CREATED,
        Json(json!({ "request": own_json(&r, now_ms()) })),
    ))
}

/// Files a token's request for more and tells the dashboard. Used by
/// `POST /v1/me/requests` and the MCP tool `request_more`.
///
/// # Errors
///
/// As [`access::submit_more`].
pub fn ask_more_for(
    state: &AppState,
    token: &crate::token::Token,
    input: MoreInput,
) -> ApiResult<AccessRequest> {
    let r = access::submit_more(&state.store, token, input)?;
    announce(state, &r);
    Ok(r)
}
