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
const INTERNAL_ERROR: i64 = -32603;

/// What `initialize` tells the client about this server, with the
/// longest an `ask_<agent>` call waits.
fn instructions(wait_secs: u64) -> String {
    format!(
        "Hashi relays prompts to coding agents (such as Claude Code and Codex) that run on \
its owner's machine, in a folder the owner chose. The agent cannot see your files or this \
conversation, so put everything it needs in the prompt (up to 32 KB). \
Each agent this token may use has an ask_<agent> tool. A call waits for the agent to finish, \
up to {wait_secs} seconds; runs often take minutes. If the answer is not ready by then, the \
result says the request is still queued or running and gives its request id. Then call \
get_result with that id, which waits up to {GET_RESULT_WAIT_SECS} seconds per call, until it \
returns the answer. Do not send the prompt again: that starts a second run. Call cancel with \
the id to stop a run you no longer need. \
The owner sets this token's limits: prompts and spend per day and month, and prompts in \
flight at once. A refused call says which limit applies and when to try again. my_usage \
shows the limits and what is used; request_more asks the owner for more, and nothing \
changes until they accept."
    )
}

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
        "initialize" => Ok(initialize(state, &params)),
        "ping" => Ok(json!({})),
        "tools/list" => {
            let agents = api::offered_agents(state, token)
                .map_err(|e| RpcError(INTERNAL_ERROR, e.to_string()))?;
            Ok(json!({ "tools": tools(state.run_timeout_secs, &agents) }))
        }
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

