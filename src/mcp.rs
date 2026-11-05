//! MCP (Model Context Protocol) server at `POST /mcp`.
//!
//! Streamable HTTP transport with plain JSON responses (no SSE) and
//! JSON-RPC 2.0 messages. The bearer token is the same one used for the
//! HTTP API, and every prompt goes through [`api::submit`], so pause, scope
//! and limits apply exactly as they do for `POST /v1/ask`.
//!
//! Tool failures (paused token, limit reached, agent not allowed, a failed
//! run) are tool results with `isError: true`, as the MCP spec asks.
//! Protocol mistakes are JSON-RPC errors.

use axum::Json;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::agent::AgentKind;
use crate::api::{self, ApiError, AppState, Auth, SubmitInput};
use crate::store::{Request, RequestStatus};
use crate::token::Token;

/// Protocol versions this server speaks, newest first.
const PROTOCOL_VERSIONS: [&str; 3] = ["2025-06-18", "2025-03-26", "2024-11-05"];

/// How long `get_result` waits for an unfinished request, in seconds.
const GET_RESULT_WAIT_SECS: u64 = 30;

const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;

const INSTRUCTIONS: &str = "Hashi relays prompts to coding agents (such as Claude Code and \
Codex) running on its owner's machine. Use an ask_<agent> tool to send a prompt and get the \
answer. Long runs return a request id: pass it to get_result, or to cancel to stop it. \
my_usage shows this token's limits and usage.";

/// A JSON-RPC error: code and message.
struct RpcError(i64, String);

impl RpcError {
    fn invalid_params(e: impl std::fmt::Display) -> Self {
        Self(INVALID_PARAMS, format!("invalid params: {e}"))
    }
}

/// `GET /mcp`: this server does not offer an SSE stream.
pub async fn get_not_allowed() -> Response {
    let mut res = (
        StatusCode::METHOD_NOT_ALLOWED,
        Json(json!({ "error": {
            "code": "method_not_allowed",
            "message": "use POST for MCP messages; this server does not stream",
        }})),
    )
        .into_response();
    res.headers_mut()
        .insert(header::ALLOW, HeaderValue::from_static("POST"));
    res
}

/// `POST /mcp`: one JSON-RPC message in, one JSON response (or 202) out.
pub async fn handle(State(state): State<AppState>, Auth(token): Auth, body: Bytes) -> Response {
    let Ok(msg) = serde_json::from_slice::<Value>(&body) else {
        return rpc_error(
            Value::Null,
            PARSE_ERROR,
            "parse error: body is not valid JSON",
        );
    };
    let Value::Object(msg) = msg else {
        let text = if msg.is_array() {
            "batches are not supported"
        } else {
            "invalid request: expected a JSON-RPC object"
        };
        return rpc_error(Value::Null, INVALID_REQUEST, text);
    };

    let id = msg.get("id").cloned();
    let Some(method) = msg.get("method").and_then(Value::as_str) else {
        // A response from the client (to a request we never send) is accepted.
        if id.is_some() && (msg.contains_key("result") || msg.contains_key("error")) {
            return StatusCode::ACCEPTED.into_response();
        }
        return rpc_error(
            id.unwrap_or(Value::Null),
            INVALID_REQUEST,
            "invalid request: missing method",
        );
    };
    // Notifications (no id) get no answer.
    let Some(id) = id else {
        return StatusCode::ACCEPTED.into_response();
    };
    let params = msg.get("params").cloned().unwrap_or(Value::Null);

    match dispatch(&state, &token, method, params).await {
        Ok(result) => Json(json!({ "jsonrpc": "2.0", "id": id, "result": result })).into_response(),
        Err(RpcError(code, message)) => rpc_error(id, code, &message),
    }
}

fn rpc_error(id: Value, code: i64, message: &str) -> Response {
    Json(json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    }))
    .into_response()
}

async fn dispatch(
    state: &AppState,
    token: &Token,
    method: &str,
    params: Value,
) -> Result<Value, RpcError> {
    match method {
        "initialize" => Ok(initialize(&params)),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tools(state, token) })),
        "tools/call" => {
            let call: CallParams =
                serde_json::from_value(params).map_err(RpcError::invalid_params)?;
            call_tool(state, token, call).await
        }
        other => Err(RpcError(
            METHOD_NOT_FOUND,
            format!("method not found: {other}"),
        )),
    }
}

fn initialize(params: &Value) -> Value {
    let asked = params.get("protocolVersion").and_then(Value::as_str);
    let version = asked
        .filter(|v| PROTOCOL_VERSIONS.contains(v))
        .unwrap_or(PROTOCOL_VERSIONS[0]);
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": "hashi", "version": env!("CARGO_PKG_VERSION") },
        "instructions": INSTRUCTIONS,
    })
}

// ── tools ──────────────────────────────────────────────────────────────────

fn ask_tool_name(agent: AgentKind) -> String {
    format!("ask_{}", agent.as_str())
}

/// The agent behind an `ask_<agent>` tool name.
fn ask_tool_agent(name: &str) -> Option<AgentKind> {
    let agent: AgentKind = name.strip_prefix("ask_")?.parse().ok()?;
    // Only canonical names, so `ask_claude-code` is not a second spelling.
    (ask_tool_name(agent) == name).then_some(agent)
}

