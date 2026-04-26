use axum::Json;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use serde_json::{Value, json};
use wattetheria_kernel::audit::AuditEntry;
use wattetheria_kernel::civilization::topics::{TopicProfile, TopicProjectionKind};
use wattetheria_kernel::relationships::RelationshipKind;
use wattetheria_kernel::swarm_sync::{SwarmTaskRunProjectionSnapshot, SwarmTopicActivitySnapshot};

use crate::auth::{authorize, internal_error};
use crate::routes::civilization::{
    build_agent_dm_messages_payload, build_agent_dm_threads_payload,
};
use crate::routes::identity::resolve_identity_context;
use crate::state::{ClientIdentityQuery, ClientListQuery, ControlPlaneState, TopicMessagesQuery};
use crate::swarm_sync::{load_cached_task_run_projection, load_cached_topic_activity};

#[derive(Debug, Clone, Serialize)]
struct ClientTopicMessageView {
    message_id: String,
    author_node_id: String,
    author_public_id: Option<String>,
    author_display_name: Option<String>,
    content: Value,
    text_preview: Option<String>,
    reply_to_message_id: Option<String>,
    created_at: u64,
}

#[derive(Debug, Clone, Serialize)]
struct ClientHiveView {
    topic_id: String,
    feed_key: String,
    scope_hint: String,
    display_name: String,
    projection_kind: TopicProjectionKind,
    organization_id: Option<String>,
    mission_id: Option<String>,
    summary: Option<String>,
    status: &'static str,
    member_count: usize,
    mission_count: usize,
    recent_message_count: usize,
    last_message_text: Option<String>,
    last_message_at: Option<u64>,
    last_message_author: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct ClientConversationView {
    dm_thread_id: String,
    counterpart_public_id: String,
    counterpart_display_name: Option<String>,
    counterpart_status: &'static str,
    session_state: Option<String>,
    last_message_text: Option<String>,
    last_message_at: Option<u64>,
    unread_count: usize,
}

#[derive(Debug, Clone, Serialize)]
struct ClientFriendView {
    public_id: String,
    display_name: Option<String>,
    relationship_kind: RelationshipKind,
    status: &'static str,
    active: bool,
    has_dm_thread: bool,
    dm_thread_id: Option<String>,
    last_message_text: Option<String>,
    last_message_at: Option<u64>,
}

fn is_hive_topic(kind: &TopicProjectionKind) -> bool {
    matches!(
        kind,
        TopicProjectionKind::ChatRoom
            | TopicProjectionKind::WorkingGroup
            | TopicProjectionKind::Guild
            | TopicProjectionKind::Organization
            | TopicProjectionKind::MissionThread
    )
}

fn message_text_preview(content: &Value) -> Option<String> {
    content
        .get("text")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| content.get("content").and_then(message_text_preview))
        .or_else(|| content.as_str().map(ToOwned::to_owned))
}

fn latest_message(
    activity: &SwarmTopicActivitySnapshot,
) -> Option<&wattetheria_kernel::swarm_bridge::SwarmTopicMessageView> {
    activity
        .messages
        .iter()
        .max_by_key(|message| message.created_at)
}

async fn topic_activity_or_empty(
    state: &ControlPlaneState,
    topic: &TopicProfile,
) -> Option<SwarmTopicActivitySnapshot> {
    if let Some(snapshot) =
        load_cached_topic_activity(&state.local_db, &topic.feed_key, &topic.scope_hint).await
    {
        return Some(snapshot);
    }
    state
        .swarm_bridge
        .topic_activity_snapshot(&topic.feed_key, &topic.scope_hint, 25, None)
        .await
        .ok()
}

fn topic_message_view(
    _state: &ControlPlaneState,
    public_id_by_controller: &std::collections::BTreeMap<String, (Option<String>, Option<String>)>,
    message: wattetheria_kernel::swarm_bridge::SwarmTopicMessageView,
) -> ClientTopicMessageView {
    let (author_public_id, author_display_name) = public_id_by_controller
        .get(&message.author_node_id)
        .cloned()
        .unwrap_or((None, None));
    ClientTopicMessageView {
        message_id: message.message_id,
        author_node_id: message.author_node_id,
        author_public_id,
        author_display_name,
        text_preview: message_text_preview(&message.content),
        content: message.content,
        reply_to_message_id: message.reply_to_message_id,
        created_at: message.created_at,
    }
}

