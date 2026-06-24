use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

use crate::auth::{authorize, internal_error};
use crate::routes::agent_events::replay_deferred_dm_agent_events_for_friendship;
use crate::routes::mcp::collective::record_collective_participation_from_dm;
use crate::social_host::{
    SignedAgentEnvelopeArgs, SocialCounterpartTarget, SocialLocalContext,
    build_signed_agent_envelope_for_nodes, capability_for_relationship_action,
    counterpart_public_id_for_remote_node, load_social_identity_maps, public_agent_id,
    resolve_dm_counterpart_target, resolve_social_counterpart_target,
    resolve_social_counterpart_target_by_agent_did,
    resolve_social_counterpart_target_by_remote_node, resolve_social_local_context,
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
    SwarmAgentEnvelope, SwarmDirectMessageCommand, SwarmPeerDmMessageView, SwarmPeerDmThreadView,
    SwarmPeerRelationshipView, SwarmPeerView, SwarmRelationshipAction,
    SwarmRelationshipActionCommand, SwarmSourceAgentCard,
};
use wattetheria_social::application::{
    block_service, friend_request_service, friendship_service, message_service,
    orchestration_service, policy_service, receipt_service, remote_identity_service,
    thread_service, transport_binding_service,
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
use wattetheria_social::domain::transport_bindings::{RemoteTransportBinding, TransportKind};
use wattetheria_social::policy::decisions::PolicyDecision;
use wattetheria_social::ports::repositories::RemoteIdentityRepository;

const FRIEND_REQUEST_MESSAGE_MAX_CHARS: usize = 120;

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
    reply_to_message_id: Option<String>,
    agent_envelope_json: Value,
    agent_signature: Option<String>,
    response_json: Value,
}

#[derive(Debug, Deserialize)]
pub(crate) struct FriendRequestsQuery {
    pub limit: Option<usize>,
    pub offset: Option<usize>,
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
                "reply_to_message_id": args.reply_to_message_id,
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
    let counterpart_public_id = relationship_remote_public_id(view)
        .or_else(|| counterpart_public_id_for_remote_node(bindings, &view.remote_node_id))
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

fn transport_binding_remote_node_id(
    bindings: &[RemoteTransportBinding],
    counterpart_public_id: &str,
) -> Option<String> {
    bindings
        .iter()
        .find(|binding| {
            binding.public_id == counterpart_public_id
                && matches!(binding.transport_kind, TransportKind::Wattswarm)
                && !binding.transport_node_id.trim().is_empty()
        })
        .map(|binding| binding.transport_node_id.clone())
}

fn envelope_dm_counterpart_public_id(
    envelope: Option<&SwarmAgentEnvelope>,
    local_public_id: &str,
    direction: &str,
) -> Option<String> {
    let message = &envelope?.message;
    let source_public_id = message
        .get("source_public_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let target_public_id = message
        .get("target_public_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());

    let preferred = if direction == "inbound" {
        source_public_id
    } else if direction == "outbound" {
        target_public_id
    } else {
        None
    };
    preferred
        .filter(|public_id| *public_id != local_public_id)
        .or_else(|| source_public_id.filter(|public_id| *public_id != local_public_id))
        .or_else(|| target_public_id.filter(|public_id| *public_id != local_public_id))
        .map(ToOwned::to_owned)
}

fn social_transport_counterpart_public_id_for_remote_node(
    transport_bindings: &[RemoteTransportBinding],
    friendships: &[Friendship],
    remote_node_id: &str,
) -> Option<String> {
    let candidates = transport_bindings
        .iter()
        .filter(|binding| {
            matches!(binding.transport_kind, TransportKind::Wattswarm)
                && binding.transport_node_id == remote_node_id
        })
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return None;
    }

    candidates
        .iter()
        .find(|binding| {
            friendships.iter().any(|friendship| {
                friendship.remote_public_id == binding.public_id
                    && friendship.state == FriendshipState::Active
            })
        })
        .or_else(|| {
            candidates
                .iter()
                .find(|binding| binding.public_id != remote_node_id)
        })
        .or_else(|| candidates.first())
        .map(|binding| binding.public_id.clone())
}

fn has_active_friendship(friendships: &[Friendship], public_id: &str) -> bool {
    friendships.iter().any(|friendship| {
        friendship.remote_public_id == public_id && friendship.state == FriendshipState::Active
    })
}

fn has_matching_transport_binding(
    transport_bindings: &[RemoteTransportBinding],
    public_id: &str,
    remote_node_id: &str,
) -> bool {
    transport_bindings.iter().any(|binding| {
        binding.public_id == public_id
            && matches!(binding.transport_kind, TransportKind::Wattswarm)
            && binding.transport_node_id == remote_node_id
    })
}

fn has_matching_controller_binding(
    bindings: &BTreeMap<String, ControllerBinding>,
    public_id: &str,
    remote_node_id: &str,
) -> bool {
    bindings
        .get(public_id)
        .is_some_and(|binding| binding.controller_node_id.as_deref() == Some(remote_node_id))
}

fn dm_counterpart_public_id(
    local_public_id: &str,
    bindings: &BTreeMap<String, ControllerBinding>,
    transport_bindings: &[RemoteTransportBinding],
    friendships: &[Friendship],
    remote_node_id: &str,
    envelope: Option<&SwarmAgentEnvelope>,
    direction: &str,
) -> String {
    envelope_dm_counterpart_public_id(envelope, local_public_id, direction)
        .filter(|public_id| {
            has_active_friendship(friendships, public_id)
                && (has_matching_transport_binding(transport_bindings, public_id, remote_node_id)
                    || has_matching_controller_binding(bindings, public_id, remote_node_id))
        })
        .or_else(|| {
            social_transport_counterpart_public_id_for_remote_node(
                transport_bindings,
                friendships,
                remote_node_id,
            )
        })
        .or_else(|| counterpart_public_id_for_remote_node(bindings, remote_node_id))
        .unwrap_or_else(|| remote_node_id.to_string())
}

fn insert_payload_if_present(object: &mut Map<String, Value>, key: &str, value: Option<Value>) {
    let Some(value) = value else {
        return;
    };
    match &value {
        Value::Null => {}
        Value::String(value) if value.trim().is_empty() => {}
        Value::Array(value) if value.is_empty() => {}
        Value::Object(value) if value.is_empty() => {}
        _ => {
            object.insert(key.to_string(), value);
        }
    }
}

fn request_direction_label(direction: FriendRequestDirection) -> &'static str {
    match direction {
        FriendRequestDirection::Inbound => "inbound",
        FriendRequestDirection::Outbound => "outbound",
    }
}

fn request_state_label(state: FriendRequestState) -> &'static str {
    match state {
        FriendRequestState::Pending => "pending",
        FriendRequestState::Accepted => "accepted",
        FriendRequestState::Rejected => "rejected",
        FriendRequestState::Blocked => "blocked",
        FriendRequestState::Cancelled => "cancelled",
        FriendRequestState::Expired => "expired",
    }
}

fn iroh_endpoint_id(value: &Value) -> Option<&str> {
    value
        .get("endpoint_id")
        .and_then(Value::as_str)
        .or_else(|| value.get("local_iroh_endpoint_id").and_then(Value::as_str))
        .or_else(|| {
            value
                .get("metadata")
                .and_then(|metadata| metadata.get("endpoint_id"))
                .and_then(Value::as_str)
        })
        .or_else(|| {
            value
                .get("extra")
                .and_then(|extra| extra.get("endpoint_id"))
                .and_then(Value::as_str)
        })
        .or_else(|| {
            value
                .get("transports")
                .and_then(Value::as_array)
                .and_then(|transports| transports.iter().find_map(iroh_endpoint_id))
        })
}

fn relationship_request_id(view: &SwarmPeerRelationshipView) -> Option<&str> {
    view.agent_envelope
        .as_ref()
        .and_then(|envelope| envelope.message.get("request_id"))
        .and_then(Value::as_str)
}

fn source_agent_card_is_remote(
    view: &SwarmPeerRelationshipView,
    envelope: &SwarmAgentEnvelope,
) -> bool {
    if envelope.source_node_id.as_deref() == Some(view.remote_node_id.as_str()) {
        return true;
    }
    if envelope.target_node_id.as_deref() == Some(view.remote_node_id.as_str()) {
        return false;
    }
    view.initiated_by == "remote"
}

fn relationship_remote_public_id(view: &SwarmPeerRelationshipView) -> Option<String> {
    let envelope = view.agent_envelope.as_ref()?;
    let source_card_is_remote = source_agent_card_is_remote(view, envelope);
    let key = if source_card_is_remote {
        "source_public_id"
    } else {
        "target_public_id"
    };
    let message_public_id = envelope
        .message
        .get(key)
        .and_then(Value::as_str)
        .and_then(public_agent_id);
    if source_card_is_remote {
        relationship_remote_source_agent_card(view)
            .and_then(source_agent_card_public_id)
            .or(message_public_id)
    } else {
        message_public_id
    }
}

