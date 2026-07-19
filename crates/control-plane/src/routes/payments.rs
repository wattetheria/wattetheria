use anyhow::{Context, bail};
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use chrono::Utc;
use serde_json::{Map, Value, json};
use uuid::Uuid;

use crate::auth::{authorize, internal_error};
use crate::social_host::{
    SignedAgentEnvelopeArgs, SocialCounterpartTarget, build_signed_agent_envelope_for_nodes,
    public_agent_id, resolve_social_counterpart_target, resolve_social_local_context,
};
use crate::state::{
    AgentPaymentAuthorizeBody, AgentPaymentProposeBody, AgentPaymentRejectBody,
    AgentPaymentSettleBody, AgentPaymentSubmitBody, AgentPaymentsQuery, ControlPlaneState,
    StreamEvent, WalletBindWeb3PaymentAccountBody, WalletCreatePaymentAccountBody,
    agent_commit_context_from_headers,
};
use watt_did::PaymentAccountBindingProof;
use watt_wallet::{PaymentAccountKind, verify_payment_account_binding_proof};
use wattetheria_kernel::audit::AuditEntry;
use wattetheria_kernel::identities::PublicIdentity;
use wattetheria_kernel::local_db;
use wattetheria_kernel::payments::{
    AuthorizePaymentRequest, PaymentAgentMessage, PaymentLedger, PaymentMessageKind, PaymentQuery,
    PaymentStatus, PaymentTransaction, ProposePaymentRequest, RejectPaymentRequest,
    SettlePaymentRequest, SettlementLayer, authorization_payload_bytes,
    source_payment_account_binding_required,
};
use wattetheria_kernel::swarm_bridge::SwarmAgentPaymentCommand;
use wattetheria_kernel::wallet_identity::{
    active_payment_account_binding_proof, open_local_wallet,
};
use wattetheria_social::application::friendship_service;
use wattetheria_social::domain::friendships::{Friendship, FriendshipState};
use wattetheria_social::ports::repositories::RemoteIdentityRepository;

const PAYMENT_MESSAGE_CAPABILITY: &str = "payments.agent.transfer";
const WALLET_BIND_CAPABILITY: &str = "wallet.bind";
const A2A_X402_EXTENSION_URI: &str = "https://github.com/google-a2a/a2a-x402/v0.1";

struct PaymentProposalTarget {
    recipient_public_id: String,
    recipient_did: String,
    remote_node_id: String,
    social_counterpart: Option<SocialCounterpartTarget>,
}

struct PaymentDisplayNames {
    sender: Option<String>,
    recipient: Option<String>,
    counterpart: Option<String>,
}

struct CommitResponseArgs<'a> {
    action_type: &'a str,
    target_id: Option<String>,
    actor_public_id: Option<String>,
    actor_agent_did: Option<String>,
    request_json: &'a Value,
    response_json: &'a Value,
}

fn replay_commit_response(
    state: &ControlPlaneState,
    headers: &HeaderMap,
    action_type: &str,
) -> anyhow::Result<Option<Response>> {
    let Some(context) = agent_commit_context_from_headers(headers) else {
        return Ok(None);
    };
    let Some(entry) = state.local_db.load_agent_action_commit(
        &context.event_id,
        &context.decision_id,
        action_type,
    )?
    else {
        return Ok(None);
    };
    let payload: Value = serde_json::from_str(&entry.result_json)?;
    Ok(Some(Json(payload).into_response()))
}

fn append_commit_response(
    state: &ControlPlaneState,
    headers: &HeaderMap,
    args: CommitResponseArgs<'_>,
) -> anyhow::Result<()> {
    let Some(context) = agent_commit_context_from_headers(headers) else {
        return Ok(());
    };
    state.local_db.append_agent_action_commit(
        &wattetheria_kernel::local_db::AgentActionCommitLogEntry {
            commit_id: Uuid::new_v4().to_string(),
            event_id: context.event_id,
            decision_id: context.decision_id,
            action_type: args.action_type.to_owned(),
            domain: "payment".to_owned(),
            target_id: args.target_id,
            expected_state: None,
            result_state: None,
            request_json: serde_json::to_string(args.request_json)?,
            result_json: serde_json::to_string(args.response_json)?,
            status: "accepted".to_owned(),
            actor_public_id: args.actor_public_id,
            actor_agent_did: args.actor_agent_did,
            created_at: Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        },
    )
}

pub(crate) async fn bind_web3_payment_account(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<WalletBindWeb3PaymentAccountBody>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let address = body.address.trim().to_string();
    if !is_evm_address(&address) {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "invalid EVM address"})),
        )
            .into_response();
    }
    let rail = body.rail.unwrap_or_else(|| "x402".to_string());
    let network = body
        .network
        .or_else(|| network_from_chain_id(body.chain_id.as_deref()));

    let mut wallet_state = match open_local_wallet(&state.data_dir) {
        Ok(wallet) => wallet,
        Err(error) => return internal_error(&error),
    };
    let account_id = wallet_state
        .profile
        .payment_accounts
        .iter()
        .find(|account| {
            account.kind == PaymentAccountKind::Web3Evm
                && account
                    .address
                    .as_deref()
                    .is_some_and(|stored| stored.eq_ignore_ascii_case(address.as_str()))
                && account.rail.as_str() == rail.as_str()
                && account.network.as_deref() == network.as_deref()
        })
        .map(|account| account.account_id.clone());

    let now_ms = wallet_now_ms();
    let account = match account_id {
        Some(account_id) => {
            if let Err(error) = wallet_state.wallet.set_active_payment_account(
                &mut wallet_state.profile,
                &account_id,
                now_ms,
            ) {
                return wallet_internal_error(error);
            }
            match wallet_state
                .wallet
                .active_payment_account(&wallet_state.profile)
            {
                Ok(account) => account.clone(),
                Err(error) => return wallet_internal_error(error),
            }
        }
        None => match wallet_state.wallet.register_watch_payment_account_web3_evm(
            &mut wallet_state.profile,
            address,
            body.label.or_else(|| Some("browser-wallet".to_string())),
            network,
            Some(rail),
            now_ms,
        ) {
            Ok(account) => {
                if let Err(error) = wallet_state.wallet.set_active_payment_account(
                    &mut wallet_state.profile,
                    &account.account_id,
                    now_ms,
                ) {
                    return wallet_internal_error(error);
                }
                account
            }
            Err(error) => return wallet_internal_error(error),
        },
    };

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "wallet".to_string(),
        action: "wallet.payment_account.bind_web3".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: account.address.clone(),
        capability: Some(WALLET_BIND_CAPABILITY.to_string()),
        reason: None,
        duration_ms: None,
        details: Some(payment_account_to_json(&account)),
    });

    Json(json!({
        "ok": true,
        "active_payment_account": payment_account_to_json(&account),
    }))
    .into_response()
}