async fn author_lookup(
    state: &ControlPlaneState,
) -> std::collections::BTreeMap<String, (Option<String>, Option<String>)> {
    let bindings = state.controller_binding_registry.lock().await.list();
    let identities = state.public_identity_registry.lock().await.list();
    let identity_by_public_id = identities
        .into_iter()
        .map(|identity| (identity.public_id.clone(), identity))
        .collect::<std::collections::BTreeMap<_, _>>();
    bindings
        .into_iter()
        .map(|binding| {
            let public_identity = identity_by_public_id.get(&binding.public_id);
            (
                binding
                    .controller_node_id
                    .clone()
                    .unwrap_or_else(|| binding.public_id.clone()),
                (
                    Some(binding.public_id.clone()),
                    public_identity.map(|identity| identity.display_name.clone()),
                ),
            )
        })
        .collect()
}

fn latest_dm_messages_by_thread(messages: &[Value]) -> std::collections::BTreeMap<String, Value> {
    let mut latest = std::collections::BTreeMap::<String, Value>::new();
    for message in messages {
        let Some(thread_id) = message.get("thread_id").and_then(Value::as_str) else {
            continue;
        };
        let created_at = message
            .get("created_at")
            .and_then(Value::as_u64)
            .unwrap_or_default();
        let replace = latest
            .get(thread_id)
            .and_then(|existing| existing.get("created_at").and_then(Value::as_u64))
            .is_none_or(|existing_created_at| created_at > existing_created_at);
        if replace {
            latest.insert(thread_id.to_string(), message.clone());
        }
    }
    latest
}

fn dm_threads_by_counterpart(threads: &[Value]) -> std::collections::BTreeMap<String, Value> {
    let mut indexed = std::collections::BTreeMap::<String, Value>::new();
    for thread in threads {
        let Some(counterpart_public_id) =
            thread.get("counterpart_public_id").and_then(Value::as_str)
        else {
            continue;
        };
        let updated_at = thread
            .get("updated_at")
            .and_then(Value::as_u64)
            .unwrap_or_default();
        let replace = indexed
            .get(counterpart_public_id)
            .and_then(|existing| existing.get("updated_at").and_then(Value::as_u64))
            .is_none_or(|existing_updated_at| updated_at > existing_updated_at);
        if replace {
            indexed.insert(counterpart_public_id.to_string(), thread.clone());
        }
    }
    indexed
}

fn peer_status(
    state: &ControlPlaneState,
    peer_ids: &std::collections::BTreeSet<String>,
    remote_node_id: Option<&str>,
) -> &'static str {
    remote_node_id.map_or("unknown", |controller_id| {
        if controller_id == state.agent_did || peer_ids.contains(controller_id) {
            "online"
        } else {
            "offline"
        }
    })
}

pub(crate) async fn build_hives_payload(
    state: &ControlPlaneState,
    limit: usize,
) -> anyhow::Result<Vec<Value>> {
    let topics = state.topic_registry.lock().await.list();
    let organizations = state.organization_registry.lock().await;
    let missions = state.mission_board.lock().await.list(None);
    let mut items = Vec::new();

    for topic in topics
        .into_iter()
        .filter(|topic| topic.active && is_hive_topic(&topic.projection_kind))
        .take(limit)
    {
        let activity = topic_activity_or_empty(state, &topic).await;
        let latest = activity.as_ref().and_then(latest_message);
        let member_count = topic
            .organization_id
            .as_deref()
            .map_or(0, |organization_id| {
                organizations
                    .memberships(organization_id)
                    .into_iter()
                    .filter(|membership| membership.active)
                    .count()
            });
        let mission_count = topic
            .organization_id
            .as_deref()
            .map_or(0, |organization_id| {
                missions
                    .iter()
                    .filter(|mission| mission.publisher == organization_id)
                    .count()
            });
        items.push(json!(ClientHiveView {
            topic_id: topic.topic_id,
            feed_key: topic.feed_key,
            scope_hint: topic.scope_hint,
            display_name: topic.display_name,
            projection_kind: topic.projection_kind,
            organization_id: topic.organization_id,
            mission_id: topic.mission_id,
            summary: topic.summary,
            status: if activity.is_some() { "active" } else { "idle" },
            member_count,
            mission_count,
            recent_message_count: activity
                .as_ref()
                .map_or(0, |activity| activity.messages.len()),
            last_message_text: latest.and_then(|message| message_text_preview(&message.content)),
            last_message_at: latest.map(|message| message.created_at),
            last_message_author: latest.map(|message| message.author_node_id.clone()),
        }));
    }

    items.sort_by(|left, right| {
        right["last_message_at"]
            .as_u64()
            .unwrap_or_default()
            .cmp(&left["last_message_at"].as_u64().unwrap_or_default())
            .then_with(|| {
                left["topic_id"]
                    .as_str()
                    .unwrap_or_default()
                    .cmp(right["topic_id"].as_str().unwrap_or_default())
            })
    });
    Ok(items)
}