fn relationship_remote_agent_id(view: &SwarmPeerRelationshipView) -> Option<String> {
    let envelope = view.agent_envelope.as_ref()?;
    if source_agent_card_is_remote(view, envelope) {
        envelope.source_agent_id.clone().or_else(|| {
            envelope
                .source_agent_card
                .as_ref()
                .map(|card| card.agent_id.clone())
        })
    } else {
        envelope.target_agent_id.clone()
    }
}

fn relationship_remote_agent_card(view: &SwarmPeerRelationshipView) -> Option<&Value> {
    relationship_remote_source_agent_card(view).map(|card| &card.card)
}

fn relationship_remote_source_agent_card(
    view: &SwarmPeerRelationshipView,
) -> Option<&SwarmSourceAgentCard> {
    let envelope = view.agent_envelope.as_ref()?;
    if !source_agent_card_is_remote(view, envelope) {
        return None;
    }
    envelope.source_agent_card.as_ref()
}

fn source_agent_card_public_id(source_agent_card: &SwarmSourceAgentCard) -> Option<String> {
    source_agent_card
        .card
        .get("metadata")
        .and_then(|metadata| metadata.get("public_id"))
        .and_then(Value::as_str)
        .and_then(public_agent_id)
}

fn dm_remote_source_agent_card<'a>(
    local_public_id: &str,
    view: &'a SwarmPeerDmMessageView,
) -> Option<&'a SwarmSourceAgentCard> {
    if view.direction != "inbound" {
        return None;
    }
    let envelope = view.agent_envelope.as_ref()?;
    let source_public_id = envelope
        .message
        .get("source_public_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if source_public_id == Some(local_public_id) {
        return None;
    }
    if let Some(target_public_id) = envelope
        .message
        .get("target_public_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        && target_public_id != local_public_id
    {
        return None;
    }
    envelope.source_agent_card.as_ref()
}

fn dm_remote_display_name(local_public_id: &str, view: &SwarmPeerDmMessageView) -> Option<String> {
    let source_agent_card = dm_remote_source_agent_card(local_public_id, view)?;
    agent_card_display_name(&source_agent_card.card)
}

fn agent_card_display_name(card: &Value) -> Option<String> {
    card.get("name")
        .and_then(Value::as_str)
        .or_else(|| {
            card.get("metadata")
                .and_then(|metadata| metadata.get("display_name"))
                .and_then(Value::as_str)
        })
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn relationship_remote_agent_display_name(
    view: Option<&SwarmPeerRelationshipView>,
) -> Option<String> {
    view.and_then(relationship_remote_agent_card)
        .and_then(agent_card_display_name)
}

fn relationship_remote_agent_display_skills(
    view: Option<&SwarmPeerRelationshipView>,
) -> Vec<String> {
    view.and_then(relationship_remote_agent_card)
        .map(agent_card_skills)
        .unwrap_or_default()
}

fn agent_card_skill_label(value: &Value) -> Option<String> {
    value
        .as_str()
        .or_else(|| value.get("name").and_then(Value::as_str))
        .or_else(|| value.get("id").and_then(Value::as_str))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn agent_card_skills(card: &Value) -> Vec<String> {
    card.get("skills")
        .and_then(Value::as_array)
        .map(|skills| skills.iter().filter_map(agent_card_skill_label).collect())
        .unwrap_or_default()
}

fn peer_network_id(peer: Option<&SwarmPeerView>) -> Option<String> {
    peer.and_then(|peer| {
        peer.metadata
            .as_ref()
            .and_then(|metadata| metadata.get("network_id"))
            .and_then(Value::as_str)
            .or_else(|| {
                peer.discovery
                    .as_ref()
                    .and_then(|discovery| discovery.get("network_id"))
                    .and_then(Value::as_str)
            })
            .map(ToOwned::to_owned)
    })
}

fn relationship_peer_status(
    peer: Option<&SwarmPeerView>,
    relationship_state: Option<&str>,
) -> &'static str {
    if peer.and_then(|peer| peer.connected).unwrap_or(false) {
        "online"
    } else if relationship_state == Some("blocked") {
        "blocked"
    } else if peer.is_some_and(|peer| peer.discovery.is_some()) {
        "discovered"
    } else {
        "offline"
    }
}

fn enrich_relationship_payload_from_bridge(
    payload: &mut Value,
    view: Option<&SwarmPeerRelationshipView>,
    peer: Option<&SwarmPeerView>,
) {
    let relationship_state = payload
        .get("relationship_state")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let Some(object) = payload.as_object_mut() else {
        return;
    };
    if let Some(view) = view {
        insert_payload_if_present(
            object,
            "remote_node_id",
            Some(Value::String(view.remote_node_id.clone())),
        );
        insert_payload_if_present(
            object,
            "counterpart_agent_public_id",
            relationship_remote_public_id(view).map(Value::String),
        );
        insert_payload_if_present(
            object,
            "counterpart_agent_did",
            relationship_remote_agent_id(view).map(Value::String),
        );
        if let Some(card) = relationship_remote_agent_card(view) {
            if let Some(name) = card
                .get("name")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                && object
                    .get("counterpart_display_name")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .is_none_or(str::is_empty)
            {
                object.insert(
                    "counterpart_display_name".to_string(),
                    Value::String(name.to_string()),
                );
                object.insert(
                    "counterpart_agent_name".to_string(),
                    Value::String(name.to_string()),
                );
            }
            if let Some(description) = card
                .get("description")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                object.insert(
                    "counterpart_description".to_string(),
                    Value::String(description.to_string()),
                );
            }
            let skills = agent_card_skills(card);
            if !skills.is_empty() {
                object.insert("counterpart_skills".to_string(), json!(skills));
            }
        }
    }
    object.insert(
        "status".to_string(),
        Value::String(relationship_peer_status(peer, relationship_state.as_deref()).to_string()),
    );
    object.insert(
        "connected".to_string(),
        Value::Bool(peer.and_then(|peer| peer.connected).unwrap_or(false)),
    );
    insert_payload_if_present(
        object,
        "network_id",
        peer_network_id(peer).map(Value::String),
    );
}

fn matching_relationship_view<'a>(
    views: &'a [SwarmPeerRelationshipView],
    request: &FriendRequest,
) -> Option<&'a SwarmPeerRelationshipView> {
    views
        .iter()
        .find(|view| relationship_request_id(view) == Some(request.request_id.as_str()))
        .or_else(|| {
            request.remote_node_id.as_ref().and_then(|remote_node_id| {
                views
                    .iter()
                    .find(|view| view.remote_node_id == *remote_node_id)
            })
        })
}

fn matching_peer<'a>(
    peers: &'a [SwarmPeerView],
    request: &FriendRequest,
) -> Option<&'a SwarmPeerView> {
    request
        .remote_node_id
        .as_ref()
        .and_then(|remote_node_id| peers.iter().find(|peer| peer.node_id == *remote_node_id))
}

fn matching_peer_for_node<'a>(
    peers: &'a [SwarmPeerView],
    remote_node_id: Option<&str>,
) -> Option<&'a SwarmPeerView> {
    remote_node_id
        .and_then(|remote_node_id| peers.iter().find(|peer| peer.node_id == remote_node_id))
}

fn matching_relationship_view_for_payload<'a>(
    views: &'a [SwarmPeerRelationshipView],
    payload: &Value,
) -> Option<&'a SwarmPeerRelationshipView> {
    let remote_node_id = payload.get("remote_node_id").and_then(Value::as_str);
    let counterpart_public_id = payload.get("counterpart_public_id").and_then(Value::as_str);
    views.iter().find(|view| {
        Some(view.remote_node_id.as_str()) == remote_node_id
            || Some(view.remote_node_id.as_str()) == counterpart_public_id
            || relationship_remote_public_id(view).as_deref() == counterpart_public_id
    })
}

fn relationship_payload_identity_key(payload: &Value) -> Option<String> {
    [
        "remote_node_id",
        "counterpart_agent_public_id",
        "counterpart_agent_did",
        "counterpart_public_id",
    ]
    .into_iter()
    .find_map(|key| {
        payload
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
    })
}

