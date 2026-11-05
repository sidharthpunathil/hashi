//! HTTP API.
//!
//! | Method   | Path                | Auth  | Purpose                          |
//! |----------|---------------------|-------|----------------------------------|
//! | `GET`    | `/healthz`          |       | Liveness probe                   |
//! | `GET`    | `/v1/status`        |       | Version, uptime, agent readiness |
//! | `POST`   | `/v1/ask`           | token | Queue a prompt                   |
//! | `GET`    | `/v1/ask/{id}`      | token | Poll (or long-poll) a result     |
//! | `DELETE` | `/v1/session/{agent}` | token | Reset a stateful conversation  |
//! | `GET`    | `/v1/stats`         | admin | Queue and request counters       |
//! | `GET`    | `/v1/tokens`        | admin | List tokens                      |
//! | `POST`   | `/v1/tokens`        | admin | Create a token                   |
//! | `DELETE` | `/v1/tokens/{id}`   | admin | Revoke a token                   |

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use axum::extract::{FromRequestParts, Path, Query, State};
use axum::http::{HeaderValue, StatusCode, header, request::Parts};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::sync::broadcast::error::RecvError;
use tower_http::cors::{Any, CorsLayer};
use tower_http::limit::RequestBodyLimitLayer;

use crate::agent::{self, AgentKind, Usage};
use crate::events::{Bus, Event};
use crate::queue::Dispatcher;
use crate::store::{NewRequest, NewToken, Request, RequestStatus, Revoke, Store, StoreError};
use crate::token::{Role, SessionMode, Token};
use crate::util::rfc3339;

/// Largest accepted prompt, in bytes.
pub const MAX_MESSAGE_BYTES: usize = 32 * 1024;
/// Largest accepted request body, in bytes.
pub const MAX_BODY_BYTES: usize = 64 * 1024;
/// Upper bound for `wait` long-polls, in seconds.
pub const MAX_WAIT_SECS: u64 = 120;
/// Upper bound on stored tokens.
pub const MAX_TOKENS: usize = 100;

/// Readiness of one agent, discovered at startup.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentInfo {
    /// Which agent.
    pub agent: AgentKind,
    /// Whether the executable answered `--version`.
    pub available: bool,
    /// Reported version string.
    pub version: Option<String>,
    /// Resolved executable path.
    pub path: String,
}

/// Facts about the running server that the API and TUI both display.
#[derive(Debug)]
pub struct Runtime {
    /// Process start.
    pub started: Instant,
    /// Local listen address.
    pub local_url: String,
    /// Public tunnel URL, once known.
    pub public_url: RwLock<Option<String>>,
    /// Agent readiness.
    pub agents: Vec<AgentInfo>,
}

impl Runtime {
    /// Best URL to hand to clients.
    #[must_use]
    pub fn base_url(&self) -> String {
        self.public_url
            .read()
            .ok()
            .and_then(|u| u.clone())
            .unwrap_or_else(|| self.local_url.clone())
    }
}

/// Shared handler state.
#[derive(Debug, Clone)]
pub struct AppState {
    /// Persistence.
    pub store: Arc<Store>,
    /// Event publisher.
    pub bus: Bus,
    /// Worker wake-ups.
    pub dispatcher: Dispatcher,
    /// Server facts.
    pub runtime: Arc<Runtime>,
    /// Per-token rate limiter.
    pub limiter: Arc<RateLimiter>,
    /// Agent used when a request does not name one.
    pub default_agent: AgentKind,
    /// Maximum pending requests across all agents.
    pub queue_limit: u64,
}

/// Builds the router.
pub fn router(state: AppState) -> Router {
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([
            axum::http::Method::GET,
            axum::http::Method::POST,
            axum::http::Method::DELETE,
        ])
        .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE]);

    Router::new()
        .route("/", get(root))
        .route("/healthz", get(|| async { "ok" }))
        .route("/v1/status", get(status))
        .route("/v1/ask", post(ask))
        .route("/v1/ask/{id}", get(poll))
        .route("/v1/session/{agent}", delete(reset_session))
        .route("/v1/stats", get(stats))
        .route("/v1/tokens", get(list_tokens).post(create_token))
        .route("/v1/tokens/{id}", delete(revoke_token))
        .fallback(|| async { ApiError::NotFound("no such route") })
        .layer(RequestBodyLimitLayer::new(MAX_BODY_BYTES))
        .layer(cors)
        .layer(axum::middleware::map_response(security_headers))
        .with_state(state)
}

async fn security_headers(mut res: Response) -> Response {
    let h = res.headers_mut();
    h.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    h.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res
}

// ── errors ─────────────────────────────────────────────────────────────────

