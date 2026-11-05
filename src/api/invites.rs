//! Invite links: admins create them, people claim them without any token.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{Admin, ApiError, ApiResult, AppState, ClientKey, claim_outcome};
use crate::accounts::AccountMap;
use crate::agent::AgentKind;
use crate::store::{Invite, InviteState, NewInvite, Redeemed};
use crate::token::{Limits, SessionMode};
use crate::util::{now_ms, rfc3339};

/// Default in-flight cap for invited tokens.
pub const INVITE_DEFAULT_MAX_PENDING: u32 = 3;
/// Upper bound on pending invites.
pub const MAX_PENDING_INVITES: usize = 100;
const LINK_HOURS: [u32; 3] = [1, 24, 168];

/// Body of `POST /v1/invites`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateInviteBody {
    /// Who it is for. Becomes the token's name.
    pub label: String,
    /// `stateless` (default) or `stateful`.
    #[serde(default)]
    pub session_mode: Option<SessionMode>,
    /// Allowed agents. Defaults to all.
    #[serde(default)]
    pub agents: Option<Vec<AgentKind>>,
    /// Token lifetime after it is claimed, 1 to 3650 days.
    #[serde(default)]
    pub expires_in_days: Option<u32>,
    /// Usage caps. `maxPending` defaults to 3.
    #[serde(default)]
    pub limits: Option<Limits>,
    /// Link validity in hours: 1, 24 (default) or 168.
    #[serde(default)]
    pub link_hours: Option<u32>,
    /// Accounts per agent the token may use, as on `POST /v1/tokens`.
    #[serde(default)]
    pub accounts: Option<AccountMap>,
}

/// Body of the claim endpoints.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ClaimBody {
    code: String,
}

fn invite_json(i: &Invite, now: i64) -> Value {
    json!({
        "id": i.id,
        "label": i.label,
        "status": i.status(now),
        "sessionMode": i.session_mode,
        "agents": i.agents,
        "tokenExpiresInDays": i.token_expires_in_days,
        "limits": i.limits,
        "expiresAt": rfc3339(i.expires_at),
        "createdAt": rfc3339(i.created_at),
        "claimedAt": i.claimed_at.and_then(rfc3339),
        "accounts": i.accounts,
    })
}

/// Validates a create request and fills in defaults.
///
/// # Errors
///
/// Returns [`ApiError::BadRequest`] for invalid fields.
pub fn validate_new_invite(body: CreateInviteBody) -> ApiResult<NewInvite> {
    let probe = super::validate_new_token(super::CreateTokenBody {
        name: body.label,
        role: None,
        session_mode: body.session_mode,
        agents: body.agents,
        expires_in_days: body.expires_in_days,
        note: None,
        limits: body.limits,
        accounts: body.accounts,
    })?;
    let link_hours = body.link_hours.unwrap_or(24);
    if !LINK_HOURS.contains(&link_hours) {
        return Err(ApiError::BadRequest(
            "linkHours must be 1, 24 or 168".to_owned(),
        ));
    }
    let mut limits = probe.limits;
    limits.max_pending = limits.max_pending.or(Some(INVITE_DEFAULT_MAX_PENDING));
    Ok(NewInvite {
        label: probe.name,
        session_mode: probe.session_mode,
        agents: probe.agents,
        token_expires_in_days: probe.expires_in_days,
        limits,
        link_hours,
        accounts: probe.accounts,
    })
}

