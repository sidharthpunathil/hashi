//! HTTP API.
//!
//! | Method   | Path                  | Auth  | Purpose                            |
//! |----------|-----------------------|-------|------------------------------------|
//! | `GET`    | `/healthz`            |       | Liveness probe                     |
//! | `GET`    | `/v1/status`          |       | Version, uptime, agent readiness   |
//! | `POST`   | `/v1/ask`             | token | Queue a prompt                     |
//! | `GET`    | `/v1/ask/{id}`        | token | Poll (or long-poll) a result       |
//! | `DELETE` | `/v1/session/{agent}` | token | Reset a stateful conversation      |
//! | `GET`    | `/v1/me`              | any   | The token's own access and usage   |
//! | `GET`    | `/v1/stats`           | admin | Counters and machine resources     |
//! | `GET`    | `/v1/tokens`          | admin | List tokens                        |
//! | `POST`   | `/v1/tokens`          | admin | Create a token                     |
//! | `PATCH`  | `/v1/tokens/{id}`     | admin | Pause, resume, extend, set limits  |
//! | `DELETE` | `/v1/tokens/{id}`     | admin | Revoke a token                     |
//! | `GET`    | `/v1/audit`           | admin | Recent sharing and access changes  |
//! | `GET`    | `/v1/invites`         | admin | List invite links                  |
//! | `POST`   | `/v1/invites`         | admin | Create a one-time invite link      |
//! | `DELETE` | `/v1/invites/{id}`    | admin | Cancel a pending invite            |
//! | `POST`   | `/v1/claim/preview`   | code  | What an invite would grant         |
//! | `POST`   | `/v1/claim/redeem`    | code  | Use an invite, get a token once    |
//! | `GET`    | `/claim/{code}`, `/me`|       | Pages for people you share with    |
//! | `POST`   | `/mcp`                | token | MCP server for agent clients       |
//!
//! "any" accepts paused and expired tokens, so they can see why they stopped.

mod ask;
mod invites;
mod tokens;

pub use ask::{AskBody, SubmitInput, me_json, request_json, submit, wait_until_done};
pub use invites::{CreateInviteBody, snippets, validate_new_invite};
pub use tokens::{CreateTokenBody, UpdateTokenBody, validate_limits, validate_new_token};

use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use axum::extract::{FromRequestParts, State};
use axum::http::{HeaderValue, StatusCode, header, request::Parts};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::Serialize;
use serde_json::json;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::limit::RequestBodyLimitLayer;

use crate::agent::AgentKind;
use crate::events::Bus;
use crate::guard::ResourceGuard;
use crate::queue::Dispatcher;
use crate::store::{Store, StoreError};
use crate::token::{Token, TokenStatus};
use crate::util::now_ms;

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
    /// Public URL: configured with `--public-url`, or the tunnel's once known.
    pub public_url: RwLock<Option<String>>,
    /// Agent readiness.
    pub agents: Vec<AgentInfo>,
    /// Name shown to people you share with.
    pub name: String,
}

impl Runtime {
    /// The public URL, if one is configured or a tunnel is up.
    #[must_use]
    pub fn public_url(&self) -> Option<String> {
        self.public_url.read().ok().and_then(|u| u.clone())
    }

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
    /// Per-run time limit, in seconds. MCP calls wait this long for an answer.
    pub run_timeout_secs: u64,
    /// Machine protection. Its figures are shown to admins only.
    pub guard: Arc<ResourceGuard>,
    /// Long polls held open per token.
    pub waits: Arc<ask::WaitSlots>,
}

/// Cross-origin access for the configured origins only. With none, no CORS
/// headers are sent and browsers keep other sites' pages away from the API.
fn cors(origins: &[String]) -> Option<CorsLayer> {
    let origins: Vec<HeaderValue> = origins
        .iter()
        .filter_map(|o| match HeaderValue::from_str(o) {
            Ok(v) => Some(v),
            Err(_) => {
                tracing::warn!(origin = %o, "ignoring an unusable CORS origin");
                None
            }
        })
        .collect();
    if origins.is_empty() {
        return None;
    }
    Some(
        CorsLayer::new()
            .allow_origin(AllowOrigin::list(origins))
            .allow_methods([
                axum::http::Method::GET,
                axum::http::Method::POST,
                axum::http::Method::PATCH,
                axum::http::Method::DELETE,
            ])
            .allow_headers([header::AUTHORIZATION, header::CONTENT_TYPE]),
    )
}