pub(crate) async fn create_payment_account(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<WalletCreatePaymentAccountBody>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let rail = body.rail.unwrap_or_else(|| "x402".to_string());
    let network = body.network.clone();

    let mut wallet_state = match open_local_wallet(&state.data_dir) {
        Ok(wallet) => wallet,
        Err(error) => return internal_error(&error),
    };
    let now_ms = wallet_now_ms();
    if let Some(account) = wallet_state
        .profile
        .payment_accounts
        .iter()
        .find(|account| {
            account.kind == PaymentAccountKind::Web3Evm
                && account.key_handle.is_some()
                && account.rail.as_str() == rail.as_str()
                && account.network.as_deref() == network.as_deref()
        })
        .cloned()
    {
        if let Err(error) = wallet_state.wallet.set_active_payment_account(
            &mut wallet_state.profile,
            &account.account_id,
            now_ms,
        ) {
            return wallet_internal_error(error);
        }
        return Json(json!({
            "ok": true,
            "already_exists": true,
            "active_payment_account": payment_account_to_json(&account),
        }))
        .into_response();
    }
    let account = match wallet_state.wallet.create_payment_account_web3_evm(
        &mut wallet_state.profile,
        body.label.or_else(|| Some("agent-wallet".to_string())),
        network,
        Some(rail),
        now_ms,
    ) {
        Ok(account) => account,
        Err(error) => return wallet_internal_error(error),
    };
    if let Err(error) = wallet_state.wallet.set_active_payment_account(
        &mut wallet_state.profile,
        &account.account_id,
        now_ms,
    ) {
        return wallet_internal_error(error);
    }

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "wallet".to_string(),
        action: "wallet.payment_account.create".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: account.address.clone(),
        capability: Some(WALLET_BIND_CAPABILITY.to_string()),
        reason: None,
        duration_ms: None,
        details: Some(payment_account_to_json(&account)),
    });

    Json(json!({
        "ok": true,
        "already_exists": false,
        "active_payment_account": payment_account_to_json(&account),
    }))
    .into_response()
}

pub(crate) async fn list_agent_payments(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<AgentPaymentsQuery>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let local = resolve_social_local_context(&state, query.public_id.as_deref()).await;
    let ledger = state.payment_ledger.lock().await;
    let payments = ledger
        .query(&PaymentQuery {
            status: query.status.clone(),
            sender_did: None,
            recipient_did: None,
            sender_public_id: match query.role.as_deref() {
                Some("outbound") => Some(local.public_id.clone()),
                _ => None,
            },
            recipient_public_id: match query.role.as_deref() {
                Some("inbound") => Some(local.public_id.clone()),
                _ => None,
            },
            remote_node_id: None,
            mission_id: None,
            task_id: None,
            rail: query.rail.clone(),
            since: None,
            limit: query.limit,
        })
        .into_iter()
        .filter(|payment| {
            if payment.sender_public_id != local.public_id
                && payment.recipient_public_id != local.public_id
            {
                return false;
            }
            if let Some(counterpart_public_id) = query.counterpart_public_id.as_deref() {
                return payment.sender_public_id == counterpart_public_id
                    || payment.recipient_public_id == counterpart_public_id;
            }
            if let Some(recipient_address) = query.recipient_address.as_deref() {
                return payment
                    .recipient_address
                    .as_deref()
                    .is_some_and(|address| address.eq_ignore_ascii_case(recipient_address));
            }
            true
        })
        .cloned()
        .collect::<Vec<_>>();
    let summary = serde_json::to_value(ledger.summary()).unwrap_or(Value::Null);
    drop(ledger);
    let display_name = normalized_display_name(query.display_name.as_deref());
    let mut items = Vec::new();
    for payment in payments {
        let include = if let Some(display_name) = display_name {
            payment_display_name_matches(&state, &local.public_id, &payment, display_name).await
        } else {
            true
        };
        if include {
            items.push(payment_to_display_json(&state, &local.public_id, &payment).await);
        }
    }

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "payments".to_string(),
        action: "payments.agent.list".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(local.public_id),
        capability: Some(PAYMENT_MESSAGE_CAPABILITY.to_string()),
        reason: None,
        duration_ms: None,
        details: Some(json!({"count": items.len()})),
    });

    Json(json!({
        "items": items,
        "count": items.len(),
        "summary": summary,
    }))
    .into_response()
}

pub(crate) async fn get_agent_payment(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Path(payment_id): Path<String>,
    Query(query): Query<AgentPaymentsQuery>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let local = resolve_social_local_context(&state, query.public_id.as_deref()).await;
    let ledger = state.payment_ledger.lock().await;
    let Some(payment) = ledger.get(&payment_id).cloned() else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": format!("payment not found: {payment_id}")})),
        )
            .into_response();
    };
    if payment.sender_public_id != local.public_id && payment.recipient_public_id != local.public_id
    {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error": "payment is not visible to this public identity"})),
        )
            .into_response();
    }
    drop(ledger);
    let payload = payment_to_display_json(&state, &local.public_id, &payment).await;

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "payments".to_string(),
        action: "payments.agent.get".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(payment_id),
        capability: Some(PAYMENT_MESSAGE_CAPABILITY.to_string()),
        reason: None,
        duration_ms: None,
        details: None,
    });

    Json(payload).into_response()
}

async fn resolve_payment_proposal_target(
    state: &ControlPlaneState,
    local_public_id: &str,
    body: &mut AgentPaymentProposeBody,
) -> Result<PaymentProposalTarget, String> {
    let counterpart_public_id =
        trimmed_optional(body.counterpart_public_id.as_deref()).map(ToOwned::to_owned);
    let display_name = trimmed_optional(body.display_name.as_deref()).map(ToOwned::to_owned);
    let agent_id = trimmed_optional(body.agent_id.as_deref()).map(ToOwned::to_owned);
    let recipient_address =
        trimmed_optional(body.recipient_address.as_deref()).map(ToOwned::to_owned);
    let target_count = [
        counterpart_public_id.is_some(),
        display_name.is_some(),
        agent_id.is_some(),
    ]
    .into_iter()
    .filter(|present| *present)
    .count();
    if target_count > 1 {
        return Err(
            "provide only one of display_name, counterpart_public_id, or agent_id for payment target"
                .to_string(),
        );
    }
    match (
        counterpart_public_id.as_deref(),
        display_name.as_deref(),
        agent_id.as_deref(),
    ) {
        (Some(counterpart_public_id), None, None) => {
            let counterpart =
                resolve_social_counterpart_target(state, counterpart_public_id).await?;
            resolve_network_agent_recipient_address(state, &counterpart, body)?;
            Ok(PaymentProposalTarget {
                recipient_public_id: counterpart.counterpart_public_id.clone(),
                recipient_did: counterpart.target_agent.clone(),
                remote_node_id: counterpart.remote_node.clone(),
                social_counterpart: Some(counterpart),
            })
        }
        (None, Some(display_name), None) => {
            let counterpart_public_id = resolve_payment_counterpart_public_id_by_display_name(
                state,
                local_public_id,
                display_name,
            )
            .await?;
            let counterpart =
                resolve_social_counterpart_target(state, &counterpart_public_id).await?;
            resolve_network_agent_recipient_address(state, &counterpart, body)?;
            body.counterpart_public_id = Some(counterpart.counterpart_public_id.clone());
            Ok(PaymentProposalTarget {
                recipient_public_id: counterpart.counterpart_public_id.clone(),
                recipient_did: counterpart.target_agent.clone(),
                remote_node_id: counterpart.remote_node.clone(),
                social_counterpart: Some(counterpart),
            })
        }
        (None, None, Some(agent_id)) => resolve_servicenet_payment_target(state, agent_id, body).await,
        (None, None, None) => recipient_address.map_or_else(
            || {
                Err(
                    "display_name, counterpart_public_id, agent_id, or recipient_address is required for payment target"
                        .to_string(),
                )
            },
            |recipient_address| {
                Ok(PaymentProposalTarget {
                    recipient_public_id: recipient_address.clone(),
                    recipient_did: recipient_address.clone(),
                    remote_node_id: format!("payment:{recipient_address}"),
                    social_counterpart: None,
                })
            },
        ),
        _ => unreachable!("payment target count already validated"),
    }
}

