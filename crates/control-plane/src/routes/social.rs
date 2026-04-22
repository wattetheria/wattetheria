use axum::Json;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use uuid::Uuid;

use crate::auth::{authorize, internal_error};
use crate::social_host::{
    SocialCounterpartTarget, SocialLocalContext, build_signed_agent_envelope,
    capability_for_relationship_action, counterpart_public_id_for_remote_node,
    load_social_identity_maps, resolve_social_counterpart_target, resolve_social_local_context,
    with_social_defaults,
};
use crate::state::{
    AgentDmMessagesQuery, AgentDmSendBody, AgentDmThreadsQuery, AgentRelationshipActionBody,
    ControlPlaneState, RelationshipBody, RelationshipQuery, StreamEvent,
    agent_commit_context_from_headers,
};
use wattetheria_kernel::audit::AuditEntry;
use wattetheria_kernel::identities::{ControllerBinding, PublicIdentity};
use wattetheria_kernel::relationships::RelationshipEdge;
use wattetheria_kernel::swarm_bridge::{
    SwarmDirectMessageCommand, SwarmPeerDmMessageView, SwarmPeerDmThreadView,
    SwarmPeerRelationshipView, SwarmRelationshipAction, SwarmRelationshipActionCommand,
};
use wattetheria_social::application::{
    block_service, friend_request_service, friendship_service, message_service,
    orchestration_service, policy_service, receipt_service, thread_service,
};
use wattetheria_social::domain::blocks::SocialBlock;
use wattetheria_social::domain::friend_requests::{
    FriendRequest, FriendRequestDirection, FriendRequestState,
};
use wattetheria_social::domain::friendships::{Friendship, FriendshipState};
use wattetheria_social::domain::messages::{
    DeliveryState, DirectMessage, MessageDirection, MessageKind,
};
use wattetheria_social::domain::receipts::{MessageReceipt, ReceiptKind};
use wattetheria_social::domain::threads::{DirectThread, ThreadState};
use wattetheria_social::policy::decisions::PolicyDecision;

struct CommitResponseArgs<'a> {
    action_type: &'a str,
    target_id: Option<String>,
    actor_public_id: Option<String>,
    request_json: &'a Value,
    response_json: &'a Value,
}

struct FinalizeRelationshipActionArgs {
    auth: String,
    local_public_id: String,
    counterpart_public_id: String,
    target_agent: String,
    remote_node_id: String,
    action: SwarmRelationshipAction,
    capability: String,
    request_counterpart_public_id: String,
    message: Value,
    response_json: Value,
}

struct FinalizeDmArgs {
    auth: String,
    local_public_id: String,
    counterpart_public_id: String,
    target_agent: String,
    remote_node_id: String,
    thread_id: String,
    message_id: String,
    request_counterpart_public_id: String,
    content: Value,
    agent_envelope_json: Value,
    agent_signature: Option<String>,
    response_json: Value,
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
            domain: "social".to_owned(),
            target_id: args.target_id,
            expected_state: None,
            result_state: None,
            request_json: serde_json::to_string(args.request_json)?,
            result_json: serde_json::to_string(args.response_json)?,
            status: "accepted".to_owned(),
            actor_public_id: args.actor_public_id,
            actor_agent_did: None,
            created_at: Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string(),
        },
    )
}

async fn finalize_agent_relationship_action(
    state: &ControlPlaneState,
    headers: &HeaderMap,
    args: FinalizeRelationshipActionArgs,
) -> Response {
    if let Err(error) = persist_social_relationship_action(
        state,
        &args.local_public_id,
        &args.counterpart_public_id,
        &args.target_agent,
        &args.remote_node_id,
        &args.action,
        &args.message,
    )
    .await
    {
        return internal_error(&error);
    }

    let _ = state.stream_tx.send(StreamEvent {
        kind: "civilization.agent_relationship.command".to_string(),
        timestamp: Utc::now().timestamp(),
        payload: json!({
            "counterpart_public_id": args.counterpart_public_id,
            "remote_node_id": args.remote_node_id,
            "action": args.action,
            "response": args.response_json,
        }),
    });
    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "civilization".to_string(),
        action: "civilization.agent_relationships.command".to_string(),
        status: "ok".to_string(),
        actor: Some(args.auth),
        subject: Some(args.local_public_id.clone()),
        capability: Some(args.capability),
        reason: None,
        duration_ms: None,
        details: Some(json!({
            "counterpart_public_id": args.counterpart_public_id,
            "remote_node_id": args.remote_node_id,
            "action": args.action,
        })),
    });
    if let Err(error) = append_commit_response(
        state,
        headers,
        CommitResponseArgs {
            action_type: "social.agent_relationship_action",
            target_id: Some(args.counterpart_public_id),
            actor_public_id: Some(args.local_public_id),
            request_json: &json!({
                "counterpart_public_id": args.request_counterpart_public_id,
                "action": args.action,
            }),
            response_json: &args.response_json,
        },
    ) {
        return internal_error(&error);
    }
    (StatusCode::ACCEPTED, Json(args.response_json)).into_response()
}

async fn finalize_agent_dm_message(
    state: &ControlPlaneState,
    headers: &HeaderMap,
    args: FinalizeDmArgs,
) -> Response {
    if let Err(error) = persist_social_dm_message(
        state,
        PersistSocialDmMessageArgs {
            local_public_id: args.local_public_id.clone(),
            counterpart_public_id: args.counterpart_public_id.clone(),
            target_agent: args.target_agent,
            remote_node_id: args.remote_node_id.clone(),
            thread_id: args.thread_id,
            message_id: args.message_id,
            content: args.content.clone(),
            agent_envelope_json: args.agent_envelope_json,
            agent_signature: args.agent_signature,
        },
    )
    .await
    {
        return internal_error(&error);
    }
    let _ = state.stream_tx.send(StreamEvent {
        kind: "civilization.agent_dm.command".to_string(),
        timestamp: Utc::now().timestamp(),
        payload: json!({
            "counterpart_public_id": args.counterpart_public_id,
            "remote_node_id": args.remote_node_id,
            "response": args.response_json,
        }),
    });
    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "civilization".to_string(),
        action: "civilization.agent_dm.send".to_string(),
        status: "ok".to_string(),
        actor: Some(args.auth),
        subject: Some(args.local_public_id.clone()),
        capability: Some("social.dm.send".to_string()),
        reason: None,
        duration_ms: None,
        details: Some(json!({
            "counterpart_public_id": args.counterpart_public_id,
            "remote_node_id": args.remote_node_id,
        })),
    });
    if let Err(error) = append_commit_response(
        state,
        headers,
        CommitResponseArgs {
            action_type: "social.agent_dm_send",
            target_id: Some(args.counterpart_public_id),
            actor_public_id: Some(args.local_public_id),
            request_json: &json!({
                "counterpart_public_id": args.request_counterpart_public_id,
                "content": args.content,
            }),
            response_json: &args.response_json,
        },
    ) {
        return internal_error(&error);
    }
    (StatusCode::ACCEPTED, Json(args.response_json)).into_response()
}

