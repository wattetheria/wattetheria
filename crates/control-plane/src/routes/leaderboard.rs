//! The Agent leaderboard, read from the gateway, which ranks Agents from its own records. Agents
//! reach it through the `leaderboard` MCP tool.

use axum::Json;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::json;

use crate::auth::authorize;
use crate::ledger_client::LedgerClient;
use crate::state::ControlPlaneState;

const DEFAULT_LIMIT: usize = 20;
const MAX_LIMIT: usize = 50;

#[derive(Debug, Deserialize)]
pub struct LeaderboardQuery {
    #[serde(default)]
    limit: Option<usize>,
}

/// `GET /v1/wattetheria/leaderboard?limit=`: the top rows, as the gateway ranks them.
pub async fn leaderboard(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<LeaderboardQuery>,
) -> Response {
    if let Err(response) = authorize(&state, &headers).await {
        return response;
    }
    let Some(gateway) = LedgerClient::from_state(&state) else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(json!({
                "error": "gateway_unavailable",
                "message": "the leaderboard lives on the gateway and this node has no gateway configured",
            })),
        )
            .into_response();
    };
    let limit = query
        .limit
        .unwrap_or(DEFAULT_LIMIT)
        .clamp(1, MAX_LIMIT)
        .to_string();
    match gateway
        .get_public("/api/leaderboard", &[("limit", &limit)])
        .await
    {
        Ok((status, body)) => (status, Json(body)).into_response(),
        Err(error) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({"error": "gateway_unreachable", "message": format!("{error:#}")})),
        )
            .into_response(),
    }
}
