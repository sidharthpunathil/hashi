//! Admin token management and the audit log.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};

use super::{Admin, ApiError, ApiResult, AppState, MAX_TOKENS};
use crate::agent::AgentKind;
use crate::store::{NewToken, Revoke};
use crate::token::{Limits, Role, SessionMode};
use crate::util::now_ms;

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
    /// Who or what the token is for.
    #[serde(default)]
    pub note: Option<String>,
    /// Usage caps.
    #[serde(default)]
    pub limits: Option<Limits>,
}

/// Body of `PATCH /v1/tokens/{id}`. Omitted fields are left as they are.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateTokenBody {
    /// New label.
    #[serde(default)]
    pub name: Option<String>,
    /// New note; an empty string clears it.
    #[serde(default)]
    pub note: Option<String>,
    /// Pause (`true`) or resume (`false`).
    #[serde(default)]
    pub paused: Option<bool>,
    /// New lifetime counted from now; `0` removes the expiry.
    #[serde(default)]
    pub expires_in_days: Option<u32>,
    /// Replaces the whole set of limits. Omit a field inside for "unlimited".
    #[serde(default)]
    pub limits: Option<Limits>,
}

fn validate_name(name: &str) -> ApiResult<String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 100 || name.chars().any(char::is_control) {
        return Err(ApiError::BadRequest(
            "name must be 1 to 100 printable characters".to_owned(),
        ));
    }
    Ok(name.to_owned())
}

fn validate_note(note: Option<String>) -> ApiResult<Option<String>> {
    match note.map(|n| n.trim().to_owned()) {
        Some(n) if n.is_empty() => Ok(None),
        Some(n) if n.chars().count() > 200 || n.chars().any(char::is_control) => Err(
            ApiError::BadRequest("note must be at most 200 printable characters".to_owned()),
        ),
        other => Ok(other),
    }
}

/// Checks that budgets are finite and non-negative and the in-flight cap is at least 1.
///
/// # Errors
///
/// Returns [`ApiError::BadRequest`] for invalid limits.
pub fn validate_limits(limits: Limits) -> ApiResult<Limits> {
    if !limits.is_valid() {
        return Err(ApiError::BadRequest(
            "budgets must be zero or more".to_owned(),
        ));
    }
    if limits.max_pending == Some(0) {
        return Err(ApiError::BadRequest(
            "maxPending must be at least 1 (pause the token to stop it)".to_owned(),
        ));
    }
    Ok(limits)
}

/// Validates a create request and turns it into store input.
///
/// # Errors
///
/// Returns [`ApiError::BadRequest`] for invalid fields.
pub fn validate_new_token(body: CreateTokenBody) -> ApiResult<NewToken> {
    let name = validate_name(&body.name)?;
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
        name,
        role: body.role.unwrap_or(Role::User),
        session_mode: body.session_mode.unwrap_or_default(),
        agents,
        expires_in_days: body.expires_in_days,
        note: validate_note(body.note)?,
        limits: validate_limits(body.limits.unwrap_or_default())?,
    })
}

pub(super) async fn list(State(state): State<AppState>, Admin(_): Admin) -> ApiResult<Json<Value>> {
    let now = now_ms();
    let tokens: Vec<Value> = state
        .store
        .list_tokens()?
        .into_iter()
        .map(|t| {
            let status = t.status(now);
            let mut v = serde_json::to_value(&t).unwrap_or(Value::Null);
            if let Some(obj) = v.as_object_mut() {
                obj.insert("status".into(), json!(status));
            }
            v
        })
        .collect();
    Ok(Json(json!({ "tokens": tokens })))
}

