use axum::Json;
use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use wattetheria_kernel::swarm_bridge::{SwarmGlobalMessageView, SwarmTopicMessageView};

use super::{
    BOARD_CHANNELS, BoardMessagePost, BoardMessagesQuery, BoardPageCursor, SERVICES_BOARD_FEED_KEY,
    SERVICES_BOARD_SCOPE_HINT, bad_request, board_channel_payloads, local_board_subscription,
    normalized_category, resolve_network_id, service_name_is_valid, services_board_route,
};
use crate::auth::{authorize, internal_error};
use crate::routes::identity::resolve_identity_context;
use crate::state::{ControlPlaneState, StreamEvent};

const BOARD_SEARCH_PAGE_LIMIT: usize = 200;

pub(crate) async fn client_board(
    State(state): State<ControlPlaneState>,
    headers: axum::http::HeaderMap,
    Query(query): Query<BoardMessagesQuery>,
) -> Response {
    if let Err(response) = authorize(&state, &headers).await {
        return response;
    }
    let network_id = match resolve_network_id(&state, None).await {
        Ok(network_id) => network_id,
        Err(error) => return internal_error(&error),
    };
    let context = resolve_identity_context(&state, None, None).await;
    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    let search = query
        .search
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let result = if let Some(search) = search {
        build_search_board_page(&state, &context, &network_id, limit, search).await
    } else {
        match query.source.as_deref().unwrap_or("network") {
            "network" => {
                build_network_board_page(&state, &context, &network_id, &query, limit).await
            }
            "global" => build_global_board_page(&state, &query, limit).await,
            "services" => build_services_board_page(&state, &network_id, &query, limit).await,
            _ => Err(bad_request(
                "source must be one of network, global, or services",
            )),
        }
    };
    match result {
        Ok(payload) => Json(payload).into_response(),
        Err(response) => response,
    }
}

fn parse_cursor_map(
    raw: Option<&str>,
    field_name: &str,
) -> Result<BTreeMap<String, BoardPageCursor>, Response> {
    let Some(raw) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(BTreeMap::new());
    };
    serde_json::from_str(raw).map_err(|_| bad_request(format!("{field_name} must be valid JSON")))
}

fn topic_page_cursor(messages: &[SwarmTopicMessageView], limit: usize) -> Value {
    let last = messages.last();
    json!({
        "before_created_at": last.map(|message| message.created_at),
        "before_message_id": last.map(|message| message.message_id.clone()),
        "has_more": messages.len() >= limit,
    })
}

fn global_page_cursor(messages: &[SwarmGlobalMessageView], limit: usize) -> Value {
    json!({
        "before_sequence": messages.last().map(|message| message.sequence),
        "has_more": messages.len() >= limit,
    })
}

fn sort_board_messages(messages: &mut [Value]) {
    messages.sort_by(|left, right| {
        right["created_at"]
            .as_u64()
            .unwrap_or_default()
            .cmp(&left["created_at"].as_u64().unwrap_or_default())
            .then_with(|| {
                right["sequence"]
                    .as_u64()
                    .unwrap_or_default()
                    .cmp(&left["sequence"].as_u64().unwrap_or_default())
            })
            .then_with(|| {
                right["message_id"]
                    .as_str()
                    .unwrap_or_default()
                    .cmp(left["message_id"].as_str().unwrap_or_default())
            })
    });
}

fn search_value_contains(value: &Value, query: &str) -> bool {
    serde_json::to_string(value).is_ok_and(|serialized| serialized.to_lowercase().contains(query))
}

async fn load_all_topic_messages(
    state: &ControlPlaneState,
    network_id: &str,
    feed_key: &str,
    scope_hint: &str,
) -> anyhow::Result<Vec<SwarmTopicMessageView>> {
    let mut messages = Vec::new();
    let mut before_created_at = None;
    let mut before_message_id = None;
    loop {
        let page = state
            .swarm_bridge
            .list_topic_messages(
                Some(network_id),
                feed_key,
                scope_hint,
                BOARD_SEARCH_PAGE_LIMIT,
                before_created_at,
                before_message_id.clone(),
            )
            .await?;
        if page.is_empty() {
            break;
        }
        let page_len = page.len();
        let last = page.last().expect("non-empty page has a last message");
        let next_before_created_at = last.created_at;
        let next_before_message_id = last.message_id.clone();
        let made_progress = before_created_at != Some(next_before_created_at)
            || before_message_id.as_deref() != Some(next_before_message_id.as_str());
        messages.extend(page);
        if page_len < BOARD_SEARCH_PAGE_LIMIT || !made_progress {
            break;
        }
        before_created_at = Some(next_before_created_at);
        before_message_id = Some(next_before_message_id);
    }
    Ok(messages)
}