fn resolve_network_agent_recipient_address(
    state: &ControlPlaneState,
    counterpart: &SocialCounterpartTarget,
    body: &mut AgentPaymentProposeBody,
) -> Result<(), String> {
    if trimmed_optional(body.recipient_address.as_deref()).is_some() {
        return Ok(());
    }
    let Some(address) = verified_network_agent_payment_address(
        state,
        counterpart,
        &body.rail,
        body.network.as_deref(),
    )?
    else {
        if requires_recipient_payment_address(body) {
            return Err(format!(
                "network agent {} has no verified payment address for {}{}",
                counterpart.counterpart_public_id,
                body.rail,
                body.network
                    .as_deref()
                    .map(|network| format!("/{network}"))
                    .unwrap_or_default()
            ));
        }
        return Ok(());
    };
    body.recipient_address = Some(address);
    Ok(())
}

fn requires_recipient_payment_address(body: &AgentPaymentProposeBody) -> bool {
    body.rail.trim().eq_ignore_ascii_case("x402") && matches!(body.layer, SettlementLayer::Web3)
}

fn verified_network_agent_payment_address(
    state: &ControlPlaneState,
    counterpart: &SocialCounterpartTarget,
    rail: &str,
    network: Option<&str>,
) -> Result<Option<String>, String> {
    let remote_identity = state
        .social_store
        .get_remote_identity(&counterpart.counterpart_public_id)
        .map_err(|error| error.to_string())?;
    let Some(remote_identity) = remote_identity else {
        return Ok(None);
    };
    let Some(document) = remote_identity.did_document_json.as_ref() else {
        return Ok(None);
    };
    verified_payment_address_from_document(document, &counterpart.target_agent, rail, network)
}

fn verified_payment_address_from_document(
    document: &Value,
    expected_agent_did: &str,
    rail: &str,
    network: Option<&str>,
) -> Result<Option<String>, String> {
    for candidate in payment_binding_candidates(document) {
        let proof = serde_json::from_value::<PaymentAccountBindingProof>(candidate.clone())
            .map_err(|error| format!("invalid payment_account_binding: {error}"))?;
        verify_payment_binding_identity(&proof, expected_agent_did)?;
        verify_payment_account_binding_proof(&proof)
            .map_err(|error| format!("payment_account_binding: {error}"))?;
        if payment_binding_matches_request(&proof, rail, network) {
            return Ok(Some(proof.payment_address.trim().to_string()));
        }
    }
    Ok(None)
}

fn payment_binding_candidates(document: &Value) -> Vec<&Value> {
    let mut candidates = Vec::new();
    push_optional_payment_binding(&mut candidates, document, "payment_account_binding");
    push_optional_payment_binding(&mut candidates, document, "paymentAccountBinding");
    push_optional_payment_bindings(&mut candidates, document, "payment_account_bindings");
    push_optional_payment_bindings(&mut candidates, document, "paymentAccountBindings");
    if let Some(payment) = document.get("payment") {
        push_optional_payment_binding(&mut candidates, payment, "account_binding");
        push_optional_payment_binding(&mut candidates, payment, "payment_account_binding");
        push_optional_payment_bindings(&mut candidates, payment, "account_bindings");
        push_optional_payment_bindings(&mut candidates, payment, "payment_account_bindings");
    }
    candidates
}

fn push_optional_payment_binding<'a>(
    candidates: &mut Vec<&'a Value>,
    document: &'a Value,
    key: &str,
) {
    if let Some(value) = document.get(key) {
        candidates.push(value);
    }
}

fn push_optional_payment_bindings<'a>(
    candidates: &mut Vec<&'a Value>,
    document: &'a Value,
    key: &str,
) {
    if let Some(values) = document.get(key).and_then(Value::as_array) {
        candidates.extend(values);
    }
}

fn verify_payment_binding_identity(
    proof: &PaymentAccountBindingProof,
    expected_agent_did: &str,
) -> Result<(), String> {
    if proof.agent_did.to_string() != expected_agent_did {
        return Err(
            "payment_account_binding agent_did does not match target agent DID".to_string(),
        );
    }
    Ok(())
}

fn payment_binding_matches_request(
    proof: &PaymentAccountBindingProof,
    rail: &str,
    network: Option<&str>,
) -> bool {
    if !proof.rail.trim().eq_ignore_ascii_case(rail.trim()) {
        return false;
    }
    if let Some(network) = trimmed_optional(network) {
        let Some(proof_network) = trimmed_optional(proof.network.as_deref()) else {
            return false;
        };
        if !proof_network.eq_ignore_ascii_case(network) {
            return false;
        }
    }
    true
}

async fn resolve_servicenet_payment_target(
    state: &ControlPlaneState,
    agent_id: &str,
    body: &mut AgentPaymentProposeBody,
) -> Result<PaymentProposalTarget, String> {
    let client = state
        .servicenet_client
        .as_deref()
        .ok_or_else(|| "servicenet is not configured".to_string())?;
    let agent = client
        .get_agent(agent_id)
        .await
        .map_err(|error| error.to_string())?;
    let accept = servicenet_payment_accept(&agent, &body.rail, body.network.as_deref())?
        .ok_or_else(|| {
            format!("servicenet agent {agent_id} does not expose x402 payment address")
        })?;
    if body.recipient_address.is_none() {
        body.recipient_address = string_at(&accept, &["payTo"]);
    }
    if body.network.is_none() {
        body.network = string_at(&accept, &["network"]);
    }
    if body.metadata.is_none() {
        body.metadata = Some(json!({
            "servicenet_agent_id": agent_id,
            "x402_accept": accept,
        }));
    }
    Ok(PaymentProposalTarget {
        recipient_public_id: agent_id.to_string(),
        recipient_did: agent_id.to_string(),
        remote_node_id: format!("servicenet:{agent_id}"),
        social_counterpart: None,
    })
}