pub(crate) async fn build_public_topic_messages_snapshot_payload(
    state: &ControlPlaneState,
    limit: usize,
) -> anyhow::Result<Vec<Value>> {
    let topics = state.topic_registry.lock().await.list();
    let author_lookup = author_lookup(state).await;
    let mut items = Vec::new();
    for topic in topics.into_iter().filter(|topic| topic.active).take(limit) {
        let Some(activity) = topic_activity_or_empty(state, &topic).await else {
            continue;
        };
        items.extend(
            activity
                .messages
                .into_iter()
                .take(limit)
                .map(|message| {
                    serde_json::to_value(topic_message_view(state, &author_lookup, message))
                })
                .collect::<Result<Vec<_>, _>>()?,
        );
    }
    items.sort_by(|left, right| {
        right["created_at"]
            .as_u64()
            .unwrap_or_default()
            .cmp(&left["created_at"].as_u64().unwrap_or_default())
            .then_with(|| {
                left["message_id"]
                    .as_str()
                    .unwrap_or_default()
                    .cmp(right["message_id"].as_str().unwrap_or_default())
            })
    });
    items.dedup_by(|left, right| left["message_id"] == right["message_id"]);
    items.truncate(limit);
    Ok(items)
}

async fn build_conversations_payload(
    state: &ControlPlaneState,
    query: &ClientIdentityQuery,
    limit: usize,
) -> anyhow::Result<Vec<Value>> {
    let peer_ids = state
        .swarm_bridge
        .peers()
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|peer| peer.node_id)
        .collect::<std::collections::BTreeSet<_>>();
    let threads = build_agent_dm_threads_payload(state, query.public_id.as_deref())
        .await
        .unwrap_or_default();
    let messages =
        build_agent_dm_messages_payload(state, query.public_id.as_deref(), None, None, 200)
            .await
            .unwrap_or_default();
    let latest_by_thread = latest_dm_messages_by_thread(&messages);
    let mut items = Vec::new();

    for thread in threads.into_iter().take(limit) {
        let Some(thread_id) = thread.get("thread_id").and_then(Value::as_str) else {
            continue;
        };
        let Some(counterpart_public_id) =
            thread.get("counterpart_public_id").and_then(Value::as_str)
        else {
            continue;
        };
        let latest = latest_by_thread.get(thread_id);
        items.push(json!(ClientConversationView {
            dm_thread_id: thread_id.to_string(),
            counterpart_public_id: counterpart_public_id.to_string(),
            counterpart_display_name: thread
                .get("counterpart_display_name")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            counterpart_status: peer_status(
                state,
                &peer_ids,
                thread.get("remote_node_id").and_then(Value::as_str),
            ),
            session_state: thread
                .get("session_state")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            last_message_text: latest.and_then(message_text_preview),
            last_message_at: latest
                .and_then(|message| message.get("created_at").and_then(Value::as_u64))
                .or_else(|| thread.get("last_message_at").and_then(Value::as_u64)),
            unread_count: 0,
        }));
    }

    items.sort_by(|left, right| {
        right["last_message_at"]
            .as_u64()
            .unwrap_or_default()
            .cmp(&left["last_message_at"].as_u64().unwrap_or_default())
            .then_with(|| {
                left["dm_thread_id"]
                    .as_str()
                    .unwrap_or_default()
                    .cmp(right["dm_thread_id"].as_str().unwrap_or_default())
            })
    });
    Ok(items)
}

