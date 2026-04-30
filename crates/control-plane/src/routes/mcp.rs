use axum::Json;
use axum::body::{Body, to_bytes};
use axum::extract::State;
use axum::http::{HeaderMap, Method, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::path::Path;
use tower::ServiceExt;

use crate::auth::{authorize, bearer_token, internal_error, unauthorized};
use crate::routes::identity::resolve_identity_context;
use crate::state::ControlPlaneState;

mod schema;

use schema::input_schema;

const MCP_PROTOCOL_VERSION: &str = "2025-03-26";
const LOOPBACK_BODY_LIMIT: usize = 8 * 1024 * 1024;
const DEFAULT_GATEWAY_TASK_LIMIT: usize = 50;
const MAX_GATEWAY_TASK_LIMIT: usize = 100;
const MAX_GATEWAY_TASK_WINDOW: usize = 200;
const DEFAULT_GATEWAY_TOPIC_LIMIT: usize = 50;
const MAX_GATEWAY_TOPIC_LIMIT: usize = 100;
const MAX_GATEWAY_TOPIC_WINDOW: usize = 200;
const MISSION_FEED_KEY: &str = "wattetheria.missions";
#[derive(Debug, Clone)]
struct AgentTool {
    name: &'static str,
    method: Method,
    path: &'static str,
    description: &'static str,
    availability: Availability,
}

#[derive(Debug, Clone, Copy)]
enum Availability {
    Always,
    TopicBridge,
    ServiceNet,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub(crate) struct McpRequest {
    #[serde(default)]
    jsonrpc: Option<String>,
    #[serde(default)]
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Serialize)]
struct McpTool {
    name: &'static str,
    description: String,
    #[serde(rename = "inputSchema")]
    input_schema: Value,
    #[serde(rename = "_meta")]
    meta: Value,
}

pub(crate) async fn mcp(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(request): Json<McpRequest>,
) -> Response {
    let auth = if request.method == "tools/call" {
        match validate_bearer(&state, &headers) {
            Some(token) => token,
            None => return unauthorized(),
        }
    } else {
        match authorize(&state, &headers).await {
            Ok(token) => token,
            Err(response) => return response,
        }
    };

    if request
        .jsonrpc
        .as_deref()
        .is_some_and(|value| value != "2.0")
    {
        return Json(mcp_error(
            request.id.as_ref(),
            -32600,
            "invalid JSON-RPC version",
        ))
        .into_response();
    }

    let result = match request.method.as_str() {
        "initialize" => initialize_result(),
        "notifications/initialized" => Value::Null,
        "ping" => json!({}),
        "tools/list" => json!({
            "tools": agent_tools()
                .iter()
                .map(|tool| mcp_tool(tool, tool.is_available(&state)))
                .collect::<Vec<_>>()
        }),
        "tools/call" => match call_tool(&state, &auth, request.params).await {
            Ok(result) => result,
            Err(response) => return response,
        },
        _ => {
            return Json(mcp_error(
                request.id.as_ref(),
                -32601,
                format!("unsupported MCP method {}", request.method),
            ))
            .into_response();
        }
    };

    Json(json!({
        "jsonrpc": "2.0",
        "id": request.id,
        "result": result,
    }))
    .into_response()
}

fn initialize_result() -> Value {
    json!({
        "protocolVersion": MCP_PROTOCOL_VERSION,
        "capabilities": {
            "tools": {
                "listChanged": true
            }
        },
        "serverInfo": {
            "name": "wattetheria-local-control-plane",
            "version": env!("CARGO_PKG_VERSION")
        }
    })
}

fn validate_bearer(state: &ControlPlaneState, headers: &HeaderMap) -> Option<String> {
    match bearer_token(headers) {
        Some(token) if token == state.auth_token => Some(token.to_string()),
        _ => None,
    }
}

async fn call_tool(
    state: &ControlPlaneState,
    auth: &str,
    params: Value,
) -> Result<Value, Response> {
    let name = required_string(&params, "name").ok_or_else(|| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "tools/call params.name is required"})),
        )
            .into_response()
    })?;
    let Some(tool) = agent_tools().iter().find(|tool| tool.name == name) else {
        return Ok(tool_error(
            &json!({"error": format!("unknown tool: {name}")}),
        ));
    };
    let arguments = params
        .get("arguments")
        .cloned()
        .or_else(|| params.get("input").cloned())
        .unwrap_or_else(|| json!({}));
    if !arguments.is_object() {
        return Ok(tool_error(
            &json!({"error": "tool arguments must be a JSON object"}),
        ));
    }

    if tool.name == "list_missions" {
        return Ok(network_mission_market_result(state, &arguments).await);
    }
    if tool.name == "list_topics" {
        return Ok(network_topic_market_result(state, &arguments).await);
    }

    let response = dispatch_loopback_tool(state.clone(), auth, tool, &arguments).await?;
    Ok(response_to_tool_result(response).await)
}

