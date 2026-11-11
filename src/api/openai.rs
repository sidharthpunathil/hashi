//! OpenAI-compatible endpoints: `POST /v1/chat/completions` and
//! `GET /v1/models`.
//!
//! The same bearer tokens work here, and every prompt goes through
//! [`submit`], so pause, scope, limits, queue and account rules apply exactly
//! as they do for `POST /v1/ask`. `model` picks the agent (`claude`) and can
//! carry the agent's own model after a slash (`claude/opus`).
//!
//! Errors use OpenAI's shape, `{"error": {"message", "type", "code"}}`, with
//! the same status codes as the rest of the API. Tool calling, images and
//! token-by-token streaming are not supported: `stream: true` sends the whole
//! answer as one chunk once the run finishes, with comment lines as
//! keep-alives until then. Unknown fields such as `temperature` and `tools`
//! are ignored.

use std::convert::Infallible;
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::{HeaderValue, StatusCode};
use axum::response::sse::{Event as SseEvent, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::broadcast::error::RecvError;

use super::{
    ApiError, AppState, Auth, MAX_WAIT_SECS, SubmitInput, ask::MAX_WAITS_PER_TOKEN, submit,
    wait_until_done,
};
use crate::agent::{AgentKind, Usage};
use crate::events::Event;
use crate::store::{Request, RequestStatus};
use crate::token::TokenStatus;

/// How often a stream sends a `: queued` or `: running` comment while the
/// request waits or runs.
pub const STREAM_KEEPALIVE: Duration = Duration::from_secs(1);

/// Time a stream allows past its queue wait and run timeout.
pub const STREAM_GRACE: Duration = Duration::from_secs(10);

/// Longest a stream stays open: as long again as the run timeout for the
/// queue wait, then the run timeout, then [`STREAM_GRACE`]. A request held
/// longer (its token paused, or the machine short of memory) keeps its
/// place in the queue, but the stream ends and frees its wait slot.
#[must_use]
pub fn stream_deadline(run_timeout_secs: u64) -> Duration {
    Duration::from_secs(run_timeout_secs.saturating_mul(2)).saturating_add(STREAM_GRACE)
}

/// The OpenAI-compatible routes, merged into the main router.
pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/chat/completions", post(chat_completions))
        .route("/v1/models", get(models))
        .route("/v1/models/{*model}", get(model))
}

// ── errors ─────────────────────────────────────────────────────────────────

/// An [`ApiError`] rendered in OpenAI's error shape.
#[derive(Debug)]
pub struct OpenAiError(pub ApiError);

impl From<ApiError> for OpenAiError {
    fn from(e: ApiError) -> Self {
        Self(e)
    }
}

/// OpenAI's error `type` for a status code.
fn error_type(status: StatusCode) -> &'static str {
    match status.as_u16() {
        401 => "authentication_error",
        403 => "permission_error",
        404 => "not_found_error",
        429 => "rate_limit_error",
        s if s >= 500 => "server_error",
        _ => "invalid_request_error",
    }
}

/// The request field an error is about, as OpenAI's `param`.
fn error_param(code: &str) -> Option<&'static str> {
    match code {
        "model_not_found" => Some("model"),
        "unsupported_content" | "unsupported_role" => Some("messages"),
        _ => None,
    }
}

fn error_object(status: StatusCode, code: &str, message: &str) -> Value {
    json!({
        "message": message,
        "type": error_type(status),
        "param": error_param(code),
        "code": code,
    })
}

impl IntoResponse for OpenAiError {
    fn into_response(self) -> Response {
        let e = self.0;
        let (status, code) = e.parts();
        let mut error = error_object(status, code, &e.to_string());
        if status == StatusCode::INTERNAL_SERVER_ERROR {
            let error_id = crate::diagnostics::new_error_id();
            tracing::error!(error_id = %error_id, error = %e.log_detail(), "internal error");
            if let Some(obj) = error.as_object_mut() {
                obj.insert("errorId".to_owned(), json!(error_id));
            }
        }
        if let (
            ApiError::Coded {
                extra: Some(Value::Object(extra)),
                ..
            },
            Some(obj),
        ) = (&e, error.as_object_mut())
        {
            obj.extend(extra.clone());
        }
        let mut res = (status, Json(json!({ "error": error }))).into_response();
        super::error_headers(&mut res, &e);
        res
    }
}

