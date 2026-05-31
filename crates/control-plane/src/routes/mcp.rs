use axum::Json;
use axum::body::{Body, to_bytes};
use axum::extract::State;
use axum::http::{HeaderMap, Method, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::path::Path;
use std::time::Instant;
use tower::ServiceExt;
use uuid::Uuid;

use crate::auth::{authorize, bearer_token, internal_error, unauthorized};
use crate::diagnostics::{DiagnosticEvent, record_diagnostic};
use crate::routes::identity::resolve_identity_context;
use crate::social_host::{SignedAgentEnvelopeArgs, build_signed_agent_envelope_for_nodes};
use crate::state::ControlPlaneState;
use wattetheria_kernel::payments::{
    stablecoin_amount_from_base_units, stablecoin_amount_to_base_units,
};
use wattetheria_kernel::servicenet::ServiceNetInvokeRequest;

mod collective;
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
const DEFAULT_SERVICENET_AGENT_LIMIT: usize = 50;
const MAX_SERVICENET_AGENT_LIMIT: usize = 100;
const A2A_X402_EXTENSION_URI: &str = "https://github.com/google-a2a/a2a-x402/v0.1";
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
        record_mcp_tool_diagnostic(
            state,
            &json!({}),
            McpToolDiagnosticEvent {
                tool_name: &name,
                level: "warn",
                phase: "tool.call.failed",
                status: "unknown_tool",
                message: format!("MCP tool {name} is unknown"),
                duration_ms: None,
                result_kind: "validation",
            },
        );
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
        record_mcp_tool_diagnostic(
            state,
            &arguments,
            McpToolDiagnosticEvent {
                tool_name: tool.name,
                level: "warn",
                phase: "tool.call.failed",
                status: "invalid_arguments",
                message: format!("MCP tool {} received invalid arguments", tool.name),
                duration_ms: None,
                result_kind: "validation",
            },
        );
        return Ok(tool_error(
            &json!({"error": "tool arguments must be a JSON object"}),
        ));
    }

    let started_at = Instant::now();
    record_mcp_tool_diagnostic(
        state,
        &arguments,
        McpToolDiagnosticEvent {
            tool_name: tool.name,
            level: "info",
            phase: "tool.call.received",
            status: "accepted",
            message: format!("MCP tool {} call received", tool.name),
            duration_ms: None,
            result_kind: "request",
        },
    );

    if let Some(result) = direct_mcp_tool_result(state, auth, tool.name, &arguments).await {
        record_mcp_tool_result(state, tool.name, &arguments, &result, started_at);
        return Ok(result);
    }

    let response = match dispatch_loopback_tool(state.clone(), auth, tool, &arguments).await {
        Ok(response) => response,
        Err(response) => {
            record_mcp_tool_diagnostic(
                state,
                &arguments,
                McpToolDiagnosticEvent {
                    tool_name: tool.name,
                    level: "error",
                    phase: "tool.call.failed",
                    status: "loopback_error",
                    message: format!("MCP tool {} loopback dispatch failed", tool.name),
                    duration_ms: Some(started_at.elapsed().as_millis()),
                    result_kind: "http",
                },
            );
            return Err(response);
        }
    };
    let result = response_to_tool_result(tool.name, response).await;
    record_mcp_tool_result(state, tool.name, &arguments, &result, started_at);
    Ok(result)
}

async fn direct_mcp_tool_result(
    state: &ControlPlaneState,
    auth: &str,
    tool_name: &str,
    arguments: &Value,
) -> Option<Value> {
    match tool_name {
        "list_missions" => Some(network_mission_market_result(state, arguments).await),
        "publish_collective_mission" => {
            Some(collective::publish_collective_mission_result(state, auth, arguments).await)
        }
        "get_collective_mission_result" => {
            Some(collective::get_collective_mission_result(state, arguments).await)
        }
        "list_servicenet_agents" => Some(servicenet_agents_result(state, arguments).await),
        "get_servicenet_agent" => Some(servicenet_agent_result(state, arguments).await),
        "invoke_servicenet_agent_sync" => {
            Some(servicenet_invoke_agent_result(state, arguments, ServiceNetInvokeMode::Sync).await)
        }
        "invoke_servicenet_agent_async" => Some(
            servicenet_invoke_agent_result(state, arguments, ServiceNetInvokeMode::Async).await,
        ),
        "get_servicenet_receipt" => Some(servicenet_receipt_result(state, arguments).await),
        "list_hives" => Some(network_hive_market_result(state, arguments).await),
        _ => None,
    }
}

fn record_mcp_tool_result(
    state: &ControlPlaneState,
    tool_name: &str,
    arguments: &Value,
    result: &Value,
    started_at: Instant,
) {
    let is_error = result
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    record_mcp_tool_diagnostic(
        state,
        arguments,
        McpToolDiagnosticEvent {
            tool_name,
            level: if is_error { "error" } else { "info" },
            phase: if is_error {
                "tool.call.failed"
            } else {
                "tool.call.succeeded"
            },
            status: if is_error { "error" } else { "ok" },
            message: format!(
                "MCP tool {tool_name} {}",
                if is_error { "failed" } else { "succeeded" }
            ),
            duration_ms: Some(started_at.elapsed().as_millis()),
            result_kind: "tool_result",
        },
    );
}

struct McpToolDiagnosticEvent<'a> {
    tool_name: &'a str,
    level: &'static str,
    phase: &'static str,
    status: &'static str,
    message: String,
    duration_ms: Option<u128>,
    result_kind: &'static str,
}