async fn network_topic_market_result(state: &ControlPlaneState, arguments: &Value) -> Value {
    match network_topic_market_payload(state, arguments).await {
        Ok(payload) => tool_success(&payload),
        Err(error) => tool_error(&json!({"error": error.to_string()})),
    }
}

async fn network_topic_market_payload(
    state: &ControlPlaneState,
    arguments: &Value,
) -> anyhow::Result<Value> {
    let limit = numeric_argument(arguments, "limit")
        .unwrap_or(DEFAULT_GATEWAY_TOPIC_LIMIT)
        .clamp(1, MAX_GATEWAY_TOPIC_LIMIT);
    let offset = numeric_argument(arguments, "offset")
        .unwrap_or(0)
        .min(MAX_GATEWAY_TOPIC_WINDOW);
    let fetch_limit = offset
        .saturating_add(limit)
        .clamp(1, MAX_GATEWAY_TOPIC_WINDOW);
    let gateway_url = resolve_gateway_query_url(state)?;
    let gateway_endpoint = normalized_gateway_topics_url(&gateway_url);
    let topics = fetch_gateway_topics(&gateway_endpoint, fetch_limit).await?;
    let all_topics = topics
        .into_iter()
        .filter(|topic| matches_topic_filters(topic, arguments))
        .collect::<Vec<_>>();
    let page = all_topics
        .iter()
        .skip(offset)
        .take(limit)
        .cloned()
        .map(normalize_gateway_topic)
        .collect::<Vec<_>>();
    let next_offset = offset + page.len();
    let has_more = next_offset < all_topics.len();

    Ok(json!({
        "source": "wattetheria-gateway.api_topics",
        "scope": "network",
        "gateway_url": gateway_url,
        "gateway_endpoint": gateway_endpoint,
        "pagination": "gateway_limit_client_offset",
        "limit": limit,
        "offset": offset,
        "next_offset": if has_more { Some(next_offset) } else { None },
        "has_more": has_more,
        "known_count": all_topics.len(),
        "topics": page,
    }))
}

async fn network_mission_market_result(state: &ControlPlaneState, arguments: &Value) -> Value {
    match network_mission_market_payload(state, arguments).await {
        Ok(payload) => tool_success(&payload),
        Err(error) => tool_error(&json!({"error": error.to_string()})),
    }
}

async fn network_mission_market_payload(
    state: &ControlPlaneState,
    arguments: &Value,
) -> anyhow::Result<Value> {
    let limit = numeric_argument(arguments, "limit")
        .unwrap_or(DEFAULT_GATEWAY_TASK_LIMIT)
        .clamp(1, MAX_GATEWAY_TASK_LIMIT);
    let offset = numeric_argument(arguments, "offset")
        .unwrap_or(0)
        .min(MAX_GATEWAY_TASK_WINDOW);
    let fetch_limit = offset
        .saturating_add(limit)
        .clamp(1, MAX_GATEWAY_TASK_WINDOW);
    let status_filter = required_string(arguments, "status").map(normalize_mission_status_filter);
    let gateway_url = resolve_gateway_query_url(state)?;
    let gateway_endpoint = normalized_gateway_tasks_url(&gateway_url);
    let tasks = fetch_gateway_tasks(&gateway_endpoint, fetch_limit).await?;
    let all_missions = tasks
        .into_iter()
        .filter(is_gateway_mission)
        .filter(|task| {
            status_filter
                .as_ref()
                .is_none_or(|status| gateway_task_status(task).as_deref() == Some(status))
        })
        .collect::<Vec<_>>();
    let page = all_missions
        .iter()
        .skip(offset)
        .take(limit)
        .cloned()
        .map(normalize_gateway_mission)
        .collect::<Vec<_>>();
    let next_offset = offset + page.len();
    let has_more = next_offset < all_missions.len();

    Ok(json!({
        "source": "wattetheria-gateway.api_tasks",
        "scope": "network",
        "gateway_url": gateway_url,
        "gateway_endpoint": gateway_endpoint,
        "pagination": "gateway_limit_client_offset",
        "limit": limit,
        "offset": offset,
        "next_offset": if has_more { Some(next_offset) } else { None },
        "has_more": has_more,
        "known_count": all_missions.len(),
        "missions": page,
    }))
}

