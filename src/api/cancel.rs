//! `DELETE /v1/ask/{id}`: cancel a queued or running request.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde_json::Value;

use super::{ApiError, ApiResult, AppState, Auth, reads_any_request, request_json};
use crate::queue::{self, CancelBy, CancelOutcome};
use crate::store::Request;
use crate::token::Token;

/// Cancels request `id` for `token`, shared by HTTP and MCP. A token may
/// cancel its own requests and an active admin any request; another
/// token's request answers 404, as a missing one does. A finished request
/// answers 409.
///
/// # Errors
///
/// [`ApiError::NotFound`], [`ApiError::Conflict`], or a database error.
pub async fn cancel_request(state: &AppState, token: &Token, id: &str) -> ApiResult<Request> {
    let req = state
        .store
        .get_request(id)?
        .ok_or(ApiError::NotFound("request not found"))?;
    let by = if req.token_id == token.id {
        CancelBy::Owner
    } else if reads_any_request(token) {
        CancelBy::Other(&token.name)
    } else {
        return Err(ApiError::NotFound("request not found"));
    };
    match queue::cancel(state, id, by).await? {
        CancelOutcome::Cancelled(req) => Ok(req),
        CancelOutcome::Finished(_) => Err(ApiError::Conflict("request already finished")),
        CancelOutcome::NotFound => Err(ApiError::NotFound("request not found")),
    }
}

pub(super) async fn delete(
    State(state): State<AppState>,
    Auth(token): Auth,
    Path(id): Path<String>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let req = cancel_request(&state, &token, &id).await?;
    let (code, v) = request_json(&state.store, req)?;
    Ok((code, Json(v)))
}