async fn load_all_global_messages(
    state: &ControlPlaneState,
) -> anyhow::Result<Vec<SwarmGlobalMessageView>> {
    let mut messages = Vec::new();
    let mut before_sequence = None;
    loop {
        let page = state
            .swarm_bridge
            .list_global_messages(BOARD_SEARCH_PAGE_LIMIT, before_sequence)
            .await?;
        if page.is_empty() {
            break;
        }
        let page_len = page.len();
        let last_sequence = page
            .last()
            .expect("non-empty page has a last message")
            .sequence;
        let made_progress = before_sequence != Some(last_sequence);
        messages.extend(page);
        if page_len < BOARD_SEARCH_PAGE_LIMIT || !made_progress {
            break;
        }
        before_sequence = Some(last_sequence);
    }
    Ok(messages)
}

async fn build_network_board_page(
    state: &ControlPlaneState,
    context: &crate::routes::identity::IdentityContextView,
    network_id: &str,
    query: &BoardMessagesQuery,
    limit: usize,
) -> Result<Value, Response> {
    let channels = board_channel_payloads(state, context, network_id, true)
        .await
        .map_err(|error| internal_error(&error))?;
    let selected_category = match query.category.as_deref() {
        Some(category) => Some(normalized_category(category).ok_or_else(|| {
            bad_request("category must be one of general, trade, search, request")
        })?),
        None => None,
    };
    let cursors = parse_cursor_map(query.network_cursors.as_deref(), "network_cursors")?;
    let mut messages = Vec::new();
    let mut next_network = Map::new();
    for channel in BOARD_CHANNELS {
        if selected_category.is_some_and(|selected| selected.category != channel.category) {
            continue;
        }
        let scope_hint = format!("group:{}", channel.group);
        let cursor = cursors.get(channel.category).cloned().unwrap_or_default();
        if cursor.has_more == Some(false) && cursors.contains_key(channel.category) {
            next_network.insert(
                channel.category.to_owned(),
                json!({
                    "before_created_at": cursor.before_created_at,
                    "before_message_id": cursor.before_message_id,
                    "has_more": false,
                }),
            );
            continue;
        }
        let topic_messages = state
            .swarm_bridge
            .list_topic_messages(
                Some(network_id),
                channel.feed_key,
                &scope_hint,
                limit,
                cursor.before_created_at,
                cursor.before_message_id,
            )
            .await
            .map_err(|error| internal_error(&error))?;
        let next_cursor = topic_page_cursor(&topic_messages, limit);
        messages.extend(
            board_topic_message_views(state, &topic_messages, Some(channel.category)).await,
        );
        next_network.insert(channel.category.to_owned(), next_cursor);
    }
    sort_board_messages(&mut messages);
    let has_more = next_network
        .values()
        .any(|cursor| cursor["has_more"].as_bool().unwrap_or(false));
    Ok(json!({
        "source": "network",
        "network_id": network_id,
        "channels": channels,
        "messages": messages,
        "next": {"network": next_network},
        "has_more": has_more,
    }))
}

async fn build_global_board_page(
    state: &ControlPlaneState,
    query: &BoardMessagesQuery,
    limit: usize,
) -> Result<Value, Response> {
    let global_messages = state
        .swarm_bridge
        .list_global_messages(limit, query.global_before_sequence)
        .await
        .map_err(|error| internal_error(&error))?;
    let messages = global_message_views(&global_messages);
    Ok(json!({
        "source": "global",
        "messages": messages,
        "next": {"global": global_page_cursor(&global_messages, limit)},
        "has_more": global_messages.len() >= limit,
    }))
}