fn record_mcp_tool_diagnostic(
    state: &ControlPlaneState,
    arguments: &Value,
    event: McpToolDiagnosticEvent<'_>,
) {
    let (object_kind, object_id) = mcp_tool_object(arguments);
    record_diagnostic(
        &state.data_dir,
        DiagnosticEvent::new(
            event.level,
            "wattetheria.mcp",
            "tool_call",
            event.phase,
            event.status,
            event.message,
        )
        .object(object_kind.unwrap_or("mcp_tool"), object_id)
        .details(json!({
            "tool_name": event.tool_name,
            "duration_ms": event.duration_ms,
            "result_kind": event.result_kind,
            "argument_keys": mcp_argument_keys(arguments),
            "identifiers": mcp_argument_identifiers(arguments),
        })),
    );
}

fn mcp_tool_object(arguments: &Value) -> (Option<&'static str>, Option<String>) {
    [
        ("mission", "mission_id"),
        ("task", "task_id"),
        ("hive", "hive_id"),
        ("hive", "topic_id"),
        ("hive", "feed_key"),
        ("payment", "payment_id"),
        ("message", "message_id"),
        ("friend_request", "request_id"),
        ("friend", "counterpart_public_id"),
        ("node", "remote_node_id"),
        ("subnet", "subnet_id"),
    ]
    .into_iter()
    .find_map(|(kind, key)| {
        arguments
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| (Some(kind), Some(value.to_owned())))
    })
    .unwrap_or((None, None))
}

fn mcp_argument_keys(arguments: &Value) -> Vec<String> {
    arguments
        .as_object()
        .map(|object| object.keys().cloned().collect())
        .unwrap_or_default()
}

fn mcp_argument_identifiers(arguments: &Value) -> Map<String, Value> {
    let mut identifiers = Map::new();
    for key in [
        "mission_id",
        "task_id",
        "hive_id",
        "topic_id",
        "feed_key",
        "scope_hint",
        "mission_scope_hint",
        "payment_id",
        "message_id",
        "request_id",
        "counterpart_public_id",
        "remote_node_id",
        "subnet_id",
        "agent_did",
    ] {
        if let Some(value) = arguments.get(key) {
            identifiers.insert(key.to_owned(), value.clone());
        }
    }
    identifiers
}

async fn network_hive_market_result(state: &ControlPlaneState, arguments: &Value) -> Value {
    match network_hive_market_payload(state, arguments).await {
        Ok(payload) => tool_success(&payload),
        Err(error) => tool_error(&json!({"error": error.to_string()})),
    }
}

async fn network_hive_market_payload(
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
    let gateway_endpoint = normalized_gateway_hives_url(&gateway_url);
    let hives = fetch_gateway_hives(&gateway_endpoint, fetch_limit).await?;
    let all_hives = hives
        .into_iter()
        .filter(|hive| matches_hive_filters(hive, arguments))
        .collect::<Vec<_>>();
    let page = all_hives
        .iter()
        .skip(offset)
        .take(limit)
        .cloned()
        .map(normalize_gateway_hive)
        .collect::<Vec<_>>();
    let next_offset = offset + page.len();
    let has_more = next_offset < all_hives.len();

    Ok(json!({
        "source": "wattetheria-gateway.api_hives",
        "scope": "network",
        "gateway_url": gateway_url,
        "gateway_endpoint": gateway_endpoint,
        "pagination": "gateway_limit_client_offset",
        "limit": limit,
        "offset": offset,
        "next_offset": if has_more { Some(next_offset) } else { None },
        "has_more": has_more,
        "known_count": all_hives.len(),
        "hives": page,
    }))
}

async fn network_mission_market_result(state: &ControlPlaneState, arguments: &Value) -> Value {
    match network_mission_market_payload(state, arguments).await {
        Ok(payload) => tool_success(&payload),
        Err(error) => tool_error(&json!({"error": error.to_string()})),
    }
}

async fn servicenet_agents_result(state: &ControlPlaneState, arguments: &Value) -> Value {
    let Some(client) = state.servicenet_client.as_deref() else {
        return tool_error(&json!({"error": "servicenet is not configured"}));
    };
    let limit = numeric_argument(arguments, "limit")
        .unwrap_or(DEFAULT_SERVICENET_AGENT_LIMIT)
        .clamp(1, MAX_SERVICENET_AGENT_LIMIT);
    let offset = numeric_argument(arguments, "offset").unwrap_or(0);
    let agents = match client.list_agents(limit, offset).await {
        Ok(response) => response,
        Err(error) => return tool_error(&json!({"error": error.to_string()})),
    };
    let health = match client.list_agent_health().await {
        Ok(items) => items,
        Err(error) => return tool_error(&json!({"error": error.to_string()})),
    };
    let trust = match client.list_agent_trust().await {
        Ok(items) => items,
        Err(error) => return tool_error(&json!({"error": error.to_string()})),
    };
    let items = servicenet_agent_list_summaries(&agents.items, health, trust);
    tool_success(&json!({
        "items": items,
        "count": agents.count,
        "limit": agents.limit,
        "offset": agents.offset,
        "next_offset": agents.next_offset,
        "has_more": agents.has_more,
        "known_count": agents.known_count,
    }))
}

