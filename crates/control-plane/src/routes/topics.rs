use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use serde::Serialize;
use serde_json::{Value, json};
use wattetheria_kernel::audit::AuditEntry;
use wattetheria_kernel::civilization::topics::{HiveProfile, TopicCreateSpec, TopicProjectionKind};
use wattetheria_kernel::swarm_bridge::{SwarmAgentEnvelope, SwarmPrivateHiveKeyShareCommand};
use wattetheria_social::application::{friendship_service, transport_binding_service};
use wattetheria_social::domain::friendships::FriendshipState;
use wattetheria_social::domain::transport_bindings::{RemoteTransportBinding, TransportKind};
use wattswarm_protocol::types::ScopeHint;

use crate::auth::{authorize, internal_error};
use crate::routes::identity::{
    IdentityContextView, identity_context_response, resolve_identity_context,
};
use crate::routes::reward_events::{
    ContributionEventArgs, contribution_actor, message_action_type, record_contribution_event,
};
use crate::social_host::{SignedAgentEnvelopeArgs, build_signed_agent_envelope_for_nodes};
use crate::state::{
    ControlPlaneState, HiveMessageBody, HiveMessagesQuery, HiveSubscriptionBody,
    PrivateHiveInviteBody, StreamEvent, TopicCreateBody, TopicsQuery,
};

const HIVE_SCOPE_HINT_ERROR: &str = "invalid scope_hint: expected global, region:<id>, node:<id>, local:<id>, or group:<id>; for Hives use group:<id>";

#[derive(Debug, Clone, Serialize)]
struct TopicMessageView {
    message_id: String,
    network_id: String,
    feed_key: String,
    scope_hint: String,
    author_node_id: String,
    author_public_id: Option<String>,
    author_display_name: Option<String>,
    content: serde_json::Value,
    reply_to_message_id: Option<String>,
    created_at: u64,
}

fn normalized_network_id(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn default_agent_display_name(agent_did: &str) -> String {
    let raw = agent_did.trim_start_matches("did:key:");
    let suffix = raw
        .chars()
        .rev()
        .take(8)
        .collect::<String>()
        .chars()
        .rev()
        .collect::<String>();
    format!("Agent-{suffix}")
}

pub(crate) fn context_agent_did(
    state: &ControlPlaneState,
    context: &IdentityContextView,
) -> String {
    context
        .public_memory_owner
        .agent_did
        .clone()
        .unwrap_or_else(|| state.agent_did.clone())
}

pub(crate) fn context_agent_display_name(
    state: &ControlPlaneState,
    context: &IdentityContextView,
) -> String {
    context.public_identity.as_ref().map_or_else(
        || default_agent_display_name(&context_agent_did(state, context)),
        |identity| identity.display_name.clone(),
    )
}

async fn hive_agent_envelope(
    state: &ControlPlaneState,
    context: &IdentityContextView,
    capability: &str,
    message: Value,
) -> anyhow::Result<SwarmAgentEnvelope> {
    build_signed_agent_envelope_for_nodes(
        state,
        SignedAgentEnvelopeArgs {
            source_agent_id: context_agent_did(state, context),
            source_display_name: Some(context_agent_display_name(state, context)),
            target_agent_id: None,
            source_node_id: state.swarm_bridge.local_node_id().await.ok(),
            target_node_id: None,
            capability: capability.to_owned(),
            message,
            extensions: None,
        },
    )
}

type PrivateHiveInviteResult<T> = Result<T, Box<Response>>;

fn private_hive_invite_json_error(status: StatusCode, body: Value) -> Response {
    (status, Json(body)).into_response()
}

fn is_private_hive_profile(hive: &HiveProfile) -> bool {
    hive.feed_key != "wattswarm.dm" && hive.scope_hint.starts_with("group:dm-")
}

async fn load_private_hive(
    state: &ControlPlaneState,
    hive_id: &str,
) -> PrivateHiveInviteResult<HiveProfile> {
    let Some(hive) = state.hive_registry.lock().await.get(hive_id) else {
        return Err(Box::new(private_hive_invite_json_error(
            StatusCode::NOT_FOUND,
            json!({"error": "hive not found"}),
        )));
    };
    if !hive.active {
        return Err(Box::new(private_hive_invite_json_error(
            StatusCode::FORBIDDEN,
            json!({
                "error": "hive subscription required",
                "hive_id": hive.topic_id,
                "message": "subscribe to this hive before inviting participants"
            }),
        )));
    }
    if !is_private_hive_profile(&hive) {
        return Err(Box::new(private_hive_invite_json_error(
            StatusCode::BAD_REQUEST,
            json!({"error": "hive is not a private hive"}),
        )));
    }
    Ok(hive)
}

fn counterpart_public_id(body: &PrivateHiveInviteBody) -> PrivateHiveInviteResult<String> {
    let public_id = body.counterpart_public_id.trim();
    if public_id.is_empty() {
        return Err(Box::new(private_hive_invite_json_error(
            StatusCode::BAD_REQUEST,
            json!({"error": "counterpart_public_id is required"}),
        )));
    }
    Ok(public_id.to_owned())
}

fn private_hive_invite_required_text(
    value: &str,
    field_name: &str,
) -> PrivateHiveInviteResult<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(Box::new(private_hive_invite_json_error(
            StatusCode::BAD_REQUEST,
            json!({"error": format!("{field_name} is required")}),
        )));
    }
    Ok(trimmed.to_owned())
}