fn normalized_display_name(display_name: Option<&str>) -> Option<&str> {
    display_name
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn payment_identity_display_name(identities: &[PublicIdentity], public_id: &str) -> Option<String> {
    identities
        .iter()
        .find(|identity| identity.public_id == public_id)
        .map(|identity| identity.display_name.trim())
        .filter(|display_name| !display_name.is_empty())
        .map(ToOwned::to_owned)
}

fn payment_remote_identity_display_name(
    state: &ControlPlaneState,
    public_id: &str,
) -> Option<String> {
    state
        .social_store
        .get_remote_identity(public_id)
        .ok()
        .flatten()
        .filter(|identity| identity.active)
        .map(|identity| identity.display_name.trim().to_string())
        .filter(|display_name| !display_name.is_empty())
}

fn payment_public_display_name(
    state: &ControlPlaneState,
    identities: &[PublicIdentity],
    public_id: &str,
) -> Option<String> {
    payment_identity_display_name(identities, public_id)
        .or_else(|| payment_remote_identity_display_name(state, public_id))
}

fn payment_local_public_id<'a>(
    state: &ControlPlaneState,
    payment: &'a PaymentTransaction,
) -> &'a str {
    if payment.sender_did == state.agent_did {
        &payment.sender_public_id
    } else {
        &payment.recipient_public_id
    }
}

fn payment_friendship_display_name(
    friendship: &Friendship,
    identities: &[PublicIdentity],
) -> Option<String> {
    friendship
        .display_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| payment_identity_display_name(identities, &friendship.remote_public_id))
}

async fn payment_display_names(
    state: &ControlPlaneState,
    local_public_id: &str,
    payment: &PaymentTransaction,
) -> PaymentDisplayNames {
    let identities = state.public_identity_registry.lock().await.list();
    let sender_display_name =
        payment_public_display_name(state, &identities, &payment.sender_public_id);
    let recipient_display_name =
        payment_public_display_name(state, &identities, &payment.recipient_public_id);
    let counterpart_public_id = if payment.sender_public_id == local_public_id {
        &payment.recipient_public_id
    } else {
        &payment.sender_public_id
    };
    let counterpart_display_name = if counterpart_public_id == payment.sender_public_id.as_str() {
        sender_display_name.clone()
    } else {
        recipient_display_name.clone()
    };
    PaymentDisplayNames {
        sender: sender_display_name,
        recipient: recipient_display_name,
        counterpart: counterpart_display_name,
    }
}

async fn payment_to_display_json(
    state: &ControlPlaneState,
    local_public_id: &str,
    payment: &PaymentTransaction,
) -> Value {
    let mut value = payment_to_json(payment);
    let names = payment_display_names(state, local_public_id, payment).await;
    if let Value::Object(object) = &mut value {
        insert_optional_string(object, "sender_display_name", names.sender);
        insert_optional_string(object, "recipient_display_name", names.recipient);
        insert_optional_string(object, "counterpart_display_name", names.counterpart);
    }
    value
}

fn insert_optional_string(object: &mut Map<String, Value>, key: &str, value: Option<String>) {
    if let Some(value) = value {
        object.insert(key.to_string(), Value::String(value));
    }
}

async fn payment_display_name_matches(
    state: &ControlPlaneState,
    local_public_id: &str,
    payment: &PaymentTransaction,
    display_name: &str,
) -> bool {
    let names = payment_display_names(state, local_public_id, payment).await;
    [
        names.sender.as_deref(),
        names.recipient.as_deref(),
        names.counterpart.as_deref(),
    ]
    .into_iter()
    .flatten()
    .any(|name| name.trim() == display_name)
}

async fn resolve_payment_counterpart_public_id_by_display_name(
    state: &ControlPlaneState,
    local_public_id: &str,
    display_name: &str,
) -> Result<String, String> {
    let display_name = display_name.trim();
    if display_name.is_empty() {
        return Err("display_name is required".to_string());
    }

    let identities = state.public_identity_registry.lock().await.list();
    let active_friendships =
        friendship_service::list_friendships(&*state.social_store, local_public_id)
            .map_err(|error| format!("query friendships: {error}"))?
            .into_iter()
            .filter(|friendship| friendship.state == FriendshipState::Active)
            .collect::<Vec<_>>();
    let mut matches = Vec::new();
    for friendship in active_friendships {
        let remote_identity = state
            .social_store
            .get_remote_identity(&friendship.remote_public_id)
            .map_err(|error| format!("query remote identity: {error}"))?;
        let remote_display_name = remote_identity
            .as_ref()
            .filter(|identity| identity.active)
            .map(|identity| identity.display_name.trim())
            .filter(|value| !value.is_empty());
        let friendship_display_name = payment_friendship_display_name(&friendship, &identities);
        if friendship_display_name.as_deref() == Some(display_name)
            || remote_display_name == Some(display_name)
        {
            matches.push(friendship.remote_public_id);
        }
    }

    match matches.as_slice() {
        [counterpart_public_id] => Ok(counterpart_public_id.clone()),
        [] => Err(format!(
            "active friend not found for display_name {display_name}"
        )),
        _ => Err(
            "multiple active friends matched display_name; provide counterpart_public_id"
                .to_string(),
        ),
    }
}

fn servicenet_x402_accept(agent: &Value) -> Option<Value> {
    value_at(agent, &["agent_card", "capabilities", "extensions"])?
        .as_array()?
        .iter()
        .filter(|extension| {
            string_at(extension, &["uri"]).as_deref() == Some(A2A_X402_EXTENSION_URI)
        })
        .find_map(|extension| {
            value_at(extension, &["params", "accepts"])?
                .as_array()?
                .iter()
                .find(|accept| {
                    string_at(accept, &["payTo"]).is_some_and(|pay_to| !pay_to.trim().is_empty())
                })
                .cloned()
        })
}

fn servicenet_payment_accept(
    agent: &Value,
    rail: &str,
    network: Option<&str>,
) -> Result<Option<Value>, String> {
    if let Some(accept) = servicenet_payment_binding_accept(agent, rail, network)? {
        return Ok(Some(accept));
    }
    Ok(servicenet_x402_accept(agent))
}

fn servicenet_payment_binding_accept(
    agent: &Value,
    rail: &str,
    network: Option<&str>,
) -> Result<Option<Value>, String> {
    for document in servicenet_did_document_candidates(agent) {
        for candidate in payment_binding_candidates(document) {
            let proof = serde_json::from_value::<PaymentAccountBindingProof>(candidate.clone())
                .map_err(|error| format!("invalid payment_account_binding: {error}"))?;
            verify_servicenet_payment_binding_identity(document, &proof)?;
            verify_payment_account_binding_proof(&proof)
                .map_err(|error| format!("payment_account_binding: {error}"))?;
            if payment_binding_matches_request(&proof, rail, network) {
                return Ok(Some(payment_binding_accept_value(&proof)));
            }
        }
    }
    Ok(None)
}

fn servicenet_did_document_candidates(agent: &Value) -> Vec<&Value> {
    let mut candidates = Vec::new();
    if let Some(document) = value_at(agent, &["agent_card", "didDocument"]) {
        candidates.push(document);
    }
    if let Some(document) = value_at(agent, &["agent_card", "did_document"]) {
        candidates.push(document);
    }
    if let Some(card) = value_at(agent, &["agent_card"]) {
        candidates.push(card);
    }
    candidates
}

fn verify_servicenet_payment_binding_identity(
    document: &Value,
    proof: &PaymentAccountBindingProof,
) -> Result<(), String> {
    let Some(document_id) = string_at(document, &["id"]) else {
        return Ok(());
    };
    if proof.agent_did.to_string() != document_id {
        return Err(
            "payment_account_binding agent_did does not match ServiceNet DID document id"
                .to_string(),
        );
    }
    Ok(())
}

