//! The local agent's Watt wallet: balance, locked bounties and ledger history, read from the
//! gateway ledger for this node's own account only. Agents reach it through the
//! `get_watt_balance` and `list_watt_ledger_entries` MCP tools.

use axum::Json;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::auth::authorize;
use crate::ledger_client::LedgerClient;
use crate::routes::missions::agent_display_name_for_did;
use crate::state::ControlPlaneState;

const DEFAULT_ENTRIES_LIMIT: i64 = 50;
const MAX_ENTRIES_LIMIT: i64 = 200;
const LOCKED_ESCROWS_LIMIT: i64 = 100;

#[derive(Debug, Default, Deserialize)]
pub struct WalletEntriesQuery {
    #[serde(default)]
    pub limit: Option<i64>,
    /// Cursor from the previous page's `next_before_seq`.
    #[serde(default)]
    pub before_seq: Option<i64>,
}

/// `GET /v1/wattetheria/wallet`: this agent's balance plus the bounties it currently has
/// locked in escrow.
pub async fn get_wallet(State(state): State<ControlPlaneState>, headers: HeaderMap) -> Response {
    if let Err(response) = authorize(&state, &headers).await {
        return response;
    }
    let Some(ledger) = LedgerClient::from_state(&state) else {
        return ledger_unavailable();
    };
    let account_id = state.agent_did.clone();
    let account = match ledger.account(&account_id).await {
        Ok(account) => account,
        Err(error) => return ledger_error(&error),
    };
    let locked_escrows = match ledger
        .escrows(&account_id, Some("locked"), Some(LOCKED_ESCROWS_LIMIT))
        .await
    {
        Ok(escrows) => escrows,
        Err(error) => return ledger_error(&error),
    };
    let display_name = agent_display_name_for_did(&state, &account_id).await;
    Json(json!({
        "agent_did": account_id,
        "display_name": display_name,
        "ledger_account": account.get("account_id").cloned().unwrap_or(Value::Null),
        "balance": account.get("balance").and_then(Value::as_i64).unwrap_or(0),
        "locked_in_escrow": account
            .get("locked_in_escrow")
            .and_then(Value::as_i64)
            .unwrap_or(0),
        "account_exists": account.get("exists").and_then(Value::as_bool).unwrap_or(false),
        "locked_escrows": locked_escrows,
        "ledger_url": ledger.base_url(),
    }))
    .into_response()
}

/// `GET /v1/wattetheria/wallet/entries?limit&before_seq`: this agent's ledger history, newest
/// first, each entry already labelled with direction, counterparty and subject by the ledger.
pub async fn list_wallet_entries(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<WalletEntriesQuery>,
) -> Response {
    if let Err(response) = authorize(&state, &headers).await {
        return response;
    }
    if query.before_seq.is_some_and(|seq| seq <= 0) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "before_seq must be a positive ledger sequence number"})),
        )
            .into_response();
    }
    let Some(ledger) = LedgerClient::from_state(&state) else {
        return ledger_unavailable();
    };
    let limit = query
        .limit
        .unwrap_or(DEFAULT_ENTRIES_LIMIT)
        .clamp(1, MAX_ENTRIES_LIMIT);
    let account_id = state.agent_did.clone();
    let mut page = match ledger
        .entries(&account_id, Some(limit), query.before_seq)
        .await
    {
        Ok(page) => page,
        Err(error) => return ledger_error(&error),
    };
    if let Some(entries) = page.get_mut("entries").and_then(Value::as_array_mut) {
        for entry in entries.iter_mut() {
            fill_local_counterparty_label(&state, entry).await;
        }
    }
    let display_name = agent_display_name_for_did(&state, &account_id).await;
    Json(json!({
        "agent_did": account_id,
        "display_name": display_name,
        "limit": limit,
        "entries": page.get("entries").cloned().unwrap_or_else(|| json!([])),
        "next_before_seq": page.get("next_before_seq").cloned().unwrap_or(Value::Null),
    }))
    .into_response()
}

/// A counterparty the ledger could not name may still be an identity this node knows.
async fn fill_local_counterparty_label(state: &ControlPlaneState, entry: &mut Value) {
    let Some(object) = entry.as_object_mut() else {
        return;
    };
    let unnamed = object.get("counterparty_label").is_none_or(Value::is_null);
    if !unnamed {
        return;
    }
    let Some(counterparty) = object
        .get("counterparty_account")
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        return;
    };
    if let Some(label) = agent_display_name_for_did(state, &counterparty).await {
        object.insert("counterparty_label".to_owned(), Value::String(label));
    }
}

fn ledger_unavailable() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({
            "error": "Watt lives in the gateway ledger and this node has no gateway configured",
            "code": "ledger_unavailable",
        })),
    )
        .into_response()
}

fn ledger_error(error: &anyhow::Error) -> Response {
    (
        StatusCode::BAD_GATEWAY,
        Json(json!({
            "error": format!("gateway ledger request failed: {error:#}"),
            "code": "ledger_error",
        })),
    )
        .into_response()
}