fn envelope_message_text(envelope: Option<&SwarmAgentEnvelope>) -> Option<String> {
    envelope.and_then(|envelope| {
        ["text", "payload", "message"].into_iter().find_map(|key| {
            envelope
                .message
                .get(key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
        })
    })
}

fn truncate_preview(text: &str) -> String {
    const MAX_PREVIEW_CHARS: usize = 80;
    let trimmed = text.trim();
    let mut chars = trimmed.chars();
    let preview = chars.by_ref().take(MAX_PREVIEW_CHARS).collect::<String>();
    if chars.next().is_some() {
        format!("{preview}...")
    } else {
        preview
    }
}

fn friend_request_summary_payload(
    request: &FriendRequest,
    identities: &BTreeMap<String, PublicIdentity>,
    view: Option<&SwarmPeerRelationshipView>,
    peer: Option<&SwarmPeerView>,
    counterpart_label: &str,
) -> Value {
    let display_name = relationship_remote_agent_display_name(view).unwrap_or_else(|| {
        identities.get(&request.remote_public_id).map_or_else(
            || request.remote_public_id.clone(),
            |identity| identity.display_name.clone(),
        )
    });
    let mut object = Map::new();
    object.insert(
        "request_id".to_string(),
        Value::String(request.request_id.clone()),
    );
    object.insert(
        "direction".to_string(),
        Value::String(request_direction_label(request.direction).to_string()),
    );
    object.insert(
        "state".to_string(),
        Value::String(request_state_label(request.state).to_string()),
    );
    object.insert("created_at".to_string(), json!(request.created_at));
    object.insert("updated_at".to_string(), json!(request.updated_at));
    object.insert(counterpart_label.to_string(), Value::String(display_name));
    object.insert(
        "counterpart_public_id".to_string(),
        Value::String(request.remote_public_id.clone()),
    );
    object.insert(
        "counterpart_agent_public_id".to_string(),
        Value::String(request.remote_public_id.clone()),
    );
    insert_payload_if_present(
        &mut object,
        "counterpart_agent_did",
        view.and_then(relationship_remote_agent_id)
            .map(Value::String),
    );
    insert_payload_if_present(
        &mut object,
        "remote_node_id",
        request
            .remote_node_id
            .clone()
            .or_else(|| view.map(|view| view.remote_node_id.clone()))
            .map(Value::String),
    );
    insert_payload_if_present(
        &mut object,
        "preview",
        envelope_message_text(view.and_then(|view| view.agent_envelope.as_ref()))
            .map(|text| Value::String(truncate_preview(&text))),
    );
    let skills = relationship_remote_agent_display_skills(view);
    if !skills.is_empty() {
        object.insert("counterpart_skills".to_string(), json!(skills));
    }
    object.insert(
        "agent".to_string(),
        friend_request_agent_payload(request, identities, view),
    );
    object.insert("message".to_string(), friend_request_message_payload(view));
    object.insert(
        "network".to_string(),
        friend_request_network_payload(request, view, peer),
    );
    if let Some(source_agent_card) = view.and_then(relationship_remote_source_agent_card) {
        object.insert("source_agent_card".to_string(), json!(source_agent_card));
        object.insert("agent_card".to_string(), source_agent_card.card.clone());
        object.insert(
            "agent_card_hash".to_string(),
            Value::String(source_agent_card.card_hash.clone()),
        );
        object.insert(
            "agent_card_issued_at".to_string(),
            json!(source_agent_card.issued_at),
        );
        insert_payload_if_present(
            &mut object,
            "agent_card_signature",
            source_agent_card.signature.clone().map(Value::String),
        );
    }
    Value::Object(object)
}

fn friend_request_agent_payload(
    request: &FriendRequest,
    identities: &BTreeMap<String, PublicIdentity>,
    view: Option<&SwarmPeerRelationshipView>,
) -> Value {
    let identity = identities.get(&request.remote_public_id);
    let card = view.and_then(relationship_remote_agent_card);
    let mut object = Map::new();
    object.insert(
        "public_id".to_string(),
        Value::String(request.remote_public_id.clone()),
    );
    insert_payload_if_present(
        &mut object,
        "display_name",
        card.and_then(agent_card_display_name)
            .or_else(|| identity.map(|identity| identity.display_name.clone()))
            .map(Value::String),
    );
    insert_payload_if_present(
        &mut object,
        "description",
        card.and_then(|card| {
            card.get("description")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
        })
        .map(Value::String),
    );
    insert_payload_if_present(
        &mut object,
        "agent_did",
        view.and_then(relationship_remote_agent_id)
            .or_else(|| {
                identity
                    .and_then(|identity| identity.agent_did.clone())
                    .or_else(|| {
                        view.and_then(|view| view.agent_envelope.as_ref())
                            .and_then(|envelope| envelope.source_agent_id.clone())
                    })
            })
            .map(Value::String),
    );
    insert_payload_if_present(
        &mut object,
        "node_id",
        view.and_then(relationship_remote_source_agent_card)
            .and_then(|card| card.node_id.clone())
            .or_else(|| request.remote_node_id.clone())
            .or_else(|| view.map(|view| view.remote_node_id.clone()))
            .map(Value::String),
    );
    insert_friend_request_agent_card_payload(&mut object, card, view);
    insert_payload_if_present(
        &mut object,
        "identity_agent_did",
        identity
            .and_then(|identity| identity.agent_did.clone())
            .map(Value::String),
    );
    insert_payload_if_present(
        &mut object,
        "active",
        identity.map(|identity| Value::Bool(identity.active)),
    );
    let skills = relationship_remote_agent_display_skills(view);
    if !skills.is_empty() {
        object.insert("skills".to_string(), json!(skills));
        object.insert("counterpart_skills".to_string(), json!(skills));
    }
    Value::Object(object)
}

fn insert_friend_request_agent_card_payload(
    object: &mut Map<String, Value>,
    card: Option<&Value>,
    view: Option<&SwarmPeerRelationshipView>,
) {
    insert_payload_if_present(
        object,
        "card_hash",
        view.and_then(relationship_remote_source_agent_card)
            .map(|card| Value::String(card.card_hash.clone())),
    );
    insert_payload_if_present(
        object,
        "card_issued_at",
        view.and_then(relationship_remote_source_agent_card)
            .map(|card| json!(card.issued_at)),
    );
    insert_payload_if_present(
        object,
        "protocol_version",
        card.and_then(|card| card.get("protocolVersion"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| Value::String(value.to_string())),
    );
    insert_payload_if_present(
        object,
        "preferred_transport",
        card.and_then(|card| card.get("preferredTransport"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| Value::String(value.to_string())),
    );
    insert_payload_if_present(
        object,
        "metadata",
        card.and_then(|card| card.get("metadata")).cloned(),
    );
    insert_payload_if_present(
        object,
        "capabilities",
        card.and_then(|card| card.get("capabilities")).cloned(),
    );
    insert_payload_if_present(object, "agent_card", card.cloned());
    insert_payload_if_present(
        object,
        "source_agent_card",
        view.and_then(relationship_remote_source_agent_card)
            .map(|card| json!(card)),
    );
}

fn friend_request_message_payload(view: Option<&SwarmPeerRelationshipView>) -> Value {
    let Some(envelope) = view.and_then(|view| view.agent_envelope.as_ref()) else {
        return Value::Object(Map::new());
    };
    let mut object = envelope.message.as_object().cloned().unwrap_or_default();
    if !object.contains_key("text")
        && let Some(text) = envelope_message_text(Some(envelope))
    {
        object.insert("text".to_string(), Value::String(text));
    }
    Value::Object(object)
}

fn friend_request_network_payload(
    request: &FriendRequest,
    view: Option<&SwarmPeerRelationshipView>,
    peer: Option<&SwarmPeerView>,
) -> Value {
    let remote_node_id = request
        .remote_node_id
        .clone()
        .or_else(|| view.map(|view| view.remote_node_id.clone()));
    let connected = peer.and_then(|peer| peer.connected).unwrap_or(false);
    let relationship_state = peer
        .and_then(|peer| peer.relationship.as_ref())
        .and_then(|relationship| relationship.get("relationship_state"))
        .and_then(Value::as_str)
        .or_else(|| view.map(|view| view.relationship_state.as_str()));
    let status = if connected {
        "online"
    } else if relationship_state == Some("blocked") {
        "blocked"
    } else if peer.is_some_and(|peer| peer.discovery.is_some()) {
        "discovered"
    } else {
        "offline"
    };
    let endpoint = peer
        .and_then(|peer| peer.metadata.as_ref().and_then(iroh_endpoint_id))
        .or_else(|| peer.and_then(|peer| peer.discovery.as_ref().and_then(iroh_endpoint_id)));

    let mut object = Map::new();
    insert_payload_if_present(
        &mut object,
        "remote_node_id",
        remote_node_id.map(Value::String),
    );
    object.insert("status".to_string(), Value::String(status.to_string()));
    object.insert("connected".to_string(), Value::Bool(connected));
    insert_payload_if_present(
        &mut object,
        "endpoint",
        endpoint.map(|value| Value::String(value.to_string())),
    );
    insert_payload_if_present(
        &mut object,
        "discovery",
        peer.and_then(|peer| peer.discovery.clone()),
    );
    insert_payload_if_present(
        &mut object,
        "metadata",
        peer.and_then(|peer| peer.metadata.clone()),
    );
    Value::Object(object)
}

fn relationship_payload_from_friendship(
    friendship: &Friendship,
    identities: &BTreeMap<String, PublicIdentity>,
    bindings: &BTreeMap<String, ControllerBinding>,
    transport_bindings: &[RemoteTransportBinding],
) -> Value {
    let identity = identities.get(&friendship.remote_public_id);
    let display_name = identity
        .map(|identity| identity.display_name.clone())
        .or_else(|| friendship.display_name.clone());
    let transport_binding = transport_bindings
        .iter()
        .find(|binding| binding.public_id == friendship.remote_public_id);
    let remote_node_id =
        transport_binding_remote_node_id(transport_bindings, &friendship.remote_public_id)
            .or_else(|| binding_remote_node_id(bindings, &friendship.remote_public_id));
    let counterpart_agent_did = identity
        .and_then(|identity| identity.agent_did.clone())
        .or_else(|| transport_binding.and_then(|binding| binding.agent_did.clone()));
    json!({
        "counterpart_public_id": friendship.remote_public_id.clone(),
        "counterpart_agent_public_id": friendship.remote_public_id.clone(),
        "counterpart_agent_did": counterpart_agent_did,
        "counterpart_agent_name": display_name.clone(),
        "counterpart_display_name": display_name,
        "remote_node_id": remote_node_id,
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

fn relationship_payload_is_active_friend(item: &Value) -> bool {
    matches!(
        item.get("relationship_state").and_then(Value::as_str),
        Some("accepted" | "active" | "friend")
    )
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
            skills: Vec::new(),
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

fn known_identity_from_source_agent_card(
    counterpart_public_id: &str,
    source_agent_card: &SwarmSourceAgentCard,
    fallback_identity: Option<&PublicIdentity>,
    observed_at: i64,
) -> orchestration_service::KnownIdentitySnapshot {
    let created_at = i64::try_from(source_agent_card.issued_at).unwrap_or(observed_at);
    let public_id = source_agent_card_public_id(source_agent_card)
        .unwrap_or_else(|| counterpart_public_id.to_string());
    orchestration_service::KnownIdentitySnapshot {
        public_id,
        agent_did: fallback_identity
            .and_then(|identity| identity.agent_did.clone())
            .or_else(|| Some(source_agent_card.agent_id.clone())),
        display_name: fallback_identity
            .map(|identity| identity.display_name.clone())
            .or_else(|| agent_card_display_name(&source_agent_card.card))
            .unwrap_or_else(|| counterpart_public_id.to_string()),
        skills: agent_card_skills(&source_agent_card.card),
        active: fallback_identity.is_none_or(|identity| identity.active),
        created_at,
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
        let counterpart_public_id = relationship_remote_public_id(view)
            .or_else(|| counterpart_public_id_for_remote_node(bindings, &view.remote_node_id))
            .unwrap_or_else(|| view.remote_node_id.clone());
        let target_agent = relationship_remote_agent_id(view)
            .or_else(|| {
                identities
                    .get(&counterpart_public_id)
                    .and_then(|identity| identity.agent_did.clone())
            })
            .unwrap_or_else(|| counterpart_public_id.clone());
        let observed_at = i64::try_from(view.updated_at).unwrap_or_default();
        let mut counterpart = counterpart_snapshot(
            identities,
            bindings,
            &counterpart_public_id,
            &target_agent,
            &view.remote_node_id,
            observed_at,
        );
        if let Some(source_agent_card) = relationship_remote_source_agent_card(view) {
            counterpart.known_identity = Some(known_identity_from_source_agent_card(
                &counterpart_public_id,
                source_agent_card,
                identities.get(&counterpart_public_id),
                observed_at,
            ));
        }
        synced.push(orchestration_service::RelationshipSyncView {
            counterpart,
            relationship_state: view.relationship_state.clone(),
            last_action: Some(view.last_action.clone()),
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
            updated_at: observed_at,
        });
    }
    orchestration_service::reconcile_relationship_views(
        &*state.social_store,
        local_public_id,
        &synced,
    )
    .map_err(anyhow::Error::msg)?;
    for view in views
        .iter()
        .filter(|view| view.relationship_state == "accepted" || view.relationship_state == "active")
    {
        let counterpart_public_id = relationship_remote_public_id(view)
            .or_else(|| counterpart_public_id_for_remote_node(bindings, &view.remote_node_id))
            .unwrap_or_else(|| view.remote_node_id.clone());
        let state = state.clone();
        let local_public_id = local_public_id.to_string();
        tokio::spawn(async move {
            if let Err(error) = replay_deferred_dm_agent_events_for_friendship(
                &state,
                &local_public_id,
                &counterpart_public_id,
            )
            .await
            {
                tracing::warn!(
                    error = %error,
                    local_public_id = %local_public_id,
                    remote_public_id = %counterpart_public_id,
                    "failed to replay deferred DM agent events after relationship sync"
                );
            }
        });
    }
    Ok(())
}

pub(crate) fn reconcile_swarm_dm_threads(
    state: &ControlPlaneState,
    local_public_id: &str,
    identities: &BTreeMap<String, PublicIdentity>,
    bindings: &BTreeMap<String, ControllerBinding>,
    views: &[SwarmPeerDmThreadView],
) -> anyhow::Result<()> {
    let transport_bindings =
        transport_binding_service::list_transport_bindings(&*state.social_store)
            .unwrap_or_default();
    let friendships = friendship_service::list_friendships(&*state.social_store, local_public_id)
        .unwrap_or_default();
    let mut synced = Vec::with_capacity(views.len());
    for view in views {
        let counterpart_public_id = dm_counterpart_public_id(
            local_public_id,
            bindings,
            &transport_bindings,
            &friendships,
            &view.remote_node_id,
            None,
            "thread",
        );
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
    let transport_bindings =
        transport_binding_service::list_transport_bindings(&*state.social_store)
            .unwrap_or_default();
    let friendships = friendship_service::list_friendships(&*state.social_store, local_public_id)
        .unwrap_or_default();
    let mut synced = Vec::with_capacity(views.len());
    let mut display_name_refreshes = Vec::new();
    for view in views {
        if let Err(error) = record_collective_participation_from_dm(state, view) {
            tracing::warn!(
                target = "wattetheria.collective",
                message_id = %view.message_id,
                error = %error,
                "failed to record collective participation DM"
            );
        }
        let counterpart_public_id = dm_counterpart_public_id(
            local_public_id,
            bindings,
            &transport_bindings,
            &friendships,
            &view.remote_node_id,
            view.agent_envelope.as_ref(),
            &view.direction,
        );
        let target_agent = identities
            .get(&counterpart_public_id)
            .and_then(|identity| identity.agent_did.clone())
            .unwrap_or_else(|| counterpart_public_id.clone());
        if let Some(display_name) = dm_remote_display_name(local_public_id, view) {
            display_name_refreshes.push((counterpart_public_id.clone(), display_name));
        }
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
        .map_err(anyhow::Error::msg)?;
    for (counterpart_public_id, display_name) in display_name_refreshes {
        remote_identity_service::refresh_remote_display_name(
            &*state.social_store,
            &counterpart_public_id,
            &display_name,
        )
        .map_err(anyhow::Error::msg)?;
    }
    Ok(())
}

async fn reconcile_bridge_relationships_for_local(
    state: &ControlPlaneState,
    local_public_id: &str,
    identities: &BTreeMap<String, PublicIdentity>,
    bindings: &BTreeMap<String, ControllerBinding>,
) -> anyhow::Result<()> {
    if let Ok(views) = state.swarm_bridge.list_peer_relationships().await {
        reconcile_swarm_relationship_views(state, local_public_id, identities, bindings, &views)?;
    }
    Ok(())
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
    let accepted = matches!(action, orchestration_service::RelationshipAction::Accept);
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
    .map_err(anyhow::Error::msg)?;
    if accepted {
        replay_deferred_dm_agent_events_for_friendship(
            state,
            local_public_id,
            counterpart_public_id,
        )
        .await?;
    }
    Ok(())
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
    let now = Utc::now().timestamp_millis();
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

struct OutboundFriendRequestGate {
    denied_response: Option<Response>,
    pending_request_id: Option<String>,
}

fn message_with_request_id(
    base_message: Option<Value>,
    request_id: Option<String>,
) -> Option<Value> {
    let Some(request_id) = request_id else {
        return base_message;
    };
    Some(with_social_defaults(
        base_message.unwrap_or_else(|| json!({})),
        [("request_id", Value::String(request_id))],
    ))
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

fn friend_request_message_char_count(message: &Value) -> usize {
    match message {
        Value::String(value) => value.chars().count(),
        _ => serde_json::to_string(message)
            .unwrap_or_else(|_| message.to_string())
            .chars()
            .count(),
    }
}

fn friend_request_message_error(message: Option<&Value>) -> Option<Response> {
    let message = message?;
    let length = friend_request_message_char_count(message);
    if length > FRIEND_REQUEST_MESSAGE_MAX_CHARS {
        return Some((
            StatusCode::BAD_REQUEST,
            Json(json!({
                "error": format!(
                    "friend request message must be at most {FRIEND_REQUEST_MESSAGE_MAX_CHARS} characters"
                ),
                "max_chars": FRIEND_REQUEST_MESSAGE_MAX_CHARS,
                "actual_chars": length
            })),
        )
            .into_response());
    }
    None
}

struct SignedRelationshipActionArgs {
    local_agent_id: String,
    local_public_id: String,
    local_display_name: Option<String>,
    target_agent_id: String,
    remote_node_id: String,
    action: SwarmRelationshipAction,
    capability: String,
    message: Value,
    extensions: Option<Value>,
}

struct SignedDirectMessageArgs {
    local_agent_id: String,
    local_public_id: String,
    local_display_name: Option<String>,
    target_agent_id: String,
    remote_node_id: String,
    content: Value,
    message: Value,
    extensions: Option<Value>,
}

async fn send_signed_relationship_action_command(
    state: &ControlPlaneState,
    args: SignedRelationshipActionArgs,
) -> anyhow::Result<Value> {
    let local_node_id = state.swarm_bridge.local_node_id().await.ok();
    let remote_node_id = args.remote_node_id;
    let agent_envelope = build_signed_agent_envelope_for_nodes(
        state,
        SignedAgentEnvelopeArgs {
            source_agent_id: args.local_agent_id,
            source_public_id: public_agent_id(&args.local_public_id),
            source_display_name: args.local_display_name,
            target_agent_id: Some(args.target_agent_id),
            source_node_id: local_node_id,
            target_node_id: Some(remote_node_id.clone()),
            capability: args.capability,
            message: args.message,
            extensions: args.extensions,
        },
    )?;
    state
        .swarm_bridge
        .send_peer_relationship_action(SwarmRelationshipActionCommand {
            remote_node_id,
            action: args.action,
            agent_envelope,
        })
        .await
}

async fn send_signed_direct_message_command(
    state: &ControlPlaneState,
    args: SignedDirectMessageArgs,
) -> anyhow::Result<(Value, Value, Option<String>)> {
    let local_node_id = state.swarm_bridge.local_node_id().await.ok();
    let remote_node_id = args.remote_node_id;
    let agent_envelope = build_signed_agent_envelope_for_nodes(
        state,
        SignedAgentEnvelopeArgs {
            source_agent_id: args.local_agent_id,
            source_public_id: public_agent_id(&args.local_public_id),
            source_display_name: args.local_display_name,
            target_agent_id: Some(args.target_agent_id),
            source_node_id: local_node_id,
            target_node_id: Some(remote_node_id.clone()),
            capability: "social.dm.send".to_string(),
            message: args.message,
            extensions: args.extensions,
        },
    )?;
    let agent_envelope_json = serde_json::to_value(&agent_envelope).unwrap_or(Value::Null);
    let agent_signature = agent_envelope.signature.clone();
    let response = state
        .swarm_bridge
        .send_peer_direct_message(SwarmDirectMessageCommand {
            remote_node_id,
            agent_envelope,
            content: args.content,
        })
        .await?;
    Ok((response, agent_envelope_json, agent_signature))
}

async fn ensure_outbound_friend_request_allowed(
    state: &ControlPlaneState,
    local_public_id: &str,
    counterpart_public_id: &str,
    remote_node_id: &str,
    now: i64,
) -> anyhow::Result<OutboundFriendRequestGate> {
    let (identities, bindings) = load_social_identity_maps(state).await;
    if let Ok(views) = state.swarm_bridge.list_peer_relationships().await {
        reconcile_swarm_relationship_views(state, local_public_id, &identities, &bindings, &views)?;
    }
    let pending_request_id =
        friend_request_service::list_friend_requests(&*state.social_store, local_public_id)
            .unwrap_or_default()
            .into_iter()
            .find(|request| {
                request.remote_public_id == counterpart_public_id
                    && request.direction == FriendRequestDirection::Outbound
                    && request.state == FriendRequestState::Pending
            })
            .map(|request| request.request_id);
    let evaluation = policy_service::evaluate_outbound_friend_request_policy(
        &*state.social_store,
        local_public_id,
        counterpart_public_id,
        Some(remote_node_id),
        now,
    )
    .map_err(anyhow::Error::msg)?;
    Ok(OutboundFriendRequestGate {
        denied_response: (evaluation.decision == PolicyDecision::Deny)
            .then(|| policy_denied_response(&evaluation.reason)),
        pending_request_id,
    })
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
    let transport_bindings =
        transport_binding_service::list_transport_bindings(&*state.social_store)
            .unwrap_or_default();
    let bridge_views = state.swarm_bridge.list_peer_relationships().await;
    if let Ok(views) = &bridge_views {
        reconcile_swarm_relationship_views(state, &local.public_id, &identities, &bindings, views)?;
    }
    let peers = state.swarm_bridge.peers().await.unwrap_or_default();
    let friendships = friendship_service::list_friendships(&*state.social_store, &local.public_id)
        .unwrap_or_default();
    let blocks =
        block_service::list_blocks(&*state.social_store, &local.public_id).unwrap_or_default();
    let mut items = if friendships.is_empty() && blocks.is_empty() {
        bridge_views?
            .into_iter()
            .filter(|view| matches!(view.relationship_state.as_str(), "accepted" | "active"))
            .map(|view| {
                let peer = matching_peer_for_node(&peers, Some(&view.remote_node_id));
                let mut payload = relationship_view_to_payload(&view, &identities, &bindings);
                enrich_relationship_payload_from_bridge(&mut payload, Some(&view), peer);
                payload
            })
            .collect::<Vec<_>>()
    } else {
        let bridge_view_items = bridge_views.as_deref().unwrap_or(&[]);
        let mut items = Vec::new();
        let mut seen_friendship_keys = BTreeSet::new();
        for friendship in friendships
            .iter()
            .filter(|friendship| friendship.state == FriendshipState::Active)
        {
            let mut payload = relationship_payload_from_friendship(
                friendship,
                &identities,
                &bindings,
                &transport_bindings,
            );
            let view = matching_relationship_view_for_payload(bridge_view_items, &payload);
            let peer = view
                .and_then(|view| matching_peer_for_node(&peers, Some(&view.remote_node_id)))
                .or_else(|| {
                    payload
                        .get("remote_node_id")
                        .and_then(Value::as_str)
                        .and_then(|remote_node_id| {
                            matching_peer_for_node(&peers, Some(remote_node_id))
                        })
                });
            enrich_relationship_payload_from_bridge(&mut payload, view, peer);
            let key = relationship_payload_identity_key(&payload);
            if key
                .as_ref()
                .is_none_or(|key| seen_friendship_keys.insert(key.clone()))
            {
                items.push(payload);
            }
        }
        items.extend(blocks.iter().map(|block| {
            let mut payload = relationship_payload_from_block(block, &identities);
            let view = matching_relationship_view_for_payload(bridge_view_items, &payload);
            let peer = view
                .and_then(|view| matching_peer_for_node(&peers, Some(&view.remote_node_id)))
                .or_else(|| {
                    payload
                        .get("remote_node_id")
                        .and_then(Value::as_str)
                        .and_then(|remote_node_id| {
                            matching_peer_for_node(&peers, Some(remote_node_id))
                        })
                });
            enrich_relationship_payload_from_bridge(&mut payload, view, peer);
            payload
        }));
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
    Ok(items)
}

pub(crate) async fn build_agent_dm_threads_payload(
    state: &ControlPlaneState,
    public_id: Option<&str>,
) -> anyhow::Result<Vec<Value>> {
    let local = resolve_social_local_context(state, public_id).await;
    let (identities, bindings) = load_social_identity_maps(state).await;
    reconcile_bridge_relationships_for_local(state, &local.public_id, &identities, &bindings)
        .await?;
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

fn normalized_reply_to_message_id(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn dm_message_reply_to_message_id(message: &DirectMessage) -> Option<&str> {
    message
        .agent_envelope_json
        .as_ref()
        .and_then(|envelope| {
            envelope
                .pointer("/message/reply_to_message_id")
                .and_then(Value::as_str)
        })
        .or_else(|| {
            message.agent_envelope_json.as_ref().and_then(|envelope| {
                envelope
                    .pointer("/message_json/reply_to_message_id")
                    .and_then(Value::as_str)
            })
        })
        .or_else(|| {
            message
                .content_json
                .get("reply_to_message_id")
                .and_then(Value::as_str)
        })
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn outbound_dm_reply_already_sent(
    state: &ControlPlaneState,
    local_public_id: &str,
    counterpart_public_id: &str,
    thread_id: &str,
    reply_to_message_id: Option<&str>,
) -> anyhow::Result<Option<DirectMessage>> {
    let Some(reply_to_message_id) = normalized_reply_to_message_id(reply_to_message_id) else {
        return Ok(None);
    };
    let messages = message_service::list_thread_messages(&*state.social_store, thread_id)
        .map_err(anyhow::Error::msg)?;
    Ok(messages.into_iter().find(|message| {
        message.direction == MessageDirection::Outbound
            && message.local_public_id == local_public_id
            && message.remote_public_id == counterpart_public_id
            && dm_message_reply_to_message_id(message) == Some(reply_to_message_id.as_str())
    }))
}

#[derive(Clone, Copy)]
struct DmReplyDedupeResponseArgs<'a> {
    local_public_id: &'a str,
    counterpart_public_id: &'a str,
    request_counterpart_public_id: &'a str,
    thread_id: &'a str,
    content: &'a Value,
    reply_to_message_id: Option<&'a str>,
}

fn outbound_dm_reply_dedupe_response(
    state: &ControlPlaneState,
    headers: &HeaderMap,
    args: DmReplyDedupeResponseArgs<'_>,
) -> anyhow::Result<Option<Response>> {
    let Some(message) = outbound_dm_reply_already_sent(
        state,
        args.local_public_id,
        args.counterpart_public_id,
        args.thread_id,
        args.reply_to_message_id,
    )?
    else {
        return Ok(None);
    };
    let reply_to_message_id = normalized_reply_to_message_id(args.reply_to_message_id);
    let response_json = json!({
        "ok": true,
        "source": "agent_dm_reply_dedupe",
        "message_id": message.message_id,
        "thread_id": args.thread_id,
        "reply_to_message_id": reply_to_message_id.clone(),
    });
    append_commit_response(
        state,
        headers,
        CommitResponseArgs {
            action_type: "social.agent_dm_send",
            target_id: Some(args.counterpart_public_id.to_owned()),
            actor_public_id: Some(args.local_public_id.to_owned()),
            request_json: &json!({
                "counterpart_public_id": args.request_counterpart_public_id,
                "content": args.content,
                "reply_to_message_id": reply_to_message_id,
            }),
            response_json: &response_json,
        },
    )?;
    Ok(Some(
        (StatusCode::ACCEPTED, Json(response_json)).into_response(),
    ))
}

fn build_outbound_dm_message(
    local_public_id: &str,
    counterpart_public_id: &str,
    thread_id: &str,
    content: &Value,
    reply_to_message_id: Option<&str>,
    now: i64,
) -> (String, Value) {
    let message_id = Uuid::new_v4().to_string();
    let mut base_message = json!({
        "content": content.clone(),
    });
    if let Some(reply_to_message_id) = normalized_reply_to_message_id(reply_to_message_id) {
        base_message["reply_to_message_id"] = Value::String(reply_to_message_id);
    }
    let message = with_social_defaults(
        base_message,
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
    reconcile_bridge_relationships_for_local(state, &local.public_id, &identities, &bindings)
        .await?;
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
    let mut items = match build_agent_relationship_payload(
        &state,
        query.public_id.as_deref(),
        query.counterpart_public_id.as_deref(),
    )
    .await
    {
        Ok(items) => items,
        Err(error) => return internal_error(&error),
    };
    items.retain(relationship_payload_is_active_friend);
    if let Some(display_name) = normalized_display_name_filter(query.display_name.as_deref()) {
        items.retain(|item| payload_display_name_matches(item, display_name));
    }
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

async fn load_friend_request_views(
    state: &ControlPlaneState,
    public_id: Option<&str>,
) -> anyhow::Result<(
    String,
    BTreeMap<String, PublicIdentity>,
    Vec<FriendRequest>,
    Vec<SwarmPeerRelationshipView>,
    Vec<SwarmPeerView>,
)> {
    let local = resolve_social_local_context(state, public_id).await;
    let (identities, bindings) = load_social_identity_maps(state).await;
    let relationship_views = state.swarm_bridge.list_peer_relationships().await.ok();
    if let Some(views) = &relationship_views {
        reconcile_swarm_relationship_views(state, &local.public_id, &identities, &bindings, views)?;
    }
    let friend_requests =
        friend_request_service::list_friend_requests(&*state.social_store, &local.public_id)
            .unwrap_or_default();
    let peers = state.swarm_bridge.peers().await.unwrap_or_default();
    Ok((
        local.public_id,
        identities,
        friend_requests,
        relationship_views.unwrap_or_default(),
        peers,
    ))
}

fn bounded_friend_request_page(query: &FriendRequestsQuery) -> (usize, usize) {
    const DEFAULT_LIMIT: usize = 20;
    const MAX_LIMIT: usize = 100;
    (
        query.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT),
        query.offset.unwrap_or(0),
    )
}

fn friend_request_list_payload(items: Vec<Value>, total: usize, offset: usize) -> Value {
    let returned = items.len();
    let has_more = offset.saturating_add(returned) < total;
    let mut object = Map::new();
    object.insert("ok".to_string(), Value::Bool(true));
    object.insert("count".to_string(), json!(total));
    object.insert("returned".to_string(), json!(returned));
    object.insert("has_more".to_string(), Value::Bool(has_more));
    if has_more {
        object.insert(
            "next_offset".to_string(),
            json!(offset.saturating_add(returned)),
        );
    }
    object.insert("items".to_string(), Value::Array(items));
    Value::Object(object)
}

async fn load_decidable_friend_request(
    state: &ControlPlaneState,
    local_public_id: &str,
    request_id: &str,
) -> Result<FriendRequest, Response> {
    let (identities, bindings) = load_social_identity_maps(state).await;
    if let Ok(views) = state.swarm_bridge.list_peer_relationships().await
        && let Err(error) = reconcile_swarm_relationship_views(
            state,
            local_public_id,
            &identities,
            &bindings,
            &views,
        )
    {
        return Err(internal_error(&error));
    }
    let friend_requests =
        friend_request_service::list_friend_requests(&*state.social_store, local_public_id)
            .unwrap_or_default();
    let Some(request) = friend_requests
        .iter()
        .find(|request| request.request_id == request_id)
    else {
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({"error": "friend request not found"})),
        )
            .into_response());
    };
    if request.direction != FriendRequestDirection::Inbound
        || request.state != FriendRequestState::Pending
    {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "friend request is not an inbound pending request"})),
        )
            .into_response());
    }
    Ok(request.clone())
}

fn friend_request_decision_message(request: &FriendRequest) -> Value {
    let mut base_message = Map::new();
    base_message.insert(
        "request_id".to_string(),
        Value::String(request.request_id.clone()),
    );
    insert_payload_if_present(
        &mut base_message,
        "correlation_id",
        request.correlation_id.clone().map(Value::String),
    );
    Value::Object(base_message)
}

pub(crate) async fn list_friend_requests(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<FriendRequestsQuery>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let (local_public_id, identities, friend_requests, relationship_views, peers) =
        match load_friend_request_views(&state, None).await {
            Ok(result) => result,
            Err(error) => return internal_error(&error),
        };
    let (limit, offset) = bounded_friend_request_page(&query);
    let filtered = friend_requests
        .iter()
        .filter(|request| {
            request.direction == FriendRequestDirection::Inbound
                && request.state == FriendRequestState::Pending
        })
        .collect::<Vec<_>>();
    let total = filtered.len();
    let items = filtered
        .into_iter()
        .skip(offset)
        .take(limit)
        .map(|request| {
            friend_request_summary_payload(
                request,
                &identities,
                matching_relationship_view(&relationship_views, request),
                matching_peer(&peers, request),
                "from",
            )
        })
        .collect::<Vec<_>>();
    let payload = friend_request_list_payload(items, total, offset);
    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "civilization".to_string(),
        action: "civilization.friend_requests.query".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(local_public_id),
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({"count": total, "direction": "inbound", "state": "pending"})),
    });
    Json(payload).into_response()
}

pub(crate) async fn list_sent_friend_requests(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<FriendRequestsQuery>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let (local_public_id, identities, friend_requests, relationship_views, peers) =
        match load_friend_request_views(&state, None).await {
            Ok(result) => result,
            Err(error) => return internal_error(&error),
        };
    let (limit, offset) = bounded_friend_request_page(&query);
    let filtered = friend_requests
        .iter()
        .filter(|request| request.direction == FriendRequestDirection::Outbound)
        .collect::<Vec<_>>();
    let total = filtered.len();
    let items = filtered
        .into_iter()
        .skip(offset)
        .take(limit)
        .map(|request| {
            let mut item = friend_request_summary_payload(
                request,
                &identities,
                matching_relationship_view(&relationship_views, request),
                matching_peer(&peers, request),
                "to",
            );
            if let Some(object) = item.as_object_mut() {
                object.insert(
                    "state".to_string(),
                    Value::String(request_state_label(request.state).to_string()),
                );
            }
            item
        })
        .collect::<Vec<_>>();
    let payload = friend_request_list_payload(items, total, offset);
    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "civilization".to_string(),
        action: "civilization.sent_friend_requests.query".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(local_public_id),
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({"count": total, "direction": "outbound"})),
    });
    Json(payload).into_response()
}

pub(crate) async fn get_friend_request(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Path(request_id): Path<String>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let (local_public_id, identities, friend_requests, relationship_views, peers) =
        match load_friend_request_views(&state, None).await {
            Ok(result) => result,
            Err(error) => return internal_error(&error),
        };
    let Some(request) = friend_requests
        .iter()
        .find(|request| request.request_id == request_id)
    else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error": "friend request not found"})),
        )
            .into_response();
    };
    let relationship_view = matching_relationship_view(&relationship_views, request);
    let peer = matching_peer(&peers, request);
    let payload = json!({
        "ok": true,
        "request_id": request.request_id,
        "direction": request_direction_label(request.direction),
        "state": request_state_label(request.state),
        "received_at": request.created_at,
        "updated_at": request.updated_at,
        "agent": friend_request_agent_payload(request, &identities, relationship_view),
        "message": friend_request_message_payload(relationship_view),
        "network": friend_request_network_payload(request, relationship_view, peer),
    });
    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "civilization".to_string(),
        action: "civilization.friend_request.get".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(local_public_id),
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({"request_id": request.request_id})),
    });
    Json(payload).into_response()
}