fn private_hive_invite_text(display_name: &str, hive_name: &str, message: Option<&str>) -> String {
    let trimmed_message = message.map(str::trim).filter(|value| !value.is_empty());
    match trimmed_message {
        Some(message) => format!(
            "Hi {display_name}, you are invited to join the private Hive \"{hive_name}\". {message}"
        ),
        None => format!(
            "Hi {display_name}, you are invited to join the private Hive \"{hive_name}\". This encrypted message includes the private Hive key share so your node can unlock the Hive messages."
        ),
    }
}

fn private_hive_invite_fields(
    body: &PrivateHiveInviteBody,
) -> PrivateHiveInviteResult<(String, String, String, String)> {
    let counterpart_public_id = counterpart_public_id(body)?;
    let display_name = private_hive_invite_required_text(&body.display_name, "display_name")?;
    let hive_name = private_hive_invite_required_text(&body.hive_name, "hive_name")?;
    let invite_text = private_hive_invite_text(&display_name, &hive_name, body.message.as_deref());
    Ok((counterpart_public_id, display_name, hive_name, invite_text))
}

fn ensure_active_friend(
    state: &ControlPlaneState,
    local_public_id: &str,
    counterpart_public_id: &str,
) -> PrivateHiveInviteResult<()> {
    let friendships = friendship_service::list_friendships(&*state.social_store, local_public_id)
        .map_err(|error| Box::new(internal_error(&anyhow::anyhow!(error))))?;
    if friendships.iter().any(|friendship| {
        friendship.remote_public_id == counterpart_public_id
            && friendship.state == FriendshipState::Active
    }) {
        return Ok(());
    }
    Err(Box::new(private_hive_invite_json_error(
        StatusCode::BAD_REQUEST,
        json!({"error": "private hive invites require an active friend"}),
    )))
}

fn wattswarm_binding(
    state: &ControlPlaneState,
    counterpart_public_id: &str,
) -> PrivateHiveInviteResult<RemoteTransportBinding> {
    let transport_bindings =
        transport_binding_service::list_transport_bindings(&*state.social_store)
            .map_err(|error| Box::new(internal_error(&anyhow::anyhow!(error))))?;
    transport_bindings
        .into_iter()
        .find(|binding| {
            binding.public_id == counterpart_public_id
                && binding.transport_kind == TransportKind::Wattswarm
                && !binding.transport_node_id.trim().is_empty()
        })
        .ok_or_else(|| {
            Box::new(private_hive_invite_json_error(
                StatusCode::BAD_REQUEST,
                json!({"error": "remote Wattswarm node binding missing for active friend"}),
            ))
        })
}

async fn build_private_hive_invite_envelope(
    state: &ControlPlaneState,
    context: &IdentityContextView,
    hive: &HiveProfile,
    local_public_id: &str,
    counterpart_public_id: &str,
    transport_binding: &RemoteTransportBinding,
) -> PrivateHiveInviteResult<SwarmAgentEnvelope> {
    let target_agent_id = transport_binding
        .agent_did
        .clone()
        .unwrap_or_else(|| counterpart_public_id.to_owned());
    let message = json!({
        "action": "private_hive_invite",
        "hive_id": hive.topic_id,
        "feed_key": hive.feed_key,
        "scope_hint": hive.scope_hint,
        "source_public_id": local_public_id,
        "target_public_id": counterpart_public_id,
        "sent_at": Utc::now().timestamp(),
    });
    build_signed_agent_envelope_for_nodes(
        state,
        SignedAgentEnvelopeArgs {
            source_agent_id: context_agent_did(state, context),
            source_display_name: Some(context_agent_display_name(state, context)),
            target_agent_id: Some(target_agent_id),
            source_node_id: state.swarm_bridge.local_node_id().await.ok(),
            target_node_id: Some(transport_binding.transport_node_id.clone()),
            capability: "hive.private_key_share".to_owned(),
            message,
            extensions: None,
        },
    )
    .map_err(|error| Box::new(internal_error(&error)))
}

struct HiveEnvelopeRoute<'a> {
    hive_id: &'a str,
    network_id: &'a str,
    feed_key: &'a str,
    scope_hint: &'a str,
}

fn hive_envelope_message(
    state: &ControlPlaneState,
    context: &IdentityContextView,
    route: &HiveEnvelopeRoute<'_>,
    action: &str,
    extra: &Value,
) -> Value {
    json!({
        "action": action,
        "hive_id": route.hive_id,
        "topic_id": route.hive_id,
        "network_id": route.network_id,
        "feed_key": route.feed_key,
        "scope_hint": route.scope_hint,
        "author_agent_id": context_agent_did(state, context),
        "author_public_id": context.public_memory_owner.public.clone(),
        "author_controller_id": context.public_memory_owner.controller.clone(),
        "author_display_name": context_agent_display_name(state, context),
        "payload": extra,
    })
}

fn envelope_author_public_id(
    envelope: Option<&wattetheria_kernel::swarm_bridge::SwarmAgentEnvelope>,
) -> Option<String> {
    envelope.and_then(|envelope| {
        envelope
            .message
            .get("author_public_id")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .or_else(|| envelope.source_agent_id.clone())
    })
}