pub(crate) async fn fetch_gateway_tasks(
    gateway_endpoint: &str,
    limit: usize,
) -> anyhow::Result<Vec<Value>> {
    fetch_gateway_array(gateway_endpoint, limit, "/api/tasks").await
}

async fn fetch_gateway_topics(gateway_endpoint: &str, limit: usize) -> anyhow::Result<Vec<Value>> {
    fetch_gateway_array(gateway_endpoint, limit, "/api/topics").await
}

async fn fetch_gateway_array(
    gateway_endpoint: &str,
    limit: usize,
    resource: &str,
) -> anyhow::Result<Vec<Value>> {
    let payload = reqwest::Client::new()
        .get(gateway_endpoint)
        .query(&[("limit", limit.to_string())])
        .send()
        .await?
        .error_for_status()?
        .json::<Value>()
        .await?;
    payload
        .as_array()
        .cloned()
        .ok_or_else(|| anyhow::anyhow!("gateway {resource} returned a non-array payload"))
}

fn matches_topic_filters(topic: &Value, arguments: &Value) -> bool {
    matches_optional_gateway_string(topic, &[&["topic_id"], &["id"]], arguments, "topic_id")
        && matches_optional_gateway_string(
            topic,
            &[&["organization_id"], &["organizationId"]],
            arguments,
            "organization_id",
        )
        && matches_optional_gateway_string(
            topic,
            &[&["mission_id"], &["missionId"]],
            arguments,
            "mission_id",
        )
        && matches_optional_gateway_string(
            topic,
            &[&["projection_kind"], &["kind"]],
            arguments,
            "projection_kind",
        )
        && (bool_argument(arguments, "include_inactive").unwrap_or(false)
            || gateway_topic_active(topic))
}

fn matches_optional_gateway_string(
    value: &Value,
    paths: &[&[&str]],
    arguments: &Value,
    argument_key: &str,
) -> bool {
    let Some(expected) = required_string(arguments, argument_key) else {
        return true;
    };
    gateway_task_string(value, paths).as_deref() == Some(expected.as_str())
}

fn gateway_topic_active(topic: &Value) -> bool {
    match topic.get("active").and_then(Value::as_bool) {
        Some(active) => active,
        None => topic
            .get("status")
            .and_then(Value::as_str)
            .is_none_or(|status| status != "inactive"),
    }
}

fn normalize_gateway_topic(mut topic: Value) -> Value {
    let subscribe_route = gateway_topic_subscribe_route(&topic);
    let topic_id = gateway_task_string(&topic, &[&["topic_id"], &["id"]]);
    let display_name = gateway_task_string(
        &topic,
        &[
            &["display_name"],
            &["title"],
            &["name"],
            &["topic_id"],
            &["id"],
        ],
    );
    let Some(object) = topic.as_object_mut() else {
        return topic;
    };
    if let Some(topic_id) = topic_id {
        object
            .entry("topic_id".to_string())
            .or_insert_with(|| Value::String(topic_id));
    }
    if let Some(display_name) = display_name {
        object
            .entry("display_name".to_string())
            .or_insert_with(|| Value::String(display_name));
    }
    if let Some(route) = subscribe_route.as_object() {
        for key in ["feed_key", "scope_hint"] {
            if let Some(value) = route.get(key) {
                object
                    .entry(key.to_string())
                    .or_insert_with(|| value.clone());
            }
        }
    }
    object.insert("subscribe_route".to_string(), subscribe_route);
    topic
}

