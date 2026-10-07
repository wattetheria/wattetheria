use crate::state::ControlPlaneState;
use axum::Json;
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

pub(super) async fn serve_modern(
    state: &ControlPlaneState,
    headers: &HeaderMap,
    request: super::McpRequest,
) -> Response {
    if let Err(error) = validate_modern_request(headers, &request) {
        return error_response(
            error.status,
            request.id.as_ref(),
            error.code,
            error.message,
            error.data,
        );
    }
    if matches!(
        request.method.as_str(),
        "events/subscribe" | "events/unsubscribe"
    ) && super::validate_bearer(state, headers).is_none()
    {
        return error_response(
            StatusCode::OK,
            request.id.as_ref(),
            -32012,
            "authenticated node owner is required",
            json!({}),
        );
    }
    let auth = match super::resolve_mcp_auth(state, headers, &request.method).await {
        Ok(auth) => auth,
        Err(response) => return response,
    };
    let mut result = match request.method.as_str() {
        "server/discover" => {
            discover_result(state.agent_event_mode == crate::mcp_events::AgentEventMode::McpEvents)
        }
        "events/list" | "events/subscribe" | "events/unsubscribe" => {
            match crate::mcp_events::subscriptions::handle_request(
                state,
                &request.method,
                &request.params,
            )
            .await
            {
                Ok(result) => result,
                Err(error) => {
                    return error_response(
                        error.status,
                        request.id.as_ref(),
                        error.code,
                        error.message,
                        error.data,
                    );
                }
            }
        }
        "tools/list" => {
            let mut tools = super::mcp_tools(state);
            tools.sort_by_key(|tool| tool.name);
            json!({"tools": tools})
        }
        "tools/call" => {
            if !super::agent_tools()
                .iter()
                .any(|tool| Some(tool.name) == request.params["name"].as_str())
                || request
                    .params
                    .get("arguments")
                    .is_some_and(|args| !args.is_object())
            {
                return error_response(
                    StatusCode::BAD_REQUEST,
                    request.id.as_ref(),
                    -32602,
                    "unknown tool or invalid arguments",
                    json!({}),
                );
            }
            match super::call_tool(state, &auth, request.params).await {
                Ok(result) => result,
                Err(response) => return tool_execution_error(request.id.as_ref(), response).await,
            }
        }
        _ => {
            return error_response(
                StatusCode::NOT_FOUND,
                request.id.as_ref(),
                -32601,
                "method not found",
                json!({}),
            );
        }
    };
    complete_result(&mut result, &request.method);
    Json(json!({"jsonrpc": "2.0", "id": request.id, "result": result})).into_response()
}

async fn tool_execution_error(id: Option<&Value>, response: Response) -> Response {
    let status = response.status();
    let detail = match axum::body::to_bytes(response.into_body(), super::LOOPBACK_BODY_LIMIT).await
    {
        Ok(bytes) => serde_json::from_slice::<Value>(&bytes)
            .unwrap_or_else(|_| json!(String::from_utf8_lossy(&bytes))),
        Err(error) => json!(error.to_string()),
    };
    error_response(
        StatusCode::OK,
        id,
        -32603,
        "tool execution failed",
        json!({"httpStatus": status.as_u16(), "detail": detail}),
    )
}

fn error_response(
    status: StatusCode,
    id: Option<&Value>,
    code: i32,
    message: &str,
    data: Value,
) -> Response {
    let mut error = json!({"code": code, "message": message});
    error["data"] = data;
    (
        status,
        Json(json!({"jsonrpc": "2.0", "id": id, "error": error})),
    )
        .into_response()
}

fn validate_modern_request(
    headers: &HeaderMap,
    request: &super::McpRequest,
) -> Result<(), ProtocolError> {
    if request.jsonrpc.as_deref() != Some("2.0")
        || !request
            .id
            .as_ref()
            .is_some_and(|id| id.is_string() || id.as_i64().is_some() || id.as_u64().is_some())
    {
        return Err(modern_error(-32600, "invalid JSON-RPC request", json!({})));
    }
    validate_modern(headers, &request.method, &request.params)
}