fn relationship_view_to_payload(
    view: &SwarmPeerRelationshipView,
    identities: &BTreeMap<String, PublicIdentity>,
    bindings: &BTreeMap<String, ControllerBinding>,
) -> Value {
    let counterpart_public_id =
        counterpart_public_id_for_remote_node(bindings, &view.remote_node_id)
            .unwrap_or_else(|| view.remote_node_id.clone());
    let display_name = identities
        .get(&counterpart_public_id)
        .map(|identity| identity.display_name.clone());
    json!({
        "counterpart_public_id": counterpart_public_id,
        "counterpart_display_name": display_name,
        "remote_node_id": view.remote_node_id,
        "relationship_state": view.relationship_state,
        "last_action": view.last_action,
        "initiated_by": view.initiated_by,
        "agent_envelope": view.agent_envelope,
        "requested_at": view.requested_at,
        "responded_at": view.responded_at,
        "blocked_at": view.blocked_at,
        "cleared_at": view.cleared_at,
        "updated_at": view.updated_at,
        "pending_inbound": view.relationship_state == "requested" && view.initiated_by == "remote",
        "pending_outbound": view.relationship_state == "requested" && view.initiated_by == "local",
    })
}

fn dm_thread_view_to_payload(
    view: &SwarmPeerDmThreadView,
    identities: &BTreeMap<String, PublicIdentity>,
    bindings: &BTreeMap<String, ControllerBinding>,
) -> Value {
    let counterpart_public_id =
        counterpart_public_id_for_remote_node(bindings, &view.remote_node_id)
            .unwrap_or_else(|| view.remote_node_id.clone());
    let display_name = identities
        .get(&counterpart_public_id)
        .map(|identity| identity.display_name.clone());
    json!({
        "counterpart_public_id": counterpart_public_id,
        "counterpart_display_name": display_name,
        "remote_node_id": view.remote_node_id,
        "thread_id": view.thread_id,
        "thread_kind": view.thread_kind,
        "session_state": view.session_state,
        "relationship_established_at": view.relationship_established_at,
        "created_at": view.created_at,
        "updated_at": view.updated_at,
        "last_message_at": view.last_message_at,
    })
}

fn dm_message_view_to_payload(
    view: &SwarmPeerDmMessageView,
    identities: &BTreeMap<String, PublicIdentity>,
    bindings: &BTreeMap<String, ControllerBinding>,
) -> Value {
    let counterpart_public_id =
        counterpart_public_id_for_remote_node(bindings, &view.remote_node_id)
            .unwrap_or_else(|| view.remote_node_id.clone());
    let display_name = identities
        .get(&counterpart_public_id)
        .map(|identity| identity.display_name.clone());
    json!({
        "counterpart_public_id": counterpart_public_id,
        "counterpart_display_name": display_name,
        "thread_id": view.thread_id,
        "message_id": view.message_id,
        "remote_node_id": view.remote_node_id,
        "message_kind": view.message_kind,
        "direction": view.direction,
        "delivery_state": view.delivery_state,
        "a2a_protocol": view.a2a_protocol,
        "agent_envelope": view.agent_envelope,
        "content": view.content,
        "encrypted_body": view.encrypted_body,
        "content_encoding": view.content_encoding,
        "created_at": view.created_at,
        "acknowledged_at": view.acknowledged_at,
    })
}

fn pair_stable_id(prefix: &str, left: &str, right: &str) -> String {
    if left <= right {
        format!("{prefix}:{left}:{right}")
    } else {
        format!("{prefix}:{right}:{left}")
    }
}

fn thread_state_label(state: ThreadState) -> &'static str {
    match state {
        ThreadState::Pending => "pending",
        ThreadState::Ready => "ready",
        ThreadState::Closed => "closed",
        ThreadState::Blocked => "blocked",
    }
}

fn friendship_state_label(state: FriendshipState) -> &'static str {
    match state {
        FriendshipState::Active => "active",
        FriendshipState::Removed => "removed",
        FriendshipState::Blocked => "blocked",
    }
}

fn delivery_state_label(state: DeliveryState) -> &'static str {
    match state {
        DeliveryState::Pending => "pending",
        DeliveryState::Delivered => "delivered",
        DeliveryState::Acknowledged => "acknowledged",
        DeliveryState::Failed => "failed",
    }
}

fn message_kind_label(kind: MessageKind) -> &'static str {
    match kind {
        MessageKind::Message => "message",
        MessageKind::RelationshipEstablished => "relationship_established",
        MessageKind::SessionInit => "session_init",
    }
}

fn message_direction_label(direction: MessageDirection) -> &'static str {
    match direction {
        MessageDirection::Inbound => "inbound",
        MessageDirection::Outbound => "outbound",
    }
}

fn binding_remote_node_id(
    bindings: &BTreeMap<String, ControllerBinding>,
    counterpart_public_id: &str,
) -> Option<String> {
    bindings
        .get(counterpart_public_id)
        .and_then(|binding| binding.controller_node_id.clone())
}

fn relationship_payload_from_friend_request(
    request: &FriendRequest,
    identities: &BTreeMap<String, PublicIdentity>,
    bindings: &BTreeMap<String, ControllerBinding>,
) -> Value {
    let display_name = identities
        .get(&request.remote_public_id)
        .map(|identity| identity.display_name.clone());
    let remote_node_id = request
        .remote_node_id
        .clone()
        .or_else(|| binding_remote_node_id(bindings, &request.remote_public_id));
    let relationship_state = match request.state {
        FriendRequestState::Pending => "requested",
        FriendRequestState::Accepted => "accepted",
        FriendRequestState::Rejected => "rejected",
        FriendRequestState::Blocked => "blocked",
        FriendRequestState::Cancelled => "cancelled",
        FriendRequestState::Expired => "expired",
    };
    let initiated_by = match request.direction {
        FriendRequestDirection::Inbound => "remote",
        FriendRequestDirection::Outbound => "local",
    };
    json!({
        "counterpart_public_id": request.remote_public_id.clone(),
        "counterpart_display_name": display_name,
        "remote_node_id": remote_node_id,
        "relationship_state": relationship_state,
        "last_action": relationship_state,
        "initiated_by": initiated_by,
        "agent_envelope": Value::Null,
        "requested_at": request.created_at,
        "responded_at": if request.state == FriendRequestState::Pending { Value::Null } else { json!(request.updated_at) },
        "blocked_at": if request.state == FriendRequestState::Blocked { json!(request.updated_at) } else { Value::Null },
        "cleared_at": Value::Null,
        "updated_at": request.updated_at,
        "pending_inbound": request.state == FriendRequestState::Pending && request.direction == FriendRequestDirection::Inbound,
        "pending_outbound": request.state == FriendRequestState::Pending && request.direction == FriendRequestDirection::Outbound,
    })
}