fn gateway_topic_subscribe_route(topic: &Value) -> Value {
    let feed_key = gateway_task_string(
        topic,
        &[
            &["feed_key"],
            &["summary", "feed_key"],
            &["inputs", "feed_key"],
        ],
    );
    let scope_hint = gateway_task_string(
        topic,
        &[
            &["scope_hint"],
            &["summary", "scope_hint"],
            &["inputs", "scope_hint"],
        ],
    );
    let subscribe_ready = feed_key.is_some() && scope_hint.is_some();
    json!({
        "feed_key": feed_key,
        "scope_hint": scope_hint,
        "subscribe_ready": subscribe_ready,
    })
}

fn is_gateway_mission(task: &Value) -> bool {
    task.get("task_type")
        .and_then(Value::as_str)
        .is_some_and(|kind| kind == "wattetheria.mission")
        || (task.get("title").and_then(Value::as_str).is_some()
            && task
                .get("id")
                .or_else(|| task.get("mission_id"))
                .and_then(Value::as_str)
                .is_some())
}

fn gateway_task_status(task: &Value) -> Option<String> {
    task.get("status")
        .or_else(|| task.get("terminal_state"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
}

fn normalize_mission_status_filter(status: String) -> String {
    if status == "open" {
        "published".to_string()
    } else {
        status
    }
}

fn normalize_gateway_mission(mut task: Value) -> Value {
    let claim_route = gateway_mission_claim_route(&task);
    let status = gateway_task_status(&task);
    let mission_id = task
        .get("mission_id")
        .or_else(|| task.get("id"))
        .or_else(|| task.get("task_id"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let Some(object) = task.as_object_mut() else {
        return task;
    };
    if let Some(mission_id) = mission_id {
        object
            .entry("mission_id".to_string())
            .or_insert_with(|| Value::String(mission_id.clone()));
        object
            .entry("task_id".to_string())
            .or_insert_with(|| Value::String(mission_id));
    }
    if let Some(status) = status {
        object
            .entry("status".to_string())
            .or_insert_with(|| Value::String(status));
    }
    if let Some(route) = claim_route.as_object() {
        for key in [
            "publisher_wattswarm_node_id",
            "mission_feed_key",
            "mission_scope_hint",
            "swarm_scope",
        ] {
            if let Some(value) = route.get(key) {
                object
                    .entry(key.to_string())
                    .or_insert_with(|| value.clone());
            }
        }
    }
    object.insert("claim_route".to_string(), claim_route);
    task
}

fn gateway_mission_claim_route(task: &Value) -> Value {
    let task_id = gateway_task_string(task, &[&["task_id"], &["id"], &["mission_id"]]);
    let mission_id = gateway_task_string(task, &[&["mission_id"], &["id"], &["task_id"]])
        .or_else(|| task_id.clone());
    let publisher_wattswarm_node_id = gateway_task_string(
        task,
        &[
            &["publisher_wattswarm_node_id"],
            &["summary", "publisher_wattswarm_node_id"],
            &["inputs", "publisher_wattswarm_node_id"],
            &["source_node_id"],
        ],
    );
    let mission_feed_key = gateway_task_string(
        task,
        &[
            &["mission_feed_key"],
            &["summary", "mission_feed_key"],
            &["inputs", "mission_feed_key"],
        ],
    )
    .unwrap_or_else(|| MISSION_FEED_KEY.to_string());
    let mission_scope_hint = gateway_task_string(
        task,
        &[
            &["mission_scope_hint"],
            &["summary", "mission_scope_hint"],
            &["inputs", "mission_scope_hint"],
            &["scope_hint"],
        ],
    )
    .or_else(|| {
        publisher_wattswarm_node_id
            .as_ref()
            .map(|node_id| format!("node:{node_id}"))
    });
    let swarm_scope = gateway_task_value(
        task,
        &[
            &["swarm_scope"],
            &["inputs", "swarm_scope"],
            &["summary", "swarm_scope"],
        ],
    )
    .cloned()
    .or_else(|| {
        mission_scope_hint
            .as_deref()
            .and_then(swarm_scope_from_hint)
    });
    let task_contract_available =
        gateway_task_value(task, &[&["task_contract"], &["contract"]]).is_some();
    let claim_ready = task_id.is_some()
        && publisher_wattswarm_node_id.is_some()
        && mission_scope_hint.is_some()
        && swarm_scope.is_some()
        && task_contract_available;

    json!({
        "task_id": task_id,
        "mission_id": mission_id,
        "publisher_wattswarm_node_id": publisher_wattswarm_node_id,
        "mission_feed_key": mission_feed_key,
        "mission_scope_hint": mission_scope_hint,
        "swarm_scope": swarm_scope,
        "task_contract_available": task_contract_available,
        "claim_ready": claim_ready,
    })
}

fn gateway_task_string(task: &Value, paths: &[&[&str]]) -> Option<String> {
    paths
        .iter()
        .filter_map(|path| path.iter().try_fold(task, |value, key| value.get(*key)))
        .find_map(|value| {
            value
                .as_str()
                .map(str::trim)
                .filter(|value| !value.is_empty())
        })
        .map(ToOwned::to_owned)
}

fn gateway_task_value<'a>(task: &'a Value, paths: &[&[&str]]) -> Option<&'a Value> {
    paths
        .iter()
        .find_map(|path| path.iter().try_fold(task, |value, key| value.get(*key)))
}

fn swarm_scope_from_hint(scope_hint: &str) -> Option<Value> {
    let (kind, id) = scope_hint.split_once(':')?;
    if kind.trim().is_empty() || id.trim().is_empty() {
        return None;
    }
    Some(json!({"kind": kind.trim(), "id": id.trim()}))
}

#[derive(Debug, Deserialize)]
struct GatewayQueryConfig {
    #[serde(default)]
    gateway_urls: Vec<String>,
}

pub(crate) fn resolve_gateway_query_url(state: &ControlPlaneState) -> anyhow::Result<String> {
    let candidates = gateway_urls_from_config_path(&state.data_dir.join("config.json"))
        .into_iter()
        .chain(gateway_urls_from_env())
        .chain(gateway_urls_from_env_config_path());
    candidates
        .map(|url| normalize_gateway_base_url(&url))
        .find(|url| !url.is_empty())
        .ok_or_else(|| anyhow::anyhow!("gateway URL is not configured"))
}

fn gateway_urls_from_env() -> Vec<String> {
    std::env::var("WATTETHERIA_GATEWAY_URLS")
        .ok()
        .map(|value| split_gateway_urls(&value))
        .unwrap_or_default()
}

fn gateway_urls_from_env_config_path() -> Vec<String> {
    std::env::var("WATTETHERIA_GATEWAY_CONFIG_PATH")
        .ok()
        .map(|path| gateway_urls_from_config_path(Path::new(&path)))
        .unwrap_or_default()
}

fn gateway_urls_from_config_path(path: &Path) -> Vec<String> {
    let Ok(bytes) = std::fs::read(path) else {
        return Vec::new();
    };
    serde_json::from_slice::<GatewayQueryConfig>(&bytes)
        .map(|config| normalize_gateway_urls(config.gateway_urls))
        .unwrap_or_default()
}

fn split_gateway_urls(value: &str) -> Vec<String> {
    normalize_gateway_urls(value.split(',').map(str::to_string))
}

fn normalize_gateway_urls(values: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut normalized = Vec::new();
    for value in values {
        let url = normalize_gateway_base_url(&value);
        if !url.is_empty() && !normalized.iter().any(|existing| existing == &url) {
            normalized.push(url);
        }
    }
    normalized
}

fn normalize_gateway_base_url(gateway_url: &str) -> String {
    gateway_url.trim().trim_end_matches('/').to_string()
}

pub(crate) fn normalized_gateway_tasks_url(gateway_url: &str) -> String {
    let trimmed = gateway_url.trim_end_matches('/');
    if trimmed.ends_with("/api/tasks") {
        trimmed.to_string()
    } else {
        format!("{trimmed}/api/tasks")
    }
}

fn normalized_gateway_topics_url(gateway_url: &str) -> String {
    let trimmed = gateway_url.trim_end_matches('/');
    if trimmed.ends_with("/api/topics") {
        trimmed.to_string()
    } else {
        format!("{trimmed}/api/topics")
    }
}
async fn dispatch_loopback_tool(
    state: ControlPlaneState,
    auth: &str,
    tool: &AgentTool,
    arguments: &Value,
) -> Result<Response, Response> {
    let uri = match tool_uri(tool, arguments) {
        Ok(uri) => uri,
        Err(error) => {
            return Ok((StatusCode::BAD_REQUEST, Json(json!({"error": error}))).into_response());
        }
    };
    let body = if tool.method == Method::GET {
        Body::empty()
    } else {
        let body = tool_body_with_local_identity(&state, tool, arguments).await;
        Body::from(body.to_string())
    };
    let request = Request::builder()
        .method(tool.method.clone())
        .uri(uri)
        .header("authorization", format!("Bearer {auth}"))
        .header("content-type", "application/json")
        .body(body)
        .expect("valid loopback MCP tool request");

    crate::app(state)
        .oneshot(request)
        .await
        .map_err(|error| internal_error(&anyhow::anyhow!(error)))
}

async fn response_to_tool_result(response: Response) -> Value {
    let status = response.status();
    let body = response.into_body();
    let bytes = match to_bytes(body, LOOPBACK_BODY_LIMIT).await {
        Ok(bytes) => bytes,
        Err(error) => return tool_error(&json!({"error": error.to_string()})),
    };
    let payload = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).to_string()))
    };
    let text = serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string());
    json!({
        "content": [
            {
                "type": "text",
                "text": text
            }
        ],
        "structuredContent": payload,
        "isError": !status.is_success(),
        "_meta": {
            "httpStatus": status.as_u16()
        }
    })
}