async fn decide_friend_request(
    state: ControlPlaneState,
    headers: HeaderMap,
    request_id: String,
    action: SwarmRelationshipAction,
    public_id: Option<&str>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let local = resolve_social_local_context(&state, public_id).await;
    let request = match load_decidable_friend_request(&state, &local.public_id, &request_id).await {
        Ok(request) => request,
        Err(response) => return response,
    };
    let Some(remote_node_id) = request.remote_node_id.clone() else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "friend request missing remote_node_id"})),
        )
            .into_response();
    };
    let counterpart = match resolve_social_counterpart_target_by_remote_node(
        &state,
        &remote_node_id,
        Some(request.remote_public_id.clone()),
    )
    .await
    {
        Ok(counterpart) => counterpart,
        Err(error) => return internal_error(&anyhow::Error::msg(error.to_string())),
    };
    let capability = capability_for_relationship_action(&action).to_string();
    let now = Utc::now().timestamp();
    let message = build_relationship_action_message(
        &local.public_id,
        &counterpart.counterpart_public_id,
        &action,
        Some(friend_request_decision_message(&request)),
        now,
    );
    let response_json = match send_signed_relationship_action_command(
        &state,
        SignedRelationshipActionArgs {
            local_agent_id: local.agent_id,
            local_public_id: local.public_id.clone(),
            local_display_name: local.display_name,
            target_agent_id: counterpart.target_agent.clone(),
            remote_node_id: counterpart.remote_node.clone(),
            action: action.clone(),
            capability: capability.clone(),
            message: message.clone(),
            extensions: None,
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
            local_public_id: local.public_id,
            counterpart_public_id: counterpart.counterpart_public_id.clone(),
            target_agent: counterpart.target_agent,
            remote_node_id: counterpart.remote_node,
            action,
            capability,
            request_counterpart_public_id: request.remote_public_id.clone(),
            message,
            response_json,
        },
    )
    .await
}