/// Errors rendered as `{"error": {"code", "message"}}`.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    /// 400
    #[error("{0}")]
    BadRequest(String),
    /// 401
    #[error("{0}")]
    Unauthorized(&'static str),
    /// 403
    #[error("{0}")]
    Forbidden(String),
    /// 404
    #[error("{0}")]
    NotFound(&'static str),
    /// 409
    #[error("{0}")]
    Conflict(&'static str),
    /// 429
    #[error("{0}")]
    TooManyRequests(&'static str),
    /// 503
    #[error("{0}")]
    Unavailable(String),
    /// 500
    #[error("internal error")]
    Internal(#[from] StoreError),
}

impl ApiError {
    fn parts(&self) -> (StatusCode, &'static str) {
        match self {
            Self::BadRequest(_) => (StatusCode::BAD_REQUEST, "bad_request"),
            Self::Unauthorized(_) => (StatusCode::UNAUTHORIZED, "unauthorized"),
            Self::Forbidden(_) => (StatusCode::FORBIDDEN, "forbidden"),
            Self::NotFound(_) => (StatusCode::NOT_FOUND, "not_found"),
            Self::Conflict(_) => (StatusCode::CONFLICT, "conflict"),
            Self::TooManyRequests(_) => (StatusCode::TOO_MANY_REQUESTS, "rate_limited"),
            Self::Unavailable(_) => (StatusCode::SERVICE_UNAVAILABLE, "unavailable"),
            Self::Internal(_) => (StatusCode::INTERNAL_SERVER_ERROR, "internal"),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        if let Self::Internal(e) = &self {
            tracing::error!(error = %e, "internal error");
        }
        let (status, code) = self.parts();
        let mut res = (
            status,
            Json(json!({ "error": { "code": code, "message": self.to_string() } })),
        )
            .into_response();
        if status == StatusCode::UNAUTHORIZED {
            res.headers_mut()
                .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        }
        res
    }
}

type ApiResult<T> = Result<T, ApiError>;

// ── auth ───────────────────────────────────────────────────────────────────

/// Fixed-window request limiter keyed by token id.
#[derive(Debug)]
pub struct RateLimiter {
    per_minute: u32,
    windows: Mutex<HashMap<String, (Instant, u32)>>,
}

impl RateLimiter {
    /// Allows `per_minute` authenticated requests per token. Zero disables limiting.
    #[must_use]
    pub fn new(per_minute: u32) -> Self {
        Self {
            per_minute,
            windows: Mutex::new(HashMap::new()),
        }
    }

    /// Records a hit and returns whether it is within budget.
    pub fn check(&self, key: &str) -> bool {
        if self.per_minute == 0 {
            return true;
        }
        let Ok(mut map) = self.windows.lock() else {
            return false;
        };
        let now = Instant::now();
        if map.len() > 10_000 {
            map.retain(|_, (start, _)| now.duration_since(*start) < Duration::from_secs(60));
        }
        let entry = map.entry(key.to_owned()).or_insert((now, 0));
        if now.duration_since(entry.0) >= Duration::from_secs(60) {
            *entry = (now, 0);
        }
        entry.1 += 1;
        entry.1 <= self.per_minute
    }
}

/// Extractor for any valid token.
#[derive(Debug)]
pub struct Auth(pub Token);

/// Extractor for admin tokens.
#[derive(Debug)]
pub struct Admin(pub Token);

impl FromRequestParts<AppState> for Auth {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> ApiResult<Self> {
        let header = parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .ok_or(ApiError::Unauthorized(
                "missing Authorization: Bearer <token> header",
            ))?;
        let raw = header
            .strip_prefix("Bearer ")
            .map(str::trim)
            .filter(|t| !t.is_empty() && t.len() <= 256)
            .ok_or(ApiError::Unauthorized("malformed Authorization header"))?;
        let token = state
            .store
            .authenticate(raw)?
            .ok_or(ApiError::Unauthorized("invalid or expired token"))?;
        if !state.limiter.check(&token.id) {
            return Err(ApiError::TooManyRequests("rate limit exceeded, slow down"));
        }
        Ok(Self(token))
    }
}

impl FromRequestParts<AppState> for Admin {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> ApiResult<Self> {
        let Auth(token) = Auth::from_request_parts(parts, state).await?;
        if !token.is_admin() {
            return Err(ApiError::Forbidden("admin token required".to_owned()));
        }
        Ok(Self(token))
    }
}

// ── public ─────────────────────────────────────────────────────────────────

async fn root(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(json!({
        "name": "hashi",
        "version": env!("CARGO_PKG_VERSION"),
        "description": "A bridge to Claude Code and Codex",
        "baseUrl": state.runtime.base_url(),
        "docs": "POST /v1/ask with Authorization: Bearer <token>",
    }))
}

async fn status(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(json!({
        "status": "ok",
        "version": env!("CARGO_PKG_VERSION"),
        "uptimeSecs": state.runtime.started.elapsed().as_secs(),
        "defaultAgent": state.default_agent,
        "agents": state.runtime.agents.iter().map(|a| json!({
            "agent": a.agent, "available": a.available, "version": a.version,
        })).collect::<Vec<_>>(),
    }))
}

// ── ask ────────────────────────────────────────────────────────────────────

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

fn view(store: &Store, r: Request) -> ApiResult<(StatusCode, Json<RequestView>)> {
    let position = (r.status == RequestStatus::Queued)
        .then(|| store.queue_position(&r))
        .transpose()?;
    let code = if r.status.is_terminal() {
        StatusCode::OK
    } else {
        StatusCode::ACCEPTED
    };
    Ok((
        code,
        Json(RequestView {
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
        }),
    ))
}

async fn ask(
    State(state): State<AppState>,
    Auth(token): Auth,
    body: Result<Json<AskBody>, axum::extract::rejection::JsonRejection>,
) -> ApiResult<(StatusCode, Json<RequestView>)> {
    let Json(body) = body.map_err(|e| ApiError::BadRequest(e.body_text()))?;

    if body.message.trim().is_empty() {
        return Err(ApiError::BadRequest("message must not be empty".to_owned()));
    }
    if body.message.len() > MAX_MESSAGE_BYTES {
        return Err(ApiError::BadRequest(format!(
            "message exceeds {MAX_MESSAGE_BYTES} bytes"
        )));
    }
    if body.message.contains('\0') {
        return Err(ApiError::BadRequest(
            "message must not contain NUL bytes".to_owned(),
        ));
    }
    let agent = match body.agent.as_deref() {
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
    if let Some(model) = &body.model
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
    if state.store.pending_count()? >= state.queue_limit {
        return Err(ApiError::TooManyRequests(
            "queue is full, try again shortly",
        ));
    }

    let id = uuid::Uuid::new_v4().to_string();
    // Subscribe before enqueueing so a fast completion cannot be missed.
    let events = body.wait.map(|_| state.bus.subscribe());
    state.store.insert_request(NewRequest {
        id: &id,
        token: &token,
        agent,
        model: body.model.as_deref(),
        message: &body.message,
    })?;
    state.bus.emit(Event::Queued {
        id: id.clone(),
        agent,
        token: token.name.clone(),
    });
    state.dispatcher.wake(agent);

    if let (Some(secs), Some(rx)) = (body.wait, events) {
        wait_for(rx, &id, secs).await;
    }
    let req = state
        .store
        .get_request(&id)?
        .ok_or(ApiError::NotFound("request vanished"))?;
    view(&state.store, req)
}

async fn poll(
    State(state): State<AppState>,
    Auth(token): Auth,
    Path(id): Path<String>,
    Query(q): Query<WaitQuery>,
) -> ApiResult<(StatusCode, Json<RequestView>)> {
    let rx = q.wait.map(|_| state.bus.subscribe());
    let req = state
        .store
        .get_request(&id)?
        .ok_or(ApiError::NotFound("request not found"))?;
    if req.token_id != token.id && !token.is_admin() {
        // Same answer as a missing id, so ids cannot be probed across tokens.
        return Err(ApiError::NotFound("request not found"));
    }
    if let (Some(secs), Some(rx), false) = (q.wait, rx, req.status.is_terminal()) {
        wait_for(rx, &id, secs).await;
        let req = state
            .store
            .get_request(&id)?
            .ok_or(ApiError::NotFound("request not found"))?;
        return view(&state.store, req);
    }
    view(&state.store, req)
}

async fn wait_for(mut rx: tokio::sync::broadcast::Receiver<Event>, id: &str, secs: u64) {
    let deadline = Duration::from_secs(secs.min(MAX_WAIT_SECS));
    let _ = tokio::time::timeout(deadline, async {
        loop {
            match rx.recv().await {
                Ok(Event::Finished { id: done, .. }) if done == id => return,
                // On lag the caller re-reads the store anyway.
                Err(RecvError::Closed) => return,
                Ok(_) | Err(RecvError::Lagged(_)) => {}
            }
        }
    })
    .await;
}

async fn reset_session(
    State(state): State<AppState>,
    Auth(token): Auth,
    Path(agent): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    let agent: AgentKind = agent
        .parse()
        .map_err(|e: agent::UnknownAgent| ApiError::BadRequest(e.to_string()))?;
    state.store.clear_session(&token.id, agent)?;
    Ok(Json(json!({ "ok": true, "agent": agent })))
}

// ── admin ──────────────────────────────────────────────────────────────────

async fn stats(
    State(state): State<AppState>,
    Admin(_): Admin,
) -> ApiResult<Json<serde_json::Value>> {
    Ok(Json(json!({ "stats": state.store.stats()? })))
}

/// Body of `POST /v1/tokens`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateTokenBody {
    /// Label, 1 to 100 characters.
    pub name: String,
    /// `admin` or `user` (default).
    #[serde(default)]
    pub role: Option<Role>,
    /// `stateless` (default) or `stateful`.
    #[serde(default)]
    pub session_mode: Option<SessionMode>,
    /// Allowed agents. Defaults to all.
    #[serde(default)]
    pub agents: Option<Vec<AgentKind>>,
    /// Lifetime in days (1 to 3650). Omit for no expiry.
    #[serde(default)]
    pub expires_in_days: Option<u32>,
}

/// Validates a create request and turns it into store input.
///
/// # Errors
///
/// Returns [`ApiError::BadRequest`] for invalid fields.
pub fn validate_new_token(body: CreateTokenBody) -> ApiResult<NewToken> {
    let name = body.name.trim();
    if name.is_empty() || name.chars().count() > 100 || name.chars().any(char::is_control) {
        return Err(ApiError::BadRequest(
            "name must be 1 to 100 printable characters".to_owned(),
        ));
    }
    if body.expires_in_days.is_some_and(|d| d == 0 || d > 3_650) {
        return Err(ApiError::BadRequest(
            "expiresInDays must be between 1 and 3650".to_owned(),
        ));
    }
    let mut agents = body.agents.unwrap_or_else(|| AgentKind::ALL.to_vec());
    agents.sort_by_key(|a| a.as_str());
    agents.dedup();
    if agents.is_empty() {
        return Err(ApiError::BadRequest(
            "agents must name at least one agent".to_owned(),
        ));
    }
    Ok(NewToken {
        name: name.to_owned(),
        role: body.role.unwrap_or(Role::User),
        session_mode: body.session_mode.unwrap_or_default(),
        agents,
        expires_in_days: body.expires_in_days,
        note: None,
        limits: crate::token::Limits::default(),
    })
}

async fn list_tokens(
    State(state): State<AppState>,
    Admin(_): Admin,
) -> ApiResult<Json<serde_json::Value>> {
    Ok(Json(json!({ "tokens": state.store.list_tokens()? })))
}

async fn create_token(
    State(state): State<AppState>,
    Admin(_): Admin,
    body: Result<Json<CreateTokenBody>, axum::extract::rejection::JsonRejection>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    let Json(body) = body.map_err(|e| ApiError::BadRequest(e.body_text()))?;
    let new = validate_new_token(body)?;
    if state.store.list_tokens()?.len() >= MAX_TOKENS {
        return Err(ApiError::Conflict("token limit reached, revoke one first"));
    }
    let (token, secret) = state.store.create_token(&new)?;
    state
        .bus
        .log(format!("token created: {} ({})", token.name, token.prefix));
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "token": secret.raw,
            "warning": "Store this token now. It cannot be shown again.",
            "record": token,
        })),
    ))
}