fn tools(state: &AppState, token: &Token) -> Vec<Value> {
    let mut list: Vec<Value> = AgentKind::ALL
        .into_iter()
        .filter(|a| token.allows(*a))
        // Crush is opt-in: not listed unless the owner enabled it.
        .filter(|a| *a != AgentKind::Crush || state.runtime.agents.iter().any(|i| i.agent == *a))
        .map(|agent| {
            json!({
                "name": ask_tool_name(agent),
                "description": format!(
                    "Send a prompt to {} on the Hashi host and return its answer. \
                     Waits for the run to finish; if it takes too long, returns a \
                     request id to pass to get_result.",
                    agent.display_name()
                ),
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "prompt": { "type": "string", "description": "The prompt to send." },
                        "model": { "type": "string", "description": "Optional model override." },
                    },
                    "required": ["prompt"],
                },
            })
        })
        .collect();
    list.push(json!({
        "name": "get_result",
        "description": "Fetch the result of an earlier ask_* call by request id. \
                        Waits up to 30 seconds if it is still running.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "Request id returned by ask_*." },
            },
            "required": ["id"],
        },
    }));
    list.push(json!({
        "name": "cancel",
        "description": "Cancel an earlier ask_* request by id while it is queued or \
                        running. A running agent is stopped.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "id": { "type": "string", "description": "Request id returned by ask_*." },
            },
            "required": ["id"],
        },
    }));
    list.push(json!({
        "name": "my_usage",
        "description": "Show this token's access: status, allowed agents, limits, \
                        usage today and this month, and when limits reset.",
        "inputSchema": { "type": "object", "properties": {}, "required": [] },
    }));
    list
}

#[derive(Debug, Deserialize)]
struct CallParams {
    name: String,
    #[serde(default)]
    arguments: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct AskArgs {
    prompt: String,
    #[serde(default)]
    model: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GetResultArgs {
    id: String,
}

fn args<T: serde::de::DeserializeOwned>(raw: Option<Value>) -> Result<T, RpcError> {
    let raw = raw.unwrap_or_else(|| json!({}));
    serde_json::from_value(raw).map_err(RpcError::invalid_params)
}

async fn call_tool(state: &AppState, token: &Token, call: CallParams) -> Result<Value, RpcError> {
    let outcome = match call.name.as_str() {
        name if ask_tool_agent(name).is_some() => {
            let agent =
                ask_tool_agent(name).ok_or_else(|| RpcError::invalid_params("unknown tool"))?;
            ask(state, token, agent, args(call.arguments)?).await
        }
        "get_result" => get_result(state, token, args(call.arguments)?).await,
        "cancel" => {
            let a: GetResultArgs = args(call.arguments)?;
            // Ownership is checked as `get_result` checks it.
            api::cancel_request(state, token, &a.id)
                .await
                .map(|r| (format!("Cancelled request {}.", r.id), false))
        }
        "my_usage" => api::me_json(state, token).await.map(|v| {
            (
                serde_json::to_string_pretty(&v).unwrap_or_else(|_| v.to_string()),
                false,
            )
        }),
        other => {
            return Err(RpcError(INVALID_PARAMS, format!("unknown tool: {other}")));
        }
    };
    let (text, is_error) = outcome.unwrap_or_else(|e: ApiError| (e.to_string(), true));
    Ok(json!({
        "content": [{ "type": "text", "text": text }],
        "isError": is_error,
    }))
}

async fn ask(
    state: &AppState,
    token: &Token,
    agent: AgentKind,
    a: AskArgs,
) -> api::ApiResult<(String, bool)> {
    let req = api::submit(
        state,
        token,
        SubmitInput {
            message: a.prompt,
            agent: Some(agent.as_str().to_owned()),
            model: a.model,
        },
    )
    .await?;
    let secs = state.run_timeout_secs;
    let req = api::wait_until_done(state, &token.id, &req.id, secs, secs).await?;
    Ok(outcome(req))
}

async fn get_result(
    state: &AppState,
    token: &Token,
    a: GetResultArgs,
) -> api::ApiResult<(String, bool)> {
    let req = state
        .store
        .get_request(&a.id)?
        .ok_or(ApiError::NotFound("request not found"))?;
    if req.token_id != token.id && !api::reads_any_request(token) {
        // Same answer as a missing id, as for `GET /v1/ask/{id}`.
        return Err(ApiError::NotFound("request not found"));
    }
    let req = if req.status.is_terminal() {
        req
    } else {
        api::wait_until_done(
            state,
            &token.id,
            &req.id,
            GET_RESULT_WAIT_SECS,
            GET_RESULT_WAIT_SECS,
        )
        .await?
    };
    Ok(outcome(req))
}

/// Text and `isError` flag for a request's current state.
fn outcome(r: Request) -> (String, bool) {
    match r.status {
        RequestStatus::Completed => (r.response.unwrap_or_default(), false),
        RequestStatus::Failed => (r.error.unwrap_or_else(|| "the run failed".to_owned()), true),
        RequestStatus::Queued | RequestStatus::Running => (
            format!(
                "Still {} (request id {}). Call get_result with this id to fetch the answer.",
                r.status.as_str(),
                r.id
            ),
            false,
        ),
    }
}