/// Ready-to-paste ways to use a token.
#[must_use]
pub fn snippets(base: &str, token: &str) -> Value {
    json!({
        "curl": format!(
            "curl {base}/v1/ask \\\n  -H \"Authorization: Bearer {token}\" \\\n  -H \"Content-Type: application/json\" \\\n  -d '{{\"message\": \"Hello\", \"agent\": \"claude\", \"wait\": 60}}'"
        ),
        "python": format!(
            "import requests\n\nr = requests.post(\n    \"{base}/v1/ask\",\n    headers={{\"Authorization\": \"Bearer {token}\"}},\n    json={{\"message\": \"Hello\", \"agent\": \"claude\", \"wait\": 60}},\n)\nprint(r.json()[\"response\"])"
        ),
        "typescript": format!(
            "const res = await fetch(\"{base}/v1/ask\", {{\n  method: \"POST\",\n  headers: {{ Authorization: \"Bearer {token}\", \"Content-Type\": \"application/json\" }},\n  body: JSON.stringify({{ message: \"Hello\", agent: \"claude\", wait: 60 }}),\n}});\nconsole.log((await res.json()).response);"
        ),
        "claudeCodeMcp": format!(
            "claude mcp add --transport http hashi {base}/mcp --header \"Authorization: Bearer {token}\""
        ),
        "codexMcp": format!(
            "export HASHI_TOKEN=\"{token}\"\ncodex mcp add hashi --url {base}/mcp --bearer-token-env-var HASHI_TOKEN\n# Codex reads the token from HASHI_TOKEN each time it starts,\n# so add the export line to your shell profile too."
        ),
        "mcpJson": serde_json::to_string_pretty(&json!({
            "mcpServers": { "hashi": {
                "type": "http",
                "url": format!("{base}/mcp"),
                "headers": { "Authorization": format!("Bearer {token}") },
            }}
        })).unwrap_or_default(),
    })
}

/// One-click MCP install links for the HTTP MCP server at `<base>/mcp`.
///
/// Formats, from each editor's own documentation:
/// - Cursor (<https://cursor.com/docs/context/mcp/install-links>):
///   `cursor://anysphere.cursor-deeplink/mcp/install?name=<name>&config=<base64 JSON>`,
///   where the JSON is the server's `mcp.json` entry without its name.
/// - VS Code (<https://code.visualstudio.com/api/extension-guides/ai/mcp>):
///   `vscode:mcp/install?<encodeURIComponent(JSON)>`, where the JSON is the
///   server configuration with a `name` field.
#[must_use]
pub fn install_links(base: &str, token: &str) -> Value {
    let url = format!("{base}/mcp");
    let auth = format!("Bearer {token}");
    let cursor = json!({ "url": url, "headers": { "Authorization": auth } }).to_string();
    let vscode = json!({
        "name": "hashi",
        "type": "http",
        "url": url,
        "headers": { "Authorization": auth },
    })
    .to_string();
    json!({
        "cursor": format!(
            "cursor://anysphere.cursor-deeplink/mcp/install?name=hashi&config={}",
            percent_encode(&base64(cursor.as_bytes()))
        ),
        "vscode": format!("vscode:mcp/install?{}", percent_encode(&vscode)),
    })
}