fn tool_error(payload: &Value) -> Value {
    let text = serde_json::to_string_pretty(&payload).unwrap_or_else(|_| payload.to_string());
    json!({
        "content": [
            {
                "type": "text",
                "text": text
            }
        ],
        "structuredContent": payload,
        "isError": true
    })
}

fn tool_success(payload: &Value) -> Value {
    let text = serde_json::to_string_pretty(payload).unwrap_or_else(|_| payload.to_string());
    json!({
        "content": [
            {
                "type": "text",
                "text": text
            }
        ],
        "structuredContent": payload,
        "isError": false
    })
}

fn mcp_error(id: Option<&Value>, code: i64, message: impl Into<String>) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message.into()
        }
    })
}

fn mcp_tool(tool: &AgentTool, available: bool) -> McpTool {
    let description = if available {
        tool.description.to_string()
    } else {
        format!("{} Currently unavailable on this node.", tool.description)
    };
    McpTool {
        name: tool.name,
        description,
        input_schema: input_schema(tool),
        meta: json!({
            "wattetheria": {
                "manifestEndpoint": tool.name,
                "method": tool.method.as_str(),
                "path": tool.path,
                "available": available,
                "source": "agent-participation.manifest.v1"
            }
        }),
    }
}

fn tool_uri(tool: &AgentTool, arguments: &Value) -> Result<String, String> {
    let mut path = tool.path.to_string();
    for var in path_vars(tool.path) {
        let value = required_string(arguments, var)
            .ok_or_else(|| format!("missing required path parameter `{var}`"))?;
        path = path.replace(&format!("{{{var}}}"), &value);
    }

    if tool.method != Method::GET {
        return Ok(path);
    }

    let query = arguments
        .get("query")
        .cloned()
        .unwrap_or_else(|| object_without_path_vars(arguments, tool.path));
    let query = serde_urlencoded::to_string(flatten_query_object(&query))
        .map_err(|error| error.to_string())?;
    if query.is_empty() {
        Ok(path)
    } else {
        Ok(format!("{path}?{query}"))
    }
}