pub(crate) async fn accept_friend_request(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Path(request_id): Path<String>,
    body: Option<Json<Value>>,
) -> Response {
    let public_id = body
        .as_ref()
        .and_then(|Json(body)| body.get("public_id"))
        .and_then(Value::as_str);
    decide_friend_request(
        state,
        headers,
        request_id,
        SwarmRelationshipAction::Accept,
        public_id,
    )
    .await
}

pub(crate) async fn reject_friend_request(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Path(request_id): Path<String>,
    body: Option<Json<Value>>,
) -> Response {
    let public_id = body
        .as_ref()
        .and_then(|Json(body)| body.get("public_id"))
        .and_then(Value::as_str);
    decide_friend_request(
        state,
        headers,
        request_id,
        SwarmRelationshipAction::Reject,
        public_id,
    )
    .await
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

fn friendship_display_name<'a>(
    friendship: &'a Friendship,
    identities: &'a BTreeMap<String, PublicIdentity>,
) -> Option<&'a str> {
    friendship
        .display_name
        .as_deref()
        .or_else(|| {
            identities
                .get(&friendship.remote_public_id)
                .map(|item| item.display_name.as_str())
        })
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn normalized_display_name_filter(display_name: Option<&str>) -> Option<&str> {
    display_name
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn payload_display_name_matches(item: &Value, display_name: &str) -> bool {
    [
        "counterpart_display_name",
        "counterpart_agent_name",
        "display_name",
    ]
    .into_iter()
    .any(|key| item.get(key).and_then(Value::as_str).map(str::trim) == Some(display_name))
}

fn friendship_matches_remote_node(
    friendship: &Friendship,
    remote_node_id: &str,
    bindings: &BTreeMap<String, ControllerBinding>,
    transport_bindings: &[RemoteTransportBinding],
) -> bool {
    friendship.remote_public_id == remote_node_id
        || bindings
            .get(&friendship.remote_public_id)
            .and_then(|binding| binding.controller_node_id.as_deref())
            == Some(remote_node_id)
        || transport_bindings.iter().any(|binding| {
            binding.public_id == friendship.remote_public_id
                && binding.transport_kind == TransportKind::Wattswarm
                && binding.transport_node_id == remote_node_id
        })
}

fn friendship_matches_agent_did(
    friendship: &Friendship,
    target_agent_did: &str,
    identities: &BTreeMap<String, PublicIdentity>,
    transport_bindings: &[RemoteTransportBinding],
) -> bool {
    identities
        .get(&friendship.remote_public_id)
        .and_then(|identity| identity.agent_did.as_deref())
        == Some(target_agent_did)
        || transport_bindings.iter().any(|binding| {
            binding.public_id == friendship.remote_public_id
                && binding.agent_did.as_deref() == Some(target_agent_did)
        })
}

fn target_from_active_friendship(
    friendship: &Friendship,
    identities: &BTreeMap<String, PublicIdentity>,
    bindings: &BTreeMap<String, ControllerBinding>,
    transport_bindings: &[RemoteTransportBinding],
) -> Result<SocialCounterpartTarget, String> {
    let transport_binding = transport_bindings.iter().find(|binding| {
        binding.public_id == friendship.remote_public_id
            && binding.transport_kind == TransportKind::Wattswarm
            && !binding.transport_node_id.trim().is_empty()
    });
    let remote_node = transport_binding
        .map(|binding| binding.transport_node_id.clone())
        .or_else(|| {
            bindings
                .get(&friendship.remote_public_id)
                .and_then(|binding| binding.controller_node_id.clone())
        })
        .ok_or_else(|| {
            format!(
                "remote node binding missing for active friend {}",
                friendship.remote_public_id
            )
        })?;
    let target_agent = identities
        .get(&friendship.remote_public_id)
        .and_then(|identity| identity.agent_did.clone())
        .or_else(|| transport_binding.and_then(|binding| binding.agent_did.clone()))
        .unwrap_or_else(|| friendship.remote_public_id.clone());
    Ok(SocialCounterpartTarget {
        counterpart_public_id: friendship.remote_public_id.clone(),
        remote_node,
        target_agent,
    })
}

async fn resolve_remove_agent_friend_counterpart(
    state: &ControlPlaneState,
    local_public_id: &str,
    body: &AgentRelationshipActionBody,
) -> Result<SocialCounterpartTarget, String> {
    let display_name = body
        .display_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let counterpart_public_id = body
        .counterpart_public_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let remote_node_id = body
        .remote_node_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let target_agent_did = body
        .target_agent_did
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());

    if display_name.is_none()
        && counterpart_public_id.is_none()
        && remote_node_id.is_none()
        && target_agent_did.is_none()
    {
        return Err(
            "display_name, remote_node_id, target_agent_did, or counterpart_public_id is required"
                .to_string(),
        );
    }

    let (identities, bindings) = load_social_identity_maps(state).await;
    let transport_bindings =
        transport_binding_service::list_transport_bindings(&*state.social_store)
            .map_err(|error| format!("query transport bindings: {error}"))?;
    let active_friendships =
        friendship_service::list_friendships(&*state.social_store, local_public_id)
            .map_err(|error| format!("query friendships: {error}"))?
            .into_iter()
            .filter(|friendship| friendship.state == FriendshipState::Active)
            .collect::<Vec<_>>();

    let matches = active_friendships
        .iter()
        .filter(|friendship| {
            display_name.is_some_and(|value| {
                friendship_display_name(friendship, &identities) == Some(value)
            }) || counterpart_public_id == Some(friendship.remote_public_id.as_str())
                || target_agent_did.is_some_and(|value| {
                    friendship_matches_agent_did(
                        friendship,
                        value,
                        &identities,
                        &transport_bindings,
                    )
                })
                || remote_node_id.is_some_and(|value| {
                    friendship_matches_remote_node(
                        friendship,
                        value,
                        &bindings,
                        &transport_bindings,
                    )
                })
        })
        .collect::<Vec<_>>();

    match matches.as_slice() {
        [friendship] => {
            target_from_active_friendship(friendship, &identities, &bindings, &transport_bindings)
        }
        [] => Err("active friendship not found for remove_agent_friend".to_string()),
        _ => Err("multiple active friends matched; provide counterpart_public_id".to_string()),
    }
}