/// Standard base64 with padding.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let at = |i: usize| u32::from(chunk.get(i).copied().unwrap_or(0));
        let n = (at(0) << 16) | (at(1) << 8) | at(2);
        for i in 0..4 {
            if i <= chunk.len() {
                let six = (n >> (18 - 6 * i)) & 63;
                out.push(char::from(ALPHABET[six as usize]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Percent-encodes everything but ASCII letters, digits and `-_.~`.
fn percent_encode(s: &str) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(b));
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

pub(super) async fn create(
    State(state): State<AppState>,
    Admin(admin): Admin,
    body: Result<Json<CreateInviteBody>, axum::extract::rejection::JsonRejection>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let Json(body) = body.map_err(|e| ApiError::BadRequest(e.body_text()))?;
    let mut new = validate_new_invite(body)?;
    new.accounts = super::check_accounts(&state.store, new.accounts)?;
    let now = now_ms();
    let pending = state
        .store
        .list_invites()?
        .iter()
        .filter(|i| i.status(now) == "pending")
        .count();
    if pending >= MAX_PENDING_INVITES {
        return Err(ApiError::Conflict(
            "too many pending invites, cancel some first",
        ));
    }
    let (invite, code) = state.store.create_invite(&new)?;
    state.store.audit(
        &admin.name,
        "invite.create",
        &invite.label,
        &format!("link valid {}h", new.link_hours),
    )?;
    state
        .bus
        .log(format!("invite created for {}", invite.label));
    let public = state.runtime.public_url();
    let base = public
        .clone()
        .unwrap_or_else(|| state.runtime.local_url.clone());
    Ok((
        StatusCode::CREATED,
        Json(json!({
            "url": format!("{base}/claim/{code}"),
            "localOnly": public.is_none(),
            "warning": "Send this link privately. It works once and cannot be shown again.",
            "invite": invite_json(&invite, now),
        })),
    ))
}

pub(super) async fn list(State(state): State<AppState>, Admin(_): Admin) -> ApiResult<Json<Value>> {
    let now = now_ms();
    let invites: Vec<Value> = state
        .store
        .list_invites()?
        .iter()
        .map(|i| invite_json(i, now))
        .collect();
    Ok(Json(json!({ "invites": invites })))
}

pub(super) async fn cancel(
    State(state): State<AppState>,
    Admin(admin): Admin,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let label = state
        .store
        .list_invites()?
        .into_iter()
        .find(|i| i.id == id)
        .map(|i| i.label);
    if !state.store.cancel_invite(&id)? {
        return Err(ApiError::NotFound("no pending invite with that id"));
    }
    let label = label.unwrap_or(id);
    state
        .store
        .audit(&admin.name, "invite.cancel", &label, "")?;
    state.bus.log(format!("invite cancelled for {label}"));
    Ok(Json(json!({ "ok": true })))
}

fn check_code(code: &str) -> ApiResult<()> {
    // Failed attempts are rate limited per client address by the handlers
    // (see `claim_outcome`).
    if !code.starts_with(crate::store::INVITE_PREFIX) || code.len() > 128 {
        return Err(not_found());
    }
    Ok(())
}

fn not_found() -> ApiError {
    ApiError::coded(
        StatusCode::NOT_FOUND,
        "invite_not_found",
        "this invite link is not valid",
    )
}

fn claimed() -> ApiError {
    ApiError::coded(
        StatusCode::CONFLICT,
        "already_claimed",
        "this invite link was already used",
    )
}

fn expired() -> ApiError {
    ApiError::coded(
        StatusCode::GONE,
        "invite_expired",
        "this invite link has expired, ask for a new one",
    )
}

type ClaimJson = Result<Json<ClaimBody>, axum::extract::rejection::JsonRejection>;

pub(super) async fn preview(
    State(state): State<AppState>,
    client: ClientKey,
    body: ClaimJson,
) -> ApiResult<Json<Value>> {
    let result = preview_code(&state, body);
    claim_outcome(&state, &client, result)
}

fn preview_code(state: &AppState, body: ClaimJson) -> ApiResult<Json<Value>> {
    let Json(body) = body.map_err(|e| ApiError::BadRequest(e.body_text()))?;
    check_code(&body.code)?;
    match state.store.preview_invite(&body.code)? {
        InviteState::Pending(i) => Ok(Json(json!({
            "label": i.label,
            "server": state.runtime.name,
            "agents": i.agents,
            "sessionMode": i.session_mode,
            "limits": i.limits,
            "tokenExpiresInDays": i.token_expires_in_days,
            "linkExpiresAt": rfc3339(i.expires_at),
            "notice": "The owner of this Hashi can see the prompts you send and the answers you get.",
        }))),
        InviteState::Claimed => Err(claimed()),
        InviteState::Expired => Err(expired()),
        InviteState::NotFound => Err(not_found()),
    }
}

pub(super) async fn redeem(
    State(state): State<AppState>,
    client: ClientKey,
    body: ClaimJson,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let result = redeem_code(&state, body);
    claim_outcome(&state, &client, result)
}

fn redeem_code(state: &AppState, body: ClaimJson) -> ApiResult<(StatusCode, Json<Value>)> {
    let Json(body) = body.map_err(|e| ApiError::BadRequest(e.body_text()))?;
    check_code(&body.code)?;
    match state.store.redeem_invite(&body.code)? {
        Redeemed::Token(token, secret) => {
            state
                .store
                .audit(&token.name, "invite.claim", &token.name, &token.prefix)?;
            state.bus.log(format!("invite claimed by {}", token.name));
            let base = state.runtime.base_url();
            Ok((
                StatusCode::CREATED,
                Json(json!({
                    "token": secret.raw,
                    "warning": "Save this token now. It cannot be shown again.",
                    "name": token.name,
                    "server": state.runtime.name,
                    "baseUrl": base,
                    "meUrl": format!("{base}/me"),
                    "agents": token.agents,
                    "sessionMode": token.session_mode,
                    "limits": token.limits,
                    "expiresAt": token.expires_at.and_then(rfc3339),
                    "snippets": snippets(&base, &secret.raw),
                    "installLinks": install_links(&base, &secret.raw),
                })),
            ))
        }
        Redeemed::Claimed => Err(claimed()),
        Redeemed::Expired => Err(expired()),
        Redeemed::NotFound => Err(not_found()),
        Redeemed::TokenLimit => Err(ApiError::Conflict(
            "this server has reached its token limit; ask the owner to free a slot",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(label: &str) -> CreateInviteBody {
        CreateInviteBody {
            label: label.into(),
            session_mode: None,
            agents: None,
            expires_in_days: None,
            limits: None,
            link_hours: None,
            accounts: None,
        }
    }

    #[test]
    fn invites_default_to_24h_links_and_three_in_flight() {
        let i = validate_new_invite(body("alice")).unwrap();
        assert_eq!((i.link_hours, i.limits.max_pending), (24, Some(3)));
    }

    #[test]
    fn invites_reject_odd_link_lifetimes() {
        let b = CreateInviteBody {
            link_hours: Some(5),
            ..body("alice")
        };
        assert!(validate_new_invite(b).is_err());
    }

    #[test]
    fn base64_matches_known_vectors() {
        for (input, want) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(input.as_bytes()), want);
        }
    }

    #[test]
    fn percent_encode_keeps_only_unreserved() {
        assert_eq!(
            percent_encode("a+b/c=d e\"\u{e9}"),
            "a%2Bb%2Fc%3Dd%20e%22%C3%A9"
        );
        assert_eq!(percent_encode("Az09-_.~"), "Az09-_.~");
    }

    #[test]
    fn install_links_follow_the_documented_formats() {
        let l = install_links("https://h.example", "hashi_abc");
        let cursor = l["cursor"].as_str().unwrap();
        let prefix = "cursor://anysphere.cursor-deeplink/mcp/install?name=hashi&config=";
        let config = cursor.strip_prefix(prefix).unwrap();
        let want = json!({
            "url": "https://h.example/mcp",
            "headers": { "Authorization": "Bearer hashi_abc" },
        })
        .to_string();
        assert_eq!(config, percent_encode(&base64(want.as_bytes())));

        let vscode = l["vscode"].as_str().unwrap();
        let encoded = vscode.strip_prefix("vscode:mcp/install?").unwrap();
        let want = json!({
            "name": "hashi",
            "type": "http",
            "url": "https://h.example/mcp",
            "headers": { "Authorization": "Bearer hashi_abc" },
        })
        .to_string();
        assert_eq!(encoded, percent_encode(&want));
        assert!(!encoded.contains(['{', '"', ' ', '&', '#']));
    }

    #[test]
    fn snippets_embed_base_and_token() {
        let s = snippets("https://h.example", "hashi_abc");
        let curl = s["curl"].as_str().unwrap();
        assert!(curl.contains("https://h.example/v1/ask") && curl.contains("Bearer hashi_abc"));
        assert!(
            s["mcpJson"]
                .as_str()
                .unwrap()
                .contains("https://h.example/mcp")
        );
    }
}