fn payment_binding_accept_value(proof: &PaymentAccountBindingProof) -> Value {
    json!({
        "scheme": "exact",
        "rail": proof.rail,
        "network": proof.network,
        "payTo": proof.payment_address,
        "source": "payment_account_binding",
    })
}

fn value_at<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    path.iter()
        .try_fold(value, |current, key| current.get(*key))
}

fn string_at(value: &Value, path: &[&str]) -> Option<String> {
    value_at(value, path)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn trimmed_optional(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

async fn dispatch_payment_proposal_message(
    state: &ControlPlaneState,
    target: &PaymentProposalTarget,
    message: &PaymentAgentMessage,
) -> anyhow::Result<Value> {
    if let Some(counterpart) = target.social_counterpart.as_ref() {
        return send_payment_message(state, counterpart, message).await;
    }
    if let Some(agent_id) = target.remote_node_id.strip_prefix("servicenet:") {
        return Ok(json!({
            "ok": true,
            "mode": "servicenet",
            "agent_id": agent_id,
        }));
    }
    if let Some(address) = target.remote_node_id.strip_prefix("payment:") {
        return Ok(json!({
            "ok": true,
            "mode": "payment_address",
            "recipient_address": address,
        }));
    }
    Ok(json!({"ok": true, "mode": "unknown"}))
}

fn append_payment_proposed_event(
    state: &ControlPlaneState,
    payment: &PaymentTransaction,
    target: &PaymentProposalTarget,
    agent_id: Option<&str>,
) -> anyhow::Result<()> {
    state.append_signed_event(
        "AGENT_PAYMENT_PROPOSED",
        json!({
            "payment_id": payment.payment_id,
            "counterpart_public_id": target.social_counterpart.as_ref().map(|counterpart| counterpart.counterpart_public_id.clone()),
            "agent_id": agent_id,
        }),
    )?;
    Ok(())
}

pub(crate) async fn propose_agent_payment(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(mut body): Json<AgentPaymentProposeBody>,
) -> Response {
    if let Ok(Some(response)) = replay_commit_response(&state, &headers, "payments.propose") {
        return response;
    }
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let local = resolve_social_local_context(&state, body.public_id.as_deref()).await;
    let request_counterpart_public_id = body.counterpart_public_id.clone();
    let request_display_name = body.display_name.clone();
    let request_agent_id = body.agent_id.clone();
    let request_amount = body.amount.clone();
    let request_currency = body.currency.clone();
    let request_rail = body.rail.clone();
    let target = match resolve_payment_proposal_target(&state, &local.public_id, &mut body).await {
        Ok(target) => target,
        Err(error) => {
            return (StatusCode::BAD_REQUEST, Json(json!({"error": error}))).into_response();
        }
    };

    let mut ledger = state.payment_ledger.lock().await;
    let payment = match ledger.propose(
        &local.agent_id,
        ProposePaymentRequest {
            sender_public_id: local.public_id.clone(),
            remote_node_id: target.remote_node_id.clone(),
            recipient_public_id: target.recipient_public_id.clone(),
            recipient_did: target.recipient_did.clone(),
            amount: body.amount,
            currency: body.currency,
            rail: body.rail,
            layer: body.layer,
            network: body.network,
            recipient_address: body.recipient_address,
            mission_id: body.mission_id,
            task_id: body.task_id,
            description: body.description,
            metadata: body.metadata,
            expires_at: body.expires_at,
        },
    ) {
        Ok(payment) => payment,
        Err(error) => return internal_error(&error),
    };
    if let Err(error) = persist_payment_ledger(&state, &ledger) {
        return internal_error(&error);
    }
    drop(ledger);

    let message = payment.agent_message(PaymentMessageKind::Request, Utc::now().timestamp());
    let response = match dispatch_payment_proposal_message(&state, &target, &message).await {
        Ok(response) => response,
        Err(error) => return internal_error(&error),
    };

    if let Err(error) =
        append_payment_proposed_event(&state, &payment, &target, request_agent_id.as_deref())
    {
        return internal_error(&error);
    }
    emit_payment_stream_event(&state, "payments.proposed", &payment);
    append_payment_audit(
        &state,
        auth,
        "payments.agent.propose",
        Some(local.public_id.clone()),
        Some(json!({
            "payment_id": payment.payment_id.clone(),
            "remote_node_id": target.remote_node_id,
            "agent_id": request_agent_id.clone(),
        })),
    );

    let response_json = json!({
        "ok": true,
        "payment": payment_to_display_json(&state, &local.public_id, &payment).await,
        "transport": response,
    });
    if let Err(error) = append_commit_response(
        &state,
        &headers,
        CommitResponseArgs {
            action_type: "payments.propose",
            target_id: Some(payment.payment_id.clone()),
            actor_public_id: Some(local.public_id),
            actor_agent_did: Some(local.agent_id),
            request_json: &json!({
                "counterpart_public_id": request_counterpart_public_id,
                "display_name": request_display_name,
                "agent_id": request_agent_id,
                "amount": request_amount,
                "currency": request_currency,
                "rail": request_rail,
            }),
            response_json: &response_json,
        },
    ) {
        return internal_error(&error);
    }
    Json(response_json).into_response()
}

pub(crate) async fn authorize_agent_payment(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Path(payment_id): Path<String>,
    Json(body): Json<AgentPaymentAuthorizeBody>,
) -> Response {
    if let Ok(Some(response)) = replay_commit_response(&state, &headers, "payments.authorize") {
        return response;
    }
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let authorized =
        match update_outbound_payment_with_wallet_signature(&state, &payment_id, body).await {
            Ok(payment) => payment,
            Err(response) => return response,
        };
    if let Err(error) =
        notify_counterpart_of_payment_change(&state, &authorized, PaymentMessageKind::Authorized)
            .await
    {
        return internal_error(&error);
    }
    if let Err(error) = state.append_signed_event(
        "AGENT_PAYMENT_AUTHORIZED",
        json!({"payment_id": authorized.payment_id}),
    ) {
        return internal_error(&error);
    }
    emit_payment_stream_event(&state, "payments.authorized", &authorized);
    append_payment_audit(
        &state,
        auth,
        "payments.agent.authorize",
        Some(authorized.sender_public_id.clone()),
        Some(json!({"payment_id": authorized.payment_id})),
    );
    let response_json = payment_to_display_json(
        &state,
        payment_local_public_id(&state, &authorized),
        &authorized,
    )
    .await;
    if let Err(error) = append_commit_response(
        &state,
        &headers,
        CommitResponseArgs {
            action_type: "payments.authorize",
            target_id: Some(authorized.payment_id.clone()),
            actor_public_id: Some(authorized.sender_public_id.clone()),
            actor_agent_did: Some(authorized.sender_did.clone()),
            request_json: &json!({"payment_id": payment_id}),
            response_json: &response_json,
        },
    ) {
        return internal_error(&error);
    }
    Json(response_json).into_response()
}

pub(crate) async fn submit_agent_payment(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Path(payment_id): Path<String>,
    body: Option<Json<AgentPaymentSubmitBody>>,
) -> Response {
    if let Ok(Some(response)) = replay_commit_response(&state, &headers, "payments.submit") {
        return response;
    }
    let body = body.map(|Json(body)| body).unwrap_or_default();
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let mut settlement_receipt = body.settlement_receipt.clone();
    if settlement_receipt.is_none() {
        let current = match payment_snapshot_for_submit(&state, &payment_id).await {
            Ok(payment) => payment,
            Err(response) => return response,
        };
        match super::payment_chain::submit_x402_erc20_payment(&state.data_dir, &current).await {
            Ok(receipt) => {
                settlement_receipt = receipt;
            }
            Err(error) => {
                return payment_error_response(StatusCode::BAD_REQUEST, &error.to_string());
            }
        }
    }
    let submitted_receipt = settlement_receipt.clone();
    let updated = match mutate_payment(&state, &payment_id, |ledger, payment| {
        ensure_sender_controls_payment(payment, &state)?;
        ledger.submit(&payment.payment_id, submitted_receipt)
    })
    .await
    {
        Ok(payment) => payment,
        Err(response) => return response,
    };
    if let Err(error) =
        notify_counterpart_of_payment_change(&state, &updated, PaymentMessageKind::Submitted).await
    {
        return internal_error(&error);
    }
    emit_payment_stream_event(&state, "payments.submitted", &updated);
    append_payment_audit(
        &state,
        auth,
        "payments.agent.submit",
        Some(updated.sender_public_id.clone()),
        Some(json!({
            "payment_id": updated.payment_id,
            "settlement_receipt": updated.settlement_receipt
        })),
    );
    let response_json =
        payment_to_display_json(&state, payment_local_public_id(&state, &updated), &updated).await;
    if let Err(error) = append_commit_response(
        &state,
        &headers,
        CommitResponseArgs {
            action_type: "payments.submit",
            target_id: Some(updated.payment_id.clone()),
            actor_public_id: Some(updated.sender_public_id.clone()),
            actor_agent_did: Some(updated.sender_did.clone()),
            request_json: &json!({
                "payment_id": payment_id,
                "settlement_receipt": settlement_receipt
            }),
            response_json: &response_json,
        },
    ) {
        return internal_error(&error);
    }
    Json(response_json).into_response()
}

async fn payment_snapshot_for_submit(
    state: &ControlPlaneState,
    payment_id: &str,
) -> Result<PaymentTransaction, Response> {
    let ledger = state.payment_ledger.lock().await;
    let current = match ledger.get(payment_id) {
        Some(payment) => payment.clone(),
        None => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(json!({"error": format!("payment not found: {payment_id}")})),
            )
                .into_response());
        }
    };
    if let Err(error) = ensure_sender_controls_payment(&current, state) {
        return Err(payment_error_response(
            StatusCode::BAD_REQUEST,
            &error.to_string(),
        ));
    }
    if current.status != PaymentStatus::Authorized {
        return Err(payment_error_response(
            StatusCode::BAD_REQUEST,
            &format!(
                "payment {payment_id} is not in authorized state (current: {:?})",
                current.status
            ),
        ));
    }
    Ok(current)
}

