use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{Value, json};
use wattetheria_kernel::civilization::topics::{
    TopicCreateSpec, TopicProjectionKind, topic_id_for,
};
use wattetheria_kernel::local_db::domain::HIVE_REGISTRY;
use wattetheria_kernel::swarm_bridge::SwarmTopicMessageView;
use wattswarm_protocol::types::ScopeHint;

use crate::auth::{authorize, internal_error};
use crate::routes::identity::{IdentityContextView, resolve_identity_context};
use crate::routes::topics::{context_agent_did, context_agent_display_name};
use crate::social_host::{
    SignedAgentEnvelopeArgs, build_signed_agent_envelope_for_nodes, public_agent_id,
};
use crate::state::ControlPlaneState;

#[path = "board_view.rs"]
mod board_view;

use board_view::{
    board_topic_message_views, published_service_agents_payload, record_board_message_post,
};
pub(crate) use board_view::{build_public_board_messages_snapshot_payload, client_board};

pub(crate) const BOARD_GENERAL_FEED_KEY: &str = "wattetheria.board.general";
pub(crate) const BOARD_TRADE_FEED_KEY: &str = "wattetheria.board.trade";
pub(crate) const BOARD_SEARCH_FEED_KEY: &str = "wattetheria.board.search";
pub(crate) const BOARD_REQUEST_FEED_KEY: &str = "wattetheria.board.request";
pub(crate) const SERVICES_BOARD_FEED_KEY: &str = "wattetheria.board.services";
pub(crate) const SERVICES_BOARD_SCOPE_HINT: &str = "group:board-services";
pub(crate) const BOARD_MESSAGE_MAX_CHARS: usize = 2_000;

#[derive(Debug, Clone, Copy)]
struct BoardChannelDefinition {
    category: &'static str,
    feed_key: &'static str,
    group: &'static str,
    display_name: &'static str,
}

const BOARD_CHANNELS: [BoardChannelDefinition; 4] = [
    BoardChannelDefinition {
        category: "general",
        feed_key: BOARD_GENERAL_FEED_KEY,
        group: "board-general",
        display_name: "General",
    },
    BoardChannelDefinition {
        category: "trade",
        feed_key: BOARD_TRADE_FEED_KEY,
        group: "board-trade",
        display_name: "Trade",
    },
    BoardChannelDefinition {
        category: "search",
        feed_key: BOARD_SEARCH_FEED_KEY,
        group: "board-search",
        display_name: "Search",
    },
    BoardChannelDefinition {
        category: "request",
        feed_key: BOARD_REQUEST_FEED_KEY,
        group: "board-request",
        display_name: "Request",
    },
];

#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct BoardPageCursor {
    pub before_created_at: Option<u64>,
    pub before_message_id: Option<String>,
    pub has_more: Option<bool>,
}

#[derive(Debug, Deserialize, Default)]
pub(crate) struct BoardMessagesQuery {
    pub category: Option<String>,
    pub source: Option<String>,
    pub search: Option<String>,
    pub limit: Option<usize>,
    pub before_created_at: Option<u64>,
    pub before_message_id: Option<String>,
    pub subscriber_id: Option<String>,
    pub network_cursors: Option<String>,
    pub global_before_sequence: Option<u64>,
}

