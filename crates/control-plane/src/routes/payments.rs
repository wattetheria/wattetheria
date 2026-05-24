use anyhow::{Context, bail};
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use chrono::Utc;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::auth::{authorize, internal_error};
use crate::social_host::{
    SignedAgentEnvelopeArgs, build_signed_agent_envelope_for_nodes,
    resolve_social_counterpart_target, resolve_social_local_context,
};
use crate::state::{
    AgentPaymentAuthorizeBody, AgentPaymentProposeBody, AgentPaymentRejectBody,
    AgentPaymentSettleBody, AgentPaymentsQuery, ControlPlaneState, StreamEvent,
    WalletBindWeb3PaymentAccountBody, WalletCreatePaymentAccountBody,
    agent_commit_context_from_headers,
};
use watt_did::PaymentAccountCustody;
use watt_wallet::{
    PaymentAccountBindingProofOptions, PaymentAccountKind, PaymentAccountSigner,
    build_payment_account_binding_proof,
};
use wattetheria_kernel::audit::AuditEntry;
use wattetheria_kernel::local_db;
use wattetheria_kernel::payments::{
    AuthorizePaymentRequest, PaymentAgentMessage, PaymentLedger, PaymentMessageKind, PaymentQuery,
    PaymentTransaction, ProposePaymentRequest, RejectPaymentRequest, SettlePaymentRequest,
    authorization_payload_bytes, source_payment_account_binding_required,
};
use wattetheria_kernel::swarm_bridge::SwarmAgentPaymentCommand;
use wattetheria_kernel::wallet_identity::{
    load_or_create_wallet_backed_identity, open_local_wallet,
};

const PAYMENT_MESSAGE_CAPABILITY: &str = "payments.agent.transfer";
const WALLET_BIND_CAPABILITY: &str = "wallet.bind";

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

    if let Err(error) = load_or_create_wallet_backed_identity(&state.data_dir) {
        return internal_error(&error);
    }
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

    if let Err(error) = load_or_create_wallet_backed_identity(&state.data_dir) {
        return internal_error(&error);
    }
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
    let items = ledger
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
            true
        })
        .map(payment_to_json)
        .collect::<Vec<_>>();
    let summary = serde_json::to_value(ledger.summary()).unwrap_or(Value::Null);
    drop(ledger);

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
    let Some(payment) = ledger.get(&payment_id) else {
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
    let payload = payment_to_json(payment);
    drop(ledger);

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

pub(crate) async fn propose_agent_payment(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<AgentPaymentProposeBody>,
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
    let request_amount = body.amount.clone();
    let request_currency = body.currency.clone();
    let request_rail = body.rail.clone();
    let counterpart =
        match resolve_social_counterpart_target(&state, &body.counterpart_public_id).await {
            Ok(counterpart) => counterpart,
            Err(error) => {
                return (StatusCode::BAD_REQUEST, Json(json!({"error": error}))).into_response();
            }
        };

    let mut ledger = state.payment_ledger.lock().await;
    let payment = match ledger.propose(
        &local.agent_id,
        ProposePaymentRequest {
            sender_public_id: local.public_id.clone(),
            remote_node_id: counterpart.remote_node.clone(),
            recipient_public_id: counterpart.counterpart_public_id.clone(),
            recipient_did: counterpart.target_agent.clone(),
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
    let response = match send_payment_message(&state, &counterpart, &message).await {
        Ok(response) => response,
        Err(error) => return internal_error(&error),
    };

    if let Err(error) = state.append_signed_event(
        "AGENT_PAYMENT_PROPOSED",
        json!({"payment_id": payment.payment_id, "counterpart_public_id": counterpart.counterpart_public_id}),
    ) {
        return internal_error(&error);
    }
    emit_payment_stream_event(&state, "payments.proposed", &payment);
    append_payment_audit(
        &state,
        auth,
        "payments.agent.propose",
        Some(local.public_id.clone()),
        Some(json!({"payment_id": payment.payment_id, "remote_node_id": counterpart.remote_node})),
    );

    let response_json = json!({
        "ok": true,
        "payment": payment_to_json(&payment),
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
    let response_json = payment_to_json(&authorized);
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
) -> Response {
    if let Ok(Some(response)) = replay_commit_response(&state, &headers, "payments.submit") {
        return response;
    }
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let updated = match mutate_payment(&state, &payment_id, |ledger, payment| {
        ensure_sender_controls_payment(payment, &state)?;
        ledger.submit(&payment.payment_id)
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
        Some(json!({"payment_id": updated.payment_id})),
    );
    let response_json = payment_to_json(&updated);
    if let Err(error) = append_commit_response(
        &state,
        &headers,
        CommitResponseArgs {
            action_type: "payments.submit",
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
    let response_json = payment_to_json(&updated);
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
    let response_json = payment_to_json(&updated);
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
    let response_json = payment_to_json(&updated);
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
    let counterpart =
        match resolve_social_counterpart_target(state, &payment.recipient_public_id).await {
            Ok(counterpart) => counterpart,
            Err(_) => resolve_social_counterpart_target(state, &payment.sender_public_id)
                .await
                .map_err(anyhow::Error::msg)?,
        };
    send_payment_message(
        state,
        &counterpart,
        &payment.agent_message(kind, Utc::now().timestamp()),
    )
    .await
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
    let Ok(wallet_state) = open_local_wallet(&state.data_dir) else {
        return Ok(None);
    };
    let agent_key_info = wallet_state
        .wallet
        .active_identity_key_info(&wallet_state.profile)
        .ok();
    let Some(agent_key_info) = agent_key_info else {
        return Ok(None);
    };
    let active_account = wallet_state
        .wallet
        .active_payment_account(&wallet_state.profile)
        .ok()
        .cloned();
    let Some(active_account) = active_account else {
        return Ok(None);
    };
    if active_account.key_handle.is_none() {
        return Ok(None);
    }
    let payment_key_info = wallet_state
        .wallet
        .active_payment_account_key_info(&wallet_state.profile)
        .context("load active payment account key")?
        .clone();
    let issued_at_ms = Utc::now()
        .timestamp_millis()
        .max(0)
        .try_into()
        .unwrap_or(u64::MAX);
    let proof = build_payment_account_binding_proof(
        wallet_state.wallet.keystore(),
        PaymentAccountBindingProofOptions {
            agent_did: agent_key_info.did.clone(),
            agent_key_handle: &agent_key_info.key_handle,
            agent_public_key_multibase: agent_key_info.public_key_multibase.clone(),
            rail: active_account.rail.clone(),
            network: active_account.network.clone(),
            custody: PaymentAccountCustody::LocalGenerated,
            receive_only: false,
            can_sign: true,
            capabilities: active_account.capabilities.clone(),
            issued_at_ms,
            expires_at_ms: None,
            nonce: None,
            payment_signer: Some(PaymentAccountSigner {
                key_handle: &payment_key_info.key_handle,
                public_key_multibase: payment_key_info.public_key_multibase.clone(),
            }),
            watch_only_payment_address: None,
        },
    )
    .context("build active payment account binding proof")?;
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