async fn build_friends_payload(
    state: &ControlPlaneState,
    query: &ClientIdentityQuery,
    limit: usize,
) -> anyhow::Result<Vec<Value>> {
    let context = resolve_identity_context(
        state,
        query.public_id.as_deref(),
        query.agent_did.as_deref(),
    )
    .await;
    let local_public_id = context.public_identity.as_ref().map_or_else(
        || context.public_memory_owner.controller.clone(),
        |identity| identity.public_id.clone(),
    );
    let public_identities = state.public_identity_registry.lock().await.list();
    let identity_by_public_id = public_identities
        .into_iter()
        .map(|identity| (identity.public_id.clone(), identity))
        .collect::<std::collections::BTreeMap<_, _>>();
    let peer_ids = state
        .swarm_bridge
        .peers()
        .await
        .unwrap_or_default()
        .into_iter()
        .map(|peer| peer.node_id)
        .collect::<std::collections::BTreeSet<_>>();
    let bindings = state.controller_binding_registry.lock().await.list();
    let binding_by_public_id = bindings
        .into_iter()
        .map(|binding| (binding.public_id.clone(), binding))
        .collect::<std::collections::BTreeMap<_, _>>();
    let relationships = state
        .relationship_registry
        .lock()
        .await
        .list_for_public(&local_public_id);
    let dm_threads = build_agent_dm_threads_payload(state, query.public_id.as_deref())
        .await
        .unwrap_or_default();
    let dm_threads_by_counterpart = dm_threads_by_counterpart(&dm_threads);
    let dm_messages =
        build_agent_dm_messages_payload(state, query.public_id.as_deref(), None, None, 200)
            .await
            .unwrap_or_default();
    let latest_by_thread = latest_dm_messages_by_thread(&dm_messages);

    let mut items = Vec::new();
    for edge in relationships.into_iter().take(limit) {
        let dm_thread = dm_threads_by_counterpart.get(&edge.counterpart_public_id);
        let latest = dm_thread
            .and_then(|thread| thread.get("thread_id").and_then(Value::as_str))
            .and_then(|thread_id| latest_by_thread.get(thread_id));
        let display_name = identity_by_public_id
            .get(&edge.counterpart_public_id)
            .map(|identity| identity.display_name.clone());
        let status = binding_by_public_id
            .get(&edge.counterpart_public_id)
            .and_then(|binding| binding.controller_node_id.as_deref())
            .map_or("unknown", |controller_id| {
                if controller_id == state.agent_did || peer_ids.contains(controller_id) {
                    "online"
                } else {
                    "offline"
                }
            });
        items.push(json!(ClientFriendView {
            public_id: edge.counterpart_public_id.clone(),
            display_name,
            relationship_kind: edge.kind,
            status,
            active: edge.active,
            has_dm_thread: dm_thread.is_some(),
            dm_thread_id: dm_thread
                .and_then(|thread| thread.get("thread_id").and_then(Value::as_str))
                .map(ToOwned::to_owned),
            last_message_text: latest.and_then(message_text_preview),
            last_message_at: latest
                .and_then(|message| message.get("created_at").and_then(Value::as_u64))
                .or_else(|| {
                    dm_thread
                        .and_then(|thread| thread.get("last_message_at").and_then(Value::as_u64))
                }),
        }));
    }

    items.sort_by(|left, right| {
        right["last_message_at"]
            .as_u64()
            .unwrap_or_default()
            .cmp(&left["last_message_at"].as_u64().unwrap_or_default())
            .then_with(|| {
                left["public_id"]
                    .as_str()
                    .unwrap_or_default()
                    .cmp(right["public_id"].as_str().unwrap_or_default())
            })
    });
    Ok(items)
}

pub(crate) async fn build_task_activity_payload(
    state: &ControlPlaneState,
    limit: usize,
) -> anyhow::Result<Value> {
    let snapshot = load_task_run_projection_snapshot(state, limit).await?;
    Ok(json!({
        "generated_at": snapshot.generated_at,
        "tasks": snapshot.recent_tasks,
        "runs": snapshot.recent_runs,
    }))
}