type OaResult<T> = Result<T, OpenAiError>;

// ── request ────────────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct ChatBody {
    #[serde(default)]
    model: Option<String>,
    messages: Vec<ChatMessage>,
    #[serde(default)]
    stream: Option<bool>,
    #[serde(default)]
    stream_options: Option<StreamOptions>,
    /// Hashi extension: seconds to hold a non-streaming request before
    /// answering 202 (default and maximum [`MAX_WAIT_SECS`]).
    #[serde(default)]
    wait: Option<u64>,
}

#[derive(Debug, Default, Deserialize)]
struct StreamOptions {
    #[serde(default)]
    include_usage: Option<bool>,
}

#[derive(Debug, Deserialize)]
struct ChatMessage {
    role: String,
    #[serde(default)]
    content: Value,
}

/// Splits `model` into the agent and the agent's own model: `claude` or
/// `claude/opus`. With no model the server's default agent answers.
fn parse_model(model: Option<&str>) -> Result<(Option<AgentKind>, Option<String>), ApiError> {
    let Some(model) = model.map(str::trim).filter(|m| !m.is_empty()) else {
        return Ok((None, None));
    };
    let (agent, sub) = match model.split_once('/') {
        Some((agent, sub)) => (agent, Some(sub)),
        None => (model, None),
    };
    let agent: AgentKind = agent.parse().map_err(|_| {
        ApiError::coded(
            StatusCode::BAD_REQUEST,
            "model_not_found",
            format!(
                "unknown model `{model}`; use an agent name such as claude, codex, gemini, \
                 opencode, qwen or cursor, optionally followed by /<model>"
            ),
        )
    })?;
    match sub {
        Some("") => Err(ApiError::BadRequest(
            "model is empty after the slash; use agent or agent/model".to_owned(),
        )),
        // `submit` checks the agent's model with `is_safe_arg`.
        other => Ok((Some(agent), other.map(str::to_owned))),
    }
}

fn unsupported_content() -> ApiError {
    ApiError::coded(
        StatusCode::BAD_REQUEST,
        "unsupported_content",
        "only text content is supported (a string, or parts with type \"text\")",
    )
}

/// The text of one message's `content`: a string, or `text` parts.
fn text_of(content: &Value) -> Result<String, ApiError> {
    match content {
        Value::Null => Ok(String::new()),
        Value::String(s) => Ok(s.clone()),
        Value::Array(parts) => {
            let texts = parts
                .iter()
                .map(|p| match (p.get("type"), p.get("text")) {
                    (Some(Value::String(kind)), Some(Value::String(text))) if kind == "text" => {
                        Ok(text.as_str())
                    }
                    _ => Err(unsupported_content()),
                })
                .collect::<Result<Vec<&str>, _>>()?;
            Ok(texts.join("\n"))
        }
        _ => Err(unsupported_content()),
    }
}