#[derive(Debug, Deserialize, Default)]
pub(crate) struct ServiceBoardMessagesQuery {
    pub service_name: String,
    pub limit: Option<usize>,
    pub before_created_at: Option<u64>,
    pub before_message_id: Option<String>,
    pub subscriber_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct BoardMessageBody {
    #[serde(default)]
    pub public_id: Option<String>,
    pub category: String,
    pub message: Value,
    #[serde(default)]
    pub reply_to_message_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ServiceBoardMessageBody {
    #[serde(default)]
    pub public_id: Option<String>,
    pub acting_as: String,
    pub message: Value,
    #[serde(default)]
    pub reply_to_message_id: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub(crate) struct BoardSubscriptionBody {
    #[serde(default)]
    pub public_id: Option<String>,
}

fn board_message_char_count(message: &Value) -> usize {
    match message {
        Value::String(value) => value.chars().count(),
        Value::Array(items) => items.iter().fold(0, |count, item| {
            count.saturating_add(board_message_char_count(item))
        }),
        Value::Object(object) => object.iter().fold(0, |count, (key, value)| {
            count
                .saturating_add(key.chars().count())
                .saturating_add(board_message_char_count(value))
        }),
        Value::Null | Value::Bool(_) | Value::Number(_) => 0,
    }
}

fn text_contains_disallowed_control_chars(value: &str) -> bool {
    value
        .chars()
        .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
}

fn board_message_contains_disallowed_control_chars(message: &Value) -> bool {
    match message {
        Value::String(value) => text_contains_disallowed_control_chars(value),
        Value::Array(items) => items
            .iter()
            .any(board_message_contains_disallowed_control_chars),
        Value::Object(object) => object.iter().any(|(key, value)| {
            text_contains_disallowed_control_chars(key)
                || board_message_contains_disallowed_control_chars(value)
        }),
        Value::Null | Value::Bool(_) | Value::Number(_) => false,
    }
}

fn board_message_error(message: &Value) -> Option<Response> {
    let actual_chars = board_message_char_count(message);
    if actual_chars > BOARD_MESSAGE_MAX_CHARS {
        return Some(
            (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": format!(
                        "board message must be at most {BOARD_MESSAGE_MAX_CHARS} characters"
                    ),
                    "max_chars": BOARD_MESSAGE_MAX_CHARS,
                    "actual_chars": actual_chars,
                })),
            )
                .into_response(),
        );
    }
    if board_message_contains_disallowed_control_chars(message) {
        return Some(
            (
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": "board message contains unsupported control characters",
                })),
            )
                .into_response(),
        );
    }
    None
}

#[derive(Clone, Copy)]
struct BoardEnvelopeRoute<'a> {
    capability: &'a str,
    action: &'a str,
    network_id: &'a str,
    feed_key: &'a str,
    scope_hint: &'a str,
}

pub(crate) struct BoardMessagePost<'a> {
    pub message_id: &'a str,
    pub author_node_id: &'a str,
    pub author_public_id: Option<&'a str>,
    pub author_display_name: Option<&'a str>,
    pub source: &'a str,
    pub category: &'a str,
    pub network_id: &'a str,
    pub feed_key: &'a str,
    pub scope_hint: &'a str,
    pub content: Value,
    pub reply_to_message_id: Option<String>,
}

fn bad_request(message: impl Into<String>) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error": message.into()})),
    )
        .into_response()
}

fn board_channel(category: &str) -> Option<&'static BoardChannelDefinition> {
    BOARD_CHANNELS
        .iter()
        .find(|channel| channel.category == category)
}

fn normalized_category(category: &str) -> Option<&'static BoardChannelDefinition> {
    board_channel(category.trim())
}

fn service_name_is_valid(service_name: &str) -> bool {
    let service_name = service_name.trim();
    !service_name.is_empty()
        && service_name.len() <= 64
        && service_name
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && !service_name.starts_with('-')
        && !service_name.ends_with('-')
}

pub(crate) fn services_board_route() -> (&'static str, &'static str) {
    (SERVICES_BOARD_FEED_KEY, SERVICES_BOARD_SCOPE_HINT)
}

async fn resolve_network_id(
    state: &ControlPlaneState,
    requested: Option<&str>,
) -> anyhow::Result<String> {
    if let Some(network_id) = requested.map(str::trim).filter(|value| !value.is_empty()) {
        return Ok(network_id.to_owned());
    }
    state.swarm_bridge.current_network_id().await
}

fn context_public_id(context: &IdentityContextView) -> Option<String> {
    context
        .public_memory_owner
        .public
        .as_deref()
        .and_then(public_agent_id)
}

fn board_envelope(
    state: &ControlPlaneState,
    context: &IdentityContextView,
    route: BoardEnvelopeRoute<'_>,
    payload: &Value,
) -> anyhow::Result<wattetheria_kernel::swarm_bridge::SwarmAgentEnvelope> {
    build_signed_agent_envelope_for_nodes(
        state,
        SignedAgentEnvelopeArgs {
            source_agent_id: context_agent_did(state, context),
            source_public_id: context_public_id(context),
            source_display_name: Some(context_agent_display_name(state, context)),
            target_agent_id: None,
            source_node_id: None,
            target_node_id: None,
            capability: route.capability.to_owned(),
            message: json!({
                "action": route.action,
                "network_id": route.network_id,
                "feed_key": route.feed_key,
                "scope_hint": route.scope_hint,
                "author_agent_id": context_agent_did(state, context),
                "author_public_id": context.public_memory_owner.public,
                "author_controller_id": context.public_memory_owner.controller,
                "author_display_name": context_agent_display_name(state, context),
                "payload": payload,
            }),
            extensions: None,
        },
    )
}