async fn revoke_token(
    State(state): State<AppState>,
    Admin(_): Admin,
    Path(id): Path<String>,
) -> ApiResult<Json<serde_json::Value>> {
    match state.store.revoke_token(&id)? {
        Revoke::Revoked => {
            state.bus.log(format!("token revoked: {id}"));
            Ok(Json(json!({ "ok": true })))
        }
        Revoke::NotFound => Err(ApiError::NotFound("token not found")),
        Revoke::LastAdmin => Err(ApiError::Conflict("cannot revoke the last admin token")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(name: &str) -> CreateTokenBody {
        CreateTokenBody {
            name: name.into(),
            role: None,
            session_mode: None,
            agents: None,
            expires_in_days: None,
        }
    }

    #[test]
    fn validate_new_token_defaults_to_user_with_all_agents() {
        let t = validate_new_token(body(" bot ")).unwrap();
        assert_eq!(
            (t.name.as_str(), t.role, t.agents.len()),
            ("bot", Role::User, 2)
        );
    }

    #[test]
    fn validate_new_token_rejects_control_characters() {
        assert!(validate_new_token(body("a\nb")).is_err());
    }

    #[test]
    fn validate_new_token_rejects_zero_day_expiry() {
        let b = CreateTokenBody {
            expires_in_days: Some(0),
            ..body("x")
        };
        assert!(validate_new_token(b).is_err());
    }

    #[test]
    fn rate_limiter_blocks_after_budget() {
        let rl = RateLimiter::new(2);
        assert!(rl.check("a") && rl.check("a"));
        assert!(!rl.check("a"));
        assert!(rl.check("b"));
    }
}
