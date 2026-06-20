use anyhow::{Context, Result, bail};
use reqwest::RequestBuilder;
use reqwest::header::{HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const HERMES_SESSION_HEADER: &str = "X-Hermes-Session-Id";
const OPENCLAW_SESSION_HEADER: &str = "x-openclaw-session-key";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum AgentRuntimeAdapter {
    Hermes {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_header_name: Option<String>,
    },
    OpenClaw {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session_header_name: Option<String>,
    },
    Custom {
        session_header_name: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentRuntimeAdapterMetadata {
    pub key: &'static str,
    pub label: &'static str,
    pub default_model: Option<&'static str>,
    pub session_header_name: Option<&'static str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeSessionContext {
    scope: RuntimeSessionScope,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RuntimeSessionScope {
    Precomputed {
        session_id: String,
    },
    Identity {
        agent_did: String,
        network_id: String,
    },
    ServiceNet {
        caller_agent_did: String,
        published_agent_id: String,
        network_id: String,
    },
}

impl AgentRuntimeAdapter {
    #[must_use]
    pub fn supported_metadata() -> Vec<AgentRuntimeAdapterMetadata> {
        vec![
            AgentRuntimeAdapterMetadata {
                key: "hermes",
                label: "Hermes",
                default_model: Some("hermes-agent"),
                session_header_name: Some(HERMES_SESSION_HEADER),
            },
            AgentRuntimeAdapterMetadata {
                key: "openclaw",
                label: "OpenClaw",
                default_model: Some("openclaw"),
                session_header_name: Some(OPENCLAW_SESSION_HEADER),
            },
            AgentRuntimeAdapterMetadata {
                key: "custom",
                label: "Others",
                default_model: None,
                session_header_name: None,
            },
        ]
    }

    #[must_use]
    pub fn infer(base_url: &str, model: &str, configured: Option<&Self>) -> Self {
        if let Some(configured) = configured {
            return configured.clone();
        }

        let haystack = format!("{base_url} {model}").to_ascii_lowercase();
        if haystack.contains("openclaw") {
            return Self::OpenClaw {
                session_header_name: None,
            };
        }
        if haystack.contains("hermes") {
            return Self::Hermes {
                session_header_name: None,
            };
        }
        Self::Hermes {
            session_header_name: None,
        }
    }

    pub fn from_key(kind: &str, session_header_name: Option<&str>) -> Result<Self> {
        let session_header_name = session_header_name
            .map(str::trim)
            .filter(|value| !value.is_empty());
        match kind.trim().to_ascii_lowercase().as_str() {
            "hermes" => {
                if let Some(header) = session_header_name {
                    validate_header_name(header)?;
                }
                Ok(Self::Hermes {
                    session_header_name: session_header_name.map(ToOwned::to_owned),
                })
            }
            "openclaw" => {
                if let Some(header) = session_header_name {
                    validate_header_name(header)?;
                }
                Ok(Self::OpenClaw {
                    session_header_name: session_header_name.map(ToOwned::to_owned),
                })
            }
            "custom" => {
                let session_header_name = session_header_name.ok_or_else(|| {
                    anyhow::anyhow!("custom runtime adapter requires session_header_name")
                })?;
                validate_header_name(session_header_name)?;
                Ok(Self::Custom {
                    session_header_name: session_header_name.to_owned(),
                })
            }
            other => bail!("unsupported agent runtime adapter: {other}"),
        }
    }

    #[must_use]
    pub fn key(&self) -> &'static str {
        match self {
            Self::Hermes { .. } => "hermes",
            Self::OpenClaw { .. } => "openclaw",
            Self::Custom { .. } => "custom",
        }
    }

    #[must_use]
    pub fn session_header_name(&self) -> &str {
        match self {
            Self::Hermes {
                session_header_name,
            } => session_header_name
                .as_deref()
                .unwrap_or(HERMES_SESSION_HEADER),
            Self::OpenClaw {
                session_header_name,
            } => session_header_name
                .as_deref()
                .unwrap_or(OPENCLAW_SESSION_HEADER),
            Self::Custom {
                session_header_name,
            } => session_header_name,
        }
    }

    pub fn apply_session_header(
        &self,
        request: RequestBuilder,
        context: &RuntimeSessionContext,
    ) -> Result<RequestBuilder> {
        let header_name = validate_header_name(self.session_header_name())?;
        let session_id = context.session_id();
        let header_value = HeaderValue::from_str(&session_id)
            .with_context(|| format!("build runtime session header value: {session_id}"))?;
        Ok(request.header(header_name, header_value))
    }
}

impl RuntimeSessionContext {
    #[must_use]
    pub fn new(agent_did: impl Into<String>, network_id: impl Into<String>) -> Self {
        Self::identity(agent_did, network_id)
    }

    #[must_use]
    pub fn identity(agent_did: impl Into<String>, network_id: impl Into<String>) -> Self {
        Self {
            scope: RuntimeSessionScope::Identity {
                agent_did: agent_did.into(),
                network_id: network_id.into(),
            },
        }
    }

    #[must_use]
    pub fn precomputed(session_id: impl Into<String>) -> Self {
        Self {
            scope: RuntimeSessionScope::Precomputed {
                session_id: session_id.into(),
            },
        }
    }

    #[must_use]
    pub fn servicenet(
        caller_agent_did: impl Into<String>,
        published_agent_id: impl Into<String>,
        network_id: impl Into<String>,
    ) -> Self {
        Self {
            scope: RuntimeSessionScope::ServiceNet {
                caller_agent_did: caller_agent_did.into(),
                published_agent_id: published_agent_id.into(),
                network_id: network_id.into(),
            },
        }
    }

    #[must_use]
    pub fn from_agent_event_input(event: &Value) -> Option<Self> {
        let agent_did = event
            .get("agent_did")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())?;
        let network_id = event
            .get("network_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("unknown");
        Some(Self::new(agent_did, network_id))
    }

    #[must_use]
    pub fn session_id(&self) -> String {
        match &self.scope {
            RuntimeSessionScope::Precomputed { session_id } => session_id.trim().to_string(),
            RuntimeSessionScope::Identity {
                agent_did,
                network_id,
            } => {
                format!(
                    "wattetheria:identity:{}:{}",
                    agent_did.trim(),
                    network_id.trim()
                )
            }
            RuntimeSessionScope::ServiceNet {
                caller_agent_did,
                published_agent_id,
                network_id,
            } => format!(
                "wattetheria:servicenet:{}:{}:{}",
                caller_agent_did.trim(),
                published_agent_id.trim(),
                network_id.trim()
            ),
        }
    }
}

fn validate_header_name(value: &str) -> Result<HeaderName> {
    HeaderName::from_bytes(value.trim().as_bytes())
        .with_context(|| format!("invalid runtime session header name: {value}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn adapter_session_headers_match_supported_runtimes() {
        assert_eq!(
            AgentRuntimeAdapter::Hermes {
                session_header_name: None
            }
            .session_header_name(),
            "X-Hermes-Session-Id"
        );
        assert_eq!(
            AgentRuntimeAdapter::OpenClaw {
                session_header_name: None
            }
            .session_header_name(),
            "x-openclaw-session-key"
        );
        assert_eq!(
            AgentRuntimeAdapter::Custom {
                session_header_name: "X-Agent-Session-Id".to_owned()
            }
            .session_header_name(),
            "X-Agent-Session-Id"
        );
    }

    #[test]
    fn runtime_session_context_uses_identity_and_network() {
        let context = RuntimeSessionContext::from_agent_event_input(&json!({
            "agent_did": "did:key:zAgent",
            "network_id": "mainnet:watt-etheria"
        }))
        .unwrap();

        assert_eq!(
            context.session_id(),
            "wattetheria:identity:did:key:zAgent:mainnet:watt-etheria"
        );
    }

    #[test]
    fn runtime_session_context_separates_servicenet_sessions() {
        let context = RuntimeSessionContext::servicenet(
            "did:key:zCaller",
            "stripe-agent",
            "mainnet:watt-etheria",
        );

        assert_eq!(
            context.session_id(),
            "wattetheria:servicenet:did:key:zCaller:stripe-agent:mainnet:watt-etheria"
        );
    }

    #[test]
    fn adapter_infers_openclaw_from_model() {
        assert_eq!(
            AgentRuntimeAdapter::infer("http://127.0.0.1:8642/v1", "openclaw-agent", None),
            AgentRuntimeAdapter::OpenClaw {
                session_header_name: None
            }
        );
    }
}