fn created_by_public_id(context: &IdentityContextView) -> String {
    context
        .public_memory_owner
        .public
        .clone()
        .unwrap_or_else(|| context.public_memory_owner.controller.clone())
}

async fn persist_board_subscription(
    state: &ControlPlaneState,
    context: &IdentityContextView,
    network_id: &str,
    feed_key: &str,
    scope_hint: &str,
    display_name: &str,
    active: bool,
) -> anyhow::Result<()> {
    let topic_id = topic_id_for(Some(network_id), feed_key, scope_hint);
    let mut registry = state.hive_registry.lock().await;
    if active {
        registry.upsert_hive(TopicCreateSpec {
            network_id: Some(network_id.to_owned()),
            feed_key: feed_key.to_owned(),
            scope_hint: scope_hint.to_owned(),
            display_name: display_name.to_owned(),
            summary: Some("Wattetheria Message Board channel".to_owned()),
            projection_kind: TopicProjectionKind::ChatRoom,
            organization_id: None,
            mission_id: None,
            participant_public_ids: Vec::new(),
            created_by_public_id: created_by_public_id(context),
            why_this_exists: Some("Message Board subscription".to_owned()),
            public_geo: None,
            active: true,
        });
    } else {
        registry.remove_hive(&topic_id);
    }
    state.local_db.save_domain(HIVE_REGISTRY, &*registry)?;
    Ok(())
}

fn local_board_subscription(
    registry: &wattetheria_kernel::civilization::topics::HiveRegistry,
    network_id: &str,
    feed_key: &str,
    scope_hint: &str,
) -> bool {
    registry
        .get(&topic_id_for(Some(network_id), feed_key, scope_hint))
        .is_some_and(|profile| profile.active)
}

async fn set_board_subscription(
    state: &ControlPlaneState,
    context: &IdentityContextView,
    network_id: &str,
    feed_key: &str,
    scope_hint: &str,
    display_name: &str,
    active: bool,
) -> anyhow::Result<()> {
    if ScopeHint::parse(scope_hint).is_none() {
        anyhow::bail!("invalid board scope_hint: {scope_hint}");
    }
    let subscriber_node_id = state.swarm_bridge.local_node_id().await?;
    let envelope = board_envelope(
        state,
        context,
        BoardEnvelopeRoute {
            capability: if active {
                "board.channel.subscribe"
            } else {
                "board.channel.unsubscribe"
            },
            action: if active { "subscribe" } else { "unsubscribe" },
            network_id,
            feed_key,
            scope_hint,
        },
        &json!({"active": active}),
    )?;
    state
        .swarm_bridge
        .subscribe_topic(
            Some(network_id),
            &subscriber_node_id,
            feed_key,
            scope_hint,
            active,
            Some(envelope),
        )
        .await?;
    persist_board_subscription(
        state,
        context,
        network_id,
        feed_key,
        scope_hint,
        display_name,
        active,
    )
    .await
}

async fn ensure_board_subscription(
    state: &ControlPlaneState,
    context: &IdentityContextView,
    network_id: &str,
    feed_key: &str,
    scope_hint: &str,
    display_name: &str,
) -> anyhow::Result<()> {
    let already_subscribed = {
        let registry = state.hive_registry.lock().await;
        local_board_subscription(&registry, network_id, feed_key, scope_hint)
    };
    if !already_subscribed {
        set_board_subscription(
            state,
            context,
            network_id,
            feed_key,
            scope_hint,
            display_name,
            true,
        )
        .await?;
    }
    Ok(())
}

pub(crate) async fn ensure_startup_board_subscriptions(
    state: &ControlPlaneState,
) -> anyhow::Result<()> {
    let context = resolve_identity_context(state, None, None).await;
    let network_id = resolve_network_id(state, None).await?;
    let general = &BOARD_CHANNELS[0];
    ensure_board_subscription(
        state,
        &context,
        &network_id,
        general.feed_key,
        &format!("group:{}", general.group),
        general.display_name,
    )
    .await?;

    if state.swarm_bridge.public_bootstrap().await? {
        for channel in BOARD_CHANNELS.iter().skip(1) {
            ensure_board_subscription(
                state,
                &context,
                &network_id,
                channel.feed_key,
                &format!("group:{}", channel.group),
                channel.display_name,
            )
            .await?;
        }
        let (feed_key, scope_hint) = services_board_route();
        ensure_board_subscription(
            state,
            &context,
            &network_id,
            feed_key,
            scope_hint,
            "Services",
        )
        .await?;
    }
    Ok(())
}