async fn load_task_run_projection_snapshot(
    state: &ControlPlaneState,
    limit: usize,
) -> anyhow::Result<SwarmTaskRunProjectionSnapshot> {
    if let Some(snapshot) = load_cached_task_run_projection(&state.local_db).await {
        Ok(snapshot)
    } else {
        state.swarm_bridge.task_run_projection(limit, limit).await
    }
}

pub(crate) async fn client_hives(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<ClientListQuery>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    let payload = match build_hives_payload(&state, limit).await {
        Ok(payload) => payload,
        Err(error) => return internal_error(&error),
    };

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "client".to_string(),
        action: "client.hives.query".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: None,
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({"count": payload.len()})),
    });

    Json(Value::Array(payload)).into_response()
}

pub(crate) async fn client_topic_messages(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<TopicMessagesQuery>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    let cached_snapshot = if query.before_created_at.is_none() && query.before_message_id.is_none()
    {
        load_cached_topic_activity(&state.local_db, &query.feed_key, &query.scope_hint).await
    } else {
        None
    };
    let (messages, cursor) = if let Some(snapshot) = cached_snapshot {
        (
            snapshot
                .messages
                .into_iter()
                .take(limit)
                .collect::<Vec<_>>(),
            snapshot.cursor,
        )
    } else {
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
        (messages, cursor)
    };
    let author_lookup = author_lookup(&state).await;
    let views = messages
        .into_iter()
        .map(|message| topic_message_view(&state, &author_lookup, message))
        .collect::<Vec<_>>();

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "client".to_string(),
        action: "client.topic_messages.query".to_string(),
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

pub(crate) async fn client_conversations(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<ClientIdentityQuery>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let payload = match build_conversations_payload(&state, &query, 100).await {
        Ok(payload) => payload,
        Err(error) => return internal_error(&error),
    };

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "client".to_string(),
        action: "client.conversations.query".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: None,
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({"count": payload.len()})),
    });

    Json(Value::Array(payload)).into_response()
}

pub(crate) async fn client_friends(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<ClientIdentityQuery>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let payload = match build_friends_payload(&state, &query, 100).await {
        Ok(payload) => payload,
        Err(error) => return internal_error(&error),
    };

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "client".to_string(),
        action: "client.friends.query".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: None,
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({"count": payload.len()})),
    });

    Json(Value::Array(payload)).into_response()
}

pub(crate) async fn client_task_activity(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<ClientListQuery>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    let payload = match build_task_activity_payload(&state, limit).await {
        Ok(payload) => payload,
        Err(error) => return internal_error(&error),
    };

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "client".to_string(),
        action: "client.task_activity.query".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: None,
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({"task_count": payload["tasks"].as_array().map_or(0, Vec::len)})),
    });

    Json(payload).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_preview_prefers_text_field() {
        assert_eq!(
            message_text_preview(&json!({"text": "hello"})),
            Some("hello".to_string())
        );
        assert_eq!(
            message_text_preview(&json!({"content": {"text": "nested"}})),
            Some("nested".to_string())
        );
        assert_eq!(
            message_text_preview(&json!("fallback")),
            Some("fallback".to_string())
        );
        assert_eq!(message_text_preview(&json!({})), None);
    }

    #[test]
    fn latest_dm_messages_are_indexed_by_thread() {
        let messages = vec![
            json!({"thread_id": "dm-1", "message_id": "old", "created_at": 10}),
            json!({"thread_id": "dm-1", "message_id": "new", "created_at": 20}),
            json!({"thread_id": "dm-2", "message_id": "other", "created_at": 15}),
        ];

        let indexed = latest_dm_messages_by_thread(&messages);

        assert_eq!(indexed["dm-1"]["message_id"].as_str(), Some("new"));
        assert_eq!(indexed["dm-2"]["message_id"].as_str(), Some("other"));
    }

    #[test]
    fn dm_threads_are_indexed_by_counterpart() {
        let threads = vec![
            json!({"counterpart_public_id": "friend", "thread_id": "old", "updated_at": 10}),
            json!({"counterpart_public_id": "friend", "thread_id": "new", "updated_at": 20}),
        ];

        let indexed = dm_threads_by_counterpart(&threads);

        assert_eq!(indexed["friend"]["thread_id"].as_str(), Some("new"));
    }
}