pub(crate) async fn settle_agent_payment(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Path(payment_id): Path<String>,
    Json(body): Json<AgentPaymentSettleBody>,
) -> Response {
    if let Ok(Some(response)) = replay_commit_response(&state, &headers, "payments.settle") {
        return response;
    }
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let current = match payment_snapshot_for_settle(&state, &payment_id).await {
        Ok(payment) => payment,
        Err(response) => return response,
    };
    if let Err(error) = super::payment_chain::verify_x402_erc20_settlement_receipt(
        &current,
        &body.settlement_receipt,
    )
    .await
    {
        return payment_error_response(StatusCode::BAD_REQUEST, &error.to_string());
    }
    let updated = match mutate_payment(&state, &payment_id, |ledger, payment| {
        ensure_payment_participant(payment, &state)?;
        ledger.settle(SettlePaymentRequest {
            payment_id: payment.payment_id.clone(),
            settlement_receipt: body.settlement_receipt.clone(),
        })
    })
    .await
    {
        Ok(payment) => payment,
        Err(response) => return response,
    };
    if let Err(error) =
        notify_counterpart_of_payment_change(&state, &updated, PaymentMessageKind::Settled).await
    {
        return internal_error(&error);
    }
    emit_payment_stream_event(&state, "payments.settled", &updated);
    append_payment_audit(
        &state,
        auth,
        "payments.agent.settle",
        Some(updated.sender_public_id.clone()),
        Some(json!({"payment_id": updated.payment_id})),
    );
    let response_json =
        payment_to_display_json(&state, payment_local_public_id(&state, &updated), &updated).await;
    if let Err(error) = append_commit_response(
        &state,
        &headers,
        CommitResponseArgs {
            action_type: "payments.settle",
            target_id: Some(updated.payment_id.clone()),
            actor_public_id: Some(updated.sender_public_id.clone()),
            actor_agent_did: Some(updated.sender_did.clone()),
            request_json: &json!({"payment_id": payment_id}),
            response_json: &response_json,
        },
    ) {
        return internal_error(&error);
    }
    Json(response_json).into_response()
}

async fn payment_snapshot_for_settle(
    state: &ControlPlaneState,
    payment_id: &str,
) -> Result<PaymentTransaction, Response> {
    let ledger = state.payment_ledger.lock().await;
    let current = match ledger.get(payment_id) {
        Some(payment) => payment.clone(),
        None => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(json!({"error": format!("payment not found: {payment_id}")})),
            )
                .into_response());
        }
    };
    if let Err(error) = ensure_payment_participant(&current, state) {
        return Err(payment_error_response(
            StatusCode::BAD_REQUEST,
            &error.to_string(),
        ));
    }
    if !matches!(
        current.status,
        PaymentStatus::Submitted | PaymentStatus::Authorized
    ) {
        return Err(payment_error_response(
            StatusCode::BAD_REQUEST,
            &format!(
                "payment {payment_id} is not in a settleable state (current: {:?})",
                current.status
            ),
        ));
    }
    Ok(current)
}

pub(crate) async fn reject_agent_payment(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Path(payment_id): Path<String>,
    Json(body): Json<AgentPaymentRejectBody>,
) -> Response {
    if let Ok(Some(response)) = replay_commit_response(&state, &headers, "payments.reject") {
        return response;
    }
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let updated = match mutate_payment(&state, &payment_id, |ledger, payment| {
        ensure_recipient_controls_payment(payment, &state)?;
        ledger.reject(RejectPaymentRequest {
            payment_id: payment.payment_id.clone(),
            reject_reason: body.reject_reason.clone(),
        })
    })
    .await
    {
        Ok(payment) => payment,
        Err(response) => return response,
    };
    if let Err(error) =
        notify_counterpart_of_payment_change(&state, &updated, PaymentMessageKind::Rejected).await
    {
        return internal_error(&error);
    }
    emit_payment_stream_event(&state, "payments.rejected", &updated);
    append_payment_audit(
        &state,
        auth,
        "payments.agent.reject",
        Some(updated.recipient_public_id.clone()),
        Some(json!({"payment_id": updated.payment_id})),
    );
    let response_json =
        payment_to_display_json(&state, payment_local_public_id(&state, &updated), &updated).await;
    if let Err(error) = append_commit_response(
        &state,
        &headers,
        CommitResponseArgs {
            action_type: "payments.reject",
            target_id: Some(updated.payment_id.clone()),
            actor_public_id: Some(updated.recipient_public_id.clone()),
            actor_agent_did: Some(updated.recipient_did.clone()),
            request_json: &json!({"payment_id": payment_id, "reject_reason": body.reject_reason}),
            response_json: &response_json,
        },
    ) {
        return internal_error(&error);
    }
    Json(response_json).into_response()
}