async fn servicenet_agent_result(state: &ControlPlaneState, arguments: &Value) -> Value {
    let Some(client) = state.servicenet_client.as_deref() else {
        return tool_error(&json!({"error": "servicenet is not configured"}));
    };
    let Some(agent_id) = required_string(arguments, "agent_id") else {
        return tool_error(&json!({"error": "agent_id is required"}));
    };
    let agent = match client.get_agent(&agent_id).await {
        Ok(item) => item,
        Err(error) => return tool_error(&json!({"error": error.to_string()})),
    };
    let health = match client.list_agent_health().await {
        Ok(items) => items,
        Err(error) => return tool_error(&json!({"error": error.to_string()})),
    };
    let trust = match client.list_agent_trust().await {
        Ok(items) => items,
        Err(error) => return tool_error(&json!({"error": error.to_string()})),
    };
    let health_by_agent = servicenet_records_by_agent_id(health);
    let trust_by_agent = servicenet_records_by_agent_id(trust);
    tool_success(&servicenet_agent_detail_summary(
        &agent,
        &health_by_agent,
        &trust_by_agent,
    ))
}

#[derive(Debug, Clone, Copy)]
enum ServiceNetInvokeMode {
    Sync,
    Async,
}

async fn servicenet_invoke_agent_result(
    state: &ControlPlaneState,
    arguments: &Value,
    mode: ServiceNetInvokeMode,
) -> Value {
    let Some(client) = state.servicenet_client.as_deref() else {
        return tool_error(&json!({"error": "servicenet is not configured"}));
    };
    let Some(agent_id) = required_string(arguments, "agent_id") else {
        return tool_error(&json!({"error": "agent_id is required"}));
    };
    let agent = match client.get_agent(&agent_id).await {
        Ok(item) => item,
        Err(error) => return tool_error(&json!({"error": error.to_string()})),
    };
    let mut body = arguments
        .get("body")
        .cloned()
        .unwrap_or_else(|| object_without_path_vars(arguments, servicenet_invoke_tool_path(mode)));
    if !body.is_object() {
        return tool_error(&json!({"error": "invoke body must be a JSON object"}));
    }
    let envelope = match servicenet_invoke_agent_envelope(state, &agent_id, &body).await {
        Ok(envelope) => envelope,
        Err(error) => return tool_error(&json!({"error": error.to_string()})),
    };
    if let Some(object) = body.as_object_mut() {
        object.insert("agent_envelope".to_owned(), envelope);
    }
    if servicenet_agent_requires_auth(&agent)
        && body
            .get("auth_token")
            .and_then(Value::as_str)
            .is_none_or(|value| value.trim().is_empty())
        && body.get("auth_context_id").is_none()
    {
        return tool_success(&servicenet_auth_consent_payload(&agent_id, &agent));
    }
    let request = match serde_json::from_value::<ServiceNetInvokeRequest>(body) {
        Ok(request) => request,
        Err(error) => return tool_error(&json!({"error": error.to_string()})),
    };
    let response = match mode {
        ServiceNetInvokeMode::Sync => client.invoke_agent(&agent_id, &request).await,
        ServiceNetInvokeMode::Async => client.invoke_agent_async(&agent_id, &request).await,
    };
    match response {
        Ok(response) => tool_success(&serde_json::to_value(response).unwrap_or(Value::Null)),
        Err(error) => tool_error(&json!({"error": error.to_string()})),
    }
}

fn servicenet_invoke_tool_path(mode: ServiceNetInvokeMode) -> &'static str {
    match mode {
        ServiceNetInvokeMode::Sync => "/v1/wattetheria/servicenet/agents/{agent_id}/invoke",
        ServiceNetInvokeMode::Async => "/v1/wattetheria/servicenet/agents/{agent_id}/invoke-async",
    }
}

async fn servicenet_receipt_result(state: &ControlPlaneState, arguments: &Value) -> Value {
    let Some(client) = state.servicenet_client.as_deref() else {
        return tool_error(&json!({"error": "servicenet is not configured"}));
    };
    let Some(receipt_id) = required_string(arguments, "receipt_id") else {
        return tool_error(&json!({"error": "receipt_id is required"}));
    };
    let receipt_id = match Uuid::parse_str(&receipt_id) {
        Ok(receipt_id) => receipt_id,
        Err(error) => return tool_error(&json!({"error": error.to_string()})),
    };
    match client.get_receipt(&receipt_id).await {
        Ok(response) => tool_success(&response),
        Err(error) => tool_error(&json!({"error": error.to_string()})),
    }
}

async fn servicenet_invoke_agent_envelope(
    state: &ControlPlaneState,
    agent_id: &str,
    body: &Value,
) -> anyhow::Result<Value> {
    let source_node_id = state.swarm_bridge.local_node_id().await.ok();
    let envelope = build_signed_agent_envelope_for_nodes(
        state,
        SignedAgentEnvelopeArgs {
            source_agent_id: state.agent_did.clone(),
            target_agent_id: Some(agent_id.to_owned()),
            source_node_id,
            target_node_id: None,
            capability: "servicenet.agents.invoke".to_owned(),
            message: servicenet_invoke_envelope_message(body),
            extensions: None,
        },
    )?;
    Ok(serde_json::to_value(envelope)?)
}

fn servicenet_invoke_envelope_message(body: &Value) -> Value {
    let mut message = body.clone();
    if let Some(object) = message.as_object_mut() {
        object.remove("auth_token");
        object.remove("auth_context_id");
        object.remove("agent_envelope");
    }
    message
}

fn servicenet_agent_requires_auth(agent: &Value) -> bool {
    let Some(agent_card) = value_at(agent, &["agent_card"]) else {
        return false;
    };
    match value_at(agent_card, &["security"]) {
        Some(Value::Array(items)) => {
            !items.is_empty() && !items.iter().any(security_requirement_allows_none)
        }
        Some(Value::Object(map)) => !map.is_empty() && !map.contains_key("none"),
        Some(Value::Null) | None => security_schemes_require_auth(agent_card),
        Some(_) => true,
    }
}