/// What the `Host` check accepts besides IP addresses and `localhost`.
#[derive(Debug, Clone)]
struct HostPolicy {
    runtime: Arc<Runtime>,
    extra: Arc<[String]>,
}

/// Refuses requests whose `Host` names some other site, which is what a
/// DNS-rebinding page sends. Native TLS needs no check: a rebound name fails
/// the certificate.
async fn host_check(
    State(policy): State<HostPolicy>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    if !policy.runtime.local_url.starts_with("https://") {
        let host = req
            .headers()
            .get(header::HOST)
            .map(|v| v.to_str().unwrap_or(""));
        let public = policy
            .runtime
            .public_url()
            .and_then(|u| crate::net::url_host(&u));
        if !crate::net::host_allowed(host, public.as_deref(), &policy.extra) {
            return ApiError::coded(
                StatusCode::MISDIRECTED_REQUEST,
                "unknown_host",
                "this Hashi does not answer to that host name; add it with --allowed-host",
            )
            .into_response();
        }
    }
    next.run(req).await
}

/// Builds the router, with the CORS origins and extra host names from `net`.
pub fn router(state: AppState, net: &crate::net::NetLimits) -> Router {
    let hosts = HostPolicy {
        runtime: Arc::clone(&state.runtime),
        extra: net.allowed_hosts.clone().into(),
    };
    let mut app = Router::new()
        .route("/", get(root))
        .route("/healthz", get(|| async { "ok" }))
        .route("/v1/status", get(status))
        .route("/v1/ask", post(ask::ask))
        .route("/v1/ask/{id}", get(ask::poll))
        .route("/v1/session/{agent}", delete(ask::reset_session))
        .route("/v1/me", get(ask::me))
        .route("/v1/stats", get(stats))
        .route("/v1/tokens", get(tokens::list).post(tokens::create))
        .route(
            "/v1/tokens/{id}",
            axum::routing::patch(tokens::update).delete(tokens::revoke),
        )
        .route("/v1/audit", get(tokens::audit))
        .route("/v1/invites", get(invites::list).post(invites::create))
        .route("/v1/invites/{id}", delete(invites::cancel))
        .route("/v1/claim/preview", post(invites::preview))
        .route("/v1/claim/redeem", post(invites::redeem))
        .merge(crate::web::routes())
        .route(
            "/mcp",
            post(crate::mcp::handle).get(crate::mcp::get_not_allowed),
        )
        .fallback(|| async { ApiError::NotFound("no such route") })
        .layer(RequestBodyLimitLayer::new(MAX_BODY_BYTES));
    if let Some(cors) = cors(&net.cors_origins) {
        app = app.layer(cors);
    }
    app.layer(axum::middleware::from_fn_with_state(hosts, host_check))
        .layer(axum::middleware::map_response(security_headers))
        .with_state(state)
}

/// The rate-limit key of the client behind a request (see
/// [`crate::net::client_key`]).
#[derive(Debug, Clone)]
pub struct ClientKey(pub String);

impl<S: Send + Sync> FromRequestParts<S> for ClientKey {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, Self::Rejection> {
        Ok(Self(crate::net::request_client_key(
            &parts.extensions,
            &parts.headers,
        )))
    }
}

/// Charges a failed invite claim (unknown, used or expired code, or a bad
/// body) to the client's address, and turns failures past the budget into
/// 429s. Claims that succeed are never charged or refused, so one client
/// guessing codes cannot lock everyone who shares its address out of their
/// real invite links.
fn claim_outcome<T>(state: &AppState, client: &ClientKey, result: ApiResult<T>) -> ApiResult<T> {
    match result {
        Err(e) if !matches!(e, ApiError::Internal(_)) => {
            if state.limiter.check(&format!("claim:{}", client.0)) {
                Err(e)
            } else {
                Err(ApiError::TooManyRequests(
                    "too many claim attempts, slow down",
                ))
            }
        }
        other => other,
    }
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
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(crate::web::CSP),
    );
    res
}