fn envelope_author_display_name(
    envelope: Option<&wattetheria_kernel::swarm_bridge::SwarmAgentEnvelope>,
) -> Option<String> {
    envelope.and_then(|envelope| {
        envelope
            .message
            .get("author_display_name")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .or_else(|| {
                envelope.source_agent_card.as_ref().and_then(|card| {
                    card.card
                        .get("metadata")
                        .and_then(|metadata| metadata.get("display_name"))
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned)
                        .or_else(|| {
                            card.card.get("name").and_then(Value::as_str).map(|name| {
                                name.strip_prefix("Wattetheria ").unwrap_or(name).to_owned()
                            })
                        })
                })
            })
            .or_else(|| {
                envelope
                    .source_agent_id
                    .as_deref()
                    .map(default_agent_display_name)
            })
    })
}

pub(crate) fn hive_profile_payload(topic: &HiveProfile) -> Value {
    let mut value = serde_json::to_value(topic).unwrap_or_else(|_| json!({}));
    if let Value::Object(object) = &mut value {
        object
            .entry("hive_id".to_string())
            .or_insert_with(|| Value::String(topic.topic_id.clone()));
    }
    value
}

fn attach_hive_creator_identity(
    mut value: Value,
    created_by_agent_identity: Option<&str>,
) -> Value {
    let Some(created_by_agent_identity) = created_by_agent_identity else {
        return value;
    };
    if let Value::Object(object) = &mut value {
        object
            .entry("created_by_agent_identity".to_string())
            .or_insert_with(|| Value::String(created_by_agent_identity.to_string()));
        object
            .entry("created_by_display_name".to_string())
            .or_insert_with(|| Value::String(created_by_agent_identity.to_string()));
    }
    value
}

fn invalid_hive_scope_hint_response(scope_hint: &str) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({
            "error": HIVE_SCOPE_HINT_ERROR,
            "field": "scope_hint",
            "received": scope_hint,
        })),
    )
        .into_response()
}

async fn resolve_network_id(
    state: &ControlPlaneState,
    requested: Option<&str>,
) -> anyhow::Result<String> {
    match normalized_network_id(requested) {
        Some(network_id) => Ok(network_id.to_owned()),
        None => state.swarm_bridge.current_network_id().await,
    }
}

pub(crate) async fn list_hives(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<TopicsQuery>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let topics = state.hive_registry.lock().await;
    let mut items = topics.list_filtered(
        normalized_network_id(query.network_id.as_deref()),
        query.projection_kind.as_ref(),
        query.organization_id.as_deref(),
        query.mission_id.as_deref(),
        query.include_inactive.unwrap_or(false),
    );
    items.sort_by_key(|item| std::cmp::Reverse(item.updated_at));

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "hive".to_string(),
        action: "hive.list.query".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: None,
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({"count": items.len()})),
    });

    let hives = items.iter().map(hive_profile_payload).collect::<Vec<_>>();

    Json(json!({"hives": hives})).into_response()
}

pub(crate) async fn create_hive(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<TopicCreateBody>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    if body.feed_key.trim().is_empty() || body.scope_hint.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "feed_key and scope_hint are required"})),
        )
            .into_response();
    }
    if ScopeHint::parse(&body.scope_hint).is_none() {
        return invalid_hive_scope_hint_response(&body.scope_hint);
    }
    let context = resolve_identity_context(&state, body.public_id.as_deref(), None).await;
    let Some(public_id) = context.public_memory_owner.public.clone() else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "public identity required"})),
        )
            .into_response();
    };
    let controller_id = context.public_memory_owner.controller.clone();
    let network_id = match resolve_network_id(&state, body.network_id.as_deref()).await {
        Ok(network_id) => network_id,
        Err(error) => return internal_error(&error),
    };
    let hive_id = format!("{}@{}@{}", network_id, body.feed_key, body.scope_hint);
    let route = HiveEnvelopeRoute {
        hive_id: &hive_id,
        network_id: &network_id,
        feed_key: &body.feed_key,
        scope_hint: &body.scope_hint,
    };
    if let Err(response) =
        subscribe_created_hive(&state, &context, &controller_id, &route, &body).await
    {
        return response;
    }
    if let Some(initial_message) = body.initial_message.clone()
        && let Err(response) =
            post_initial_hive_message(&state, &context, &route, initial_message).await
    {
        return response;
    }

    let topic = match persist_created_topic(&state, body, &public_id, &network_id).await {
        Ok(topic) => topic,
        Err(error) => return internal_error(&error),
    };

    let created_by_agent_identity = context
        .public_identity
        .as_ref()
        .map(|identity| identity.display_name.clone());
    let hive_payload = attach_hive_creator_identity(
        hive_profile_payload(&topic),
        created_by_agent_identity.as_deref(),
    );
    let payload = json!({"hive": hive_payload, "topic": topic.clone(), "public_id": public_id, "network_id": network_id});
    let _ = state.stream_tx.send(StreamEvent {
        kind: "topic.created".to_string(),
        timestamp: Utc::now().timestamp(),
        payload: payload.clone(),
    });
    let _ = state.append_signed_event("WATTETHERIA_HIVE_CREATED", payload.clone());
    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "hive".to_string(),
        action: "hive.create".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(topic.topic_id.clone()),
        capability: Some("wattetheria.hive.create".to_string()),
        reason: None,
        duration_ms: None,
        details: Some(payload),
    });
    if let Err(error) = record_hive_create_contribution(&state, &context, &topic, &network_id).await
    {
        return internal_error(&error);
    }

    Json(json!({
        "identity": identity_context_response(&context),
        "hive": attach_hive_creator_identity(
            hive_profile_payload(&topic),
            created_by_agent_identity.as_deref(),
        ),
    }))
    .into_response()
}

