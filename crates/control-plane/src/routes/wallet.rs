//! The local agent's Watt wallet: balance, locked bounties and ledger history, read from the
//! gateway ledger for this node's own account only, plus sending Watt to another account.
//! Agents reach it through the `get_watt_balance`, `list_watt_ledger_entries` and
//! `send_watt` MCP tools.

use axum::Json;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};
use wattetheria_kernel::identity::fingerprint_from_did_key;
use wattetheria_kernel::servicenet::normalize_service_address;

use crate::auth::authorize;
use crate::ledger_client::{LedgerClient, LedgerError, TransferLabels};
use crate::routes::mcp::find_servicenet_agent_by_service_address;
use crate::routes::missions::agent_display_name_for_did;
use crate::routes::payments::resolve_payment_counterpart_public_id_by_display_name;
use crate::social_host::{load_social_identity_maps, resolve_social_local_context};
use crate::state::ControlPlaneState;

const DEFAULT_ENTRIES_LIMIT: i64 = 50;
const MAX_ENTRIES_LIMIT: i64 = 200;
const LOCKED_ESCROWS_LIMIT: i64 = 100;
const MAX_MEMO_CHARS: usize = 200;
const MAX_TRANSFER_ID_CHARS: usize = 128;

/// Who a `send_watt` target names.
#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SendTargetKind {
    /// A network agent: its Agent DID, its `public_id`, or the display name of an active friend.
    #[default]
    Agent,
    /// A `ServiceNet` service agent named by its service address (`name@wattetheria`).
    ServiceAgent,
}

#[derive(Debug, Deserialize)]
pub struct SendWattBody {
    pub to: String,
    #[serde(default)]
    pub to_kind: SendTargetKind,
    pub amount: i64,
    #[serde(default)]
    pub memo: Option<String>,
    /// Caller-chosen idempotency key, required: a retry with the same id after a lost reply
    /// returns the entry the ledger already posted instead of paying again. The node scopes
    /// it to this agent before it reaches the ledger, so ids never collide across agents.
    #[serde(default)]
    pub transfer_id: Option<String>,
}

/// The ledger account a `send_watt` target resolved to, with the label the ledger will show.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SendTarget {
    account: String,
    label: Option<String>,
}

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

/// `POST /v1/wattetheria/wallet/send`: moves Watt from this agent's account to another
/// account in one signed ledger entry. The recipient is an agent (DID, `public_id` or friend
/// display name) or a `ServiceNet` service agent (service address); whatever it resolves to
/// must be a well-formed `did:key`, so the Watt lands on an account someone holds the key
/// for. There is no confirmation step: the ledger posts the entry as soon as the signature
/// and balance check out. The ledger is idempotent on the transfer id, so a caller that lost
/// the reply retries with the same `transfer_id` and gets the posted entry back without a
/// second debit; every reply, error replies included, echoes the `transfer_id` it was for.
pub async fn send_watt(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<SendWattBody>,
) -> Response {
    if let Err(response) = authorize(&state, &headers).await {
        return response;
    }
    let memo = match validated_memo(body.memo.as_deref()) {
        Ok(memo) => memo,
        Err(error) => return bad_request(&error),
    };
    if body.amount <= 0 {
        return bad_request("amount must be a positive whole number of Watt");
    }
    let transfer_id = match validated_transfer_id(body.transfer_id.as_deref()) {
        Ok(transfer_id) => transfer_id,
        Err(error) => return bad_request(&error),
    };
    let target = match resolve_send_target(&state, &body.to, body.to_kind).await {
        Ok(target) => target,
        Err(error) => return bad_request(&error),
    };
    if target.account == state.agent_did {
        return bad_request("cannot send Watt to your own account");
    }
    let Some(ledger) = LedgerClient::from_state(&state) else {
        return ledger_unavailable();
    };
    let from_label = agent_display_name_for_did(&state, &state.agent_did).await;
    let transferred = match ledger
        .transfer(
            &state,
            &transfer_id,
            &target.account,
            body.amount,
            memo,
            TransferLabels {
                from_label: from_label.as_deref(),
                to_label: target.label.as_deref(),
                subject: memo,
            },
        )
        .await
    {
        Ok(response) => response,
        Err(error) => return transfer_error(&error, &transfer_id),
    };
    let balance = match ledger.balance(&state.agent_did).await {
        Ok(balance) => Value::from(balance),
        Err(_) => Value::Null,
    };
    Json(json!({
        "ok": true,
        "transfer_id": transfer_id,
        "from_did": state.agent_did,
        "from_label": from_label,
        "to_did": target.account,
        "to_label": target.label,
        "amount": body.amount,
        "memo": memo,
        "entry": transferred.get("entry").cloned().unwrap_or(Value::Null),
        "entry_id": transferred["entry"].get("entry_id").cloned().unwrap_or(Value::Null),
        "balance": balance,
    }))
    .into_response()
}

/// The caller's `transfer_id`. It is required: an id the node minted would be lost with the
/// reply, and the retry it exists for would then pay a second time. It must be a single
/// printable token so it survives inside the ledger entry id.
fn validated_transfer_id(transfer_id: Option<&str>) -> Result<String, String> {
    let Some(transfer_id) = transfer_id.map(str::trim).filter(|id| !id.is_empty()) else {
        return Err(
            "transfer_id is required: choose a unique id for this transfer and reuse it if you retry"
                .to_owned(),
        );
    };
    if transfer_id.chars().count() > MAX_TRANSFER_ID_CHARS {
        return Err(format!(
            "transfer_id must be at most {MAX_TRANSFER_ID_CHARS} characters"
        ));
    }
    if transfer_id
        .chars()
        .any(|c| c.is_whitespace() || c.is_control())
    {
        return Err("transfer_id must not contain whitespace or control characters".to_owned());
    }
    Ok(transfer_id.to_owned())
}

