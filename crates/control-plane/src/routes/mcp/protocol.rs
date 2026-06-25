use axum::http::HeaderMap;
use serde_json::{Value, json};

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