fn relationship_payload_from_friendship(
    friendship: &Friendship,
    identities: &BTreeMap<String, PublicIdentity>,
    bindings: &BTreeMap<String, ControllerBinding>,
) -> Value {
    let display_name = identities
        .get(&friendship.remote_public_id)
        .map(|identity| identity.display_name.clone());
    json!({
        "counterpart_public_id": friendship.remote_public_id.clone(),
        "counterpart_display_name": display_name,
        "remote_node_id": binding_remote_node_id(bindings, &friendship.remote_public_id),
        "relationship_state": friendship_state_label(friendship.state),
        "last_action": friendship_state_label(friendship.state),
        "initiated_by": "local",
        "agent_envelope": Value::Null,
        "requested_at": friendship.created_at,
        "responded_at": friendship.updated_at,
        "blocked_at": if friendship.state == FriendshipState::Blocked { json!(friendship.updated_at) } else { Value::Null },
        "cleared_at": Value::Null,
        "updated_at": friendship.updated_at,
        "pending_inbound": false,
        "pending_outbound": false,
    })
}

fn relationship_payload_from_block(
    block: &SocialBlock,
    identities: &BTreeMap<String, PublicIdentity>,
) -> Value {
    let display_name = identities
        .get(&block.blocked_public_id)
        .map(|identity| identity.display_name.clone());
    json!({
        "counterpart_public_id": block.blocked_public_id.clone(),
        "counterpart_display_name": display_name,
        "remote_node_id": block.blocked_node_id.clone(),
        "relationship_state": "blocked",
        "last_action": "block",
        "initiated_by": "local",
        "agent_envelope": Value::Null,
        "requested_at": block.created_at,
        "responded_at": Value::Null,
        "blocked_at": block.updated_at,
        "cleared_at": Value::Null,
        "updated_at": block.updated_at,
        "pending_inbound": false,
        "pending_outbound": false,
    })
}

fn dm_thread_payload_from_social(
    thread: &DirectThread,
    identities: &BTreeMap<String, PublicIdentity>,
    bindings: &BTreeMap<String, ControllerBinding>,
) -> Value {
    let display_name = identities
        .get(&thread.remote_public_id)
        .map(|identity| identity.display_name.clone());
    json!({
        "counterpart_public_id": thread.remote_public_id.clone(),
        "counterpart_display_name": display_name,
        "remote_node_id": binding_remote_node_id(bindings, &thread.remote_public_id),
        "thread_id": thread.thread_id.clone(),
        "thread_kind": "direct",
        "session_state": thread_state_label(thread.state),
        "relationship_established_at": if thread.state == ThreadState::Ready { json!(thread.created_at) } else { Value::Null },
        "created_at": thread.created_at,
        "updated_at": thread.updated_at,
        "last_message_at": thread.last_message_at,
    })
}

fn dm_message_payload_from_social(
    message: &DirectMessage,
    identities: &BTreeMap<String, PublicIdentity>,
    bindings: &BTreeMap<String, ControllerBinding>,
    receipts: &[MessageReceipt],
) -> Value {
    let display_name = identities
        .get(&message.remote_public_id)
        .map(|identity| identity.display_name.clone());
    let acknowledged_at = receipts
        .iter()
        .filter(|receipt| {
            matches!(
                receipt.receipt_kind,
                ReceiptKind::Acknowledged | ReceiptKind::Read
            )
        })
        .map(|receipt| receipt.recorded_at)
        .max();
    let protocol = message
        .agent_envelope_json
        .as_ref()
        .and_then(|value| value.get("protocol"))
        .and_then(Value::as_str)
        .unwrap_or("google_a2a");
    json!({
        "counterpart_public_id": message.remote_public_id.clone(),
        "counterpart_display_name": display_name,
        "thread_id": message.thread_id.clone(),
        "message_id": message.message_id.clone(),
        "remote_node_id": binding_remote_node_id(bindings, &message.remote_public_id),
        "message_kind": message_kind_label(message.message_kind),
        "direction": message_direction_label(message.direction),
        "delivery_state": delivery_state_label(message.delivery_state),
        "a2a_protocol": protocol,
        "agent_envelope": message.agent_envelope_json.clone(),
        "content": message.content_json.clone(),
        "encrypted_body": message.encrypted_body.clone(),
        "content_encoding": message.content_encoding.clone(),
        "created_at": message.created_at,
        "acknowledged_at": acknowledged_at,
    })
}

fn counterpart_snapshot(
    identities: &BTreeMap<String, PublicIdentity>,
    bindings: &BTreeMap<String, ControllerBinding>,
    counterpart_public_id: &str,
    target_agent: &str,
    remote_node_id: &str,
    observed_at: i64,
) -> orchestration_service::CounterpartSnapshot {
    let known_identity = identities.get(counterpart_public_id).map(|identity| {
        orchestration_service::KnownIdentitySnapshot {
            public_id: identity.public_id.clone(),
            agent_did: identity.agent_did.clone(),
            display_name: identity.display_name.clone(),
            active: identity.active,
            created_at: identity.created_at,
        }
    });
    let known_binding = bindings.get(counterpart_public_id).map(|binding| {
        orchestration_service::KnownTransportBindingSnapshot {
            binding_source: binding.controller_ref.clone(),
            binding_confidence: 100,
            binding_verified: true,
            binding_verified_at: Some(observed_at),
        }
    });
    orchestration_service::CounterpartSnapshot {
        counterpart_public_id: counterpart_public_id.to_string(),
        target_agent: target_agent.to_string(),
        remote_node_id: remote_node_id.to_string(),
        known_identity,
        known_binding,
        observed_at,
    }
}