fn initialize(state: &AppState, params: &Value) -> Value {
    let asked = params.get("protocolVersion").and_then(Value::as_str);
    let version = asked
        .filter(|v| PROTOCOL_VERSIONS.contains(v))
        .unwrap_or(PROTOCOL_VERSIONS[0]);
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": {} },
        "serverInfo": { "name": "hashi", "version": env!("CARGO_PKG_VERSION") },
        "instructions": instructions(state.run_timeout_secs),
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

/// The schema of a request id argument.
fn id_schema() -> Value {
    json!({
        "type": "string",
        "format": "uuid",
        "minLength": 1,
        "maxLength": 64,
        "description": "The request id from an earlier ask_* result that said the request \
                        was still queued or running.",
    })
}

/// The tools for a token that may use `agents`, where an `ask_<agent>`
/// call waits up to `wait_secs` for its answer.
fn tools(wait_secs: u64, agents: &[AgentKind]) -> Vec<Value> {
    let mut list: Vec<Value> = agents
        .iter()
        .map(|&agent| {
            json!({
                "name": ask_tool_name(agent),
                "title": format!("Ask {}", agent.display_name()),
                "description": format!(
                    "Send one prompt to {name} on the Hashi owner's machine and return its \
                     answer as text. Waits for the run to finish, up to {wait_secs} seconds. \
                     If it is still queued or running after that, or this token already has \
                     {waits} calls waiting, it returns at once with the request id instead: \
                     pass that id to get_result, and do not send the prompt again. Returns \
                     isError when the token is paused or at a limit, or the run fails.",
                    name = agent.display_name(),
                    waits = api::MAX_WAITS_PER_TOKEN,
                ),
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "prompt": {
                            "type": "string",
                            "minLength": 1,
                            "maxLength": api::MAX_MESSAGE_BYTES,
                            "description": "The whole prompt, up to 32 KB of UTF-8. The agent \
                                            cannot see your files or this conversation, so \
                                            include everything it needs.",
                        },
                        "model": {
                            "type": "string",
                            "minLength": 1,
                            "maxLength": 128,
                            "pattern": "^[A-Za-z0-9][A-Za-z0-9._:/@\\[\\]-]*$",
                            "description": "The agent's own model name, such as sonnet or opus \
                                            for Claude Code. Leave it out to use the default \
                                            the owner set.",
                        },
                    },
                    "required": ["prompt"],
                    "additionalProperties": false,
                },
                "annotations": {
                    "readOnlyHint": false,
                    "destructiveHint": false,
                    "idempotentHint": false,
                    "openWorldHint": true,
                },
            })
        })
        .collect();
    list.push(json!({
        "name": "get_result",
        "title": "Get a result",
        "description": format!(
            "Read the answer to an earlier ask_* request by its id. While the request is \
             queued or running this waits up to {GET_RESULT_WAIT_SECS} seconds; if it is still \
             not done, the result says so and you call get_result again. Returns isError when \
             the run failed or was cancelled, or for an id this token did not send."
        ),
        "inputSchema": {
            "type": "object",
            "properties": { "id": id_schema() },
            "required": ["id"],
            "additionalProperties": false,
        },
        "annotations": { "readOnlyHint": true, "openWorldHint": false },
    }));
    list.push(json!({
        "name": "cancel",
        "title": "Cancel a request",
        "description": "Stop an earlier ask_* request by its id while it is queued or running. \
                        A running agent is killed and its answer is lost. Returns isError for \
                        a request that already finished (read it with get_result) or an id \
                        this token did not send.",
        "inputSchema": {
            "type": "object",
            "properties": { "id": id_schema() },
            "required": ["id"],
            "additionalProperties": false,
        },
        "annotations": {
            "readOnlyHint": false,
            "destructiveHint": true,
            "idempotentHint": true,
            "openWorldHint": false,
        },
    }));
    list.push(json!({
        "name": "my_usage",
        "title": "My access and usage",
        "description": "Show this token's access as JSON: status, allowed agents, limits, \
                        usage today and this month, and when the limits reset (UTC). When the \
                        owner shares usage with this token, also agentUsage: the headroom left \
                        per agent. Call it after a call fails at a limit.",
        "inputSchema": { "type": "object", "properties": {}, "additionalProperties": false },
        "annotations": { "readOnlyHint": true, "openWorldHint": false },
    }));
    let agent_names: Vec<&str> = AgentKind::ALL.iter().map(|a| a.as_str()).collect();
    list.push(json!({
        "name": "request_more",
        "title": "Ask the owner for more",
        "description": "Ask the owner of this Hashi for more: higher daily or monthly prompt \
                        or budget limits, more agents, a later expiry, or anything in a \
                        message. Give at least one field. The owner decides; nothing changes \
                        until they accept, and my_usage shows the new limits once they do. \
                        A token may have 3 open requests and send one an hour. Admin tokens \
                        cannot use it.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "dailyRequests": { "type": "integer", "minimum": 0, "description": "Prompts per UTC day wanted." },
                "monthlyRequests": { "type": "integer", "minimum": 0, "description": "Prompts per UTC month wanted." },
                "dailyBudgetUsd": { "type": "number", "minimum": 0, "description": "Spend per UTC day wanted, in USD." },
                "monthlyBudgetUsd": { "type": "number", "minimum": 0, "description": "Spend per UTC month wanted, in USD." },
                "agents": {
                    "type": "array",
                    "items": { "type": "string", "enum": agent_names },
                    "uniqueItems": true,
                    "description": "Agents wanted on top of the current ones.",
                },
                "expiresInDays": { "type": "integer", "minimum": 1, "maximum": 3650, "description": "A new lifetime in days, counted from when the owner accepts." },
                "message": { "type": "string", "maxLength": 500, "description": "Anything else, for the owner to read. Up to 500 characters." },
            },
            "additionalProperties": false,
        },
        "annotations": { "readOnlyHint": false, "destructiveHint": false, "openWorldHint": false },
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
            match api::cancel_request(state, token, &a.id).await {
                Ok(r) => Ok((format!("Cancelled request {}.", r.id), false)),
                Err(e) if e.code() == "conflict" => Ok((
                    format!(
                        "Request {} already finished, so there is nothing to cancel; call \
                         get_result to read it. (error code: conflict)",
                        a.id
                    ),
                    true,
                )),
                Err(e) => Err(e),
            }
        }
        "my_usage" => api::me_json(state, token).await.map(|v| {
            (
                serde_json::to_string_pretty(&v).unwrap_or_else(|_| v.to_string()),
                false,
            )
        }),
        "request_more" => {
            let input: crate::access::MoreInput = args(call.arguments)?;
            api::ask_more_for(state, token, input).map(|r| {
                (
                    format!(
                        "Sent to the owner (request {}): {}. They decide; nothing changes \
                         until they accept. Check my_usage later.",
                        r.id,
                        crate::access::asked_text(&r)
                    ),
                    false,
                )
            })
        }
        other => {
            return Err(RpcError(INVALID_PARAMS, format!("unknown tool: {other}")));
        }
    };
    let (text, is_error) = outcome.unwrap_or_else(|e: ApiError| (error_text(&e), true));
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
            account: None,
            hops: 0,
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

/// A refused tool call as text for the model: the message, what to do
/// next where that is clear, and the error code from the API reference.
fn error_text(e: &ApiError) -> String {
    let code = e.code();
    let next = match code {
        "limit_reached" => {
            " my_usage shows this token's limits; request_more asks the owner for more."
        }
        "too_many_pending" => {
            " Wait for an earlier request with get_result, or stop one with cancel."
        }
        "not_found" => " Use a request id from an ask_* result sent with this token.",
        _ => "",
    };
    format!("{e}.{next} (error code: {code})")
}

/// Text and `isError` flag for a request's current state.
fn outcome(r: Request) -> (String, bool) {
    match r.status {
        RequestStatus::Completed => {
            let text = r.response.unwrap_or_default();
            if text.trim().is_empty() {
                (
                    "(The agent finished with an empty answer.)".to_owned(),
                    false,
                )
            } else {
                (text, false)
            }
        }
        RequestStatus::Failed => (
            format!(
                "Request {} failed: {}",
                r.id,
                r.error.unwrap_or_else(|| "the run failed".to_owned())
            ),
            true,
        ),
        RequestStatus::Queued | RequestStatus::Running => (
            format!(
                "Still {} (request id {}). Call get_result with this id to fetch the answer; \
                 do not send the prompt again.",
                r.status.as_str(),
                r.id
            ),
            false,
        ),
    }
}