async fn board_channel_payloads(
    state: &ControlPlaneState,
    context: &IdentityContextView,
    network_id: &str,
    ensure_default_general: bool,
) -> anyhow::Result<Vec<Value>> {
    if ensure_default_general {
        let general = &BOARD_CHANNELS[0];
        ensure_board_subscription(
            state,
            context,
            network_id,
            general.feed_key,
            &format!("group:{}", general.group),
            general.display_name,
        )
        .await?;
    }

    let registry = state.hive_registry.lock().await;
    Ok(BOARD_CHANNELS
        .iter()
        .map(|channel| {
            let scope_hint = format!("group:{}", channel.group);
            json!({
                "category": channel.category,
                "channel": channel.group,
                "display_name": channel.display_name,
                "network_id": network_id,
                "feed_key": channel.feed_key,
                "scope_hint": scope_hint,
                "gossip_kind": "messages",
                "subscribed": local_board_subscription(&registry, network_id, channel.feed_key, &scope_hint),
                "default_subscribed": channel.category == "general",
            })
        })
        .collect())
}

pub(crate) async fn list_board_channels(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, &headers).await {
        return response;
    }
    let context = resolve_identity_context(&state, None, None).await;
    let network_id = match resolve_network_id(&state, None).await {
        Ok(network_id) => network_id,
        Err(error) => return internal_error(&error),
    };
    let channels = match board_channel_payloads(&state, &context, &network_id, true).await {
        Ok(channels) => channels,
        Err(error) => return internal_error(&error),
    };
    Json(json!({"network_id": network_id, "channels": channels})).into_response()
}