pub(crate) fn reconcile_swarm_relationship_views(
    state: &ControlPlaneState,
    local_public_id: &str,
    identities: &BTreeMap<String, PublicIdentity>,
    bindings: &BTreeMap<String, ControllerBinding>,
    views: &[SwarmPeerRelationshipView],
) -> anyhow::Result<()> {
    let mut synced = Vec::with_capacity(views.len());
    for view in views {
        let counterpart_public_id =
            counterpart_public_id_for_remote_node(bindings, &view.remote_node_id)
                .unwrap_or_else(|| view.remote_node_id.clone());
        let target_agent = identities
            .get(&counterpart_public_id)
            .and_then(|identity| identity.agent_did.clone())
            .unwrap_or_else(|| counterpart_public_id.clone());
        synced.push(orchestration_service::RelationshipSyncView {
            counterpart: counterpart_snapshot(
                identities,
                bindings,
                &counterpart_public_id,
                &target_agent,
                &view.remote_node_id,
                i64::try_from(view.updated_at).unwrap_or_default(),
            ),
            relationship_state: view.relationship_state.clone(),
            initiated_by: view.initiated_by.clone(),
            request_id: view
                .agent_envelope
                .as_ref()
                .and_then(|envelope| envelope.message.get("request_id"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            correlation_id: view
                .agent_envelope
                .as_ref()
                .and_then(|envelope| envelope.message.get("correlation_id"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            requested_at: view
                .requested_at
                .and_then(|value| i64::try_from(value).ok()),
            responded_at: view
                .responded_at
                .and_then(|value| i64::try_from(value).ok()),
            updated_at: i64::try_from(view.updated_at).unwrap_or_default(),
        });
    }
    orchestration_service::reconcile_relationship_views(
        &*state.social_store,
        local_public_id,
        &synced,
    )
    .map_err(anyhow::Error::msg)
}

pub(crate) fn reconcile_swarm_dm_threads(
    state: &ControlPlaneState,
    local_public_id: &str,
    identities: &BTreeMap<String, PublicIdentity>,
    bindings: &BTreeMap<String, ControllerBinding>,
    views: &[SwarmPeerDmThreadView],
) -> anyhow::Result<()> {
    let mut synced = Vec::with_capacity(views.len());
    for view in views {
        let counterpart_public_id =
            counterpart_public_id_for_remote_node(bindings, &view.remote_node_id)
                .unwrap_or_else(|| view.remote_node_id.clone());
        let target_agent = identities
            .get(&counterpart_public_id)
            .and_then(|identity| identity.agent_did.clone())
            .unwrap_or_else(|| counterpart_public_id.clone());
        synced.push(orchestration_service::DmThreadSyncView {
            counterpart: counterpart_snapshot(
                identities,
                bindings,
                &counterpart_public_id,
                &target_agent,
                &view.remote_node_id,
                i64::try_from(view.updated_at).unwrap_or_default(),
            ),
            transport_thread_id: view.thread_id.clone(),
            session_state: view.session_state.clone(),
            relationship_established_at: view
                .relationship_established_at
                .and_then(|value| i64::try_from(value).ok()),
            created_at: i64::try_from(view.created_at).unwrap_or_default(),
            updated_at: i64::try_from(view.updated_at).unwrap_or_default(),
            last_message_at: view
                .last_message_at
                .and_then(|value| i64::try_from(value).ok()),
        });
    }
    orchestration_service::reconcile_dm_threads(&*state.social_store, local_public_id, &synced)
        .map_err(anyhow::Error::msg)
}

pub(crate) fn reconcile_swarm_dm_messages(
    state: &ControlPlaneState,
    local_public_id: &str,
    identities: &BTreeMap<String, PublicIdentity>,
    bindings: &BTreeMap<String, ControllerBinding>,
    views: &[SwarmPeerDmMessageView],
) -> anyhow::Result<()> {
    let mut synced = Vec::with_capacity(views.len());
    for view in views {
        let counterpart_public_id =
            counterpart_public_id_for_remote_node(bindings, &view.remote_node_id)
                .unwrap_or_else(|| view.remote_node_id.clone());
        let target_agent = identities
            .get(&counterpart_public_id)
            .and_then(|identity| identity.agent_did.clone())
            .unwrap_or_else(|| counterpart_public_id.clone());
        synced.push(orchestration_service::DmMessageSyncView {
            counterpart: counterpart_snapshot(
                identities,
                bindings,
                &counterpart_public_id,
                &target_agent,
                &view.remote_node_id,
                i64::try_from(view.created_at).unwrap_or_default(),
            ),
            transport_thread_id: view.thread_id.clone(),
            message_id: view.message_id.clone(),
            message_kind: view.message_kind.clone(),
            direction: view.direction.clone(),
            delivery_state: view.delivery_state.clone(),
            a2a_protocol: view.a2a_protocol.clone(),
            content: view.content.clone(),
            encrypted_body: view.encrypted_body.clone(),
            content_encoding: view.content_encoding.clone(),
            agent_envelope_json: view
                .agent_envelope
                .as_ref()
                .and_then(|envelope| serde_json::to_value(envelope).ok()),
            agent_signature: view
                .agent_envelope
                .as_ref()
                .and_then(|envelope| envelope.signature.clone()),
            created_at: i64::try_from(view.created_at).unwrap_or_default(),
            acknowledged_at: view
                .acknowledged_at
                .and_then(|value| i64::try_from(value).ok()),
        });
    }
    orchestration_service::reconcile_dm_messages(&*state.social_store, local_public_id, &synced)
        .map_err(anyhow::Error::msg)
}

async fn persist_social_relationship_action(
    state: &ControlPlaneState,
    local_public_id: &str,
    counterpart_public_id: &str,
    target_agent: &str,
    remote_node_id: &str,
    action: &SwarmRelationshipAction,
    message: &Value,
) -> anyhow::Result<()> {
    let now = Utc::now().timestamp();
    let (identities, bindings) = load_social_identity_maps(state).await;
    let request_id = message
        .get("request_id")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let correlation_id = message
        .get("correlation_id")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let action = match action {
        SwarmRelationshipAction::Request => orchestration_service::RelationshipAction::Request,
        SwarmRelationshipAction::Accept => orchestration_service::RelationshipAction::Accept,
        SwarmRelationshipAction::Reject => orchestration_service::RelationshipAction::Reject,
        SwarmRelationshipAction::Cancel => orchestration_service::RelationshipAction::Cancel,
        SwarmRelationshipAction::Remove => orchestration_service::RelationshipAction::Remove,
        SwarmRelationshipAction::Block => orchestration_service::RelationshipAction::Block,
        SwarmRelationshipAction::Unblock => orchestration_service::RelationshipAction::Unblock,
    };
    orchestration_service::persist_relationship_action(
        &*state.social_store,
        &orchestration_service::PersistRelationshipActionInput {
            local_public_id: local_public_id.to_string(),
            counterpart: counterpart_snapshot(
                &identities,
                &bindings,
                counterpart_public_id,
                target_agent,
                remote_node_id,
                now,
            ),
            action,
            request_id,
            correlation_id,
            occurred_at: now,
        },
    )
    .map_err(anyhow::Error::msg)
}

struct PersistSocialDmMessageArgs {
    local_public_id: String,
    counterpart_public_id: String,
    target_agent: String,
    remote_node_id: String,
    thread_id: String,
    message_id: String,
    content: Value,
    agent_envelope_json: Value,
    agent_signature: Option<String>,
}

async fn persist_social_dm_message(
    state: &ControlPlaneState,
    args: PersistSocialDmMessageArgs,
) -> anyhow::Result<()> {
    let now = Utc::now().timestamp();
    let (identities, bindings) = load_social_identity_maps(state).await;
    orchestration_service::persist_dm_message(
        &*state.social_store,
        &orchestration_service::PersistDmMessageInput {
            local_public_id: args.local_public_id,
            counterpart: counterpart_snapshot(
                &identities,
                &bindings,
                &args.counterpart_public_id,
                &args.target_agent,
                &args.remote_node_id,
                now,
            ),
            thread_id: args.thread_id,
            message_id: args.message_id,
            content: args.content,
            agent_envelope_json: args.agent_envelope_json,
            agent_signature: args.agent_signature,
            occurred_at: now,
        },
    )
    .map_err(anyhow::Error::msg)
}

fn policy_denied_response(reason: &str) -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(json!({
            "error": "policy denied",
            "reason": reason,
        })),
    )
        .into_response()
}

fn build_relationship_action_message(
    local_public_id: &str,
    counterpart_public_id: &str,
    action: &SwarmRelationshipAction,
    base_message: Option<Value>,
    now: i64,
) -> Value {
    with_social_defaults(
        base_message.unwrap_or_else(|| json!({})),
        [
            (
                "source_public_id",
                Value::String(local_public_id.to_string()),
            ),
            (
                "target_public_id",
                Value::String(counterpart_public_id.to_string()),
            ),
            (
                "action",
                serde_json::to_value(action).unwrap_or(Value::Null),
            ),
            ("request_id", Value::String(Uuid::new_v4().to_string())),
            ("correlation_id", Value::String(Uuid::new_v4().to_string())),
            ("sent_at", json!(now)),
        ],
    )
}

struct SignedRelationshipActionArgs {
    local_agent_id: String,
    target_agent_id: String,
    remote_node_id: String,
    action: SwarmRelationshipAction,
    capability: String,
    message: Value,
    extensions: Option<Value>,
}

async fn send_signed_relationship_action_command(
    state: &ControlPlaneState,
    args: SignedRelationshipActionArgs,
) -> anyhow::Result<Value> {
    let agent_envelope = build_signed_agent_envelope(
        state,
        args.local_agent_id,
        args.target_agent_id,
        &args.capability,
        args.message,
        args.extensions,
    )?;
    state
        .swarm_bridge
        .send_peer_relationship_action(SwarmRelationshipActionCommand {
            remote_node_id: args.remote_node_id,
            action: args.action,
            agent_envelope,
        })
        .await
}

async fn send_signed_direct_message_command(
    state: &ControlPlaneState,
    local_agent_id: String,
    target_agent_id: String,
    remote_node_id: String,
    content: Value,
    message: Value,
    extensions: Option<Value>,
) -> anyhow::Result<(Value, Value, Option<String>)> {
    let agent_envelope = build_signed_agent_envelope(
        state,
        local_agent_id,
        target_agent_id,
        "social.dm.send",
        message,
        extensions,
    )?;
    let agent_envelope_json = serde_json::to_value(&agent_envelope).unwrap_or(Value::Null);
    let agent_signature = agent_envelope.signature.clone();
    let response = state
        .swarm_bridge
        .send_peer_direct_message(SwarmDirectMessageCommand {
            remote_node_id,
            agent_envelope,
            content,
        })
        .await?;
    Ok((response, agent_envelope_json, agent_signature))
}

fn ensure_outbound_friend_request_allowed(
    state: &ControlPlaneState,
    local_public_id: &str,
    counterpart_public_id: &str,
    remote_node_id: &str,
    now: i64,
) -> anyhow::Result<Option<Response>> {
    let evaluation = policy_service::evaluate_outbound_friend_request_policy(
        &*state.social_store,
        local_public_id,
        counterpart_public_id,
        Some(remote_node_id),
        now,
    )
    .map_err(anyhow::Error::msg)?;
    Ok((evaluation.decision == PolicyDecision::Deny)
        .then(|| policy_denied_response(&evaluation.reason)))
}

fn ensure_outbound_dm_allowed(
    state: &ControlPlaneState,
    local_public_id: &str,
    counterpart_public_id: &str,
    remote_node_id: &str,
    now: i64,
) -> anyhow::Result<Option<Response>> {
    let evaluation = policy_service::evaluate_outbound_dm_policy(
        &*state.social_store,
        local_public_id,
        counterpart_public_id,
        Some(remote_node_id),
        now,
    )
    .map_err(anyhow::Error::msg)?;
    Ok((evaluation.decision == PolicyDecision::Deny)
        .then(|| policy_denied_response(&evaluation.reason)))
}

pub(crate) async fn list_relationships(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<RelationshipQuery>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let context =
        crate::routes::identity::resolve_identity_context(&state, query.public_id.as_deref(), None)
            .await;
    let public_id = context.public_identity.as_ref().map_or_else(
        || context.public_memory_owner.controller.clone(),
        |identity| identity.public_id.clone(),
    );
    let registry = state.relationship_registry.lock().await;
    let mut items = registry.list_for_public(&public_id);
    if let Some(counterpart) = query.counterpart_public_id.as_deref() {
        items.retain(|edge| edge.counterpart_public_id == counterpart);
    }

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "civilization".to_string(),
        action: "civilization.relationships.query".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(public_id),
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({"count": items.len()})),
    });

    Json(items).into_response()
}

