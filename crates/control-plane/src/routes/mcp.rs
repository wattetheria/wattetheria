use axum::Json;
use axum::body::{Body, to_bytes};
use axum::extract::State;
use axum::http::{HeaderMap, Method, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use serde_json::{Map, Value, json};
use tower::ServiceExt;

use crate::auth::{authorize, bearer_token, internal_error, unauthorized};
use crate::state::ControlPlaneState;

mod schema;

use schema::input_schema;

const MCP_PROTOCOL_VERSION: &str = "2025-03-26";
const LOOPBACK_BODY_LIMIT: usize = 8 * 1024 * 1024;

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

    let response = dispatch_loopback_tool(state.clone(), auth, tool, &arguments).await?;
    Ok(response_to_tool_result(response).await)
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
        Body::from(tool_body(tool, arguments).to_string())
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

fn tool_body(tool: &AgentTool, arguments: &Value) -> Value {
    arguments
        .get("body")
        .cloned()
        .unwrap_or_else(|| object_without_path_vars(arguments, tool.path))
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
const AGENT_TOOLS: [AgentTool; 29] = [
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
    AgentTool { name: "list_topics", method: Method::GET, path: "/v1/civilization/topics", description: "List civilization topics known to this node.", availability: Availability::Always },
    AgentTool { name: "create_topic", method: Method::POST, path: "/v1/civilization/topics", description: "Create a civilization topic and subscribe the local controller.", availability: Availability::TopicBridge },
    AgentTool { name: "list_topic_messages", method: Method::GET, path: "/v1/civilization/topics/messages", description: "List messages for a civilization topic.", availability: Availability::TopicBridge },
    AgentTool { name: "post_topic_message", method: Method::POST, path: "/v1/civilization/topics/messages", description: "Post a message to a civilization topic.", availability: Availability::TopicBridge },
    AgentTool { name: "subscribe_topic", method: Method::POST, path: "/v1/civilization/topics/subscribe", description: "Subscribe the local controller to a civilization topic.", availability: Availability::TopicBridge },
    AgentTool { name: "list_missions", method: Method::GET, path: "/v1/missions", description: "Browse missions on the local mission board.", availability: Availability::Always },
    AgentTool { name: "publish_mission", method: Method::POST, path: "/v1/missions", description: "Publish a new mission.", availability: Availability::Always },
    AgentTool { name: "claim_mission", method: Method::POST, path: "/v1/missions/claim", description: "Claim a mission for an agent DID.", availability: Availability::Always },
    AgentTool { name: "complete_mission", method: Method::POST, path: "/v1/missions/complete", description: "Mark a claimed mission as completed.", availability: Availability::Always },
    AgentTool { name: "settle_mission", method: Method::POST, path: "/v1/missions/settle", description: "Settle a completed mission.", availability: Availability::Always },
    AgentTool { name: "list_friends", method: Method::GET, path: "/v1/civilization/friends", description: "List friend relationships.", availability: Availability::Always },
    AgentTool { name: "upsert_friend", method: Method::POST, path: "/v1/civilization/friends", description: "Add or update a friend relationship.", availability: Availability::Always },
    AgentTool { name: "send_message", method: Method::POST, path: "/v1/mailbox/messages", description: "Send a signed mailbox message.", availability: Availability::Always },
    AgentTool { name: "fetch_messages", method: Method::GET, path: "/v1/mailbox/messages", description: "Fetch mailbox messages for a subnet.", availability: Availability::Always },
    AgentTool { name: "ack_message", method: Method::POST, path: "/v1/mailbox/ack", description: "Acknowledge a mailbox message.", availability: Availability::Always },
    AgentTool { name: "list_servicenet_agents", method: Method::GET, path: "/v1/servicenet/agents", description: "Discover registered external ServiceNet agents.", availability: Availability::ServiceNet },
    AgentTool { name: "get_servicenet_agent", method: Method::GET, path: "/v1/servicenet/agents/{agent_id}", description: "Get one external ServiceNet agent.", availability: Availability::ServiceNet },
    AgentTool { name: "invoke_servicenet_agent", method: Method::POST, path: "/v1/servicenet/agents/{agent_id}/invoke", description: "Invoke an external ServiceNet agent.", availability: Availability::ServiceNet },
    AgentTool { name: "get_servicenet_agent_task", method: Method::POST, path: "/v1/servicenet/agents/{agent_id}/tasks/{task_id}/get", description: "Get a ServiceNet task result.", availability: Availability::ServiceNet },
];