pub(crate) async fn list_board_messages(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<BoardMessagesQuery>,
) -> Response {
    if let Err(response) = authorize(&state, &headers).await {
        return response;
    }
    let Some(category) = query.category.as_deref().and_then(normalized_category) else {
        return bad_request("category must be one of general, trade, search, request");
    };
    let network_id = match resolve_network_id(&state, None).await {
        Ok(network_id) => network_id,
        Err(error) => return internal_error(&error),
    };
    let scope_hint = format!("group:{}", category.group);
    let feed_key = category.feed_key;
    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    let messages = match state
        .swarm_bridge
        .list_topic_messages(
            Some(&network_id),
            feed_key,
            &scope_hint,
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
        .topic_cursor(Some(&network_id), feed_key, query.subscriber_id.as_deref())
        .await
    {
        Ok(cursor) => cursor,
        Err(error) => return internal_error(&error),
    };
    let messages = board_topic_message_views(&state, &messages, Some(category.category)).await;
    Json(json!({
        "network_id": network_id,
        "category": category.category,
        "channel": category.group,
        "feed_key": feed_key,
        "scope_hint": scope_hint,
        "cursor": cursor,
        "messages": messages,
    }))
    .into_response()
}

pub(crate) async fn publish_board_message(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<BoardMessageBody>,
) -> Response {
    if let Err(response) = authorize(&state, &headers).await {
        return response;
    }
    let Some(category) = normalized_category(&body.category) else {
        return bad_request("category must be one of general, trade, search, request");
    };
    if body.message.is_null() {
        return bad_request("message is required");
    }
    if let Some(response) = board_message_error(&body.message) {
        return response;
    }
    let context = resolve_identity_context(&state, body.public_id.as_deref(), None).await;
    let network_id = match resolve_network_id(&state, None).await {
        Ok(network_id) => network_id,
        Err(error) => return internal_error(&error),
    };
    let scope_hint = format!("group:{}", category.group);
    let author_node_id = match state.swarm_bridge.local_node_id().await {
        Ok(node_id) => node_id,
        Err(error) => return internal_error(&error),
    };
    let envelope = match board_envelope(
        &state,
        &context,
        BoardEnvelopeRoute {
            capability: "board.message.publish",
            action: "message.post",
            network_id: &network_id,
            feed_key: category.feed_key,
            scope_hint: &scope_hint,
        },
        &json!({
            "category": category.category,
            "message": body.message.clone(),
            "reply_to_message_id": body.reply_to_message_id,
        }),
    ) {
        Ok(envelope) => envelope,
        Err(error) => return internal_error(&error),
    };
    let message_id = match state
        .swarm_bridge
        .post_topic_message(
            Some(&network_id),
            category.feed_key,
            &scope_hint,
            body.message.clone(),
            body.reply_to_message_id.clone(),
            Some(envelope),
        )
        .await
    {
        Ok(message_id) => message_id,
        Err(error) => return internal_error(&error),
    };
    record_board_message_post(
        &state,
        &BoardMessagePost {
            message_id: &message_id,
            author_node_id: &author_node_id,
            author_public_id: context.public_memory_owner.public.as_deref(),
            author_display_name: context
                .public_identity
                .as_ref()
                .map(|identity| identity.display_name.as_str()),
            source: "network",
            category: category.category,
            network_id: &network_id,
            feed_key: category.feed_key,
            scope_hint: &scope_hint,
            content: body.message,
            reply_to_message_id: body.reply_to_message_id,
        },
    );
    Json(json!({"ok": true, "category": category.category, "scope_hint": scope_hint}))
        .into_response()
}

pub(crate) async fn subscribe_board_channel(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Path(category): Path<String>,
    Json(body): Json<BoardSubscriptionBody>,
) -> Response {
    update_board_channel_subscription(state, headers, category, body, true).await
}

pub(crate) async fn unsubscribe_board_channel(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Path(category): Path<String>,
    Json(body): Json<BoardSubscriptionBody>,
) -> Response {
    update_board_channel_subscription(state, headers, category, body, false).await
}

async fn update_board_channel_subscription(
    state: ControlPlaneState,
    headers: HeaderMap,
    category: String,
    body: BoardSubscriptionBody,
    active: bool,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let Some(channel) = normalized_category(&category) else {
        return bad_request("category must be one of general, trade, search, request");
    };
    let context = resolve_identity_context(&state, body.public_id.as_deref(), None).await;
    let network_id = match resolve_network_id(&state, None).await {
        Ok(network_id) => network_id,
        Err(error) => return internal_error(&error),
    };
    let scope_hint = format!("group:{}", channel.group);
    if let Err(error) = set_board_subscription(
        &state,
        &context,
        &network_id,
        channel.feed_key,
        &scope_hint,
        channel.display_name,
        active,
    )
    .await
    {
        return internal_error(&error);
    }
    let _ = state
        .audit_log
        .append(wattetheria_kernel::audit::AuditEntry {
            id: String::new(),
            timestamp: Utc::now().timestamp(),
            category: "board".to_owned(),
            action: if active {
                "board.channel.subscribe"
            } else {
                "board.channel.unsubscribe"
            }
            .to_owned(),
            status: "ok".to_owned(),
            actor: Some(auth),
            subject: Some(channel.category.to_owned()),
            capability: Some("board.channel.subscription".to_owned()),
            reason: None,
            duration_ms: None,
            details: Some(
                json!({"network_id": network_id, "scope_hint": scope_hint, "active": active}),
            ),
        });
    Json(json!({
        "ok": true,
        "category": channel.category,
        "channel": channel.group,
        "scope_hint": scope_hint,
        "subscribed": active,
    }))
    .into_response()
}

pub(crate) async fn list_published_service_agents(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, &headers).await {
        return response;
    }
    match published_service_agents_payload(&state).await {
        Ok(payload) => Json(payload).into_response(),
        Err(error) => internal_error(&error),
    }
}

const SERVICE_BOARD_SCAN_PAGE_LIMIT: usize = 200;

fn service_board_message_matches(message: &SwarmTopicMessageView, service_name: &str) -> bool {
    message
        .content
        .get("service_agent")
        .and_then(Value::as_object)
        .and_then(|author| author.get("service_name"))
        .and_then(Value::as_str)
        == Some(service_name)
}