async fn service_agents_with_board_metadata(
    state: &ControlPlaneState,
    network_id: &str,
) -> anyhow::Result<Vec<Value>> {
    let payload = published_service_agents_payload(state).await?;
    let mut items = payload["items"].as_array().cloned().unwrap_or_default();
    let registry = state.hive_registry.lock().await;
    let (feed_key, scope_hint) = services_board_route();
    for service in &mut items {
        let subscribed = local_board_subscription(&registry, network_id, feed_key, scope_hint);
        if let Some(object) = service.as_object_mut() {
            object.insert("feed_key".to_owned(), Value::String(feed_key.to_owned()));
            object.insert(
                "scope_hint".to_owned(),
                Value::String(scope_hint.to_owned()),
            );
            object.insert("subscribed".to_owned(), Value::Bool(subscribed));
        }
    }
    Ok(items)
}

async fn build_services_board_page(
    state: &ControlPlaneState,
    network_id: &str,
    query: &BoardMessagesQuery,
    limit: usize,
) -> Result<Value, Response> {
    let (service_agents, service_error) =
        match service_agents_with_board_metadata(state, network_id).await {
            Ok(items) => (items, None),
            Err(error) => (Vec::new(), Some(error.to_string())),
        };
    let (feed_key, scope_hint) = services_board_route();
    let topic_messages = state
        .swarm_bridge
        .list_topic_messages(
            Some(network_id),
            feed_key,
            scope_hint,
            limit,
            query.before_created_at,
            query.before_message_id.clone(),
        )
        .await
        .map_err(|error| internal_error(&error))?;
    let messages = board_topic_message_views(state, &topic_messages, Some("services")).await;
    let next_services = topic_page_cursor(&topic_messages, limit);
    let has_more = next_services["has_more"].as_bool().unwrap_or(false);
    Ok(json!({
        "source": "services",
        "network_id": network_id,
        "feed_key": SERVICES_BOARD_FEED_KEY,
        "scope_hint": SERVICES_BOARD_SCOPE_HINT,
        "service_agents": service_agents,
        "messages": messages,
        "service_error": service_error,
        "next": {"services": next_services},
        "has_more": has_more,
    }))
}

async fn build_search_board_page(
    state: &ControlPlaneState,
    context: &crate::routes::identity::IdentityContextView,
    network_id: &str,
    _limit: usize,
    search: &str,
) -> Result<Value, Response> {
    let search = search.to_lowercase();
    let channels = board_channel_payloads(state, context, network_id, true)
        .await
        .map_err(|error| internal_error(&error))?;
    let mut messages = Vec::new();
    for channel in BOARD_CHANNELS {
        let scope_hint = format!("group:{}", channel.group);
        let topic_messages =
            load_all_topic_messages(state, network_id, channel.feed_key, &scope_hint)
                .await
                .map_err(|error| internal_error(&error))?;
        let matching = topic_messages
            .iter()
            .filter(|message| search_value_contains(&message.content, &search))
            .cloned()
            .collect::<Vec<_>>();
        messages.extend(board_topic_message_views(state, &matching, Some(channel.category)).await);
    }

    let (global_messages, global_error) = match load_all_global_messages(state).await {
        Ok(global_messages) => {
            let matching = global_messages
                .iter()
                .filter(|message| {
                    search_value_contains(
                        &json!({
                            "kind": message.kind,
                            "lane": message.lane,
                            "content": message.content,
                        }),
                        &search,
                    )
                })
                .cloned()
                .collect::<Vec<_>>();
            (global_message_views(&matching), None)
        }
        Err(error) => (Vec::new(), Some(error.to_string())),
    };
    messages.extend(global_messages);

    let (service_agents, service_error) =
        match service_agents_with_board_metadata(state, network_id).await {
            Ok(service_agents) => {
                let (feed_key, scope_hint) = services_board_route();
                let topic_messages =
                    load_all_topic_messages(state, network_id, feed_key, scope_hint)
                        .await
                        .map_err(|error| internal_error(&error))?;
                let matching = topic_messages
                    .iter()
                    .filter(|message| search_value_contains(&message.content, &search))
                    .cloned()
                    .collect::<Vec<_>>();
                messages
                    .extend(board_topic_message_views(state, &matching, Some("services")).await);
                (service_agents, None)
            }
            Err(error) => (Vec::new(), Some(error.to_string())),
        };
    sort_board_messages(&mut messages);
    Ok(json!({
        "source": "search",
        "search": search,
        "network_id": network_id,
        "channels": channels,
        "service_agents": service_agents,
        "messages": messages,
        "search_complete": true,
        "has_more": false,
        "global_error": global_error,
        "service_error": service_error,
    }))
}