pub(crate) async fn upsert_relationship(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<RelationshipBody>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let context =
        crate::routes::identity::resolve_identity_context(&state, body.public_id.as_deref(), None)
            .await;
    let public_id = context.public_identity.as_ref().map_or_else(
        || context.public_memory_owner.controller.clone(),
        |identity| identity.public_id.clone(),
    );
    let edge: RelationshipEdge = {
        let mut registry = state.relationship_registry.lock().await;
        let edge = registry.upsert(
            &public_id,
            &body.counterpart_public_id,
            body.kind.clone(),
            body.active,
        );
        if let Err(error) = state.local_db.save_domain(
            wattetheria_kernel::local_db::domain::RELATIONSHIP_REGISTRY,
            &*registry,
        ) {
            return internal_error(&error);
        }
        edge
    };

    let _ = state.stream_tx.send(StreamEvent {
        kind: "civilization.relationship.updated".to_string(),
        timestamp: Utc::now().timestamp(),
        payload: json!(edge),
    });

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "civilization".to_string(),
        action: "civilization.relationships.upsert".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(public_id),
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({
            "counterpart_public_id": body.counterpart_public_id,
            "kind": body.kind,
            "active": body.active,
        })),
    });

    (StatusCode::ACCEPTED, Json(json!(edge))).into_response()
}