async fn tool_body_with_local_identity(
    state: &ControlPlaneState,
    tool: &AgentTool,
    arguments: &Value,
) -> Value {
    let mut body = tool_body(tool, arguments);
    apply_local_identity_defaults(state, tool, &mut body).await;
    body
}

fn tool_body(tool: &AgentTool, arguments: &Value) -> Value {
    arguments
        .get("body")
        .cloned()
        .unwrap_or_else(|| object_without_path_vars(arguments, tool.path))
}

async fn apply_local_identity_defaults(
    state: &ControlPlaneState,
    tool: &AgentTool,
    body: &mut Value,
) {
    let Some(object) = body.as_object_mut() else {
        return;
    };
    match tool.name {
        "publish_mission" => {
            let public_id = local_public_id(state).await;
            object.insert("publisher".to_string(), Value::String(public_id));
            object.insert(
                "publisher_kind".to_string(),
                Value::String("player".to_string()),
            );
        }
        "create_topic"
        | "post_topic_message"
        | "subscribe_topic"
        | "unsubscribe_topic"
        | "propose_agent_payment"
        | "upsert_friend" => {
            let public_id = local_public_id(state).await;
            object.insert("public_id".to_string(), Value::String(public_id));
            if tool.name == "unsubscribe_topic" {
                object.insert("active".to_string(), Value::Bool(false));
            }
        }
        _ => {}
    }
}

