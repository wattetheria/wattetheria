//! The Agent leaderboards, read from the gateway, which ranks Agents from its own records: the
//! Space board by experience, the farm board by what each Agent earned this farm season, the
//! cards board by card rating this season and the housing board by what each owner built plus
//! how many guests looked for cards on its lots this season.
//! Agents reach them through the `leaderboard` MCP tool.

use axum::Json;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::auth::authorize;
use crate::ledger_client::LedgerClient;
use crate::state::ControlPlaneState;

const DEFAULT_LIMIT: usize = 20;
const MAX_LIMIT: usize = 50;

#[derive(Debug, Deserialize)]
pub struct LeaderboardQuery {
    #[serde(default)]
    limit: Option<usize>,
    /// `space` (the default), `farm`, `cards` or `housing`.
    #[serde(default)]
    board: Option<String>,
}

/// `GET /v1/wattetheria/leaderboard?board=&limit=`: the top rows, as the gateway ranks them.
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
    let (board, path) = match query.board.as_deref() {
        None | Some("space") => ("space", "/api/leaderboard"),
        Some("farm") => ("farm", "/api/farm/leaderboard"),
        Some("cards") => ("cards", "/api/cards/leaderboard"),
        Some("housing") => ("housing", "/api/housing/leaderboard"),
        Some(other) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": "unknown_board",
                    "message": format!("there is no `{other}` board; use `space`, `farm`, `cards` or `housing`"),
                })),
            )
                .into_response();
        }
    };
    let limit = query
        .limit
        .unwrap_or(DEFAULT_LIMIT)
        .clamp(1, MAX_LIMIT)
        .to_string();
    match gateway.get_public(path, &[("limit", &limit)]).await {
        Ok((status, body)) if status.is_success() => {
            // The Space board is a bare list; the farm board already is an object.
            let mut body = match body {
                Value::Array(items) => json!({"items": items}),
                other => other,
            };
            body["you"] = own_row(&state, &gateway, board).await;
            (status, Json(body)).into_response()
        }
        Ok((status, body)) => (status, Json(body)).into_response(),
        Err(error) => (
            StatusCode::BAD_GATEWAY,
            Json(json!({"error": "gateway_unreachable", "message": format!("{error:#}")})),
        )
            .into_response(),
    }
}

/// This Agent's own row on `board`, however far down it is; `null` when it is not on the board
/// or the gateway does not say.
async fn own_row(state: &ControlPlaneState, gateway: &LedgerClient, board: &str) -> Value {
    let path = format!("/api/agents/{}/rank", state.agent_did);
    match gateway.get_public(&path, &[]).await {
        Ok((status, body)) if status.is_success() => body[board].clone(),
        _ => Value::Null,
    }
}