pub(super) const MODERN_PROTOCOL_VERSION: &str = "2026-07-28";

pub(super) struct ProtocolError {
    pub status: StatusCode,
    pub code: i32,
    pub message: &'static str,
    pub data: Value,
}

pub(super) fn is_modern(headers: &HeaderMap, method: &str, params: &Value) -> bool {
    method != "initialize"
        && (method == "server/discover"
            || params["_meta"]
                .get("io.modelcontextprotocol/protocolVersion")
                .is_some_and(|version| {
                    version
                        .as_str()
                        .and_then(supported_protocol_version)
                        .is_none()
                })
            || headers
                .get(MCP_PROTOCOL_VERSION_HEADER)
                .and_then(|v| v.to_str().ok())
                == Some(MODERN_PROTOCOL_VERSION))
}

pub(super) fn validate_modern(
    headers: &HeaderMap,
    method: &str,
    params: &Value,
) -> Result<(), ProtocolError> {
    let version = params["_meta"]["io.modelcontextprotocol/protocolVersion"]
        .as_str()
        .ok_or_else(|| modern_error(-32602, "request protocolVersion is required", json!({})))?;
    if !params["_meta"]["io.modelcontextprotocol/clientCapabilities"].is_object() {
        return Err(modern_error(
            -32602,
            "request clientCapabilities is required",
            json!({}),
        ));
    }
    for (header, expected) in [
        (MCP_PROTOCOL_VERSION_HEADER, version),
        ("mcp-method", method),
    ] {
        if headers.get(header).and_then(|v| v.to_str().ok()) != Some(expected) {
            return Err(modern_error(
                -32020,
                "required header is missing or does not match the request",
                json!({"header": header}),
            ));
        }
    }
    if version != MODERN_PROTOCOL_VERSION {
        return Err(modern_error(
            -32022,
            "unsupported protocol version",
            json!({
                "requested": version, "supported": [MODERN_PROTOCOL_VERSION]
            }),
        ));
    }
    if method == "tools/call" {
        let name = params["name"]
            .as_str()
            .ok_or_else(|| modern_error(-32602, "params.name is required", json!({})))?;
        let header = headers.get("mcp-name").and_then(|v| v.to_str().ok());
        let decoded = header.and_then(decode_header_value);
        if decoded.as_deref() != Some(name) {
            return Err(modern_error(
                -32020,
                "Mcp-Name does not match params.name",
                json!({"header": "Mcp-Name"}),
            ));
        }
    }
    Ok(())
}

fn decode_header_value(value: &str) -> Option<String> {
    use base64::Engine;
    if let Some(encoded) = value
        .strip_prefix("=?base64?")
        .and_then(|v| v.strip_suffix("?="))
    {
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .ok()?;
        String::from_utf8(decoded).ok()
    } else {
        Some(value.to_owned())
    }
}

fn modern_error(code: i32, message: &'static str, data: Value) -> ProtocolError {
    ProtocolError {
        status: StatusCode::BAD_REQUEST,
        code,
        message,
        data,
    }
}

pub(super) fn discover_result(events: bool) -> Value {
    let mut capabilities = json!({"tools": {}});
    if events {
        capabilities["events"] = json!({});
    }
    json!({
        "supportedVersions": [MODERN_PROTOCOL_VERSION],
        "capabilities": capabilities,
        "ttlMs": 0,
        "cacheScope": "private"
    })
}

pub(super) fn complete_result(result: &mut Value, method: &str) {
    result["resultType"] = json!("complete");
    result["_meta"]["io.modelcontextprotocol/serverInfo"] = json!({
        "name": "wattetheria-local-control-plane", "version": env!("CARGO_PKG_VERSION")
    });
    if method == "tools/list" {
        result["ttlMs"] = json!(0);
        result["cacheScope"] = json!("private");
    }
}