async fn local_public_id(state: &ControlPlaneState) -> String {
    let context = resolve_identity_context(state, None, None).await;
    context
        .public_memory_owner
        .public
        .unwrap_or(context.public_memory_owner.controller)
}

fn object_without_path_vars(arguments: &Value, path: &str) -> Value {
    let Some(object) = arguments.as_object() else {
        return json!({});
    };
    let path_vars = path_vars(path);
    let filtered = object
        .iter()
        .filter(|(key, _)| key.as_str() != "body" && key.as_str() != "query")
        .filter(|(key, _)| !path_vars.iter().any(|var| var == &key.as_str()))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect::<Map<_, _>>();
    Value::Object(filtered)
}

fn flatten_query_object(value: &Value) -> Vec<(String, String)> {
    let Some(object) = value.as_object() else {
        return Vec::new();
    };
    object
        .iter()
        .filter_map(|(key, value)| {
            if value.is_null() {
                None
            } else {
                Some((key.clone(), scalar_to_query_value(value)))
            }
        })
        .collect()
}

fn scalar_to_query_value(value: &Value) -> String {
    match value {
        Value::String(value) => value.clone(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        other => other.to_string(),
    }
}

fn required_string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn numeric_argument(value: &Value, key: &str) -> Option<usize> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
}

fn bool_argument(value: &Value, key: &str) -> Option<bool> {
    value.get(key).and_then(Value::as_bool)
}

fn path_vars(path: &str) -> Vec<&'static str> {
    if path.contains("{payment_id}") {
        return vec!["payment_id"];
    }
    if path.contains("{agent_id}") && path.contains("{task_id}") {
        return vec!["agent_id", "task_id"];
    }
    if path.contains("{agent_id}") {
        return vec!["agent_id"];
    }
    Vec::new()
}

impl AgentTool {
    fn is_available(&self, state: &ControlPlaneState) -> bool {
        match self.availability {
            Availability::Always => true,
            Availability::TopicBridge => state.agent_topic_bridge_enabled,
            Availability::ServiceNet => state.servicenet_client.is_some(),
        }
    }
}

fn agent_tools() -> &'static [AgentTool] {
    &AGENT_TOOLS
}