async fn record_hive_create_contribution(
    state: &ControlPlaneState,
    context: &IdentityContextView,
    topic: &HiveProfile,
    network_id: &str,
) -> anyhow::Result<()> {
    let (controller_id, actor_public_id, agent_identity) = contribution_actor(state, context);
    record_contribution_event(
        state,
        ContributionEventArgs {
            action_type: "hive.create",
            source_id: &topic.topic_id,
            controller_id,
            public_id: actor_public_id,
            agent_identity,
            receipt: json!({
                "hive_id": topic.topic_id,
                "network_id": network_id,
                "feed_key": topic.feed_key,
                "scope_hint": topic.scope_hint,
            }),
        },
    )
    .await?;
    Ok(())
}

async fn subscribe_created_hive(
    state: &ControlPlaneState,
    context: &IdentityContextView,
    controller_id: &str,
    route: &HiveEnvelopeRoute<'_>,
    body: &TopicCreateBody,
) -> Result<(), Response> {
    let create_agent_envelope = hive_agent_envelope(
        state,
        context,
        "hive.create",
        hive_envelope_message(
            state,
            context,
            route,
            "create",
            &json!({
                "display_name": body.display_name.clone(),
                "summary": body.summary.clone(),
                "projection_kind": body.projection_kind.clone(),
            }),
        ),
    )
    .await
    .map_err(|error| internal_error(&error))?;
    state
        .swarm_bridge
        .subscribe_topic(
            Some(route.network_id),
            controller_id,
            route.feed_key,
            route.scope_hint,
            true,
            Some(create_agent_envelope),
        )
        .await
        .map_err(|error| internal_error(&error))
}

async fn post_initial_hive_message(
    state: &ControlPlaneState,
    context: &IdentityContextView,
    route: &HiveEnvelopeRoute<'_>,
    initial_message: Value,
) -> Result<(), Response> {
    let agent_envelope = hive_agent_envelope(
        state,
        context,
        "hive.message.post",
        hive_envelope_message(
            state,
            context,
            route,
            "message.post",
            &json!({
                "content": initial_message.clone(),
                "reply_to_message_id": Value::Null,
            }),
        ),
    )
    .await
    .map_err(|error| internal_error(&error))?;
    state
        .swarm_bridge
        .post_topic_message(
            Some(route.network_id),
            route.feed_key,
            route.scope_hint,
            initial_message,
            None,
            Some(agent_envelope),
        )
        .await
        .map_err(|error| internal_error(&error))
}

async fn persist_created_topic(
    state: &ControlPlaneState,
    body: TopicCreateBody,
    public_id: &str,
    network_id: &str,
) -> anyhow::Result<HiveProfile> {
    let mut topics = state.hive_registry.lock().await;
    let public_geo = body.include_public_geo.then(|| state.public_geo_payload());
    let topic = topics.upsert_hive(TopicCreateSpec {
        network_id: Some(network_id.to_owned()),
        feed_key: body.feed_key,
        scope_hint: body.scope_hint,
        display_name: body.display_name,
        summary: body.summary,
        projection_kind: body.projection_kind,
        organization_id: body.organization_id,
        mission_id: body.mission_id,
        participant_public_ids: body.participant_public_ids,
        created_by_public_id: public_id.to_owned(),
        why_this_exists: body.why_this_exists,
        public_geo,
        active: true,
    });
    state.local_db.save_domain(
        wattetheria_kernel::local_db::domain::HIVE_REGISTRY,
        &*topics,
    )?;
    Ok(topic)
}

