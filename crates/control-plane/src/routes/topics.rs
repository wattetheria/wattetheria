use axum::Json;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use serde::Serialize;
use serde_json::json;
use wattetheria_kernel::audit::AuditEntry;
use wattetheria_kernel::civilization::topics::TopicCreateSpec;

use crate::auth::{authorize, internal_error};
use crate::routes::identity::{identity_context_response, resolve_identity_context};
use crate::state::{
    ControlPlaneState, StreamEvent, TopicCreateBody, TopicMessageBody, TopicMessagesQuery,
    TopicSubscriptionBody, TopicsQuery,
};

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

pub(crate) async fn list_topics(
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
        query.projection_kind.as_ref(),
        query.organization_id.as_deref(),
        query.mission_id.as_deref(),
        query.include_inactive.unwrap_or(false),
    );
    items.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "topic".to_string(),
        action: "topic.list.query".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: None,
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({"count": items.len()})),
    });

    Json(json!({"topics": items})).into_response()
}

pub(crate) async fn create_topic(
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
    let context = resolve_identity_context(&state, body.public_id.as_deref(), None).await;
    let Some(public_id) = context.public_memory_owner.public.clone() else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "public identity required"})),
        )
            .into_response();
    };
    let controller_id = context.public_memory_owner.controller.clone();
    if let Err(error) = state
        .swarm_bridge
        .subscribe_topic(&controller_id, &body.feed_key, &body.scope_hint, true)
        .await
    {
        return internal_error(&error);
    }
    if let Some(initial_message) = body.initial_message.clone()
        && let Err(error) = state
            .swarm_bridge
            .post_topic_message(&body.feed_key, &body.scope_hint, initial_message, None)
            .await
    {
        return internal_error(&error);
    }

    let topic = {
        let mut topics = state.topic_registry.lock().await;
        let topic = topics.upsert_topic(TopicCreateSpec {
            feed_key: body.feed_key,
            scope_hint: body.scope_hint,
            display_name: body.display_name,
            summary: body.summary,
            projection_kind: body.projection_kind,
            organization_id: body.organization_id,
            mission_id: body.mission_id,
            participant_public_ids: body.participant_public_ids,
            created_by_public_id: public_id.clone(),
            why_this_exists: body.why_this_exists,
            active: true,
        });
        if let Err(error) = state.local_db.save_domain(
            wattetheria_kernel::local_db::domain::TOPIC_REGISTRY,
            &*topics,
        ) {
            return internal_error(&error);
        }
        topic
    };

    let payload = json!({"topic": topic.clone(), "public_id": public_id});
    let _ = state.stream_tx.send(StreamEvent {
        kind: "topic.created".to_string(),
        timestamp: Utc::now().timestamp(),
        payload: payload.clone(),
    });
    let _ = state.append_signed_event("CIVILIZATION_TOPIC_CREATED", payload.clone());
    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "topic".to_string(),
        action: "topic.create".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(topic.topic_id.clone()),
        capability: Some("civilization.topic.create".to_string()),
        reason: None,
        duration_ms: None,
        details: Some(payload),
    });

    Json(json!({
        "identity": identity_context_response(&context),
        "topic": topic,
    }))
    .into_response()
}

pub(crate) async fn topic_messages(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<TopicMessagesQuery>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    let messages = match state
        .swarm_bridge
        .list_topic_messages(
            &query.feed_key,
            &query.scope_hint,
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
        .topic_cursor(&query.feed_key, query.subscriber_id.as_deref())
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
        category: "topic".to_string(),
        action: "topic.messages.query".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(format!("{}@{}", query.feed_key, query.scope_hint)),
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({"count": views.len()})),
    });

    Json(json!({
        "feed_key": query.feed_key,
        "scope_hint": query.scope_hint,
        "cursor": cursor,
        "messages": views,
    }))
    .into_response()
}

pub(crate) async fn subscribe_topic(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<TopicSubscriptionBody>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let context = resolve_identity_context(&state, body.public_id.as_deref(), None).await;
    let controller_id = context.public_memory_owner.controller.clone();
    if let Err(error) = state
        .swarm_bridge
        .subscribe_topic(
            &controller_id,
            &body.feed_key,
            &body.scope_hint,
            body.active,
        )
        .await
    {
        return internal_error(&error);
    }

    let payload = json!({
        "controller_id": controller_id,
        "feed_key": body.feed_key,
        "scope_hint": body.scope_hint,
        "active": body.active,
    });
    let _ = state.stream_tx.send(StreamEvent {
        kind: "topic.subscription.updated".to_string(),
        timestamp: Utc::now().timestamp(),
        payload: payload.clone(),
    });
    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "topic".to_string(),
        action: "topic.subscription.update".to_string(),
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
    if let Err(error) = state
        .swarm_bridge
        .post_topic_message(
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