// ── errors ─────────────────────────────────────────────────────────────────

/// Errors rendered as `{"error": {"code", "message"}}`. 500s also carry an
/// `errorId` that matches a line in the log.
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
    /// Any status with a specific machine-readable code and optional extra fields.
    #[error("{message}")]
    Coded {
        /// HTTP status.
        status: StatusCode,
        /// Machine-readable code, for example `limit_reached`.
        code: &'static str,
        /// Human-readable message.
        message: String,
        /// Extra fields merged into the error object.
        extra: Option<serde_json::Value>,
    },
    /// 500
    #[error("internal error")]
    Internal(#[from] StoreError),
}

impl ApiError {
    /// Builds a [`ApiError::Coded`] error.
    #[must_use]
    pub fn coded(status: StatusCode, code: &'static str, message: impl Into<String>) -> Self {
        Self::Coded {
            status,
            code,
            message: message.into(),
            extra: None,
        }
    }

    fn parts(&self) -> (StatusCode, &'static str) {
        match self {
            Self::Coded { status, code, .. } => (*status, code),
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

    /// The full cause for the log, which the client never sees.
    fn log_detail(&self) -> String {
        match self {
            Self::Internal(e) => e.to_string(),
            other => other.to_string(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code) = self.parts();
        let mut error = json!({ "code": code, "message": self.to_string() });
        if status == StatusCode::INTERNAL_SERVER_ERROR {
            // The id ties the user's report to this log line.
            let error_id = crate::diagnostics::new_error_id();
            tracing::error!(error_id = %error_id, error = %self.log_detail(), "internal error");
            if let Some(obj) = error.as_object_mut() {
                obj.insert("errorId".to_owned(), json!(error_id));
            }
        }
        if let (
            Self::Coded {
                extra: Some(serde_json::Value::Object(extra)),
                ..
            },
            Some(obj),
        ) = (&self, error.as_object_mut())
        {
            obj.extend(extra.clone());
        }
        let mut res = (status, Json(json!({ "error": error }))).into_response();
        if status == StatusCode::UNAUTHORIZED {
            res.headers_mut()
                .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        }
        res
    }
}

/// Result alias for handlers.
pub type ApiResult<T> = Result<T, ApiError>;

// ── auth ───────────────────────────────────────────────────────────────────

/// Fixed-window request limiter keyed by token id, and by client address for
/// invite claims (`claim:<ip>`) and failed sign-ins (`authfail:<ip>`).
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

    /// Whether `key` has used up its budget for the current window, without
    /// recording a hit.
    pub fn exhausted(&self, key: &str) -> bool {
        if self.per_minute == 0 {
            return false;
        }
        let Ok(map) = self.windows.lock() else {
            return true;
        };
        map.get(key).is_some_and(|(start, hits)| {
            start.elapsed() < Duration::from_secs(60) && *hits >= self.per_minute
        })
    }
}

/// Extractor for an unexpired token (paused tokens pass; see [`submit`]).
#[derive(Debug)]
pub struct Auth(pub Token);

/// Extractor for an active admin token.
#[derive(Debug)]
pub struct Admin(pub Token);

/// Extractor for any known token, including paused and expired ones.
#[derive(Debug)]
pub struct AnyToken(pub Token);

fn bearer(parts: &Parts) -> ApiResult<&str> {
    let header = parts
        .headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .ok_or(ApiError::Unauthorized(
            "missing Authorization: Bearer <token> header",
        ))?;
    header
        .strip_prefix("Bearer ")
        .map(str::trim)
        .filter(|t| !t.is_empty() && t.len() <= 256)
        .ok_or(ApiError::Unauthorized("malformed Authorization header"))
}

/// Looks up the bearer token, throttling clients that keep presenting
/// unknown ones. A token that looks up is never refused here: only failures
/// are charged, and the budget only turns further failures into 429s, so a
/// client sending garbage cannot lock out real users who share its address.
fn authenticate(
    parts: &Parts,
    state: &AppState,
    lookup: impl FnOnce(&Store, &str) -> Result<Option<Token>, StoreError>,
    rejection: &'static str,
) -> ApiResult<Token> {
    let secret = bearer(parts)?;
    if let Some(token) = lookup(&state.store, secret)? {
        return Ok(token);
    }
    let key = format!(
        "authfail:{}",
        crate::net::request_client_key(&parts.extensions, &parts.headers)
    );
    if state.limiter.check(&key) {
        Err(ApiError::Unauthorized(rejection))
    } else {
        Err(ApiError::TooManyRequests(
            "too many invalid tokens from this address, slow down",
        ))
    }
}

fn rate_limit(state: &AppState, token: &Token) -> ApiResult<()> {
    if state.limiter.check(&token.id) {
        Ok(())
    } else {
        Err(ApiError::TooManyRequests("rate limit exceeded, slow down"))
    }
}

impl FromRequestParts<AppState> for Auth {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> ApiResult<Self> {
        let token = authenticate(
            parts,
            state,
            |store, secret| store.authenticate(secret),
            "invalid or expired token",
        )?;
        rate_limit(state, &token)?;
        Ok(Self(token))
    }
}

impl FromRequestParts<AppState> for AnyToken {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> ApiResult<Self> {
        let token = authenticate(
            parts,
            state,
            |store, secret| store.authenticate_any(secret),
            "invalid token",
        )?;
        rate_limit(state, &token)?;
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
        if token.status(now_ms()) == TokenStatus::Paused {
            return Err(ApiError::coded(
                StatusCode::FORBIDDEN,
                "token_paused",
                "this admin token is paused",
            ));
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

// ── admin ──────────────────────────────────────────────────────────────────

async fn stats(
    State(state): State<AppState>,
    Admin(_): Admin,
) -> ApiResult<Json<serde_json::Value>> {
    // Memory and disk figures fingerprint the host, so they live here, behind
    // the admin check, and nowhere else.
    Ok(Json(json!({
        "stats": state.store.stats()?,
        "resources": state.guard.snapshot(),
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_limiter_blocks_after_budget() {
        let rl = RateLimiter::new(2);
        assert!(rl.check("a") && rl.check("a"));
        assert!(!rl.check("a"));
        assert!(rl.check("b"));
    }

    #[test]
    fn exhausted_peeks_without_charging() {
        let rl = RateLimiter::new(2);
        assert!(!rl.exhausted("a"));
        assert!(!rl.exhausted("a"));
        rl.check("a");
        assert!(!rl.exhausted("a"));
        rl.check("a");
        assert!(rl.exhausted("a"));
        assert!(!RateLimiter::new(0).exhausted("a"));
    }

    #[test]
    fn coded_errors_carry_extra_fields() {
        let err = ApiError::Coded {
            status: StatusCode::TOO_MANY_REQUESTS,
            code: "limit_reached",
            message: "daily cap".into(),
            extra: Some(json!({ "limit": "dailyRequests" })),
        };
        assert_eq!(err.into_response().status(), StatusCode::TOO_MANY_REQUESTS);
    }

    async fn error_body(err: ApiError) -> (StatusCode, serde_json::Value) {
        let res = err.into_response();
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), 64 * 1024)
            .await
            .unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    #[tokio::test]
    async fn internal_errors_carry_an_error_id() {
        let (status, body) = error_body(ApiError::Internal(StoreError::Poisoned)).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body["error"]["code"], "internal");
        assert_eq!(body["error"]["message"], "internal error");
        let id = body["error"]["errorId"].as_str().unwrap();
        assert!(id.starts_with("E-") && id.len() == 10, "{id}");
    }

    #[tokio::test]
    async fn client_errors_have_no_error_id() {
        let (status, body) = error_body(ApiError::NotFound("nope")).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert!(body["error"].get("errorId").is_none());
    }
}