/// Builds one prompt from a chat: system and developer messages first as an
/// instruction block, then earlier turns as a plain transcript, then the
/// last user turn. A lone user message is sent as it is.
fn build_prompt(messages: &[ChatMessage]) -> Result<String, ApiError> {
    let mut instructions = Vec::new();
    let mut turns: Vec<(&'static str, String)> = Vec::new();
    for m in messages {
        let text = text_of(&m.content)?;
        match m.role.as_str() {
            "system" | "developer" => instructions.push(text),
            "user" => turns.push(("User", text)),
            "assistant" => turns.push(("Assistant", text)),
            other => {
                return Err(ApiError::coded(
                    StatusCode::BAD_REQUEST,
                    "unsupported_role",
                    format!("`{other}` messages are not supported; tool calling is not available"),
                ));
            }
        }
    }
    let last = match turns.pop() {
        Some(("User", text)) => text,
        _ => {
            return Err(ApiError::BadRequest(
                "the last message must be from the user".to_owned(),
            ));
        }
    };
    let mut prompt = String::new();
    let instructions: Vec<&str> = instructions
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    if !instructions.is_empty() {
        prompt.push_str("Instructions:\n");
        prompt.push_str(&instructions.join("\n\n"));
        prompt.push_str("\n\n");
    }
    if turns.is_empty() {
        prompt.push_str(&last);
    } else {
        prompt.push_str("Conversation so far:\n\n");
        for (who, text) in &turns {
            prompt.push_str(&format!("{who}: {}\n\n", text.trim()));
        }
        prompt.push_str(&format!("User: {last}"));
    }
    Ok(prompt)
}

// ── response ───────────────────────────────────────────────────────────────

fn usage_json(usage: Option<Usage>) -> Option<Value> {
    usage.map(|u| {
        json!({
            "prompt_tokens": u.input_tokens,
            "completion_tokens": u.output_tokens,
            "total_tokens": u.input_tokens.saturating_add(u.output_tokens),
        })
    })
}

/// Fields every completion and chunk carries.
#[derive(Debug, Clone)]
struct Meta {
    id: String,
    created: i64,
    model: String,
    /// Whether the client asked for a usage chunk, in which case every
    /// other chunk carries `"usage": null`, as OpenAI's do.
    include_usage: bool,
}

impl Meta {
    fn new(req: &Request, model: String) -> Self {
        Self {
            id: format!("chatcmpl-{}", req.id),
            created: req.created_at / 1000,
            model,
            include_usage: false,
        }
    }

    /// A `chat.completion.chunk` with these `choices`.
    fn chunk_with(&self, choices: Value) -> Value {
        let mut v = json!({
            "id": self.id,
            "object": "chat.completion.chunk",
            "created": self.created,
            "model": self.model,
            "system_fingerprint": null,
            "choices": choices,
        });
        if let (true, Some(obj)) = (self.include_usage, v.as_object_mut()) {
            obj.insert("usage".to_owned(), Value::Null);
        }
        v
    }

    fn chunk(&self, delta: Value, finish: Option<&str>) -> Value {
        self.chunk_with(json!([{
            "index": 0,
            "delta": delta,
            "logprobs": null,
            "finish_reason": finish,
        }]))
    }

    /// The last chunk before `[DONE]` when the client asked for usage: no
    /// choices, and the run's usage (`null` when the agent reported none).
    fn usage_chunk(&self, usage: Option<Usage>) -> Value {
        let mut v = self.chunk_with(json!([]));
        if let Some(obj) = v.as_object_mut() {
            obj.insert("usage".to_owned(), usage_json(usage).unwrap_or(Value::Null));
        }
        v
    }
}

fn completion(meta: &Meta, req: &Request) -> Value {
    let mut v = json!({
        "id": meta.id,
        "object": "chat.completion",
        "created": meta.created,
        "model": meta.model,
        "system_fingerprint": null,
        "choices": [{
            "index": 0,
            "message": {
                "role": "assistant",
                "content": req.response.clone().unwrap_or_default(),
                "refusal": null,
            },
            "logprobs": null,
            "finish_reason": "stop",
        }],
    });
    if let (Some(u), Some(obj)) = (usage_json(req.usage), v.as_object_mut()) {
        obj.insert("usage".to_owned(), u);
    }
    v
}

fn run_failed(req: &Request) -> (StatusCode, Value) {
    let message = req
        .error
        .clone()
        .unwrap_or_else(|| "the run failed".to_owned());
    let status = StatusCode::BAD_GATEWAY;
    (status, error_object(status, "agent_failed", &message))
}

/// 202 for a request still queued or running when the wait ends.
fn not_ready(req: &Request) -> Value {
    json!({
        "id": req.id,
        "object": "hashi.request",
        "status": req.status,
        "poll": format!("/v1/ask/{}", req.id),
        "message": format!(
            "the answer is not ready yet; read it with GET /v1/ask/{}?wait=30, or send \"stream\": true next time",
            req.id
        ),
    })
}

fn with_request_id(mut res: Response, id: &str) -> Response {
    if let Ok(v) = HeaderValue::from_str(id) {
        res.headers_mut().insert("x-hashi-request-id", v);
    }
    res
}

// ── handlers ───────────────────────────────────────────────────────────────

async fn models(
    State(state): State<AppState>,
    auth: Result<Auth, ApiError>,
) -> OaResult<Json<Value>> {
    let Auth(token) = auth?;
    let data: Vec<Value> = super::offered_agents(&state, &token)?
        .into_iter()
        .map(model_json)
        .collect();
    Ok(Json(json!({ "object": "list", "data": data })))
}

/// One agent as an OpenAI `model` object. Agents have no release date to
/// give, so `created` is 0.
fn model_json(agent: AgentKind) -> Value {
    json!({ "id": agent.as_str(), "object": "model", "created": 0, "owned_by": "hashi" })
}

/// `GET /v1/models/{model}`: one model this token may use, as `/v1/models`
/// lists it. `claude/opus` names the agent `claude` and answers with it.
async fn model(
    State(state): State<AppState>,
    auth: Result<Auth, ApiError>,
    Path(name): Path<String>,
) -> OaResult<Json<Value>> {
    let Auth(token) = auth?;
    let agent = name
        .split_once('/')
        .map_or(name.as_str(), |(a, _)| a)
        .parse::<AgentKind>()
        .ok()
        .filter(|a| {
            super::offered_agents(&state, &token)
                .map(|o| o.contains(a))
                .unwrap_or(false)
        });
    match agent {
        Some(a) => Ok(Json(model_json(a))),
        None => Err(ApiError::coded(
            StatusCode::NOT_FOUND,
            "model_not_found",
            format!("this token has no model `{name}`; GET /v1/models lists the ones it may use"),
        )
        .into()),
    }
}

async fn chat_completions(
    State(state): State<AppState>,
    auth: Result<Auth, ApiError>,
    body: Bytes,
) -> OaResult<Response> {
    let Auth(token) = auth?;
    let body: ChatBody = serde_json::from_slice(&body)
        .map_err(|e| ApiError::BadRequest(format!("invalid request body: {e}")))?;
    if body.messages.is_empty() {
        return Err(ApiError::BadRequest("messages must not be empty".to_owned()).into());
    }
    let (agent, model) = parse_model(body.model.as_deref())?;
    let message = build_prompt(&body.messages)?;
    let shown_model = body
        .model
        .as_deref()
        .map(str::trim)
        .filter(|m| !m.is_empty())
        .map_or_else(|| state.default_agent.as_str().to_owned(), str::to_owned);
    let input = SubmitInput {
        message,
        agent: agent.map(|a| a.as_str().to_owned()),
        model,
        account: None,
        hops: 0,
    };

    if body.stream.unwrap_or(false) {
        // A stream holds a long poll for its whole life; take the slot before
        // queueing anything, so a refused stream leaves no orphan behind.
        let Some(slot) = state.waits.try_acquire(&token.id) else {
            return Err(ApiError::coded(
                StatusCode::TOO_MANY_REQUESTS,
                "too_many_waits",
                format!(
                    "this token already holds {MAX_WAITS_PER_TOKEN} open waits or streams; \
                     let one finish, or send without \"stream\" and poll the request id"
                ),
            )
            .into());
        };
        let req = submit(&state, &token, input).await?;
        let mut meta = Meta::new(&req, shown_model);
        meta.include_usage = body
            .stream_options
            .unwrap_or_default()
            .include_usage
            .unwrap_or(false);
        let id = req.id.clone();
        let (tx, rx) = futures::channel::mpsc::channel::<SseEvent>(16);
        tokio::spawn(async move {
            let _slot = slot;
            stream(&state, &id, &meta, tx).await;
        });
        let res = Sse::new(rx.map(Ok::<_, Infallible>)).into_response();
        return Ok(with_request_id(res, &req.id));
    }

    let secs = body.wait.unwrap_or(MAX_WAIT_SECS);
    // Only the id is needed while waiting: the body (the whole transcript)
    // and the submitted request are not held for the wait.
    drop(body);
    let id = submit(&state, &token, input).await?.id;
    let req = wait_until_done(&state, &token.id, &id, secs, MAX_WAIT_SECS).await?;
    let meta = Meta::new(&req, shown_model);
    let res = match req.status {
        RequestStatus::Completed => (StatusCode::OK, Json(completion(&meta, &req))).into_response(),
        RequestStatus::Failed => {
            let (status, error) = run_failed(&req);
            (status, Json(json!({ "error": error }))).into_response()
        }
        RequestStatus::Queued | RequestStatus::Running => {
            (StatusCode::ACCEPTED, Json(not_ready(&req))).into_response()
        }
    };
    Ok(with_request_id(res, &req.id))
}

fn data(v: &Value) -> SseEvent {
    SseEvent::default().data(v.to_string())
}

/// Why a stream whose request is still queued or running ends now: its
/// token is no longer active, or the stream is past its deadline (see
/// [`stream_deadline`]). The request itself is left as it is.
fn stream_cut(state: &AppState, req: &Request, past_deadline: bool) -> Option<Value> {
    let token = match state.store.get_token(&req.token_id) {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!(id = %req.id, error = %e, "stream could not read its token");
            None
        }
    };
    let (status, code, message) = match token.map(|t| t.status(crate::util::now_ms())) {
        Some(TokenStatus::Active) if !past_deadline => return None,
        Some(TokenStatus::Active) => (
            StatusCode::GATEWAY_TIMEOUT,
            "stream_timeout",
            format!(
                "the answer is not ready yet; read it later with GET /v1/ask/{}",
                req.id
            ),
        ),
        Some(TokenStatus::Paused) => (
            StatusCode::FORBIDDEN,
            "token_paused",
            format!(
                "{}; request {} keeps its place in the queue",
                super::TOKEN_PAUSED,
                req.id
            ),
        ),
        Some(TokenStatus::Expired) | None => (
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            super::INVALID_TOKEN.to_owned(),
        ),
    };
    Some(json!({ "error": error_object(status, code, &message) }))
}