async fn load_service_agent_board_messages(
    state: &ControlPlaneState,
    network_id: &str,
    service_name: &str,
    limit: usize,
    before_created_at: Option<u64>,
    before_message_id: Option<String>,
) -> anyhow::Result<(Vec<SwarmTopicMessageView>, Value)> {
    let (feed_key, scope_hint) = services_board_route();
    let mut messages = Vec::new();
    let mut scan_before_created_at = before_created_at;
    let mut scan_before_message_id = before_message_id;
    let mut next_before_created_at = scan_before_created_at;
    let mut next_before_message_id = scan_before_message_id.clone();
    let has_more = loop {
        let page = state
            .swarm_bridge
            .list_topic_messages(
                Some(network_id),
                feed_key,
                scope_hint,
                SERVICE_BOARD_SCAN_PAGE_LIMIT,
                scan_before_created_at,
                scan_before_message_id.clone(),
            )
            .await?;
        if page.is_empty() {
            break false;
        }

        let page_len = page.len();
        let mut reached_limit = false;
        for message in &page {
            if service_board_message_matches(message, service_name) {
                messages.push(message.clone());
                if messages.len() >= limit {
                    next_before_created_at = Some(message.created_at);
                    next_before_message_id = Some(message.message_id.clone());
                    reached_limit = true;
                    break;
                }
            }
        }

        let page_last = page
            .last()
            .expect("non-empty service board page has a last message");
        if reached_limit {
            break page_len == SERVICE_BOARD_SCAN_PAGE_LIMIT
                || page_last.message_id
                    != messages.last().expect("matched message exists").message_id;
        }

        next_before_created_at = Some(page_last.created_at);
        next_before_message_id = Some(page_last.message_id.clone());
        let made_progress = scan_before_created_at != next_before_created_at
            || scan_before_message_id != next_before_message_id;
        if page_len < SERVICE_BOARD_SCAN_PAGE_LIMIT || !made_progress {
            break false;
        }
        scan_before_created_at = next_before_created_at;
        scan_before_message_id = next_before_message_id.clone();
    };

    Ok((
        messages,
        json!({
            "before_created_at": next_before_created_at,
            "before_message_id": next_before_message_id,
            "has_more": has_more,
        }),
    ))
}

pub(crate) async fn list_service_agent_board_messages(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<ServiceBoardMessagesQuery>,
) -> Response {
    if let Err(response) = authorize(&state, &headers).await {
        return response;
    }
    if !service_name_is_valid(&query.service_name) {
        return bad_request("service_name must use lowercase letters, numbers, and hyphens");
    }
    let (feed_key, scope_hint) = services_board_route();
    let network_id = match resolve_network_id(&state, None).await {
        Ok(network_id) => network_id,
        Err(error) => return internal_error(&error),
    };
    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    let (messages, next) = match load_service_agent_board_messages(
        &state,
        &network_id,
        &query.service_name,
        limit,
        query.before_created_at,
        query.before_message_id.clone(),
    )
    .await
    {
        Ok(result) => result,
        Err(error) => return internal_error(&error),
    };
    let cursor = match state
        .swarm_bridge
        .topic_cursor(Some(&network_id), feed_key, query.subscriber_id.as_deref())
        .await
    {
        Ok(cursor) => cursor,
        Err(error) => return internal_error(&error),
    };
    let messages = board_topic_message_views(&state, &messages, Some("services")).await;
    Json(json!({
        "network_id": network_id,
        "service_name": query.service_name,
        "feed_key": feed_key,
        "scope_hint": scope_hint,
        "cursor": cursor,
        "next": next,
        "messages": messages,
    }))
    .into_response()
}

async fn post_service_agent_board_message(
    state: &ControlPlaneState,
    context: &IdentityContextView,
    network_id: &str,
    service: &Value,
    message: Value,
    reply_to_message_id: Option<String>,
) -> anyhow::Result<()> {
    let (feed_key, scope_hint) = services_board_route();
    let service_content = json!({
        "version": 1,
        "service_agent": {
            "service_name": service["service_name"],
            "service_address": service["service_address"],
            "service_did": service["service_did"],
            "provider_did": service["provider_did"],
        },
        "message": message,
    });
    let envelope = board_envelope(
        state,
        context,
        BoardEnvelopeRoute {
            capability: "board.service_agent.message.publish",
            action: "message.post",
            network_id,
            feed_key,
            scope_hint,
        },
        &json!({
            "service_agent": service_content["service_agent"],
            "reply_to_message_id": reply_to_message_id.clone(),
        }),
    )?;
    let author_node_id = state.swarm_bridge.local_node_id().await?;
    let message_id = state
        .swarm_bridge
        .post_topic_message(
            Some(network_id),
            feed_key,
            scope_hint,
            service_content.clone(),
            reply_to_message_id.clone(),
            Some(envelope),
        )
        .await?;
    record_board_message_post(
        state,
        &BoardMessagePost {
            message_id: &message_id,
            author_node_id: &author_node_id,
            author_public_id: context.public_memory_owner.public.as_deref(),
            author_display_name: context
                .public_identity
                .as_ref()
                .map(|identity| identity.display_name.as_str()),
            source: "service",
            category: "services",
            network_id,
            feed_key,
            scope_hint,
            content: service_content,
            reply_to_message_id,
        },
    );
    Ok(())
}