pub(crate) async fn published_service_agents_payload(
    state: &ControlPlaneState,
) -> anyhow::Result<Value> {
    let Some(client) = state.servicenet_client.as_deref() else {
        anyhow::bail!("servicenet is not configured");
    };
    let provider_did = state.servicenet_provider.did.clone();
    let response = client
        .list_agents_for_provider_did(&provider_did, 100, 0)
        .await
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let items = response
        .items
        .iter()
        .filter_map(|agent| published_service_agent_view(agent, &provider_did))
        .collect::<Vec<_>>();
    Ok(json!({
        "provider_did": provider_did,
        "items": items,
        "count": items.len(),
        "source": "servicenet",
    }))
}

fn published_service_agent_view(agent: &Value, provider_did: &str) -> Option<Value> {
    if let Some(agent_provider_did) = agent
        .get("provider_did")
        .and_then(Value::as_str)
        .or_else(|| agent.get("provider_attester_did").and_then(Value::as_str))
        && agent_provider_did != provider_did
    {
        return None;
    }
    let status = agent
        .get("status")
        .and_then(Value::as_str)
        .or_else(|| agent.get("state").and_then(Value::as_str))?;
    if !status.eq_ignore_ascii_case("approved") {
        return None;
    }
    let service_address = agent.get("service_address").and_then(Value::as_str)?;
    let service_name = service_address.split('@').next()?.trim();
    if !service_name_is_valid(service_name) {
        return None;
    }
    let service_did = agent
        .get("service_did")
        .and_then(Value::as_str)
        .or_else(|| {
            agent
                .get("agent_card")
                .and_then(|card| card.get("didDocument"))
                .and_then(|document| document.get("id"))
                .and_then(Value::as_str)
        })?;
    Some(json!({
        "service_name": service_name,
        "service_address": service_address,
        "display_name": agent.get("agent_card").and_then(|card| card.get("name")),
        "service_did": service_did,
        "provider_did": provider_did,
        "execution": agent.get("execution").or_else(|| agent.get("deployment")),
    }))
}