/// A ledger account the recipient resolved to. Only a well-formed `did:key` is accepted:
/// the ledger creates any account it is asked to credit, and Watt sent to a malformed DID
/// could never be signed for and spent.
fn recipient_account(did: &str, what: &str) -> Result<String, String> {
    let did = did.trim();
    fingerprint_from_did_key(did)
        .map(|_| did.to_owned())
        .map_err(|_| format!("{what} is not a valid agent did:key"))
}

fn validated_memo(memo: Option<&str>) -> Result<Option<&str>, String> {
    let Some(memo) = memo.map(str::trim).filter(|memo| !memo.is_empty()) else {
        return Ok(None);
    };
    if memo.chars().count() > MAX_MEMO_CHARS {
        return Err(format!("memo must be at most {MAX_MEMO_CHARS} characters"));
    }
    Ok(Some(memo))
}

/// Turns the `to` argument into a ledger account. Agents are looked up as a DID, then as a
/// `public_id` this node knows (its own identities and remote ones learned socially), then as
/// the display name of an active friend. Service agents resolve to their `service_did`.
async fn resolve_send_target(
    state: &ControlPlaneState,
    to: &str,
    kind: SendTargetKind,
) -> Result<SendTarget, String> {
    let to = to.trim();
    if to.is_empty() {
        return Err("to is required".to_owned());
    }
    match kind {
        SendTargetKind::ServiceAgent => resolve_service_agent_target(state, to).await,
        SendTargetKind::Agent => resolve_agent_target(state, to).await,
    }
}

async fn resolve_agent_target(state: &ControlPlaneState, to: &str) -> Result<SendTarget, String> {
    if to.starts_with("did:") {
        let account = recipient_account(to, &format!("to `{to}`"))?;
        return Ok(SendTarget {
            label: agent_display_name_for_did(state, &account).await,
            account,
        });
    }
    let (identities, _) = load_social_identity_maps(state).await;
    if let Some(identity) = identities.get(to) {
        let did = identity
            .agent_did
            .as_deref()
            .ok_or_else(|| format!("public_id {to} has no Agent DID to receive Watt"))?;
        return Ok(SendTarget {
            account: recipient_account(did, &format!("public_id {to}'s Agent DID"))?,
            label: Some(identity.display_name.clone()),
        });
    }
    let local = resolve_social_local_context(state, None).await;
    let public_id =
        resolve_payment_counterpart_public_id_by_display_name(state, &local.public_id, to)
            .await
            .map_err(|_| {
                format!(
                    "to `{to}` is not an Agent DID, a known public_id, or the display name of an active friend"
                )
            })?;
    let identity = identities
        .get(&public_id)
        .ok_or_else(|| format!("friend {to} ({public_id}) has no known identity"))?;
    let did = identity
        .agent_did
        .as_deref()
        .ok_or_else(|| format!("friend {to} ({public_id}) has no Agent DID to receive Watt"))?;
    Ok(SendTarget {
        account: recipient_account(did, &format!("friend {to} ({public_id})'s Agent DID"))?,
        label: Some(identity.display_name.clone()),
    })
}

async fn resolve_service_agent_target(
    state: &ControlPlaneState,
    to: &str,
) -> Result<SendTarget, String> {
    let client = state
        .servicenet_client
        .as_deref()
        .ok_or_else(|| "servicenet is not configured".to_owned())?;
    let service_address = normalize_service_address(to)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "to must be a service address such as name@wattetheria".to_owned())?;
    let agent = find_servicenet_agent_by_service_address(client, &service_address).await?;
    let did = agent
        .get("service_did")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|did| !did.is_empty())
        .ok_or_else(|| format!("ServiceNet agent {service_address} has no service_did"))?;
    let account = recipient_account(
        did,
        &format!("ServiceNet agent {service_address}'s service_did"),
    )?;
    let label = [&agent["agent_card"]["name"], &agent["name"]]
        .into_iter()
        .filter_map(Value::as_str)
        .map(str::trim)
        .find(|name| !name.is_empty())
        .map_or(Some(service_address), |name| Some(name.to_owned()));
    Ok(SendTarget { account, label })
}

fn bad_request(message: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error": message, "code": "invalid_request"})),
    )
        .into_response()
}

/// A failed send. The body names the `transfer_id` so the caller can retry with it; a
/// `transfer_id_reused` reply means this agent already posted a different transfer under it.
fn transfer_error(error: &LedgerError, transfer_id: &str) -> Response {
    let (status, code) = match error {
        LedgerError::InsufficientBalance(_) => {
            (StatusCode::PAYMENT_REQUIRED, "insufficient_balance")
        }
        LedgerError::NotFound(_) => (StatusCode::NOT_FOUND, "not_found"),
        LedgerError::Rejected(StatusCode::CONFLICT, _) => {
            (StatusCode::CONFLICT, "transfer_id_reused")
        }
        LedgerError::Rejected(status, _) if status.is_client_error() => {
            (*status, "ledger_rejected")
        }
        LedgerError::Rejected(..) | LedgerError::Unavailable(_) => {
            (StatusCode::BAD_GATEWAY, "ledger_error")
        }
    };
    (
        status,
        Json(json!({
            "error": error.to_string(),
            "code": code,
            "transfer_id": transfer_id,
            "retry_with_same_transfer_id": status.is_server_error(),
        })),
    )
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