fn security_requirement_allows_none(requirement: &Value) -> bool {
    requirement
        .as_object()
        .is_some_and(|object| object.contains_key("none"))
}

fn security_schemes_require_auth(agent_card: &Value) -> bool {
    value_at(agent_card, &["securitySchemes"])
        .and_then(Value::as_object)
        .is_some_and(|schemes| {
            !schemes.is_empty()
                && !schemes.iter().all(|(name, scheme)| {
                    name == "none"
                        || value_at(scheme, &["type"]).and_then(Value::as_str) == Some("none")
                })
        })
}

fn servicenet_auth_consent_payload(agent_id: &str, agent: &Value) -> Value {
    let agent_card = value_at(agent, &["agent_card"]).unwrap_or(&Value::Null);
    json!({
        "status": "auth_required",
        "agent_id": agent_id,
        "authorizationUrl": oauth_flow_field(agent_card, "authorizationUrl").cloned().unwrap_or(Value::Null),
        "tokenUrl": oauth_flow_field(agent_card, "tokenUrl").cloned().unwrap_or(Value::Null),
        "refreshUrl": oauth_flow_field(agent_card, "refreshUrl").cloned().unwrap_or(Value::Null),
        "scopes": oauth_flow_field(agent_card, "scopes").cloned().unwrap_or(Value::Null),
        "securitySchemes": value_at(agent_card, &["securitySchemes"]).cloned().unwrap_or(Value::Null),
        "security": value_at(agent_card, &["security"]).cloned().unwrap_or(Value::Null),
    })
}

fn oauth_flow_field<'a>(agent_card: &'a Value, field: &str) -> Option<&'a Value> {
    value_at(agent_card, &["securitySchemes"])
        .and_then(Value::as_object)?
        .values()
        .find_map(|scheme| {
            value_at(
                scheme,
                &["oauth2SecurityScheme", "flows", "authorizationCode", field],
            )
            .or_else(|| value_at(scheme, &["flows", "authorizationCode", field]))
        })
}

fn servicenet_agent_list_summaries(
    agents: &[Value],
    health: Vec<Value>,
    trust: Vec<Value>,
) -> Vec<Value> {
    let health_by_agent = servicenet_records_by_agent_id(health);
    let trust_by_agent = servicenet_records_by_agent_id(trust);
    agents
        .iter()
        .map(|agent| servicenet_agent_list_summary(agent, &health_by_agent, &trust_by_agent))
        .collect()
}

fn servicenet_agent_detail_summary(
    agent: &Value,
    health_by_agent: &BTreeMap<String, Value>,
    trust_by_agent: &BTreeMap<String, Value>,
) -> Value {
    let mut summary = servicenet_agent_list_summary(agent, health_by_agent, trust_by_agent);
    if let Some(object) = summary.as_object_mut() {
        object.insert("skills".to_owned(), json!(servicenet_agent_skills(agent)));
        object.insert(
            "supportsTask".to_owned(),
            json!(
                value_at(agent, &["agent_card", "supportsTask"])
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            ),
        );
        if let Some(payment) = servicenet_agent_payment(agent) {
            object.insert("payment".to_owned(), payment);
        }
    }
    summary
}

fn servicenet_agent_payment(agent: &Value) -> Option<Value> {
    let extensions = value_at(agent, &["agent_card", "capabilities", "extensions"])?.as_array()?;
    extensions
        .iter()
        .find(|extension| {
            value_at(extension, &["uri"]).and_then(Value::as_str) == Some(A2A_X402_EXTENSION_URI)
                && x402_extension_has_pay_to(extension)
        })
        .cloned()
}

fn x402_extension_has_pay_to(extension: &Value) -> bool {
    value_at(extension, &["params", "accepts"])
        .and_then(Value::as_array)
        .is_some_and(|accepts| {
            accepts.iter().any(|accept| {
                value_at(accept, &["payTo"])
                    .and_then(Value::as_str)
                    .is_some_and(|pay_to| !pay_to.trim().is_empty())
            })
        })
}

fn servicenet_records_by_agent_id(items: Vec<Value>) -> BTreeMap<String, Value> {
    items
        .into_iter()
        .filter_map(|item| {
            let agent_id = item.get("agent_id")?.as_str()?.to_owned();
            Some((agent_id, item))
        })
        .collect()
}

fn servicenet_agent_list_summary(
    agent: &Value,
    health_by_agent: &BTreeMap<String, Value>,
    trust_by_agent: &BTreeMap<String, Value>,
) -> Value {
    let agent_id = field_str(agent, &["agent_id"]).unwrap_or_default();
    let health = health_by_agent.get(agent_id);
    let trust = trust_by_agent.get(agent_id);
    json!({
        "agent_id": value_at(agent, &["agent_id"]).cloned().unwrap_or(Value::Null),
        "name": value_at(agent, &["agent_card", "name"]).cloned().unwrap_or(Value::Null),
        "description": value_at(agent, &["agent_card", "description"]).cloned().unwrap_or(Value::Null),
        "status": servicenet_agent_status(health),
        "version": value_at(agent, &["version"]).cloned().unwrap_or(Value::Null),
        "provider_id": value_at(agent, &["provider_id"]).cloned().unwrap_or(Value::Null),
        "runtime": value_at(agent, &["deployment", "runtime"]).cloned().unwrap_or(Value::Null),
        "protocol": servicenet_agent_protocol(agent),
        "risk_level": value_at(agent, &["review", "risk_level"]).cloned().unwrap_or(Value::Null),
        "reputation_score": servicenet_reputation_score(trust),
        "cost": value_at(agent, &["agent_card", "cost"]).cloned().unwrap_or(Value::Null),
        "currency": value_at(agent, &["agent_card", "currency"]).cloned().unwrap_or(Value::Null),
    })
}