pub(crate) async fn board_topic_message_views(
    state: &ControlPlaneState,
    messages: &[SwarmTopicMessageView],
    category: Option<&str>,
) -> Vec<Value> {
    let bindings = state.controller_binding_registry.lock().await.list();
    let identities = state.public_identity_registry.lock().await.list();
    let identity_by_public_id = identities
        .into_iter()
        .map(|identity| (identity.public_id.clone(), identity))
        .collect::<BTreeMap<_, _>>();
    let binding_by_node = bindings
        .into_iter()
        .map(|binding| {
            (
                binding
                    .controller_node_id
                    .clone()
                    .unwrap_or_else(|| binding.public_id.clone()),
                binding,
            )
        })
        .collect::<BTreeMap<_, _>>();
    messages
        .iter()
        .map(|message| {
            let binding = binding_by_node.get(&message.author_node_id);
            let public_id = binding
                .map(|binding| binding.public_id.clone())
                .or_else(|| {
                    message
                        .agent_envelope
                        .as_ref()
                        .and_then(|envelope| envelope.message.get("author_public_id"))
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned)
                });
            let display_name = public_id
                .as_deref()
                .and_then(|public_id| identity_by_public_id.get(public_id))
                .map(|identity| identity.display_name.clone())
                .or_else(|| {
                    message
                        .agent_envelope
                        .as_ref()
                        .and_then(|envelope| envelope.message.get("author_display_name"))
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned)
                });
            let service_author = message
                .content
                .get("service_agent")
                .and_then(Value::as_object);
            let service_name = service_author
                .and_then(|author| author.get("service_name"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            let service_did = service_author
                .and_then(|author| author.get("service_did"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            let provider_did = service_author
                .and_then(|author| author.get("provider_did"))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            json!({
                "message_id": message.message_id,
                "source": if service_name.is_some() { "service" } else { "network" },
                "category": category,
                "service_name": service_name,
                "service_did": service_did,
                "provider_did": provider_did,
                "author_node_id": message.author_node_id,
                "author_public_id": public_id,
                "author_display_name": display_name,
                "content": message.content,
                "reply_to_message_id": message.reply_to_message_id,
                "created_at": message.created_at,
                "feed_key": message.feed_key,
                "scope_hint": message.scope_hint,
                "gossip_kind": "messages",
            })
        })
        .collect()
}

fn global_message_views(messages: &[SwarmGlobalMessageView]) -> Vec<Value> {
    messages
        .iter()
        .map(|message| {
            json!({
                "message_id": message.message_id,
                "source": "global",
                "category": "global",
                "global_kind": message.kind,
                "global_lane": message.lane,
                "author_node_id": message.author_node_id,
                "content": message.content,
                "created_at": message.created_at,
                "sequence": message.sequence,
                "scope_hint": message.scope_hint,
                "gossip_kind": message.lane,
            })
        })
        .collect()
}

pub(crate) fn record_board_message_post(state: &ControlPlaneState, post: &BoardMessagePost<'_>) {
    let payload = json!({
        "source": post.source,
        "category": post.category,
        "network_id": post.network_id,
        "feed_key": post.feed_key,
        "scope_hint": post.scope_hint,
        "content": post.content,
        "reply_to_message_id": post.reply_to_message_id,
    });
    let _ = state.stream_tx.send(StreamEvent {
        kind: "board.message.posted".to_owned(),
        timestamp: Utc::now().timestamp(),
        payload: payload.clone(),
    });
    let _ = state
        .audit_log
        .append(wattetheria_kernel::audit::AuditEntry {
            id: String::new(),
            timestamp: Utc::now().timestamp(),
            category: "board".to_owned(),
            action: "board.message.post".to_owned(),
            status: "ok".to_owned(),
            actor: None,
            subject: Some(format!("{}@{}", post.feed_key, post.scope_hint)),
            capability: Some("board.message.publish".to_owned()),
            reason: None,
            duration_ms: None,
            details: Some(payload),
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn published_service_agent_view_requires_current_provider_and_approval() {
        let provider_did = "did:key:provider";
        let agent = json!({
            "agent_id": "internal-agent-id",
            "provider_id": "internal-provider-id",
            "status": "Approved",
            "service_did": "did:key:service",
            "service_address": "weather-agent@wattetheria",
            "agent_card": {"name": "Weather Agent"}
        });
        let view = published_service_agent_view(&agent, provider_did).expect("published agent");

        assert_eq!(view["service_name"], "weather-agent");
        assert_eq!(view["service_address"], "weather-agent@wattetheria");
        assert!(view.get("agent_id").is_none());
        assert!(view.get("provider_id").is_none());

        let mut mismatched_provider = agent.clone();
        mismatched_provider["provider_did"] = json!("did:key:other");
        assert!(published_service_agent_view(&mismatched_provider, provider_did).is_none());

        let mut pending = agent;
        pending["status"] = json!("pending");
        assert!(published_service_agent_view(&pending, provider_did).is_none());
    }

    #[test]
    fn topic_page_cursor_uses_the_oldest_item_and_reports_more_pages() {
        let messages = vec![
            SwarmTopicMessageView {
                message_id: "newer".to_owned(),
                network_id: "net".to_owned(),
                feed_key: "feed".to_owned(),
                scope_hint: "group:board-general".to_owned(),
                author_node_id: "node".to_owned(),
                agent_envelope: None,
                content: json!({"text": "newer"}),
                reply_to_message_id: None,
                created_at: 20,
            },
            SwarmTopicMessageView {
                message_id: "older".to_owned(),
                network_id: "net".to_owned(),
                feed_key: "feed".to_owned(),
                scope_hint: "group:board-general".to_owned(),
                author_node_id: "node".to_owned(),
                agent_envelope: None,
                content: json!({"text": "older"}),
                reply_to_message_id: None,
                created_at: 10,
            },
        ];
        let cursor = topic_page_cursor(&messages, 2);

        assert_eq!(cursor["before_created_at"], 10);
        assert_eq!(cursor["before_message_id"], "older");
        assert_eq!(cursor["has_more"], true);
    }

    #[test]
    fn search_matches_serialized_message_values_without_language_filters() {
        assert!(search_value_contains(&json!({"text": "交易 ☕"}), "交易"));
        assert!(search_value_contains(&json!({"text": "交易 ☕"}), "☕"));
        assert!(!search_value_contains(
            &json!({"text": "交易 ☕"}),
            "missing"
        ));
    }
}