pub(crate) async fn publish_service_agent_board_message(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<ServiceBoardMessageBody>,
) -> Response {
    if let Err(response) = authorize(&state, &headers).await {
        return response;
    }
    if body.message.is_null() {
        return bad_request("message is required");
    }
    if let Some(response) = board_message_error(&body.message) {
        return response;
    }
    let context = resolve_identity_context(&state, body.public_id.as_deref(), None).await;
    let service_name = body.acting_as.trim();
    if !service_name_is_valid(service_name) {
        return bad_request("acting_as must use a lowercase service_name");
    }
    let published = match published_service_agents_payload(&state).await {
        Ok(payload) => payload,
        Err(error) => return internal_error(&error),
    };
    let Some(service) = published["items"].as_array().and_then(|items| {
        items
            .iter()
            .find(|item| item["service_name"].as_str() == Some(service_name))
    }) else {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({
                "error": "service agent cannot publish Board messages",
                "reason": "acting_as is not an Approved Service Agent published by the current Provider DID",
                "acting_as": service_name,
            })),
        )
            .into_response();
    };
    let network_id = match resolve_network_id(&state, None).await {
        Ok(network_id) => network_id,
        Err(error) => return internal_error(&error),
    };
    if let Err(error) = post_service_agent_board_message(
        &state,
        &context,
        &network_id,
        service,
        body.message,
        body.reply_to_message_id,
    )
    .await
    {
        return internal_error(&error);
    }
    Json(json!({
        "ok": true,
        "service_name": service_name,
        "service_address": service["service_address"],
        "scope_hint": SERVICES_BOARD_SCOPE_HINT,
    }))
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_board_categories_map_to_fixed_group_scopes() {
        assert_eq!(
            normalized_category("general").map(|channel| channel.group),
            Some("board-general")
        );
        assert_eq!(
            normalized_category("general").map(|channel| channel.feed_key),
            Some(BOARD_GENERAL_FEED_KEY)
        );
        assert_eq!(
            normalized_category("trade").map(|channel| channel.group),
            Some("board-trade")
        );
        assert_eq!(
            normalized_category("trade").map(|channel| channel.feed_key),
            Some(BOARD_TRADE_FEED_KEY)
        );
        assert_eq!(
            normalized_category("search").map(|channel| channel.group),
            Some("board-search")
        );
        assert_eq!(
            normalized_category("search").map(|channel| channel.feed_key),
            Some(BOARD_SEARCH_FEED_KEY)
        );
        assert_eq!(
            normalized_category("request").map(|channel| channel.group),
            Some("board-request")
        );
        assert_eq!(
            normalized_category("request").map(|channel| channel.feed_key),
            Some(BOARD_REQUEST_FEED_KEY)
        );
        assert!(normalized_category("global").is_none());
    }

    #[test]
    fn services_board_route_is_fixed_for_all_service_agents() {
        assert_eq!(
            services_board_route(),
            ("wattetheria.board.services", "group:board-services")
        );
        assert!(service_name_is_valid("weather-agent"));
        assert!(!service_name_is_valid("weather_agent"));
    }

    #[test]
    fn board_message_validation_counts_all_text_without_language_or_symbol_filters() {
        let message = serde_json::json!({
            "text": "symbols !@#$%^&*() []{}<>/?+=-_~ and \u{4e16}\u{754c} \u{1f600}\n\t"
        });

        assert!(board_message_error(&message).is_none());
    }

    #[test]
    fn board_message_validation_rejects_overlong_and_control_characters() {
        let overlong = serde_json::json!({
            "text": "x".repeat(BOARD_MESSAGE_MAX_CHARS + 1)
        });
        assert!(board_message_error(&overlong).is_some());

        let control = serde_json::json!({"text": "allowed\u{0000}rejected"});
        assert!(board_message_error(&control).is_some());
    }
}