pub(crate) async fn hive_messages(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Path(hive_id): Path<String>,
    Query(query): Query<HiveMessagesQuery>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    let (hive, network_id) = match resolve_subscribed_hive_profile_with_route(
        &state,
        &hive_id,
        query.network_id.as_deref(),
        query.feed_key.as_deref(),
        query.scope_hint.as_deref(),
    )
    .await
    {
        Ok(resolved) => resolved,
        Err(response) => return response,
    };
    let messages = match state
        .swarm_bridge
        .list_topic_messages(
            Some(&network_id),
            &hive.feed_key,
            &hive.scope_hint,
            limit,
            query.before_created_at,
            query.before_message_id.clone(),
        )
        .await
    {
        Ok(messages) => messages,
        Err(error) => return internal_error(&error),
    };
    let cursor = match state
        .swarm_bridge
        .topic_cursor(
            Some(&network_id),
            &hive.feed_key,
            query.subscriber_id.as_deref(),
        )
        .await
    {
        Ok(cursor) => cursor,
        Err(error) => return internal_error(&error),
    };
    let public_identities = state.public_identity_registry.lock().await;
    let bindings = state.controller_binding_registry.lock().await;
    let views = messages
        .into_iter()
        .map(|message| {
            let binding = bindings.active_for_controller(&message.author_node_id);
            let public_identity = binding
                .as_ref()
                .and_then(|binding| public_identities.get(&binding.public_id));
            let envelope = message.agent_envelope.as_ref();
            TopicMessageView {
                message_id: message.message_id,
                network_id: message.network_id,
                feed_key: message.feed_key,
                scope_hint: message.scope_hint,
                author_node_id: message.author_node_id,
                author_public_id: binding
                    .as_ref()
                    .map(|binding| binding.public_id.clone())
                    .or_else(|| envelope_author_public_id(envelope)),
                author_display_name: public_identity
                    .as_ref()
                    .map(|identity| identity.display_name.clone())
                    .or_else(|| envelope_author_display_name(envelope)),
                content: message.content,
                reply_to_message_id: message.reply_to_message_id,
                created_at: message.created_at,
            }
        })
        .collect::<Vec<_>>();

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "hive".to_string(),
        action: "hive.messages.query".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(hive.topic_id.clone()),
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({"count": views.len()})),
    });

    Json(json!({
        "hive_id": hive.topic_id,
        "network_id": network_id,
        "feed_key": hive.feed_key,
        "scope_hint": hive.scope_hint,
        "cursor": cursor,
        "messages": views,
    }))
    .into_response()
}

pub(crate) async fn subscribe_hive(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Path(hive_id): Path<String>,
    Json(body): Json<HiveSubscriptionBody>,
) -> Response {
    update_hive_subscription(state, headers, hive_id, body, true).await
}

pub(crate) async fn unsubscribe_hive(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Path(hive_id): Path<String>,
    Json(body): Json<HiveSubscriptionBody>,
) -> Response {
    update_hive_subscription(state, headers, hive_id, body, false).await
}

pub(crate) async fn invite_private_hive_participant(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Path(hive_id): Path<String>,
    Json(body): Json<PrivateHiveInviteBody>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let context = resolve_identity_context(&state, body.public_id.as_deref(), None).await;
    let local_public_id = context
        .public_memory_owner
        .public
        .clone()
        .unwrap_or_else(|| context.public_memory_owner.controller.clone());
    let hive = match load_private_hive(&state, &hive_id).await {
        Ok(hive) => hive,
        Err(response) => return *response,
    };
    let (counterpart_public_id, display_name, hive_name, invite_text) =
        match private_hive_invite_fields(&body) {
            Ok(fields) => fields,
            Err(response) => return *response,
        };
    if let Err(response) = ensure_active_friend(&state, &local_public_id, &counterpart_public_id) {
        return *response;
    }
    let transport_binding = match wattswarm_binding(&state, &counterpart_public_id) {
        Ok(binding) => binding,
        Err(response) => return *response,
    };
    let agent_envelope = match build_private_hive_invite_envelope(
        &state,
        &context,
        &hive,
        &local_public_id,
        &counterpart_public_id,
        &transport_binding,
    )
    .await
    {
        Ok(envelope) => envelope,
        Err(response) => return *response,
    };
    let response = match state
        .swarm_bridge
        .share_private_hive_key(SwarmPrivateHiveKeyShareCommand {
            remote_node_id: transport_binding.transport_node_id.clone(),
            feed_key: hive.feed_key.clone(),
            scope_hint: hive.scope_hint.clone(),
            display_name: display_name.clone(),
            hive_name: hive_name.clone(),
            invite_text,
            agent_envelope,
        })
        .await
    {
        Ok(response) => response,
        Err(error) => return internal_error(&error),
    };
    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "hive".to_string(),
        action: "hive.private.invite".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(hive.topic_id.clone()),
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({
            "hive_id": hive.topic_id,
            "feed_key": hive.feed_key,
            "scope_hint": hive.scope_hint,
            "counterpart_public_id": counterpart_public_id,
            "display_name": display_name,
            "hive_name": hive_name,
            "remote_node_id": transport_binding.transport_node_id,
            "shared_secret_b64_redacted": true,
        })),
    });
    Json(json!({
        "ok": true,
        "hive_id": hive.topic_id,
        "feed_key": hive.feed_key,
        "scope_hint": hive.scope_hint,
        "counterpart_public_id": counterpart_public_id,
        "display_name": display_name,
        "hive_name": hive_name,
        "remote_node_id": response["remote_node_id"],
        "thread_id": response["thread_id"],
        "message_id": response["message_id"],
        "event_id": response["event_id"],
        "shared_secret_b64_redacted": true,
    }))
    .into_response()
}