async fn resolve_agent_relationship_counterpart(
    state: &ControlPlaneState,
    local_public_id: &str,
    body: &AgentRelationshipActionBody,
) -> Result<SocialCounterpartTarget, String> {
    if body.action == SwarmRelationshipAction::Remove {
        return resolve_remove_agent_friend_counterpart(state, local_public_id, body).await;
    }

    let remote_node_id = body
        .remote_node_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let target_agent_did = body
        .target_agent_did
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let counterpart_public_id = body
        .counterpart_public_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let display_name = body
        .display_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());

    match (
        target_agent_did,
        remote_node_id,
        counterpart_public_id,
        display_name,
    ) {
        (Some(target_agent_did), _, _, _) => {
            resolve_social_counterpart_target_by_agent_did(
                state,
                target_agent_did,
                counterpart_public_id.map(ToOwned::to_owned),
            )
            .await
        }
        (None, Some(remote_node_id), _, _) => resolve_social_counterpart_target_by_remote_node(
            state,
            remote_node_id,
            counterpart_public_id.map(ToOwned::to_owned),
        )
        .await
        .map_err(|error| error.to_string()),
        (None, None, Some(counterpart_public_id), _) => {
            resolve_agent_relationship_counterpart_by_public_id(state, counterpart_public_id).await
        }
        (None, None, None, Some(display_name)) => {
            resolve_agent_relationship_counterpart_by_display_name(state, display_name).await
        }
        (None, None, None, None) => Err(
            "display_name, remote_node_id, target_agent_did, or counterpart_public_id is required"
                .to_string(),
        ),
    }
}