/// Feeds one streamed completion: a role chunk, a comment with the status
/// every [`STREAM_KEEPALIVE`] until the run ends, then the answer, the
/// finish chunk and `[DONE]`, or an error event. Stops early if the client
/// goes away, and ends with an error event and `[DONE]` once the token is
/// no longer active or the stream passes [`stream_deadline`].
async fn stream(
    state: &AppState,
    id: &str,
    meta: &Meta,
    mut tx: futures::channel::mpsc::Sender<SseEvent>,
) {
    // Subscribe before the first read, so a finish in between is not missed.
    let mut events = state.bus.subscribe();
    let deadline = tokio::time::Instant::now().checked_add(stream_deadline(state.run_timeout_secs));
    let role = meta.chunk(json!({ "role": "assistant", "content": "" }), None);
    if tx.send(data(&role)).await.is_err() {
        return;
    }
    let req = loop {
        let req = match state.store.get_request(id) {
            Ok(Some(r)) => r,
            Ok(None) => {
                let status = StatusCode::NOT_FOUND;
                let e = error_object(status, "not_found", "request not found");
                let _ = tx.send(data(&json!({ "error": e }))).await;
                return;
            }
            Err(e) => {
                tracing::warn!(id = %id, error = %e, "stream could not read its request");
                let status = StatusCode::INTERNAL_SERVER_ERROR;
                let e = error_object(status, "internal", "internal error");
                let _ = tx.send(data(&json!({ "error": e }))).await;
                return;
            }
        };
        if req.status.is_terminal() {
            break req;
        }
        let late = deadline.is_some_and(|d| tokio::time::Instant::now() >= d);
        if let Some(error) = stream_cut(state, &req, late) {
            for ev in [data(&error), SseEvent::default().data("[DONE]")] {
                if tx.send(ev).await.is_err() {
                    return;
                }
            }
            return;
        }
        let comment = SseEvent::default().comment(req.status.as_str());
        // Read again after the wait; do not hold this copy (and its prompt)
        // while the stream idles.
        drop(req);
        if tx.send(comment).await.is_err() {
            return;
        }
        let _ = tokio::time::timeout(STREAM_KEEPALIVE, async {
            loop {
                match events.recv().await {
                    Ok(Event::Started { id: got, .. } | Event::Finished { id: got, .. })
                        if got == id =>
                    {
                        return;
                    }
                    // Let the timeout end the wait instead of spinning.
                    Err(RecvError::Closed) => std::future::pending::<()>().await,
                    Ok(_) | Err(RecvError::Lagged(_)) => {}
                }
            }
        })
        .await;
    };
    if req.status != RequestStatus::Completed {
        let (_, e) = run_failed(&req);
        let _ = tx.send(data(&json!({ "error": e }))).await;
        return;
    }
    let content = meta.chunk(
        json!({ "content": req.response.clone().unwrap_or_default() }),
        None,
    );
    let finish = meta.chunk(json!({}), Some("stop"));
    let mut out = vec![data(&content), data(&finish)];
    if meta.include_usage {
        out.push(data(&meta.usage_chunk(req.usage)));
    }
    out.push(SseEvent::default().data("[DONE]"));
    for ev in out {
        if tx.send(ev).await.is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn msgs(v: Value) -> Vec<ChatMessage> {
        serde_json::from_value(v).unwrap_or_default()
    }

    #[test]
    fn a_stream_lasts_a_queue_wait_and_a_run_timeout() {
        assert_eq!(stream_deadline(600), Duration::from_secs(1_210));
        assert_eq!(stream_deadline(1), Duration::from_secs(12));
        assert_eq!(stream_deadline(u64::MAX), Duration::MAX);
    }

    #[test]
    fn model_names_pick_the_agent_and_its_model() {
        assert_eq!(
            parse_model(Some("claude")).ok(),
            Some((Some(AgentKind::Claude), None))
        );
        assert_eq!(
            parse_model(Some("claude/opus")).ok(),
            Some((Some(AgentKind::Claude), Some("opus".to_owned())))
        );
        assert_eq!(
            parse_model(Some("gemini/gemini-2.5-pro")).ok(),
            Some((Some(AgentKind::Gemini), Some("gemini-2.5-pro".to_owned())))
        );
        assert_eq!(parse_model(None).ok(), Some((None, None)));
        assert!(parse_model(Some("gpt-4o")).is_err());
        assert!(parse_model(Some("claude/")).is_err());
    }

    #[test]
    fn a_lone_user_message_is_the_prompt() {
        let m = msgs(json!([{ "role": "user", "content": "hello" }]));
        assert_eq!(build_prompt(&m).ok().as_deref(), Some("hello"));
    }

    #[test]
    fn system_first_then_transcript_then_last_user_turn() {
        let m = msgs(json!([
            { "role": "system", "content": "Be brief." },
            { "role": "user", "content": "hi" },
            { "role": "assistant", "content": [{ "type": "text", "text": "hello" }] },
            { "role": "developer", "content": "Use British spelling." },
            { "role": "user", "content": "colour?" },
        ]));
        assert_eq!(
            build_prompt(&m).ok().as_deref(),
            Some(
                "Instructions:\nBe brief.\n\nUse British spelling.\n\n\
                 Conversation so far:\n\nUser: hi\n\nAssistant: hello\n\nUser: colour?"
            )
        );
    }

    #[test]
    fn non_text_content_and_tool_turns_are_refused() {
        let image = msgs(json!([{ "role": "user", "content": [
            { "type": "image_url", "image_url": { "url": "data:," } }
        ]}]));
        assert!(build_prompt(&image).is_err());
        let tool = msgs(json!([
            { "role": "user", "content": "hi" },
            { "role": "tool", "content": "42" },
        ]));
        assert!(build_prompt(&tool).is_err());
        let ends_with_assistant = msgs(json!([
            { "role": "user", "content": "hi" },
            { "role": "assistant", "content": "hello" },
        ]));
        assert!(build_prompt(&ends_with_assistant).is_err());
    }
}