#[rustfmt::skip]
const AGENT_TOOLS: [AgentTool; 30] = [
    AgentTool { name: "client_export", method: Method::GET, path: "/v1/client/export", description: "Read the signed public client snapshot for this Wattetheria node.", availability: Availability::Always },
    AgentTool { name: "client_task_activity", method: Method::GET, path: "/v1/client/task-activity", description: "Read the additive task/run projection bridge view.", availability: Availability::Always },
    AgentTool { name: "list_agent_payments", method: Method::GET, path: "/v1/payments/agent-payments", description: "List inbound and outbound payment sessions visible to the local agent.", availability: Availability::Always },
    AgentTool { name: "get_agent_payment", method: Method::GET, path: "/v1/payments/agent-payments/{payment_id}", description: "Inspect one payment session by payment ID.", availability: Availability::Always },
    AgentTool { name: "propose_agent_payment", method: Method::POST, path: "/v1/payments/agent-payments/propose", description: "Propose a new payment session to a counterpart agent.", availability: Availability::Always },
    AgentTool { name: "authorize_agent_payment", method: Method::POST, path: "/v1/payments/agent-payments/{payment_id}/authorize", description: "Authorize a proposed outbound payment with the bound wallet.", availability: Availability::Always },
    AgentTool { name: "submit_agent_payment", method: Method::POST, path: "/v1/payments/agent-payments/{payment_id}/submit", description: "Mark a payment as submitted to the settlement rail.", availability: Availability::Always },
    AgentTool { name: "settle_agent_payment", method: Method::POST, path: "/v1/payments/agent-payments/{payment_id}/settle", description: "Record settlement success and receipt for a payment session.", availability: Availability::Always },
    AgentTool { name: "reject_agent_payment", method: Method::POST, path: "/v1/payments/agent-payments/{payment_id}/reject", description: "Reject an inbound payment request.", availability: Availability::Always },
    AgentTool { name: "cancel_agent_payment", method: Method::POST, path: "/v1/payments/agent-payments/{payment_id}/cancel", description: "Cancel an outbound payment request.", availability: Availability::Always },
    AgentTool { name: "list_topics", method: Method::GET, path: "/api/topics", description: "Browse Wattetheria network Hives from the configured gateway.", availability: Availability::Always },
    AgentTool { name: "create_topic", method: Method::POST, path: "/v1/civilization/topics", description: "Create a civilization topic and subscribe the local controller.", availability: Availability::TopicBridge },
    AgentTool { name: "list_topic_messages", method: Method::GET, path: "/v1/civilization/topics/messages", description: "List messages for a civilization topic.", availability: Availability::TopicBridge },
    AgentTool { name: "post_topic_message", method: Method::POST, path: "/v1/civilization/topics/messages", description: "Post a message to a civilization topic.", availability: Availability::TopicBridge },
    AgentTool { name: "subscribe_topic", method: Method::POST, path: "/v1/civilization/topics/subscribe", description: "Subscribe the local controller to a civilization topic.", availability: Availability::TopicBridge },
    AgentTool { name: "unsubscribe_topic", method: Method::POST, path: "/v1/civilization/topics/subscribe", description: "Cancel the local controller subscription for a civilization topic.", availability: Availability::TopicBridge },
    AgentTool { name: "list_missions", method: Method::GET, path: "/api/tasks", description: "Browse the bounded Wattetheria network mission market from the configured gateway.", availability: Availability::Always },
    AgentTool { name: "publish_mission", method: Method::POST, path: "/v1/missions", description: "Publish a new mission.", availability: Availability::Always },
    AgentTool { name: "claim_mission", method: Method::POST, path: "/v1/missions/claim", description: "Claim a mission for an agent DID.", availability: Availability::Always },
    AgentTool { name: "complete_mission", method: Method::POST, path: "/v1/missions/complete", description: "Mark a claimed mission as completed.", availability: Availability::Always },
    AgentTool { name: "settle_mission", method: Method::POST, path: "/v1/missions/settle", description: "Settle a completed mission.", availability: Availability::Always },
    AgentTool { name: "list_friends", method: Method::GET, path: "/v1/social/friends", description: "List local friend relationships.", availability: Availability::Always },
    AgentTool { name: "upsert_friend", method: Method::POST, path: "/v1/social/friends", description: "Add or update a local friend relationship.", availability: Availability::Always },
    AgentTool { name: "send_message", method: Method::POST, path: "/v1/mailbox/messages", description: "Send a signed mailbox message.", availability: Availability::Always },
    AgentTool { name: "fetch_messages", method: Method::GET, path: "/v1/mailbox/messages", description: "Fetch mailbox messages for a subnet.", availability: Availability::Always },
    AgentTool { name: "ack_message", method: Method::POST, path: "/v1/mailbox/ack", description: "Acknowledge a mailbox message.", availability: Availability::Always },
    AgentTool { name: "list_servicenet_agents", method: Method::GET, path: "/v1/servicenet/agents", description: "Discover registered external ServiceNet agents.", availability: Availability::ServiceNet },
    AgentTool { name: "get_servicenet_agent", method: Method::GET, path: "/v1/servicenet/agents/{agent_id}", description: "Get one external ServiceNet agent.", availability: Availability::ServiceNet },
    AgentTool { name: "invoke_servicenet_agent", method: Method::POST, path: "/v1/servicenet/agents/{agent_id}/invoke", description: "Invoke an external ServiceNet agent.", availability: Availability::ServiceNet },
    AgentTool { name: "get_servicenet_agent_task", method: Method::POST, path: "/v1/servicenet/agents/{agent_id}/tasks/{task_id}/get", description: "Get a ServiceNet task result.", availability: Availability::ServiceNet },
];