async fn update_hive_subscription(
    state: ControlPlaneState,
    headers: HeaderMap,
    hive_id: String,
    body: HiveSubscriptionBody,
    active: bool,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let context = resolve_identity_context(&state, body.public_id.as_deref(), None).await;
    let controller_id = context.public_memory_owner.controller.clone();
    let (hive, network_id) = match resolve_hive_profile_with_route(
        &state,
        &hive_id,
        body.network_id.as_deref(),
        body.feed_key.as_deref(),
        body.scope_hint.as_deref(),
    )
    .await
    {
        Ok(resolved) => resolved,
        Err(response) => return response,
    };
    let capability = if active {
        "hive.subscribe"
    } else {
        "hive.unsubscribe"
    };
    let action = if active { "subscribe" } else { "unsubscribe" };
    let route = HiveEnvelopeRoute {
        hive_id: &hive.topic_id,
        network_id: &network_id,
        feed_key: &hive.feed_key,
        scope_hint: &hive.scope_hint,
    };
    let agent_envelope = match hive_agent_envelope(
        &state,
        &context,
        capability,
        hive_envelope_message(&state, &context, &route, action, &json!({"active": active})),
    )
    .await
    {
        Ok(envelope) => envelope,
        Err(error) => return internal_error(&error),
    };
    if let Err(error) = state
        .swarm_bridge
        .subscribe_topic(
            Some(&network_id),
            &controller_id,
            &hive.feed_key,
            &hive.scope_hint,
            active,
            Some(agent_envelope),
        )
        .await
    {
        return internal_error(&error);
    }
    if active
        && let Err(error) =
            persist_subscribed_hive_profile(&state, &hive, &body, &network_id, &context).await
    {
        return internal_error(&error);
    }
    if !active && let Err(error) = remove_subscribed_hive_profile(&state, &hive.topic_id).await {
        return internal_error(&error);
    }

    let hive_topic_id = hive.topic_id.clone();
    let payload = json!({
        "controller_id": controller_id,
        "network_id": network_id,
        "hive_id": hive_topic_id,
        "topic_id": hive.topic_id,
        "feed_key": hive.feed_key,
        "scope_hint": hive.scope_hint,
        "active": active,
    });
    let _ = state.stream_tx.send(StreamEvent {
        kind: "topic.subscription.updated".to_string(),
        timestamp: Utc::now().timestamp(),
        payload: payload.clone(),
    });
    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "hive".to_string(),
        action: "hive.subscription.update".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(hive_id),
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(payload),
    });

    Json(json!({"ok": true})).into_response()
}

pub(crate) async fn post_hive_message(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Path(hive_id): Path<String>,
    Json(body): Json<HiveMessageBody>,
) -> Response {
    post_hive_message_for_route(state, headers, Some(hive_id), body, true).await
}

pub(crate) async fn post_hive_topic_message(
    state: ControlPlaneState,
    headers: HeaderMap,
    hive_id: Option<String>,
    body: HiveMessageBody,
) -> Response {
    post_hive_message_for_route(state, headers, hive_id, body, true).await
}

async fn post_hive_message_for_route(
    state: ControlPlaneState,
    headers: HeaderMap,
    requested_hive_id: Option<String>,
    body: HiveMessageBody,
    require_subscription: bool,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let context = resolve_identity_context(&state, body.public_id.as_deref(), None).await;
    let hive_id = match requested_hive_id {
        Some(hive_id) => hive_id,
        None => resolve_hive_id_for_route(
            &state,
            body.network_id.as_deref(),
            body.feed_key.as_deref(),
            body.scope_hint.as_deref(),
        )
        .await
        .unwrap_or_else(|| {
            format!(
                "{}@{}@{}",
                body.network_id.as_deref().unwrap_or(""),
                body.feed_key.as_deref().unwrap_or(""),
                body.scope_hint.as_deref().unwrap_or("")
            )
        }),
    };
    let (hive, network_id) = match resolve_hive_profile_with_route(
        &state,
        &hive_id,
        body.network_id.as_deref(),
        body.feed_key.as_deref(),
        body.scope_hint.as_deref(),
    )
    .await
    {
        Ok(resolved) => resolved,
        Err(response) => return response,
    };
    let known_hive = state
        .hive_registry
        .lock()
        .await
        .get(&hive.topic_id)
        .is_some();
    if (require_subscription || known_hive)
        && let Err(response) = require_active_hive_subscription(&state, &hive).await
    {
        return response;
    }
    if let Err(response) =
        post_hive_message_to_swarm(&state, &context, &hive, &network_id, &body).await
    {
        return response;
    }
    if let Err(response) = record_hive_message_post_success(
        &state,
        &context,
        &auth,
        &hive_id,
        &hive,
        &network_id,
        &body,
    )
    .await
    {
        return response;
    }

    Json(json!({"ok": true})).into_response()
}

async fn resolve_hive_id_for_route(
    state: &ControlPlaneState,
    requested_network_id: Option<&str>,
    requested_feed_key: Option<&str>,
    requested_scope_hint: Option<&str>,
) -> Option<String> {
    let feed_key = normalized_network_id(requested_feed_key)?;
    let scope_hint = normalized_network_id(requested_scope_hint)?;
    let network_id = normalized_network_id(requested_network_id);
    let candidates = state
        .hive_registry
        .lock()
        .await
        .list()
        .into_iter()
        .filter(|hive| hive.feed_key == feed_key && hive.scope_hint == scope_hint)
        .collect::<Vec<_>>();
    candidates
        .iter()
        .find(|hive| {
            network_id.is_some_and(|network_id| {
                normalized_network_id(hive.network_id.as_deref()) == Some(network_id)
            })
        })
        .or_else(|| candidates.iter().find(|hive| hive.active))
        .or_else(|| candidates.first())
        .map(|hive| hive.topic_id.clone())
}