async fn resolve_agent_relationship_counterpart_by_public_id(
    state: &ControlPlaneState,
    counterpart_public_id: &str,
) -> Result<SocialCounterpartTarget, String> {
    match resolve_social_counterpart_target(state, counterpart_public_id).await {
        Ok(target) => Ok(target),
        Err(local_error) => match state
            .swarm_bridge
            .resolve_agent_public_id(counterpart_public_id)
            .await
        {
            Ok(Some(discovered)) => Ok(SocialCounterpartTarget {
                counterpart_public_id: discovered.public_id,
                remote_node: discovered.remote_node_id,
                target_agent: discovered.target_agent_did,
            }),
            Ok(None) => Err(format!(
                "{local_error}; discovery record missing for {counterpart_public_id}"
            )),
            Err(_) => Err(local_error),
        },
    }
}

async fn resolve_agent_relationship_counterpart_by_display_name(
    state: &ControlPlaneState,
    display_name: &str,
) -> Result<SocialCounterpartTarget, String> {
    let discovered = state
        .swarm_bridge
        .search_agent_display_name(display_name)
        .await
        .map_err(|error| format!("agent discovery by display_name failed: {error}"))?;
    match discovered.as_slice() {
        [agent] => Ok(SocialCounterpartTarget {
            counterpart_public_id: agent.public_id.clone(),
            remote_node: agent.remote_node_id.clone(),
            target_agent: agent.target_agent_did.clone(),
        }),
        [] => Err(format!(
            "discovery record missing for display_name {display_name}"
        )),
        _ => Err(format!(
            "multiple discovery records matched display_name {display_name}; provide counterpart_public_id or remote_node_id"
        )),
    }
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
        display_name: local_display_name,
    } = resolve_social_local_context(&state, body.public_id.as_deref()).await;
    let counterpart =
        match resolve_agent_relationship_counterpart(&state, &local_public_id, &body).await {
            Ok(counterpart) => counterpart,
            Err(error) => {
                return (StatusCode::BAD_REQUEST, Json(json!({"error": error}))).into_response();
            }
        };
    let SocialCounterpartTarget {
        counterpart_public_id,
        remote_node,
        target_agent,
    } = counterpart;
    let request_counterpart_public_id = body
        .counterpart_public_id
        .unwrap_or_else(|| counterpart_public_id.clone());
    let capability = capability_for_relationship_action(&body.action).to_string();
    let now = Utc::now().timestamp();
    let mut base_message = body.message;
    if body.action == SwarmRelationshipAction::Request {
        if let Some(response) = friend_request_message_error(base_message.as_ref()) {
            return response;
        }
        match ensure_outbound_friend_request_allowed(
            &state,
            &local_public_id,
            &counterpart_public_id,
            &remote_node,
            now,
        )
        .await
        {
            Ok(gate) => {
                if let Some(response) = gate.denied_response {
                    return response;
                }
                base_message = message_with_request_id(base_message, gate.pending_request_id);
            }
            Err(error) => return internal_error(&error),
        }
    }
    let message = build_relationship_action_message(
        &local_public_id,
        &counterpart_public_id,
        &body.action,
        base_message,
        now,
    );
    let response_json = match send_signed_relationship_action_command(
        &state,
        SignedRelationshipActionArgs {
            local_agent_id,
            local_public_id: local_public_id.clone(),
            local_display_name,
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
            request_counterpart_public_id,
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
    let mut items = match build_agent_dm_threads_payload(&state, query.public_id.as_deref()).await {
        Ok(items) => items,
        Err(error) => return internal_error(&error),
    };
    if let Some(display_name) = normalized_display_name_filter(query.display_name.as_deref()) {
        items.retain(|item| payload_display_name_matches(item, display_name));
    }
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
    let mut items = match build_agent_dm_messages_payload(
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
    if let Some(display_name) = normalized_display_name_filter(query.display_name.as_deref()) {
        items.retain(|item| payload_display_name_matches(item, display_name));
    }
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

async fn resolve_dm_counterpart_public_id(
    state: &ControlPlaneState,
    local_public_id: &str,
    body: &AgentDmSendBody,
) -> Result<String, String> {
    let display_name = body
        .display_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let counterpart_public_id = body
        .counterpart_public_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());

    if let Some(display_name) = display_name {
        return resolve_dm_counterpart_public_id_by_display_name(
            state,
            local_public_id,
            display_name,
        )
        .await;
    }

    counterpart_public_id
        .map(ToOwned::to_owned)
        .ok_or_else(|| "display_name or counterpart_public_id is required".to_string())
}

async fn resolve_dm_counterpart_public_id_by_display_name(
    state: &ControlPlaneState,
    local_public_id: &str,
    display_name: &str,
) -> Result<String, String> {
    let display_name = display_name.trim();
    if display_name.is_empty() {
        return Err("display_name is required".to_string());
    }

    let (identities, _) = load_social_identity_maps(state).await;
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
            .map(|identity| identity.display_name.as_str());
        let name_matches = friendship_display_name(&friendship, &identities) == Some(display_name)
            || remote_display_name == Some(display_name);
        if name_matches {
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

async fn handle_send_agent_dm_message(
    state: ControlPlaneState,
    headers: HeaderMap,
    body: AgentDmSendBody,
    auth: String,
) -> Response {
    let (local, dm_counterpart_public_id, counterpart) =
        match resolve_agent_dm_send_context(&state, &body).await {
            Ok(context) => context,
            Err(response) => return response,
        };
    let SocialLocalContext {
        public_id: local_public_id,
        agent_id: local_agent_id,
        display_name: local_display_name,
    } = local;
    let SocialCounterpartTarget {
        counterpart_public_id,
        remote_node,
        target_agent,
    } = counterpart;
    let now = Utc::now().timestamp();
    let content = body.content.clone();
    let reply_to_message_id = normalized_reply_to_message_id(body.reply_to_message_id.as_deref());
    let thread_id = dm_thread_id_for_counterpart(&state, &local_public_id, &counterpart_public_id);
    match outbound_dm_reply_dedupe_response(
        &state,
        &headers,
        DmReplyDedupeResponseArgs {
            local_public_id: &local_public_id,
            counterpart_public_id: &counterpart_public_id,
            request_counterpart_public_id: &dm_counterpart_public_id,
            thread_id: &thread_id,
            content: &content,
            reply_to_message_id: reply_to_message_id.as_deref(),
        },
    ) {
        Ok(Some(response)) => return response,
        Ok(None) => {}
        Err(error) => return internal_error(&error),
    }
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
    let (message_id, message) = build_outbound_dm_message(
        &local_public_id,
        &counterpart_public_id,
        &thread_id,
        &content,
        reply_to_message_id.as_deref(),
        now,
    );
    let (response, agent_envelope_json, agent_signature) = match send_signed_direct_message_command(
        &state,
        SignedDirectMessageArgs {
            local_agent_id,
            local_public_id: local_public_id.clone(),
            local_display_name,
            target_agent_id: target_agent.clone(),
            remote_node_id: remote_node.clone(),
            content: content.clone(),
            message: message.clone(),
            extensions: body.extensions,
        },
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
            request_counterpart_public_id: dm_counterpart_public_id,
            content,
            reply_to_message_id,
            agent_envelope_json,
            agent_signature,
            response_json: response,
        },
    )
    .await
}

async fn resolve_agent_dm_send_context(
    state: &ControlPlaneState,
    body: &AgentDmSendBody,
) -> Result<(SocialLocalContext, String, SocialCounterpartTarget), Response> {
    let local = resolve_social_local_context(state, body.public_id.as_deref()).await;
    let dm_counterpart_public_id = resolve_dm_counterpart_public_id(state, &local.public_id, body)
        .await
        .map_err(|error| {
            (StatusCode::BAD_REQUEST, Json(json!({"error": error}))).into_response()
        })?;
    let counterpart = resolve_dm_counterpart_target(state, &dm_counterpart_public_id)
        .await
        .map_err(|error| {
            (StatusCode::BAD_REQUEST, Json(json!({"error": error}))).into_response()
        })?;
    Ok((local, dm_counterpart_public_id, counterpart))
}
