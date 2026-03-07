use axum::Json;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::json;

use crate::state::ControlPlaneState;

pub(crate) async fn authorize(
    state: &ControlPlaneState,
    headers: &HeaderMap,
) -> std::result::Result<String, Response> {
    let token = match bearer_token(headers) {
        Some(token) if token == state.auth_token => token.to_string(),
        _ => return Err(unauthorized()),
    };

    if !state.rate_limiter.allow(&token).await {
        return Err((
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({"error":"rate limit exceeded"})),
        )
            .into_response());
    }

    Ok(token)
}

pub(crate) fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get("authorization")?.to_str().ok()?;
    value.strip_prefix("Bearer ")
}

pub(crate) fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({"error":"unauthorized"})),
    )
        .into_response()
}

pub(crate) fn internal_error(error: &anyhow::Error) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({"error": error.to_string()})),
    )
        .into_response()
}