pub(crate) async fn cancel_agent_payment(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Path(payment_id): Path<String>,
) -> Response {
    if let Ok(Some(response)) = replay_commit_response(&state, &headers, "payments.cancel") {
        return response;
    }
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let updated = match mutate_payment(&state, &payment_id, |ledger, payment| {
        ensure_sender_controls_payment(payment, &state)?;
        ledger.cancel(&payment.payment_id)
    })
    .await
    {
        Ok(payment) => payment,
        Err(response) => return response,
    };
    if let Err(error) =
        notify_counterpart_of_payment_change(&state, &updated, PaymentMessageKind::Cancelled).await
    {
        return internal_error(&error);
    }
    emit_payment_stream_event(&state, "payments.cancelled", &updated);
    append_payment_audit(
        &state,
        auth,
        "payments.agent.cancel",
        Some(updated.sender_public_id.clone()),
        Some(json!({"payment_id": updated.payment_id})),
    );
    let response_json =
        payment_to_display_json(&state, payment_local_public_id(&state, &updated), &updated).await;
    if let Err(error) = append_commit_response(
        &state,
        &headers,
        CommitResponseArgs {
            action_type: "payments.cancel",
            target_id: Some(updated.payment_id.clone()),
            actor_public_id: Some(updated.sender_public_id.clone()),
            actor_agent_did: Some(updated.sender_did.clone()),
            request_json: &json!({"payment_id": payment_id}),
            response_json: &response_json,
        },
    ) {
        return internal_error(&error);
    }
    Json(response_json).into_response()
}

async fn update_outbound_payment_with_wallet_signature(
    state: &ControlPlaneState,
    payment_id: &str,
    body: AgentPaymentAuthorizeBody,
) -> Result<PaymentTransaction, Response> {
    let mut ledger = state.payment_ledger.lock().await;
    let current = match ledger.get(payment_id) {
        Some(payment) => payment.clone(),
        None => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(json!({"error": format!("payment not found: {payment_id}")})),
            )
                .into_response());
        }
    };
    if let Err(error) = ensure_sender_controls_payment(&current, state) {
        return Err(payment_error_response(
            StatusCode::FORBIDDEN,
            &error.to_string(),
        ));
    }

    let wallet_state = match open_local_wallet(&state.data_dir) {
        Ok(wallet) => wallet,
        Err(error) => return Err(internal_error(&error)),
    };
    let mut signing_target = current.clone();
    let active_account = match wallet_state
        .wallet
        .active_payment_account(&wallet_state.profile)
    {
        Ok(account) => account.clone(),
        Err(error) => return Err(internal_error(&error.into())),
    };
    if active_account.key_handle.is_none() {
        return Err(payment_error_response(
            StatusCode::FORBIDDEN,
            "active payment account is watch-only and cannot sign payments",
        ));
    }
    let Some(active_address) = active_account.address.clone() else {
        return Err(payment_error_response(
            StatusCode::BAD_REQUEST,
            "active payment account is missing an address",
        ));
    };
    let sender_address = match body.sender_address {
        Some(address) if address.eq_ignore_ascii_case(&active_address) => Some(address),
        Some(_) => {
            return Err(payment_error_response(
                StatusCode::FORBIDDEN,
                "sender_address must match the active signing payment account",
            ));
        }
        None => Some(active_address),
    };
    signing_target.sender_address.clone_from(&sender_address);
    let payload = match authorization_payload_bytes(&signing_target) {
        Ok(payload) => payload,
        Err(error) => return Err(internal_error(&error)),
    };
    let signature = match wallet_state
        .wallet
        .sign_with_active_payment_account(&wallet_state.profile, &payload)
    {
        Ok(signature) => signature,
        Err(error) => return Err(internal_error(&error.into())),
    };
    let key_info = match wallet_state
        .wallet
        .active_payment_account_key_info(&wallet_state.profile)
    {
        Ok(info) => info,
        Err(error) => return Err(internal_error(&error.into())),
    };
    let updated = match ledger.authorize(AuthorizePaymentRequest {
        payment_id: payment_id.to_string(),
        authorization_signature: STANDARD.encode(signature.0),
        authorization_public_key: Some(key_info.public_key_multibase),
        sender_address,
    }) {
        Ok(payment) => payment,
        Err(error) => return Err(internal_error(&error)),
    };
    if let Err(error) = persist_payment_ledger(state, &ledger) {
        return Err(internal_error(&error));
    }
    Ok(updated)
}

async fn mutate_payment<F>(
    state: &ControlPlaneState,
    payment_id: &str,
    mutate: F,
) -> Result<PaymentTransaction, Response>
where
    F: FnOnce(&mut PaymentLedger, &PaymentTransaction) -> anyhow::Result<PaymentTransaction>,
{
    let mut ledger = state.payment_ledger.lock().await;
    let current = match ledger.get(payment_id) {
        Some(payment) => payment.clone(),
        None => {
            return Err((
                StatusCode::NOT_FOUND,
                Json(json!({"error": format!("payment not found: {payment_id}")})),
            )
                .into_response());
        }
    };
    let updated = match mutate(&mut ledger, &current) {
        Ok(payment) => payment,
        Err(error) => {
            return Err(payment_error_response(
                StatusCode::BAD_REQUEST,
                &error.to_string(),
            ));
        }
    };
    if let Err(error) = persist_payment_ledger(state, &ledger) {
        return Err(internal_error(&error));
    }
    Ok(updated)
}

async fn notify_counterpart_of_payment_change(
    state: &ControlPlaneState,
    payment: &PaymentTransaction,
    kind: PaymentMessageKind,
) -> anyhow::Result<Value> {
    if let Some(agent_id) = payment.remote_node_id.strip_prefix("servicenet:") {
        return Ok(json!({
            "ok": true,
            "mode": "servicenet",
            "agent_id": agent_id,
            "message_kind": kind.as_str(),
        }));
    }
    let counterpart =
        if let Some(counterpart) = payment_counterpart_from_remote_node(state, payment) {
            counterpart
        } else {
            match resolve_social_counterpart_target(state, &payment.recipient_public_id).await {
                Ok(counterpart) => counterpart,
                Err(_) => resolve_social_counterpart_target(state, &payment.sender_public_id)
                    .await
                    .map_err(anyhow::Error::msg)?,
            }
        };
    send_payment_message(
        state,
        &counterpart,
        &payment.agent_message(kind, Utc::now().timestamp()),
    )
    .await
}