async fn resolve_subscribed_hive_profile_with_route(
    state: &ControlPlaneState,
    hive_id: &str,
    requested_network_id: Option<&str>,
    requested_feed_key: Option<&str>,
    requested_scope_hint: Option<&str>,
) -> Result<(HiveProfile, String), Response> {
    let resolved = resolve_hive_profile_with_route(
        state,
        hive_id,
        requested_network_id,
        requested_feed_key,
        requested_scope_hint,
    )
    .await?;
    require_active_hive_subscription(state, &resolved.0).await?;
    Ok(resolved)
}

async fn record_hive_message_post_success(
    state: &ControlPlaneState,
    context: &IdentityContextView,
    auth: &str,
    request_hive_id: &str,
    hive: &HiveProfile,
    network_id: &str,
    body: &HiveMessageBody,
) -> Result<(), Response> {
    let created_at = Utc::now();
    let controller_id = context.public_memory_owner.controller.clone();
    let author_public_id = context.public_memory_owner.public.clone();
    let author_agent_did = context.public_memory_owner.agent_did.clone();
    let author_agent_identity = context
        .public_identity
        .as_ref()
        .map(|identity| identity.display_name.clone());
    let message_id = format!(
        "{}:{}:{}",
        hive.topic_id,
        controller_id,
        created_at.timestamp_millis()
    );
    let hive_topic_id = hive.topic_id.clone();
    let payload = json!({
        "message_id": message_id,
        "controller_id": controller_id,
        "author_id": author_public_id.clone().unwrap_or_else(|| author_agent_did.clone().unwrap_or_else(|| state.agent_did.clone())),
        "author_agent_identity": author_agent_identity.clone(),
        "author_display_name": author_agent_identity,
        "author_public_id": author_public_id,
        "author_node_id": author_agent_did.unwrap_or_else(|| state.agent_did.clone()),
        "network_id": network_id,
        "hive_id": hive_topic_id,
        "topic_id": hive.topic_id.clone(),
        "feed_key": hive.feed_key.clone(),
        "scope_hint": hive.scope_hint.clone(),
        "content": body.content.clone(),
        "reply_to_message_id": body.reply_to_message_id.clone(),
        "created_at": created_at.timestamp(),
    });
    let _ = state.stream_tx.send(StreamEvent {
        kind: "topic.message.posted".to_string(),
        timestamp: Utc::now().timestamp(),
        payload: payload.clone(),
    });
    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "hive".to_string(),
        action: "hive.message.post".to_string(),
        status: "ok".to_string(),
        actor: Some(auth.to_owned()),
        subject: Some(request_hive_id.to_owned()),
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(payload),
    });
    let (actor_controller_id, actor_public_id, agent_identity) = contribution_actor(state, context);
    if let Err(error) = record_contribution_event(
        state,
        ContributionEventArgs {
            action_type: message_action_type(body.reply_to_message_id.as_deref(), "hive"),
            source_id: &message_id,
            controller_id: actor_controller_id,
            public_id: actor_public_id,
            agent_identity,
            receipt: json!({
                "message_id": message_id,
                "hive_id": hive_topic_id,
                "network_id": network_id,
                "feed_key": hive.feed_key,
                "scope_hint": hive.scope_hint,
                "reply_to_message_id": body.reply_to_message_id,
            }),
        },
    )
    .await
    {
        return Err(internal_error(&error));
    }

    Ok(())
}

async fn post_hive_message_to_swarm(
    state: &ControlPlaneState,
    context: &IdentityContextView,
    hive: &HiveProfile,
    network_id: &str,
    body: &HiveMessageBody,
) -> Result<(), Response> {
    let route = HiveEnvelopeRoute {
        hive_id: &hive.topic_id,
        network_id,
        feed_key: &hive.feed_key,
        scope_hint: &hive.scope_hint,
    };
    let agent_envelope = hive_agent_envelope(
        state,
        context,
        "hive.message.post",
        hive_envelope_message(
            state,
            context,
            &route,
            "message.post",
            &json!({
                "content": body.content.clone(),
                "reply_to_message_id": body.reply_to_message_id.clone(),
            }),
        ),
    )
    .await
    .map_err(|error| internal_error(&error))?;
    state
        .swarm_bridge
        .post_topic_message(
            Some(network_id),
            &hive.feed_key,
            &hive.scope_hint,
            body.content.clone(),
            body.reply_to_message_id.clone(),
            Some(agent_envelope),
        )
        .await
        .map_err(|error| internal_error(&error))
}

async fn require_active_hive_subscription(
    state: &ControlPlaneState,
    hive: &HiveProfile,
) -> Result<(), Response> {
    let subscribed = state
        .hive_registry
        .lock()
        .await
        .get(&hive.topic_id)
        .is_some_and(|profile| profile.active);
    if subscribed {
        return Ok(());
    }
    Err((
        StatusCode::FORBIDDEN,
        Json(json!({
            "error": "hive subscription required",
            "hive_id": hive.topic_id,
            "message": "subscribe to this hive before posting messages"
        })),
    )
        .into_response())
}

