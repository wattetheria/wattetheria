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
use crate::social_host::{
    SignedAgentEnvelopeArgs, SocialCounterpartTarget, SocialLocalContext,
    build_signed_agent_envelope_for_nodes, capability_for_relationship_action,
    counterpart_public_id_for_remote_node, load_social_identity_maps,
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
    SwarmRelationshipActionCommand,
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
    envelope.source_node_id.as_deref() == Some(view.remote_node_id.as_str())
        || view.initiated_by == "remote"
}

fn relationship_remote_public_id(view: &SwarmPeerRelationshipView) -> Option<String> {
    let envelope = view.agent_envelope.as_ref()?;
    let key = if source_agent_card_is_remote(view, envelope) {
        "source_public_id"
    } else {
        "target_public_id"
    };
    envelope
        .message
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
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
    let envelope = view.agent_envelope.as_ref()?;
    if !source_agent_card_is_remote(view, envelope) {
        return None;
    }
    envelope.source_agent_card.as_ref().map(|card| &card.card)
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
    envelope
        .and_then(|envelope| envelope.message.get("text"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
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
    counterpart_label: &str,
) -> Value {
    let display_name = identities.get(&request.remote_public_id).map_or_else(
        || request.remote_public_id.clone(),
        |identity| identity.display_name.clone(),
    );
    let mut object = Map::new();
    object.insert(
        "request_id".to_string(),
        Value::String(request.request_id.clone()),
    );
    object.insert(counterpart_label.to_string(), Value::String(display_name));
    insert_payload_if_present(
        &mut object,
        "preview",
        envelope_message_text(view.and_then(|view| view.agent_envelope.as_ref()))
            .map(|text| Value::String(truncate_preview(&text))),
    );
    Value::Object(object)
}

fn friend_request_agent_payload(
    request: &FriendRequest,
    identities: &BTreeMap<String, PublicIdentity>,
    view: Option<&SwarmPeerRelationshipView>,
) -> Value {
    let identity = identities.get(&request.remote_public_id);
    let mut object = Map::new();
    object.insert(
        "public_id".to_string(),
        Value::String(request.remote_public_id.clone()),
    );
    insert_payload_if_present(
        &mut object,
        "display_name",
        identity.map(|identity| Value::String(identity.display_name.clone())),
    );
    insert_payload_if_present(
        &mut object,
        "agent_did",
        identity
            .and_then(|identity| identity.agent_did.clone())
            .or_else(|| {
                view.and_then(|view| view.agent_envelope.as_ref())
                    .and_then(|envelope| envelope.source_agent_id.clone())
            })
            .map(Value::String),
    );
    insert_payload_if_present(
        &mut object,
        "active",
        identity.map(|identity| Value::Bool(identity.active)),
    );
    Value::Object(object)
}

fn friend_request_message_payload(view: Option<&SwarmPeerRelationshipView>) -> Value {
    let mut object = Map::new();
    let Some(envelope) = view.and_then(|view| view.agent_envelope.as_ref()) else {
        return Value::Object(object);
    };
    for key in ["kind", "text", "sent_at"] {
        insert_payload_if_present(&mut object, key, envelope.message.get(key).cloned());
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
        let counterpart_public_id = relationship_remote_public_id(view)
            .or_else(|| counterpart_public_id_for_remote_node(bindings, &view.remote_node_id))
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
    let local_node_id = state.swarm_bridge.local_node_id().await.ok();
    let remote_node_id = args.remote_node_id;
    let agent_envelope = build_signed_agent_envelope_for_nodes(
        state,
        SignedAgentEnvelopeArgs {
            source_agent_id: args.local_agent_id,
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
    local_agent_id: String,
    target_agent_id: String,
    remote_node_id: String,
    content: Value,
    message: Value,
    extensions: Option<Value>,
) -> anyhow::Result<(Value, Value, Option<String>)> {
    let local_node_id = state.swarm_bridge.local_node_id().await.ok();
    let agent_envelope = build_signed_agent_envelope_for_nodes(
        state,
        SignedAgentEnvelopeArgs {
            source_agent_id: local_agent_id,
            target_agent_id: Some(target_agent_id),
            source_node_id: local_node_id,
            target_node_id: Some(remote_node_id.clone()),
            capability: "social.dm.send".to_string(),
            message,
            extensions,
        },
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

async fn ensure_outbound_friend_request_allowed(
    state: &ControlPlaneState,
    local_public_id: &str,
    counterpart_public_id: &str,
    remote_node_id: &str,
    now: i64,
) -> anyhow::Result<Option<Response>> {
    let (identities, bindings) = load_social_identity_maps(state).await;
    if let Ok(views) = state.swarm_bridge.list_peer_relationships().await {
        reconcile_swarm_relationship_views(state, local_public_id, &identities, &bindings, &views)?;
    }
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
            let mut payload =
                relationship_payload_from_friendship(friendship, &identities, &bindings);
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
    let (local_public_id, identities, friend_requests, relationship_views, _peers) =
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
    let (local_public_id, identities, friend_requests, relationship_views, _peers) =
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

async fn resolve_agent_relationship_counterpart(
    state: &ControlPlaneState,
    body: &AgentRelationshipActionBody,
) -> Result<SocialCounterpartTarget, String> {
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

    match (target_agent_did, remote_node_id, counterpart_public_id) {
        (Some(target_agent_did), _, _) => {
            resolve_social_counterpart_target_by_agent_did(
                state,
                target_agent_did,
                counterpart_public_id.map(ToOwned::to_owned),
            )
            .await
        }
        (None, Some(remote_node_id), _) => resolve_social_counterpart_target_by_remote_node(
            state,
            remote_node_id,
            counterpart_public_id.map(ToOwned::to_owned),
        )
        .await
        .map_err(|error| error.to_string()),
        (None, None, Some(counterpart_public_id)) => {
            resolve_social_counterpart_target(state, counterpart_public_id).await
        }
        (None, None, None) => Err(
            "remote_node_id, target_agent_did, or counterpart_public_id is required".to_string(),
        ),
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
    } = resolve_social_local_context(&state, body.public_id.as_deref()).await;
    let counterpart = match resolve_agent_relationship_counterpart(&state, &body).await {
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
    if body.action == SwarmRelationshipAction::Request {
        match ensure_outbound_friend_request_allowed(
            &state,
            &local_public_id,
            &counterpart_public_id,
            &remote_node,
            now,
        )
        .await
        {
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
    } = match resolve_dm_counterpart_target(&state, &body.counterpart_public_id).await {
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