pub(super) async fn create(
    State(state): State<AppState>,
    Admin(admin): Admin,
    body: Result<Json<CreateTokenBody>, axum::extract::rejection::JsonRejection>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let Json(body) = body.map_err(|e| ApiError::BadRequest(e.body_text()))?;
    let new = validate_new_token(body)?;
    if state.store.list_tokens()?.len() >= MAX_TOKENS {
        return Err(ApiError::Conflict("token limit reached, revoke one first"));
    }
    let (token, secret) = state.store.create_token(&new)?;
    state.store.audit(
        &admin.name,
        "token.create",
        &token.name,
        token.role.as_str(),
    )?;
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

pub(super) async fn update(
    State(state): State<AppState>,
    Admin(admin): Admin,
    Path(id): Path<String>,
    body: Result<Json<UpdateTokenBody>, axum::extract::rejection::JsonRejection>,
) -> ApiResult<Json<Value>> {
    let Json(body) = body.map_err(|e| ApiError::BadRequest(e.body_text()))?;
    let mut token = state
        .store
        .get_token(&id)?
        .ok_or(ApiError::NotFound("token not found"))?;
    let mut changes = Vec::new();

    if let Some(name) = body.name {
        token.name = validate_name(&name)?;
        changes.push("name".to_owned());
    }
    if let Some(note) = body.note {
        token.note = validate_note(Some(note))?;
        changes.push("note".to_owned());
    }
    if let Some(paused) = body.paused {
        if paused && token.id == admin.id {
            return Err(ApiError::Conflict("cannot pause the token you are using"));
        }
        token.paused = paused;
        changes.push(if paused { "paused" } else { "resumed" }.to_owned());
    }
    if let Some(days) = body.expires_in_days {
        if days > 3_650 {
            return Err(ApiError::BadRequest(
                "expiresInDays must be between 0 and 3650".to_owned(),
            ));
        }
        token.expires_at = (days > 0).then(|| now_ms() + i64::from(days) * 86_400_000);
        changes.push(if days == 0 {
            "expiry removed".to_owned()
        } else {
            format!("expires in {days}d")
        });
    }
    if let Some(limits) = body.limits {
        token.limits = validate_limits(limits)?;
        changes.push("limits".to_owned());
    }
    if changes.is_empty() {
        return Err(ApiError::BadRequest("nothing to update".to_owned()));
    }

    state.store.save_token_policy(&token)?;
    let detail = changes.join(", ");
    state
        .store
        .audit(&admin.name, "token.update", &token.name, &detail)?;
    state.bus.log(format!("token {}: {detail}", token.name));
    let status = token.status(now_ms());
    let mut v = serde_json::to_value(&token).unwrap_or(Value::Null);
    if let Some(obj) = v.as_object_mut() {
        obj.insert("status".into(), json!(status));
    }
    Ok(Json(json!({ "ok": true, "record": v })))
}

pub(super) async fn revoke(
    State(state): State<AppState>,
    Admin(admin): Admin,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let name = state.store.get_token(&id)?.map(|t| t.name);
    match state.store.revoke_token(&id)? {
        Revoke::Revoked => {
            let target = name.unwrap_or_else(|| id.clone());
            state
                .store
                .audit(&admin.name, "token.revoke", &target, "")?;
            state.bus.log(format!("token revoked: {target}"));
            Ok(Json(json!({ "ok": true })))
        }
        Revoke::NotFound => Err(ApiError::NotFound("token not found")),
        Revoke::LastAdmin => Err(ApiError::Conflict("cannot revoke the last admin token")),
    }
}

/// Query of `GET /v1/audit`.
#[derive(Debug, Deserialize)]
pub(super) struct AuditQuery {
    #[serde(default)]
    limit: Option<u32>,
}

pub(super) async fn audit(
    State(state): State<AppState>,
    Admin(_): Admin,
    Query(q): Query<AuditQuery>,
) -> ApiResult<Json<Value>> {
    let entries = state
        .store
        .list_audit(q.limit.unwrap_or(100).clamp(1, 1_000))?;
    let entries: Vec<Value> = entries
        .into_iter()
        .map(|e| {
            json!({
                "at": crate::util::rfc3339(e.at),
                "actor": e.actor,
                "action": e.action,
                "target": e.target,
                "detail": e.detail,
            })
        })
        .collect();
    Ok(Json(json!({ "entries": entries })))
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
            note: None,
            limits: None,
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
    fn validate_limits_rejects_zero_pending_and_negative_budget() {
        assert!(
            validate_limits(Limits {
                max_pending: Some(0),
                ..Limits::default()
            })
            .is_err()
        );
        assert!(
            validate_limits(Limits {
                daily_budget_usd: Some(-2.0),
                ..Limits::default()
            })
            .is_err()
        );
        assert!(
            validate_limits(Limits {
                daily_requests: Some(0),
                ..Limits::default()
            })
            .is_ok()
        );
    }
}