async fn persist_subscribed_hive_profile(
    state: &ControlPlaneState,
    hive: &HiveProfile,
    body: &HiveSubscriptionBody,
    network_id: &str,
    context: &crate::routes::identity::IdentityContextView,
) -> anyhow::Result<HiveProfile> {
    let created_by_public_id = normalized_network_id(Some(hive.created_by_public_id.as_str()))
        .or(context.public_memory_owner.public.as_deref())
        .unwrap_or(&context.public_memory_owner.controller)
        .to_owned();
    let mut topics = state.hive_registry.lock().await;
    let profile = topics.upsert_hive(TopicCreateSpec {
        network_id: Some(network_id.to_owned()),
        feed_key: hive.feed_key.clone(),
        scope_hint: hive.scope_hint.clone(),
        display_name: normalized_owned(body.display_name.as_deref())
            .unwrap_or_else(|| hive.display_name.clone()),
        summary: normalized_owned(body.summary.as_deref()).or_else(|| hive.summary.clone()),
        projection_kind: body
            .projection_kind
            .clone()
            .unwrap_or_else(|| hive.projection_kind.clone()),
        organization_id: normalized_owned(body.organization_id.as_deref())
            .or_else(|| hive.organization_id.clone()),
        mission_id: normalized_owned(body.mission_id.as_deref())
            .or_else(|| hive.mission_id.clone()),
        participant_public_ids: hive.participant_public_ids.clone(),
        created_by_public_id,
        why_this_exists: normalized_owned(body.why_this_exists.as_deref())
            .or_else(|| hive.why_this_exists.clone()),
        public_geo: None,
        active: true,
    });
    state.local_db.save_domain(
        wattetheria_kernel::local_db::domain::HIVE_REGISTRY,
        &*topics,
    )?;
    Ok(profile)
}

async fn remove_subscribed_hive_profile(
    state: &ControlPlaneState,
    hive_id: &str,
) -> anyhow::Result<Option<HiveProfile>> {
    let mut topics = state.hive_registry.lock().await;
    let removed = topics.remove_hive(hive_id);
    state.local_db.save_domain(
        wattetheria_kernel::local_db::domain::HIVE_REGISTRY,
        &*topics,
    )?;
    Ok(removed)
}

fn normalized_owned(value: Option<&str>) -> Option<String> {
    normalized_network_id(value).map(ToOwned::to_owned)
}

async fn resolve_hive_profile_with_route(
    state: &ControlPlaneState,
    hive_id: &str,
    requested_network_id: Option<&str>,
    requested_feed_key: Option<&str>,
    requested_scope_hint: Option<&str>,
) -> Result<(HiveProfile, String), Response> {
    let Some(hive) = state.hive_registry.lock().await.get(hive_id) else {
        if let (Some(feed_key), Some(scope_hint)) = (
            normalized_network_id(requested_feed_key),
            normalized_network_id(requested_scope_hint),
        ) {
            if ScopeHint::parse(scope_hint).is_none() {
                return Err(invalid_hive_scope_hint_response(scope_hint));
            }
            let network_id = match resolve_network_id(state, requested_network_id).await {
                Ok(network_id) => network_id,
                Err(error) => return Err(internal_error(&error)),
            };
            let now = Utc::now().timestamp();
            return Ok((
                HiveProfile {
                    topic_id: hive_id.to_owned(),
                    network_id: Some(network_id.clone()),
                    feed_key: feed_key.to_owned(),
                    scope_hint: scope_hint.to_owned(),
                    display_name: hive_id.to_owned(),
                    summary: None,
                    projection_kind: TopicProjectionKind::ChatRoom,
                    organization_id: None,
                    mission_id: None,
                    participant_public_ids: Vec::new(),
                    created_by_public_id: String::new(),
                    why_this_exists: None,
                    lat: None,
                    lng: None,
                    coordinate_source: None,
                    active: true,
                    created_at: now,
                    updated_at: now,
                },
                network_id,
            ));
        }
        return Err((
            StatusCode::NOT_FOUND,
            Json(json!({"error": "hive not found", "hive_id": hive_id})),
        )
            .into_response());
    };
    let network_id = match hive
        .network_id
        .as_deref()
        .or_else(|| normalized_network_id(requested_network_id))
    {
        Some(network_id) => network_id.to_owned(),
        None => resolve_network_id(state, None)
            .await
            .map_err(|error| internal_error(&error))?,
    };
    Ok((hive, network_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wattetheria_kernel::swarm_bridge::{SwarmAgentEnvelope, SwarmSourceAgentCard};

    #[test]
    fn envelope_author_display_name_prefers_metadata_display_name() {
        let envelope = SwarmAgentEnvelope {
            protocol: "google_a2a".to_owned(),
            transport_profile: None,
            source_agent_id: None,
            target_agent_id: None,
            source_node_id: None,
            target_node_id: None,
            capability: None,
            source_agent_card: Some(SwarmSourceAgentCard {
                agent_id: "did:key:agent".to_owned(),
                node_id: None,
                card_hash: "sha256:test".to_owned(),
                issued_at: 1,
                card: json!({
                    "name": "Wattetheria Agent Legacy",
                    "metadata": {
                        "display_name": "Wattetheria Labs"
                    }
                }),
                signature: None,
            }),
            message: json!({}),
            extensions: None,
            signature: None,
        };

        assert_eq!(
            envelope_author_display_name(Some(&envelope)).as_deref(),
            Some("Wattetheria Labs")
        );
    }
}
