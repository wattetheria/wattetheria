use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use serde::Serialize;
use serde_json::{Value, json};
use wattetheria_kernel::audit::AuditEntry;
use wattetheria_kernel::civilization::topics::{TopicCreateSpec, TopicProfile};
use wattswarm_protocol::types::ScopeHint;

use crate::auth::{authorize, internal_error};
use crate::routes::identity::{identity_context_response, resolve_identity_context};
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

fn hive_profile_payload(topic: &TopicProfile) -> Value {
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
    let topics = state.topic_registry.lock().await;
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
    if let Err(error) = state
        .swarm_bridge
        .subscribe_topic(
            Some(&network_id),
            &controller_id,
            &body.feed_key,
            &body.scope_hint,
            true,
        )
        .await
    {
        return internal_error(&error);
    }
    if let Some(initial_message) = body.initial_message.clone()
        && let Err(error) = state
            .swarm_bridge
            .post_topic_message(
                Some(&network_id),
                &body.feed_key,
                &body.scope_hint,
                initial_message,
                None,
            )
            .await
    {
        return internal_error(&error);
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

    Json(json!({
        "identity": identity_context_response(&context),
        "hive": attach_hive_creator_identity(
            hive_profile_payload(&topic),
            created_by_agent_identity.as_deref(),
        ),
    }))
    .into_response()
}

async fn persist_created_topic(
    state: &ControlPlaneState,
    body: TopicCreateBody,
    public_id: &str,
    network_id: &str,
) -> anyhow::Result<TopicProfile> {
    let mut topics = state.topic_registry.lock().await;
    let topic = topics.upsert_topic(TopicCreateSpec {
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
        wattetheria_kernel::local_db::domain::TOPIC_REGISTRY,
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
    let (hive, network_id) =
        match resolve_hive_profile(&state, &hive_id, query.network_id.as_deref()).await {
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
            TopicMessageView {
                message_id: message.message_id,
                network_id: message.network_id,
                feed_key: message.feed_key,
                scope_hint: message.scope_hint,
                author_node_id: message.author_node_id,
                author_public_id: binding.as_ref().map(|binding| binding.public_id.clone()),
                author_display_name: public_identity
                    .as_ref()
                    .map(|identity| identity.display_name.clone()),
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
    let (hive, network_id) =
        match resolve_hive_profile(&state, &hive_id, body.network_id.as_deref()).await {
            Ok(resolved) => resolved,
            Err(response) => return response,
        };
    if let Err(error) = state
        .swarm_bridge
        .subscribe_topic(
            Some(&network_id),
            &controller_id,
            &hive.feed_key,
            &hive.scope_hint,
            active,
        )
        .await
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
    if let Err(error) = state
        .swarm_bridge
        .post_topic_message(
            Some(&network_id),
            &body.feed_key,
            &body.scope_hint,
            body.content.clone(),
            body.reply_to_message_id.clone(),
        )
        .await
    {
        return internal_error(&error);
    }

    let payload = json!({
        "controller_id": context.public_memory_owner.controller,
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
    let (hive, network_id) =
        match resolve_hive_profile(&state, &hive_id, body.network_id.as_deref()).await {
            Ok(resolved) => resolved,
            Err(response) => return response,
        };
    if let Err(error) = state
        .swarm_bridge
        .post_topic_message(
            Some(&network_id),
            &hive.feed_key,
            &hive.scope_hint,
            body.content.clone(),
            body.reply_to_message_id.clone(),
        )
        .await
    {
        return internal_error(&error);
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

    Json(json!({"ok": true})).into_response()
}

async fn resolve_hive_profile(
    state: &ControlPlaneState,
    hive_id: &str,
    requested_network_id: Option<&str>,
) -> Result<(TopicProfile, String), Response> {
    let Some(hive) = state.topic_registry.lock().await.get(hive_id) else {
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