pub(crate) async fn build_agent_relationship_payload(
    state: &ControlPlaneState,
    public_id: Option<&str>,
    counterpart_filter: Option<&str>,
) -> anyhow::Result<Vec<Value>> {
    let local = resolve_social_local_context(state, public_id).await;
    let (identities, bindings) = load_social_identity_maps(state).await;
    let bridge_views = state.swarm_bridge.list_peer_relationships().await;
    if let Ok(views) = &bridge_views {
        reconcile_swarm_relationship_views(state, &local.public_id, &identities, &bindings, views)?;
    }
    let friend_requests =
        friend_request_service::list_friend_requests(&*state.social_store, &local.public_id)
            .unwrap_or_default();
    let friendships = friendship_service::list_friendships(&*state.social_store, &local.public_id)
        .unwrap_or_default();
    let blocks =
        block_service::list_blocks(&*state.social_store, &local.public_id).unwrap_or_default();
    let mut items = if friend_requests.is_empty() && friendships.is_empty() && blocks.is_empty() {
        bridge_views?
            .into_iter()
            .map(|view| relationship_view_to_payload(&view, &identities, &bindings))
            .collect::<Vec<_>>()
    } else {
        let mut items = friend_requests
            .iter()
            .map(|request| {
                relationship_payload_from_friend_request(request, &identities, &bindings)
            })
            .collect::<Vec<_>>();
        items.extend(friendships.iter().map(|friendship| {
            relationship_payload_from_friendship(friendship, &identities, &bindings)
        }));
        items.extend(
            blocks
                .iter()
                .map(|block| relationship_payload_from_block(block, &identities)),
        );
        items
    };
    if let Some(counterpart_public_id) = counterpart_filter {
        items.retain(|item| item["counterpart_public_id"].as_str() == Some(counterpart_public_id));
    }
    items.sort_by(|left, right| {
        right["updated_at"]
            .as_u64()
            .unwrap_or_default()
            .cmp(&left["updated_at"].as_u64().unwrap_or_default())
            .then_with(|| {
                left["counterpart_public_id"]
                    .as_str()
                    .unwrap_or_default()
                    .cmp(right["counterpart_public_id"].as_str().unwrap_or_default())
            })
    });
    items.dedup_by(|left, right| left["counterpart_public_id"] == right["counterpart_public_id"]);
    Ok(items)
}

pub(crate) async fn build_agent_dm_threads_payload(
    state: &ControlPlaneState,
    public_id: Option<&str>,
) -> anyhow::Result<Vec<Value>> {
    let local = resolve_social_local_context(state, public_id).await;
    let (identities, bindings) = load_social_identity_maps(state).await;
    let bridge_threads = state.swarm_bridge.list_peer_dm_threads().await;
    if let Ok(views) = &bridge_threads {
        reconcile_swarm_dm_threads(state, &local.public_id, &identities, &bindings, views)?;
    }
    let mut items = match thread_service::list_threads(&*state.social_store, &local.public_id) {
        Ok(threads) if !threads.is_empty() => threads
            .iter()
            .map(|thread| dm_thread_payload_from_social(thread, &identities, &bindings))
            .collect::<Vec<_>>(),
        _ => bridge_threads?
            .into_iter()
            .map(|view| dm_thread_view_to_payload(&view, &identities, &bindings))
            .collect::<Vec<_>>(),
    };
    items.sort_by(|left, right| {
        right["updated_at"]
            .as_u64()
            .unwrap_or_default()
            .cmp(&left["updated_at"].as_u64().unwrap_or_default())
    });
    Ok(items)
}

async fn resolve_requested_dm_threads(
    state: &ControlPlaneState,
    known_threads: &[DirectThread],
    bridge_threads: &anyhow::Result<Vec<SwarmPeerDmThreadView>>,
    counterpart_public_id: Option<&str>,
    thread_id: Option<&str>,
) -> anyhow::Result<(Vec<DirectThread>, Vec<String>)> {
    let mut requested_threads = Vec::new();
    let mut transport_thread_ids = Vec::new();
    if let Some(requested_thread_id) = thread_id {
        if let Some(thread) = known_threads
            .iter()
            .find(|thread| {
                thread.thread_id == requested_thread_id
                    || thread.transport_thread_id == requested_thread_id
            })
            .cloned()
        {
            requested_threads.push(thread.clone());
            transport_thread_ids.push(thread.transport_thread_id);
        } else {
            transport_thread_ids.push(requested_thread_id.to_string());
        }
    } else if let Some(counterpart_public_id) = counterpart_public_id {
        if let Some(thread) = known_threads
            .iter()
            .find(|thread| thread.remote_public_id == counterpart_public_id)
            .cloned()
        {
            requested_threads.push(thread.clone());
            transport_thread_ids.push(thread.transport_thread_id);
        } else {
            let counterpart = resolve_social_counterpart_target(state, counterpart_public_id)
                .await
                .map_err(anyhow::Error::msg)?;
            if let Ok(views) = bridge_threads
                && let Some(thread) = views
                    .iter()
                    .find(|thread| thread.remote_node_id == counterpart.remote_node)
            {
                transport_thread_ids.push(thread.thread_id.clone());
            }
        }
    } else {
        requested_threads = known_threads.to_vec();
        if requested_threads.is_empty() {
            if let Ok(views) = bridge_threads {
                transport_thread_ids.extend(views.iter().map(|thread| thread.thread_id.clone()));
            }
        } else {
            transport_thread_ids.extend(
                requested_threads
                    .iter()
                    .map(|thread| thread.transport_thread_id.clone()),
            );
        }
    }
    transport_thread_ids.sort();
    transport_thread_ids.dedup();
    Ok((requested_threads, transport_thread_ids))
}