fn servicenet_agent_status(health: Option<&Value>) -> Value {
    match health.and_then(|record| field_str(record, &["status"])) {
        Some("unknown") => json!("published"),
        Some(status) => json!(status),
        None => Value::Null,
    }
}

fn servicenet_reputation_score(trust: Option<&Value>) -> Value {
    trust
        .and_then(|record| value_at(record, &["reputation_score"]))
        .and_then(Value::as_f64)
        .map_or(Value::Null, |score| json!(score * 1000.0))
}

fn servicenet_agent_protocol(agent: &Value) -> Value {
    let interaction_protocol =
        field_str(agent, &["deployment", "endpoint", "interaction_protocol"]);
    let protocol_binding = field_str(agent, &["deployment", "endpoint", "protocol_binding"]);
    match (interaction_protocol, protocol_binding) {
        (Some(interaction_protocol), Some(protocol_binding)) => {
            json!(format!("{interaction_protocol} / {protocol_binding}"))
        }
        (Some(interaction_protocol), None) => json!(interaction_protocol),
        (None, Some(protocol_binding)) => json!(protocol_binding),
        (None, None) => Value::Null,
    }
}

fn servicenet_agent_skills(agent: &Value) -> Vec<Value> {
    value_at(agent, &["agent_card", "skills"])
        .and_then(Value::as_array)
        .map(|skills| {
            skills
                .iter()
                .map(servicenet_agent_skill)
                .filter(|skill| skill.as_object().is_some_and(|object| !object.is_empty()))
                .collect()
        })
        .unwrap_or_default()
}

fn servicenet_agent_skill(skill: &Value) -> Value {
    let mut item = Map::new();
    for field in ["name", "description"] {
        if let Some(value) = value_at(skill, &[field]) {
            item.insert(field.to_owned(), value.clone());
        }
    }
    Value::Object(item)
}

fn value_at<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut current = value;
    for segment in path {
        current = current.get(*segment)?;
    }
    Some(current)
}

fn field_str<'a>(value: &'a Value, path: &[&str]) -> Option<&'a str> {
    value_at(value, path).and_then(Value::as_str)
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
        "source": "wattetheria-gateway.api_missions",
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
    fetch_gateway_array(gateway_endpoint, limit, "/api/missions").await
}

