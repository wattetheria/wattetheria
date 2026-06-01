use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use serde::Serialize;
use serde_json::{Value, json};
use wattetheria_kernel::audit::AuditEntry;
use wattetheria_kernel::civilization::topics::{HiveProfile, TopicCreateSpec, TopicProjectionKind};
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
    ControlPlaneState, HiveMessageBody, HiveMessagesQuery, HiveSubscriptionBody, StreamEvent,
    TopicCreateBody, TopicMessageBody, TopicsQuery,
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

fn context_agent_did(state: &ControlPlaneState, context: &IdentityContextView) -> String {
    context
        .public_memory_owner
        .agent_did
        .clone()
        .unwrap_or_else(|| state.agent_did.clone())
}

fn context_agent_display_name(state: &ControlPlaneState, context: &IdentityContextView) -> String {
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
) -> anyhow::Result<wattetheria_kernel::swarm_bridge::SwarmAgentEnvelope> {
    build_signed_agent_envelope_for_nodes(
        state,
        SignedAgentEnvelopeArgs {
            source_agent_id: context_agent_did(state, context),
            target_agent_id: None,
            source_node_id: state.swarm_bridge.local_node_id().await.ok(),
            target_node_id: None,
            capability: capability.to_owned(),
            message,
            extensions: None,
        },
    )
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
                envelope
                    .source_agent_card
                    .as_ref()
                    .and_then(|card| card.card.get("name"))
                    .and_then(Value::as_str)
                    .map(|name| name.strip_prefix("Wattetheria ").unwrap_or(name).to_owned())
            })
            .or_else(|| {
                envelope
                    .source_agent_id
                    .as_deref()
                    .map(default_agent_display_name)
            })
    })
}

fn hive_profile_payload(topic: &HiveProfile) -> Value {
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
    let (hive, network_id) = match resolve_hive_profile_with_route(
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

pub(crate) async fn post_topic_message(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<TopicMessageBody>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let context = resolve_identity_context(&state, body.public_id.as_deref(), None).await;
    let network_id = match resolve_network_id(&state, body.network_id.as_deref()).await {
        Ok(network_id) => network_id,
        Err(error) => return internal_error(&error),
    };
    let created_at = Utc::now();
    if let Err(error) = state
        .swarm_bridge
        .post_topic_message(
            Some(&network_id),
            &body.feed_key,
            &body.scope_hint,
            body.content.clone(),
            body.reply_to_message_id.clone(),
            None,
        )
        .await
    {
        return internal_error(&error);
    }

    let controller_id = context.public_memory_owner.controller.clone();
    let payload = json!({
        "controller_id": controller_id,
        "network_id": network_id,
        "feed_key": body.feed_key,
        "scope_hint": body.scope_hint,
        "reply_to_message_id": body.reply_to_message_id,
    });
    let _ = state.stream_tx.send(StreamEvent {
        kind: "topic.message.posted".to_string(),
        timestamp: Utc::now().timestamp(),
        payload: payload.clone(),
    });
    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "topic".to_string(),
        action: "topic.message.post".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: None,
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(payload),
    });
    let (actor_controller_id, actor_public_id, agent_identity) =
        contribution_actor(&state, &context);
    let source_id = format!(
        "topic:{}:{}:{}:{}",
        network_id,
        body.feed_key,
        controller_id,
        created_at.timestamp_millis()
    );
    if let Err(error) = record_contribution_event(
        &state,
        ContributionEventArgs {
            action_type: message_action_type(body.reply_to_message_id.as_deref(), "topic"),
            source_id: &source_id,
            controller_id: actor_controller_id,
            public_id: actor_public_id,
            agent_identity,
            receipt: json!({
                "network_id": network_id,
                "feed_key": body.feed_key,
                "scope_hint": body.scope_hint,
                "reply_to_message_id": body.reply_to_message_id,
            }),
        },
    )
    .await
    {
        return internal_error(&error);
    }

    Json(json!({"ok": true})).into_response()
}

pub(crate) async fn post_hive_message(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Path(hive_id): Path<String>,
    Json(body): Json<HiveMessageBody>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let context = resolve_identity_context(&state, body.public_id.as_deref(), None).await;
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
    if let Err(response) =
        post_hive_message_to_swarm(&state, &context, &hive, &network_id, &body).await
    {
        return response;
    }

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
        "topic_id": hive.topic_id,
        "feed_key": hive.feed_key,
        "scope_hint": hive.scope_hint,
        "content": body.content,
        "reply_to_message_id": body.reply_to_message_id,
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
        actor: Some(auth),
        subject: Some(hive_id),
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(payload),
    });
    let (actor_controller_id, actor_public_id, agent_identity) =
        contribution_actor(&state, &context);
    if let Err(error) = record_contribution_event(
        &state,
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
        return internal_error(&error);
    }

    Json(json!({"ok": true})).into_response()
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
        active: true,
    });
    state.local_db.save_domain(
        wattetheria_kernel::local_db::domain::HIVE_REGISTRY,
        &*topics,
    )?;
    Ok(profile)
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