async fn load_reconciled_bridge_dm_messages(
    state: &ControlPlaneState,
    local_public_id: &str,
    identities: &BTreeMap<String, PublicIdentity>,
    bindings: &BTreeMap<String, ControllerBinding>,
    transport_thread_ids: &[String],
) -> anyhow::Result<BTreeMap<String, Vec<SwarmPeerDmMessageView>>> {
    let mut bridge_message_map = BTreeMap::new();
    for transport_thread_id in transport_thread_ids {
        let views = state
            .swarm_bridge
            .list_peer_dm_messages(transport_thread_id)
            .await?;
        reconcile_swarm_dm_messages(state, local_public_id, identities, bindings, &views)?;
        bridge_message_map.insert(transport_thread_id.clone(), views);
    }
    Ok(bridge_message_map)
}

fn social_dm_messages_for_request(
    state: &ControlPlaneState,
    local_public_id: &str,
    known_threads: &[DirectThread],
    requested_threads: &[DirectThread],
    counterpart_public_id: Option<&str>,
    thread_id: Option<&str>,
) -> Option<Vec<DirectMessage>> {
    if let Some(requested_thread_id) = thread_id {
        let thread = known_threads
            .iter()
            .find(|thread| {
                thread.thread_id == requested_thread_id
                    || thread.transport_thread_id == requested_thread_id
            })
            .cloned()
            .or_else(|| {
                thread_service::list_threads(&*state.social_store, local_public_id)
                    .ok()
                    .and_then(|threads| {
                        threads.into_iter().find(|thread| {
                            thread.thread_id == requested_thread_id
                                || thread.transport_thread_id == requested_thread_id
                        })
                    })
            });
        return thread.map(|thread| {
            message_service::list_thread_messages(&*state.social_store, &thread.thread_id)
                .unwrap_or_default()
        });
    }
    if let Some(counterpart_public_id) = counterpart_public_id {
        return thread_service::find_thread(
            &*state.social_store,
            local_public_id,
            counterpart_public_id,
        )
        .ok()
        .flatten()
        .map(|thread| {
            message_service::list_thread_messages(&*state.social_store, &thread.thread_id)
                .unwrap_or_default()
        });
    }
    if requested_threads.is_empty() {
        return None;
    }
    let mut messages = Vec::new();
    for thread in requested_threads {
        messages.extend(
            message_service::list_thread_messages(&*state.social_store, &thread.thread_id)
                .unwrap_or_default(),
        );
    }
    Some(messages)
}

fn build_social_dm_message_payloads(
    state: &ControlPlaneState,
    identities: &BTreeMap<String, PublicIdentity>,
    bindings: &BTreeMap<String, ControllerBinding>,
    messages: Vec<DirectMessage>,
) -> Vec<Value> {
    let mut items = Vec::new();
    for message in messages {
        let receipts =
            receipt_service::list_message_receipts(&*state.social_store, &message.message_id)
                .unwrap_or_default();
        items.push(dm_message_payload_from_social(
            &message, identities, bindings, &receipts,
        ));
    }
    items
}

fn dm_thread_id_for_counterpart(
    state: &ControlPlaneState,
    local_public_id: &str,
    counterpart_public_id: &str,
) -> String {
    thread_service::find_thread(&*state.social_store, local_public_id, counterpart_public_id)
        .ok()
        .flatten()
        .map_or_else(
            || pair_stable_id("dm", local_public_id, counterpart_public_id),
            |thread| thread.thread_id,
        )
}

fn build_outbound_dm_message(
    local_public_id: &str,
    counterpart_public_id: &str,
    thread_id: &str,
    content: &Value,
    now: i64,
) -> (String, Value) {
    let message_id = Uuid::new_v4().to_string();
    let message = with_social_defaults(
        json!({
            "content": content.clone(),
        }),
        [
            (
                "source_public_id",
                Value::String(local_public_id.to_string()),
            ),
            (
                "target_public_id",
                Value::String(counterpart_public_id.to_string()),
            ),
            ("thread_id", Value::String(thread_id.to_string())),
            ("message_id", Value::String(message_id.clone())),
            ("sent_at", json!(now)),
        ],
    );
    (message_id, message)
}

pub(crate) async fn build_agent_dm_messages_payload(
    state: &ControlPlaneState,
    public_id: Option<&str>,
    counterpart_public_id: Option<&str>,
    thread_id: Option<&str>,
    limit: usize,
) -> anyhow::Result<Vec<Value>> {
    let local = resolve_social_local_context(state, public_id).await;
    let (identities, bindings) = load_social_identity_maps(state).await;
    let bridge_threads = state.swarm_bridge.list_peer_dm_threads().await;
    if let Ok(views) = &bridge_threads {
        reconcile_swarm_dm_threads(state, &local.public_id, &identities, &bindings, views)?;
    }
    let known_threads =
        thread_service::list_threads(&*state.social_store, &local.public_id).unwrap_or_default();
    let (requested_threads, transport_thread_ids) = resolve_requested_dm_threads(
        state,
        &known_threads,
        &bridge_threads,
        counterpart_public_id,
        thread_id,
    )
    .await?;
    let bridge_message_map = load_reconciled_bridge_dm_messages(
        state,
        &local.public_id,
        &identities,
        &bindings,
        &transport_thread_ids,
    )
    .await?;
    let social_items = social_dm_messages_for_request(
        state,
        &local.public_id,
        &known_threads,
        &requested_threads,
        counterpart_public_id,
        thread_id,
    );
    let mut items = if let Some(messages) = social_items {
        build_social_dm_message_payloads(state, &identities, &bindings, messages)
    } else {
        let mut items = Vec::new();
        for views in bridge_message_map.into_values() {
            items.extend(
                views
                    .into_iter()
                    .map(|view| dm_message_view_to_payload(&view, &identities, &bindings)),
            );
        }
        items
    };
    items.sort_by(|left, right| {
        right["created_at"]
            .as_u64()
            .unwrap_or_default()
            .cmp(&left["created_at"].as_u64().unwrap_or_default())
    });
    items.dedup_by(|left, right| left["message_id"] == right["message_id"]);
    items.truncate(limit);
    Ok(items)
}