pub(super) const LATEST_PROTOCOL_VERSION: &str = "2025-11-25";

const DEFAULT_HTTP_PROTOCOL_VERSION: &str = "2025-03-26";
const MCP_PROTOCOL_VERSION_HEADER: &str = "mcp-protocol-version";
const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &[
    LATEST_PROTOCOL_VERSION,
    "2025-06-18",
    "2025-03-26",
    "2024-11-05",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct UnsupportedProtocolVersion {
    requested: String,
}

impl UnsupportedProtocolVersion {
    pub(super) fn message(&self) -> String {
        format!(
            "unsupported MCP protocol version {}; supported versions: {}",
            self.requested,
            SUPPORTED_PROTOCOL_VERSIONS.join(", ")
        )
    }
}

pub(super) fn initialize_result(params: &Value) -> Value {
    json!({
        "protocolVersion": negotiated_initialize_protocol_version(params),
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

pub(super) fn validated_request_protocol_version(
    headers: &HeaderMap,
) -> Result<&'static str, UnsupportedProtocolVersion> {
    let Some(value) = headers.get(MCP_PROTOCOL_VERSION_HEADER) else {
        return Ok(DEFAULT_HTTP_PROTOCOL_VERSION);
    };
    let requested = value
        .to_str()
        .ok()
        .filter(|version| !version.trim().is_empty())
        .unwrap_or_default();
    supported_protocol_version(requested).ok_or_else(|| UnsupportedProtocolVersion {
        requested: requested.to_string(),
    })
}

fn negotiated_initialize_protocol_version(params: &Value) -> &'static str {
    params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .and_then(supported_protocol_version)
        .unwrap_or(LATEST_PROTOCOL_VERSION)
}

fn supported_protocol_version(version: &str) -> Option<&'static str> {
    SUPPORTED_PROTOCOL_VERSIONS
        .iter()
        .copied()
        .find(|supported| *supported == version)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[tokio::test]
    async fn modern_tool_internal_errors_preserve_response_details() {
        for (status, detail) in [
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({"error": "audit write failed"}),
            ),
            (StatusCode::BAD_REQUEST, json!({"error": "missing task_id"})),
        ] {
            let response = tool_execution_error(
                Some(&json!(7)),
                (status, Json(detail.clone())).into_response(),
            )
            .await;
            assert_eq!(response.status(), StatusCode::OK);
            let body =
                axum::body::to_bytes(response.into_body(), super::super::LOOPBACK_BODY_LIMIT)
                    .await
                    .unwrap();
            let result: Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(result["id"], 7);
            assert_eq!(result["error"]["code"], -32603);
            assert_eq!(result["error"]["data"]["detail"], detail);
            assert_eq!(result["error"]["data"]["httpStatus"], status.as_u16());
        }
    }

    #[test]
    fn initialize_negotiates_supported_client_protocol() {
        let result = initialize_result(&json!({"protocolVersion": "2025-11-25"}));

        assert_eq!(result["protocolVersion"], "2025-11-25");
    }

    #[test]
    fn initialize_defaults_to_latest_protocol() {
        let result = initialize_result(&json!({}));

        assert_eq!(result["protocolVersion"], LATEST_PROTOCOL_VERSION);
    }

    #[test]
    fn request_protocol_header_defaults_for_backwards_compatibility() {
        let headers = HeaderMap::new();

        assert_eq!(
            validated_request_protocol_version(&headers),
            Ok(DEFAULT_HTTP_PROTOCOL_VERSION)
        );
    }

    #[test]
    fn request_protocol_header_rejects_unsupported_versions() {
        let mut headers = HeaderMap::new();
        headers.insert(
            MCP_PROTOCOL_VERSION_HEADER,
            HeaderValue::from_static("2099-01-01"),
        );

        assert!(validated_request_protocol_version(&headers).is_err());
    }
}