async fn fetch_gateway_hives(gateway_endpoint: &str, limit: usize) -> anyhow::Result<Vec<Value>> {
    fetch_gateway_array(gateway_endpoint, limit, "/api/hives").await
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

fn matches_hive_filters(hive: &Value, arguments: &Value) -> bool {
    matches_optional_gateway_string(
        hive,
        &[&["network_id"], &["networkId"]],
        arguments,
        "network_id",
    ) && matches_optional_gateway_string(
        hive,
        &[&["hive_id"], &["topic_id"], &["id"]],
        arguments,
        "hive_id",
    ) && matches_optional_gateway_string(
        hive,
        &[&["topic_id"], &["hive_id"], &["id"]],
        arguments,
        "topic_id",
    ) && matches_optional_gateway_string(
        hive,
        &[&["organization_id"], &["organizationId"]],
        arguments,
        "organization_id",
    ) && matches_optional_gateway_string(
        hive,
        &[&["mission_id"], &["missionId"]],
        arguments,
        "mission_id",
    ) && matches_optional_gateway_string(
        hive,
        &[&["projection_kind"], &["kind"]],
        arguments,
        "projection_kind",
    ) && (bool_argument(arguments, "include_inactive").unwrap_or(false)
        || gateway_hive_active(hive))
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

fn gateway_hive_active(hive: &Value) -> bool {
    match hive.get("active").and_then(Value::as_bool) {
        Some(active) => active,
        None => hive
            .get("status")
            .and_then(Value::as_str)
            .is_none_or(|status| status != "inactive"),
    }
}

fn normalize_gateway_hive(mut hive: Value) -> Value {
    let subscribe_route = gateway_hive_subscribe_route(&hive);
    let hive_id = gateway_task_string(&hive, &[&["hive_id"], &["topic_id"], &["id"]]);
    let display_name = gateway_task_string(
        &hive,
        &[
            &["display_name"],
            &["title"],
            &["name"],
            &["hive_id"],
            &["topic_id"],
            &["id"],
        ],
    );
    let Some(object) = hive.as_object_mut() else {
        return hive;
    };
    if let Some(hive_id) = hive_id {
        object
            .entry("hive_id".to_string())
            .or_insert_with(|| Value::String(hive_id.clone()));
        object
            .entry("topic_id".to_string())
            .or_insert_with(|| Value::String(hive_id));
    }
    if let Some(display_name) = display_name {
        object
            .entry("display_name".to_string())
            .or_insert_with(|| Value::String(display_name));
    }
    if let Some(route) = subscribe_route.as_object() {
        for key in ["network_id", "feed_key", "scope_hint"] {
            if let Some(value) = route.get(key) {
                object
                    .entry(key.to_string())
                    .or_insert_with(|| value.clone());
            }
        }
    }
    object.insert("subscribe_route".to_string(), subscribe_route);
    hive
}

fn gateway_hive_subscribe_route(hive: &Value) -> Value {
    let network_id = gateway_task_string(
        hive,
        &[
            &["network_id"],
            &["networkId"],
            &["summary", "network_id"],
            &["inputs", "network_id"],
        ],
    );
    let feed_key = gateway_task_string(
        hive,
        &[
            &["feed_key"],
            &["summary", "feed_key"],
            &["inputs", "feed_key"],
        ],
    );
    let scope_hint = gateway_task_string(
        hive,
        &[
            &["scope_hint"],
            &["summary", "scope_hint"],
            &["inputs", "scope_hint"],
        ],
    );
    let subscribe_ready = feed_key.is_some() && scope_hint.is_some();
    json!({
        "network_id": network_id,
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
        object.insert("status".to_string(), Value::String(status));
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
    normalized_gateway_api_resource_url(gateway_url, "/api/missions", "/v1/wattetheria/missions")
}

fn normalized_gateway_hives_url(gateway_url: &str) -> String {
    normalized_gateway_api_resource_url(gateway_url, "/api/hives", "/v1/wattetheria/hives")
}

fn normalized_gateway_api_resource_url(
    gateway_url: &str,
    resource_path: &str,
    legacy_resource_path: &str,
) -> String {
    let trimmed = gateway_url.trim_end_matches('/');
    let base = trimmed
        .strip_suffix(resource_path)
        .or_else(|| trimmed.strip_suffix(legacy_resource_path))
        .or_else(|| trimmed.strip_suffix("/api"))
        .unwrap_or(trimmed)
        .trim_end_matches('/');
    format!("{base}{resource_path}")
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
        let body = match tool_body_with_local_identity(&state, tool, arguments).await {
            Ok(body) => body,
            Err(error) => {
                return Ok((StatusCode::BAD_REQUEST, Json(json!({"error": error}))).into_response());
            }
        };
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

async fn response_to_tool_result(tool_name: &str, response: Response) -> Value {
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
    let payload = present_tool_response_payload(tool_name, payload);
    let structured_content = structured_content_payload(&payload);
    let text = serde_json::to_string_pretty(&structured_content)
        .unwrap_or_else(|_| structured_content.to_string());
    json!({
        "content": [
            {
                "type": "text",
                "text": text
            }
        ],
        "structuredContent": structured_content,
        "isError": !status.is_success(),
        "_meta": {
            "httpStatus": status.as_u16()
        }
    })
}

fn tool_error(payload: &Value) -> Value {
    let structured_content = structured_content_payload(payload);
    let text = serde_json::to_string_pretty(&structured_content)
        .unwrap_or_else(|_| structured_content.to_string());
    json!({
        "content": [
            {
                "type": "text",
                "text": text
            }
        ],
        "structuredContent": structured_content,
        "isError": true
    })
}

fn tool_success(payload: &Value) -> Value {
    let structured_content = structured_content_payload(payload);
    let text = serde_json::to_string_pretty(&structured_content)
        .unwrap_or_else(|_| structured_content.to_string());
    json!({
        "content": [
            {
                "type": "text",
                "text": text
            }
        ],
        "structuredContent": structured_content,
        "isError": false
    })
}

fn structured_content_payload(payload: &Value) -> Value {
    match payload {
        Value::Object(_) => payload.clone(),
        Value::Array(_) => json!({ "items": payload }),
        Value::Null => json!({}),
        _ => json!({ "value": payload }),
    }
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
                "toolName": tool.name,
                "method": tool.method.as_str(),
                "path": tool.path,
                "available": available,
                "source": "wattetheria.mcp.tools.v1"
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
) -> Result<Value, String> {
    let mut body = tool_body(tool, arguments);
    apply_local_identity_defaults(state, tool, &mut body).await;
    normalize_mcp_tool_body(tool, &mut body)?;
    Ok(body)
}

fn normalize_mcp_tool_body(tool: &AgentTool, body: &mut Value) -> Result<(), String> {
    if tool.name == "propose_agent_payment" {
        normalize_mcp_payment_request_amount(body)?;
    }
    Ok(())
}

fn normalize_mcp_payment_request_amount(body: &mut Value) -> Result<(), String> {
    let Some(object) = body.as_object_mut() else {
        return Ok(());
    };
    let Some(currency) = object.get("currency").and_then(Value::as_str) else {
        return Ok(());
    };
    let Some(amount) = object.get("amount").and_then(Value::as_str) else {
        return Ok(());
    };
    if let Some(base_units) =
        stablecoin_amount_to_base_units(amount, currency).map_err(|error| error.to_string())?
    {
        object.insert("amount".to_string(), Value::String(base_units));
    }
    Ok(())
}

fn present_tool_response_payload(tool_name: &str, mut payload: Value) -> Value {
    if is_agent_payment_tool(tool_name) {
        present_payment_amounts(&mut payload, None);
    }
    payload
}

fn is_agent_payment_tool(tool_name: &str) -> bool {
    matches!(
        tool_name,
        "list_agent_payments"
            | "get_agent_payment"
            | "propose_agent_payment"
            | "authorize_agent_payment"
            | "submit_agent_payment"
            | "settle_agent_payment"
            | "reject_agent_payment"
            | "cancel_agent_payment"
    )
}

fn present_payment_amounts(value: &mut Value, inherited_currency: Option<&str>) {
    match value {
        Value::Object(object) => {
            let currency = object
                .get("currency")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
                .or_else(|| inherited_currency.map(ToOwned::to_owned));
            if let (Some(amount), Some(currency)) = (
                object.get("amount").and_then(Value::as_str),
                currency.as_deref(),
            ) && let Some(display_amount) = stablecoin_amount_from_base_units(amount, currency)
            {
                object.insert("amount".to_string(), Value::String(display_amount));
            }
            object
                .values_mut()
                .for_each(|value| present_payment_amounts(value, currency.as_deref()));
        }
        Value::Array(items) => items
            .iter_mut()
            .for_each(|value| present_payment_amounts(value, inherited_currency)),
        _ => {}
    }
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
        "create_hive"
        | "post_hive_message"
        | "subscribe_hive"
        | "unsubscribe_hive"
        | "propose_agent_payment"
        | "upsert_local_friend"
        | "send_agent_dm_message"
        | "accept_friend_request"
        | "reject_friend_request"
        | "request_agent_friend" => {
            let public_id = local_public_id(state).await;
            object.insert("public_id".to_string(), Value::String(public_id));
            if tool.name == "request_agent_friend" {
                object.insert("action".to_string(), Value::String("request".to_string()));
                if !object.contains_key("counterpart_public_id")
                    && object
                        .get("target_agent_did")
                        .and_then(Value::as_str)
                        .is_none_or(|value| value.trim().is_empty())
                    && let Some(remote_node_id) = object
                        .get("remote_node_id")
                        .and_then(Value::as_str)
                        .map(str::trim)
                        .filter(|value| !value.is_empty())
                {
                    object.insert(
                        "counterpart_public_id".to_string(),
                        Value::String(remote_node_id.to_string()),
                    );
                }
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
    if path.contains("{request_id}") {
        return vec!["request_id"];
    }
    if path.contains("{mission_id}") {
        return vec!["mission_id"];
    }
    if path.contains("{hive_id}") {
        return vec!["hive_id"];
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
const AGENT_TOOLS: [AgentTool; 44] = [
    AgentTool { name: "client_export", method: Method::GET, path: "/v1/wattetheria/client/export", description: "Read the signed public client snapshot for this Wattetheria node.", availability: Availability::Always },
    AgentTool { name: "client_task_activity", method: Method::GET, path: "/v1/wattetheria/client/task-activity", description: "Read the additive task/run projection bridge view.", availability: Availability::Always },
    AgentTool { name: "list_agent_payments", method: Method::GET, path: "/v1/wattetheria/payments/agent-payments", description: "List inbound and outbound payment sessions visible to the local agent.", availability: Availability::Always },
    AgentTool { name: "get_agent_payment", method: Method::GET, path: "/v1/wattetheria/payments/agent-payments/{payment_id}", description: "Inspect one payment session by payment ID.", availability: Availability::Always },
    AgentTool { name: "propose_agent_payment", method: Method::POST, path: "/v1/wattetheria/payments/agent-payments/propose", description: "Propose a new payment session to a counterpart agent.", availability: Availability::Always },
    AgentTool { name: "authorize_agent_payment", method: Method::POST, path: "/v1/wattetheria/payments/agent-payments/{payment_id}/authorize", description: "Authorize a proposed outbound payment with the bound wallet.", availability: Availability::Always },
    AgentTool { name: "submit_agent_payment", method: Method::POST, path: "/v1/wattetheria/payments/agent-payments/{payment_id}/submit", description: "Mark a payment as submitted to the settlement rail.", availability: Availability::Always },
    AgentTool { name: "settle_agent_payment", method: Method::POST, path: "/v1/wattetheria/payments/agent-payments/{payment_id}/settle", description: "Record settlement success and receipt for a payment session.", availability: Availability::Always },
    AgentTool { name: "reject_agent_payment", method: Method::POST, path: "/v1/wattetheria/payments/agent-payments/{payment_id}/reject", description: "Reject an inbound payment request.", availability: Availability::Always },
    AgentTool { name: "cancel_agent_payment", method: Method::POST, path: "/v1/wattetheria/payments/agent-payments/{payment_id}/cancel", description: "Cancel an outbound payment request.", availability: Availability::Always },
    AgentTool { name: "list_hives", method: Method::GET, path: "/api/hives", description: "Browse Wattetheria network Hives from the configured gateway.", availability: Availability::Always },
    AgentTool { name: "create_hive", method: Method::POST, path: "/v1/wattetheria/hives", description: "Create a Wattetheria Hive and subscribe the local controller.", availability: Availability::TopicBridge },
    AgentTool { name: "list_hive_messages", method: Method::GET, path: "/v1/wattetheria/hives/{hive_id}/messages", description: "List messages for a Wattetheria Hive.", availability: Availability::TopicBridge },
    AgentTool { name: "post_hive_message", method: Method::POST, path: "/v1/wattetheria/hives/{hive_id}/messages", description: "Post a message to a Wattetheria Hive.", availability: Availability::TopicBridge },
    AgentTool { name: "subscribe_hive", method: Method::POST, path: "/v1/wattetheria/hives/{hive_id}/subscribe", description: "Subscribe the local controller to a Wattetheria Hive.", availability: Availability::TopicBridge },
    AgentTool { name: "unsubscribe_hive", method: Method::POST, path: "/v1/wattetheria/hives/{hive_id}/unsubscribe", description: "Cancel the local controller subscription for a Wattetheria Hive.", availability: Availability::TopicBridge },
    AgentTool { name: "list_missions", method: Method::GET, path: "/api/missions", description: "Browse the bounded Wattetheria network mission market from the configured gateway.", availability: Availability::Always },
    AgentTool { name: "publish_mission", method: Method::POST, path: "/v1/wattetheria/missions", description: "Publish a new mission.", availability: Availability::Always },
    AgentTool { name: "publish_collective_mission", method: Method::POST, path: "/v1/wattetheria/collective-missions", description: "Publish a mission and submit a Wattswarm run-queue collective task with kickoff enabled by default.", availability: Availability::Always },
    AgentTool { name: "get_collective_mission_result", method: Method::GET, path: "/v1/wattetheria/collective-missions/result", description: "Read the Wattswarm run result linked to a collective Wattetheria mission.", availability: Availability::Always },
    AgentTool { name: "claim_mission", method: Method::POST, path: "/v1/wattetheria/missions/{mission_id}/claim", description: "Claim a mission for an agent DID.", availability: Availability::Always },
    AgentTool { name: "complete_mission", method: Method::POST, path: "/v1/wattetheria/missions/{mission_id}/complete", description: "Mark a claimed mission as completed.", availability: Availability::Always },
    AgentTool { name: "settle_mission", method: Method::POST, path: "/v1/wattetheria/missions/{mission_id}/settle", description: "Settle a completed mission.", availability: Availability::Always },
    AgentTool { name: "list_friends", method: Method::GET, path: "/v1/wattetheria/social/agent-friends", description: "List accepted agent friend relationships.", availability: Availability::Always },
    AgentTool { name: "upsert_local_friend", method: Method::POST, path: "/v1/wattetheria/social/friends", description: "Add or update a local-only Wattetheria friend relationship without notifying the remote node.", availability: Availability::Always },
    AgentTool { name: "list_nearby", method: Method::GET, path: "/v1/wattetheria/social/nearby", description: "List nearby Wattswarm/Iroh peer nodes visible to this Wattetheria node.", availability: Availability::Always },
    AgentTool { name: "list_friend_requests", method: Method::GET, path: "/v1/wattetheria/social/friend-requests", description: "List inbound pending friend requests awaiting local approval.", availability: Availability::Always },
    AgentTool { name: "list_sent_friend_requests", method: Method::GET, path: "/v1/wattetheria/social/sent-friend-requests", description: "List outbound friend requests sent by this local agent.", availability: Availability::Always },
    AgentTool { name: "get_friend_request", method: Method::GET, path: "/v1/wattetheria/social/friend-requests/{request_id}", description: "Get one friend request with agent, message, and network details.", availability: Availability::Always },
    AgentTool { name: "accept_friend_request", method: Method::POST, path: "/v1/wattetheria/social/friend-requests/{request_id}/accept", description: "Accept an inbound pending friend request over Wattswarm/Iroh.", availability: Availability::Always },
    AgentTool { name: "reject_friend_request", method: Method::POST, path: "/v1/wattetheria/social/friend-requests/{request_id}/reject", description: "Reject an inbound pending friend request over Wattswarm/Iroh.", availability: Availability::Always },
    AgentTool { name: "request_agent_friend", method: Method::POST, path: "/v1/wattetheria/social/agent-friends", description: "Send a signed friend request to a discovered or known agent node over Wattswarm/Iroh.", availability: Availability::Always },
    AgentTool { name: "list_agent_dm_threads", method: Method::GET, path: "/v1/wattetheria/social/agent-dm/threads", description: "List one-to-one agent direct message threads.", availability: Availability::Always },
    AgentTool { name: "list_agent_dm_messages", method: Method::GET, path: "/v1/wattetheria/social/agent-dm/messages", description: "List messages in one-to-one agent direct message threads.", availability: Availability::Always },
    AgentTool { name: "send_agent_dm_message", method: Method::POST, path: "/v1/wattetheria/social/agent-dm/messages", description: "Send a signed one-to-one direct message to an accepted agent friend.", availability: Availability::Always },
    AgentTool { name: "send_mailbox_message", method: Method::POST, path: "/v1/wattetheria/mailbox/messages", description: "Send a signed mailbox message.", availability: Availability::Always },
    AgentTool { name: "list_mailbox_messages", method: Method::GET, path: "/v1/wattetheria/mailbox/messages", description: "List mailbox messages for a subnet.", availability: Availability::Always },
    AgentTool { name: "ack_mailbox_message", method: Method::POST, path: "/v1/wattetheria/mailbox/ack", description: "Acknowledge a mailbox message.", availability: Availability::Always },
    AgentTool { name: "list_servicenet_agents", method: Method::GET, path: "/v1/wattetheria/servicenet/agents", description: "Discover registered external ServiceNet agents.", availability: Availability::ServiceNet },
    AgentTool { name: "get_servicenet_agent", method: Method::GET, path: "/v1/wattetheria/servicenet/agents/{agent_id}", description: "Get one external ServiceNet agent.", availability: Availability::ServiceNet },
    AgentTool { name: "invoke_servicenet_agent_sync", method: Method::POST, path: "/v1/wattetheria/servicenet/agents/{agent_id}/invoke", description: "Synchronously invoke an external ServiceNet agent.", availability: Availability::ServiceNet },
    AgentTool { name: "invoke_servicenet_agent_async", method: Method::POST, path: "/v1/wattetheria/servicenet/agents/{agent_id}/invoke-async", description: "Submit an external ServiceNet agent invocation and poll the returned receipt.", availability: Availability::ServiceNet },
    AgentTool { name: "get_servicenet_agent_task", method: Method::POST, path: "/v1/wattetheria/servicenet/agents/{agent_id}/tasks/{task_id}/get", description: "Get a ServiceNet task result.", availability: Availability::ServiceNet },
    AgentTool { name: "get_servicenet_receipt", method: Method::GET, path: "/v1/wattetheria/servicenet/receipts/{receipt_id}", description: "Get a ServiceNet invocation receipt.", availability: Availability::ServiceNet },
];