pub(crate) async fn list_agent_relationships(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<RelationshipQuery>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let items = match build_agent_relationship_payload(
        &state,
        query.public_id.as_deref(),
        query.counterpart_public_id.as_deref(),
    )
    .await
    {
        Ok(items) => items,
        Err(error) => return internal_error(&error),
    };
    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "civilization".to_string(),
        action: "civilization.agent_relationships.query".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: query.public_id.clone(),
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({"count": items.len()})),
    });
    Json(Value::Array(items)).into_response()
}

pub(crate) async fn agent_relationship_action(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<AgentRelationshipActionBody>,
) -> Response {
    if let Ok(Some(response)) =
        replay_commit_response(&state, &headers, "social.agent_relationship_action")
    {
        return response;
    }
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    handle_agent_relationship_action(state, headers, body, auth).await
}

async fn handle_agent_relationship_action(
    state: ControlPlaneState,
    headers: HeaderMap,
    body: AgentRelationshipActionBody,
    auth: String,
) -> Response {
    let SocialLocalContext {
        public_id: local_public_id,
        agent_id: local_agent_id,
    } = resolve_social_local_context(&state, body.public_id.as_deref()).await;
    let SocialCounterpartTarget {
        counterpart_public_id,
        remote_node,
        target_agent,
    } = match resolve_social_counterpart_target(&state, &body.counterpart_public_id).await {
        Ok(counterpart) => counterpart,
        Err(error) => {
            return (StatusCode::BAD_REQUEST, Json(json!({"error": error}))).into_response();
        }
    };
    let capability = capability_for_relationship_action(&body.action).to_string();
    let now = Utc::now().timestamp();
    if body.action == SwarmRelationshipAction::Request {
        match ensure_outbound_friend_request_allowed(
            &state,
            &local_public_id,
            &counterpart_public_id,
            &remote_node,
            now,
        ) {
            Ok(Some(response)) => return response,
            Ok(None) => {}
            Err(error) => return internal_error(&error),
        }
    }
    let message = build_relationship_action_message(
        &local_public_id,
        &counterpart_public_id,
        &body.action,
        body.message,
        now,
    );
    let response_json = match send_signed_relationship_action_command(
        &state,
        SignedRelationshipActionArgs {
            local_agent_id,
            target_agent_id: target_agent.clone(),
            remote_node_id: remote_node.clone(),
            action: body.action.clone(),
            capability: capability.clone(),
            message: message.clone(),
            extensions: body.extensions,
        },
    )
    .await
    {
        Ok(response) => response,
        Err(error) => return internal_error(&error),
    };
    finalize_agent_relationship_action(
        &state,
        &headers,
        FinalizeRelationshipActionArgs {
            auth,
            local_public_id,
            counterpart_public_id,
            target_agent,
            remote_node_id: remote_node,
            action: body.action,
            capability,
            request_counterpart_public_id: body.counterpart_public_id,
            message,
            response_json,
        },
    )
    .await
}

pub(crate) async fn list_agent_dm_threads(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<AgentDmThreadsQuery>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let items = match build_agent_dm_threads_payload(&state, query.public_id.as_deref()).await {
        Ok(items) => items,
        Err(error) => return internal_error(&error),
    };
    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "civilization".to_string(),
        action: "civilization.agent_dm_threads.query".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: query.public_id.clone(),
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({"count": items.len()})),
    });
    Json(Value::Array(items)).into_response()
}

pub(crate) async fn list_agent_dm_messages(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<AgentDmMessagesQuery>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let items = match build_agent_dm_messages_payload(
        &state,
        query.public_id.as_deref(),
        query.counterpart.as_deref(),
        query.thread.as_deref(),
        200,
    )
    .await
    {
        Ok(items) => items,
        Err(error) => return internal_error(&error),
    };
    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "civilization".to_string(),
        action: "civilization.agent_dm_messages.query".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: query.public_id.clone(),
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({"count": items.len()})),
    });
    Json(Value::Array(items)).into_response()
}

pub(crate) async fn send_agent_dm_message(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<AgentDmSendBody>,
) -> Response {
    if let Ok(Some(response)) = replay_commit_response(&state, &headers, "social.agent_dm_send") {
        return response;
    }
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    handle_send_agent_dm_message(state, headers, body, auth).await
}

async fn handle_send_agent_dm_message(
    state: ControlPlaneState,
    headers: HeaderMap,
    body: AgentDmSendBody,
    auth: String,
) -> Response {
    let SocialLocalContext {
        public_id: local_public_id,
        agent_id: local_agent_id,
    } = resolve_social_local_context(&state, body.public_id.as_deref()).await;
    let SocialCounterpartTarget {
        counterpart_public_id,
        remote_node,
        target_agent,
    } = match resolve_social_counterpart_target(&state, &body.counterpart_public_id).await {
        Ok(counterpart) => counterpart,
        Err(error) => {
            return (StatusCode::BAD_REQUEST, Json(json!({"error": error}))).into_response();
        }
    };
    let now = Utc::now().timestamp();
    match ensure_outbound_dm_allowed(
        &state,
        &local_public_id,
        &counterpart_public_id,
        &remote_node,
        now,
    ) {
        Ok(Some(response)) => return response,
        Ok(None) => {}
        Err(error) => return internal_error(&error),
    }
    let content = body.content.clone();
    let thread_id = dm_thread_id_for_counterpart(&state, &local_public_id, &counterpart_public_id);
    let (message_id, message) = build_outbound_dm_message(
        &local_public_id,
        &counterpart_public_id,
        &thread_id,
        &content,
        now,
    );
    let (response, agent_envelope_json, agent_signature) = match send_signed_direct_message_command(
        &state,
        local_agent_id,
        target_agent.clone(),
        remote_node.clone(),
        content.clone(),
        message.clone(),
        body.extensions,
    )
    .await
    {
        Ok(result) => result,
        Err(error) => return internal_error(&error),
    };
    finalize_agent_dm_message(
        &state,
        &headers,
        FinalizeDmArgs {
            auth,
            local_public_id,
            counterpart_public_id,
            target_agent,
            remote_node_id: remote_node,
            thread_id,
            message_id,
            request_counterpart_public_id: body.counterpart_public_id,
            content,
            agent_envelope_json,
            agent_signature,
            response_json: response,
        },
    )
    .await
}