fn payment_counterpart_from_remote_node(
    state: &ControlPlaneState,
    payment: &PaymentTransaction,
) -> Option<SocialCounterpartTarget> {
    let remote_node = payment.remote_node_id.trim();
    if remote_node.is_empty() || remote_node.starts_with("servicenet:") {
        return None;
    }

    let (counterpart_public_id, target_agent) = if payment.sender_did == state.agent_did {
        (
            payment.recipient_public_id.clone(),
            payment.recipient_did.clone(),
        )
    } else if payment.recipient_did == state.agent_did {
        (payment.sender_public_id.clone(), payment.sender_did.clone())
    } else {
        return None;
    };

    Some(SocialCounterpartTarget {
        counterpart_public_id,
        remote_node: remote_node.to_owned(),
        target_agent,
    })
}

async fn send_payment_message(
    state: &ControlPlaneState,
    counterpart: &crate::social_host::SocialCounterpartTarget,
    message: &PaymentAgentMessage,
) -> anyhow::Result<Value> {
    let local = resolve_social_local_context(state, None).await;
    let local_node_id = state.swarm_bridge.local_node_id().await.ok();
    let mut message_payload = json!({
        "message_kind": message.kind.as_str(),
        "payment": message.payment,
        "counterpart_public_id": counterpart.counterpart_public_id,
    });
    let binding_required =
        source_payment_account_binding_required(&message.kind, &message.payment, &local.agent_id);
    match try_build_payment_account_binding(state) {
        Ok(Some(binding)) => {
            if let Some(map) = message_payload.as_object_mut() {
                map.insert("payment_account_binding".to_owned(), binding);
            }
        }
        Ok(None) if binding_required => {
            bail!("payment_account_binding is required for sender-signed payment state");
        }
        Err(error) if binding_required => {
            return Err(error.context("build payment_account_binding"));
        }
        Ok(None) | Err(_) => {}
    }
    let agent_envelope = build_signed_agent_envelope_for_nodes(
        state,
        SignedAgentEnvelopeArgs {
            source_agent_id: local.agent_id,
            source_public_id: public_agent_id(&local.public_id),
            source_display_name: local.display_name,
            target_agent_id: Some(counterpart.target_agent.clone()),
            source_node_id: local_node_id,
            target_node_id: Some(counterpart.remote_node.clone()),
            capability: "payment.agent_message".to_string(),
            message: message_payload,
            extensions: None,
        },
    )?;
    state
        .swarm_bridge
        .publish_agent_payment_message(SwarmAgentPaymentCommand {
            remote_node_id: counterpart.remote_node.clone(),
            message_kind: message.kind.as_str().to_string(),
            payment: serde_json::to_value(&message.payment)
                .context("serialize payment transaction")?,
            agent_envelope,
        })
        .await
}

/// Best-effort `PaymentAccountBindingProof` for the active spending payment
/// account. Missing wallets, watch-only accounts, and inactive payment accounts
/// return `Ok(None)` for backwards compatibility. Callers decide when the proof
/// is required for a payment state transition.
fn try_build_payment_account_binding(state: &ControlPlaneState) -> anyhow::Result<Option<Value>> {
    let Some(proof) = active_payment_account_binding_proof(&state.data_dir, state.signer.as_ref())?
    else {
        return Ok(None);
    };
    serde_json::to_value(&proof)
        .context("serialize active payment account binding proof")
        .map(Some)
}

fn ensure_sender_controls_payment(
    payment: &PaymentTransaction,
    state: &ControlPlaneState,
) -> anyhow::Result<()> {
    if payment.sender_did != state.agent_did {
        bail!("only the sender agent can modify this payment state");
    }
    Ok(())
}

fn ensure_recipient_controls_payment(
    payment: &PaymentTransaction,
    state: &ControlPlaneState,
) -> anyhow::Result<()> {
    if payment.recipient_did != state.agent_did {
        bail!("only the recipient agent can modify this payment state");
    }
    Ok(())
}

fn ensure_payment_participant(
    payment: &PaymentTransaction,
    state: &ControlPlaneState,
) -> anyhow::Result<()> {
    if payment.sender_did != state.agent_did && payment.recipient_did != state.agent_did {
        bail!("only payment participants can modify this payment state");
    }
    Ok(())
}

fn persist_payment_ledger(state: &ControlPlaneState, ledger: &PaymentLedger) -> anyhow::Result<()> {
    state
        .local_db
        .save_domain(local_db::domain::PAYMENT_LEDGER, ledger)
        .context("persist payment ledger")
}

pub(crate) fn payment_to_json(payment: &PaymentTransaction) -> Value {
    serde_json::to_value(payment).unwrap_or(Value::Null)
}

fn payment_account_to_json(account: &watt_wallet::PaymentAccount) -> Value {
    let can_sign = account.key_handle.is_some();
    let receive_only = !can_sign;
    json!({
        "account_id": account.account_id,
        "rail": account.rail,
        "network": account.network,
        "address": account.address,
        "kind": account.kind,
        "layer": account.layer,
        "capabilities": account.capabilities,
        "custody": if can_sign { "local_key" } else { "watch_only" },
        "can_sign": can_sign,
        "can_submit_payment": can_sign,
        "receive_only": receive_only,
    })
}

fn is_evm_address(address: &str) -> bool {
    address.len() == 42
        && address.starts_with("0x")
        && address
            .chars()
            .skip(2)
            .all(|value| value.is_ascii_hexdigit())
}

fn network_from_chain_id(chain_id: Option<&str>) -> Option<String> {
    match chain_id {
        Some("0x1") => Some("ethereum".to_string()),
        Some("0x89") => Some("polygon".to_string()),
        Some("0xa") => Some("optimism".to_string()),
        Some("0xa4b1") => Some("arbitrum-one".to_string()),
        Some("0x2105") => Some("base".to_string()),
        Some("0x14a34") => Some("base-sepolia".to_string()),
        _ => None,
    }
}

fn wallet_now_ms() -> u64 {
    Utc::now().timestamp_millis().try_into().unwrap_or(0)
}

fn wallet_internal_error(error: impl Into<anyhow::Error>) -> Response {
    let error = error.into();
    internal_error(&error)
}

fn emit_payment_stream_event(state: &ControlPlaneState, kind: &str, payment: &PaymentTransaction) {
    let _ = state.stream_tx.send(StreamEvent {
        kind: kind.to_string(),
        timestamp: Utc::now().timestamp(),
        payload: json!({
            "payment_id": payment.payment_id,
            "status": payment.status,
            "sender_public_id": payment.sender_public_id,
            "recipient_public_id": payment.recipient_public_id,
        }),
    });
}

fn append_payment_audit(
    state: &ControlPlaneState,
    auth: String,
    action: &str,
    subject: Option<String>,
    details: Option<Value>,
) {
    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "payments".to_string(),
        action: action.to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject,
        capability: Some(PAYMENT_MESSAGE_CAPABILITY.to_string()),
        reason: None,
        duration_ms: None,
        details,
    });
}

fn payment_error_response(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({"error": message}))).into_response()
}
