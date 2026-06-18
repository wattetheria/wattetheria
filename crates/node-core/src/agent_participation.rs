use anyhow::{Context, Result};
use chrono::Utc;
use serde::Serialize;
use std::fs;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::Path;
use wattetheria_kernel::brain::BrainProviderConfig;
use wattetheria_kernel::identity::IdentityCompatView;

const ARTIFACT_DIR: &str = ".agent-participation";
const MANIFEST_FILE: &str = "manifest.json";
const README_FILE: &str = "README.md";

#[derive(Debug, Clone, Default)]
pub(crate) struct AgentParticipationSurface {
    pub control_plane_endpoint: Option<String>,
    pub wattswarm_ui_base_url: Option<String>,
    pub wattswarm_sync_grpc_endpoint: Option<String>,
    pub host_data_dir: Option<String>,
    pub mcp_token_auth_required: bool,
}

#[derive(Debug, Clone, Serialize)]
struct AgentParticipationManifest {
    version: String,
    generated_at: String,
    node: NodeSurface,
    network: NetworkSurface,
    auth: AuthSurface,
    brain_provider: BrainProviderSurface,
    mcp: McpSurface,
}

#[derive(Debug, Clone, Serialize)]
struct NodeSurface {
    agent_did: String,
    data_dir: String,
}

#[derive(Debug, Clone, Serialize)]
struct NetworkSurface {
    control_plane_bind: String,
    control_plane_endpoint: String,
    wattswarm_ui_base_url: Option<String>,
    wattswarm_sync_grpc_endpoint: Option<String>,
    topic_bridge_enabled: bool,
}

#[derive(Debug, Clone, Serialize)]
struct AuthSurface {
    kind: String,
    required: bool,
    header_name: String,
    header_format: String,
    token_file: String,
}

#[derive(Debug, Clone, Serialize)]
struct BrainProviderSurface {
    kind: String,
    base_url: Option<String>,
    model: Option<String>,
    api_key_env: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct McpSurface {
    endpoint: String,
    protocol: String,
    auth_required: bool,
    tools_list_method: String,
    tools_call_method: String,
}

pub(crate) fn write_agent_participation_artifacts(
    data_dir: &Path,
    identity: &IdentityCompatView,
    brain_provider: &BrainProviderConfig,
    control_bind: &SocketAddr,
    servicenet_base_url: Option<&str>,
    agent_surface: &AgentParticipationSurface,
) -> Result<()> {
    let artifact_dir = data_dir.join(ARTIFACT_DIR);
    fs::create_dir_all(&artifact_dir).context("create agent participation directory")?;

    let manifest = build_manifest(
        data_dir,
        identity,
        brain_provider,
        control_bind,
        servicenet_base_url,
        agent_surface,
    );
    fs::write(
        artifact_dir.join(MANIFEST_FILE),
        serde_json::to_vec_pretty(&manifest)?,
    )
    .context("write agent participation manifest")?;
    fs::write(artifact_dir.join(README_FILE), render_readme(&manifest))
        .context("write agent participation readme")?;
    Ok(())
}

fn build_manifest(
    data_dir: &Path,
    identity: &IdentityCompatView,
    brain_provider: &BrainProviderConfig,
    control_bind: &SocketAddr,
    _servicenet_base_url: Option<&str>,
    agent_surface: &AgentParticipationSurface,
) -> AgentParticipationManifest {
    let control_plane_endpoint = trim_optional(agent_surface.control_plane_endpoint.as_deref())
        .unwrap_or_else(|| preferred_control_plane_endpoint(control_bind));
    let agent_data_dir = trim_optional(agent_surface.host_data_dir.as_deref())
        .unwrap_or_else(|| data_dir.display().to_string());
    let token_file = Path::new(&agent_data_dir).join("control.token");
    let wattswarm_ui_base_url = trim_optional(agent_surface.wattswarm_ui_base_url.as_deref());
    let wattswarm_sync_grpc_endpoint =
        trim_optional(agent_surface.wattswarm_sync_grpc_endpoint.as_deref());
    let topic_bridge_enabled = wattswarm_ui_base_url.is_some();

    AgentParticipationManifest {
        version: "v1".to_owned(),
        generated_at: Utc::now().to_rfc3339(),
        node: NodeSurface {
            agent_did: identity.agent_did.clone(),
            data_dir: agent_data_dir,
        },
        network: NetworkSurface {
            control_plane_bind: control_bind.to_string(),
            control_plane_endpoint: control_plane_endpoint.clone(),
            wattswarm_ui_base_url,
            wattswarm_sync_grpc_endpoint,
            topic_bridge_enabled,
        },
        auth: AuthSurface {
            kind: "bearer_token".to_owned(),
            required: agent_surface.mcp_token_auth_required,
            header_name: "authorization".to_owned(),
            header_format: "Bearer <token>".to_owned(),
            token_file: token_file.display().to_string(),
        },
        brain_provider: BrainProviderSurface::from_config(brain_provider),
        mcp: McpSurface::new(
            &control_plane_endpoint,
            agent_surface.mcp_token_auth_required,
        ),
    }
}

fn trim_optional(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn preferred_control_plane_endpoint(bind: &SocketAddr) -> String {
    let host = match bind.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(ip) if ip.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        ip => ip,
    };
    match host {
        IpAddr::V4(ip) => format!("http://{}:{}", ip, bind.port()),
        IpAddr::V6(ip) => format!("http://[{}]:{}", ip, bind.port()),
    }
}

impl BrainProviderSurface {
    fn from_config(config: &BrainProviderConfig) -> Self {
        match config {
            BrainProviderConfig::Rules => Self {
                kind: "rules".to_owned(),
                base_url: None,
                model: None,
                api_key_env: None,
            },
            BrainProviderConfig::Ollama { base_url, model } => Self {
                kind: "ollama".to_owned(),
                base_url: Some(base_url.trim_end_matches('/').to_owned()),
                model: Some(model.clone()),
                api_key_env: None,
            },
            BrainProviderConfig::OpenaiCompatible {
                base_url,
                model,
                api_key_env,
                runtime_adapter,
            } => Self {
                kind: format!(
                    "agent-runtime:{}",
                    wattetheria_kernel::brain::AgentRuntimeAdapter::infer(
                        base_url,
                        model,
                        runtime_adapter.as_ref()
                    )
                    .key()
                ),
                base_url: Some(base_url.trim_end_matches('/').to_owned()),
                model: Some(model.clone()),
                api_key_env: api_key_env.clone(),
            },
        }
    }
}

impl McpSurface {
    fn new(control_plane_endpoint: &str, auth_required: bool) -> Self {
        Self {
            endpoint: format!("{}/mcp", control_plane_endpoint.trim_end_matches('/')),
            protocol: "jsonrpc-http".to_owned(),
            auth_required,
            tools_list_method: "tools/list".to_owned(),
            tools_call_method: "tools/call".to_owned(),
        }
    }
}

fn render_readme(manifest: &AgentParticipationManifest) -> String {
    let bridge_status = if manifest.network.topic_bridge_enabled {
        "enabled"
    } else {
        "disabled"
    };
    format!(
        "# Agent Participation\n\n\
This file is generated by `wattetheria-kernel` for local agent hosts.\n\n\
## Node\n\n\
- agent DID: `{agent_did}`\n\
- data dir: `{data_dir}`\n\
- control plane bind: `{control_bind}`\n\
- control plane endpoint: `{control_endpoint}`\n\n\
## Auth\n\n\
- header: `{header_name}: {header_format}`\n\
- required for MCP: `{auth_required}`\n\
- token file: `{token_file}`\n\n\
## Brain Provider\n\n\
- kind: `{brain_kind}`\n\
- base URL: `{brain_base_url}`\n\
- model: `{brain_model}`\n\
- api key env: `{brain_api_key_env}`\n\n\
## Network Participation\n\n\
- wattswarm UI base URL: `{wattswarm_ui_base_url}`\n\
- wattswarm sync gRPC endpoint: `{wattswarm_sync_grpc_endpoint}`\n\
- topic bridge: `{bridge_status}`\n\n\
## MCP\n\n\
- endpoint: `{mcp_endpoint}`\n\
- protocol: `{mcp_protocol}`\n\
- token auth required: `{mcp_auth_required}`\n\
- list tools: `{mcp_tools_list_method}`\n\
- call tools: `{mcp_tools_call_method}`\n\n\
Use MCP `tools/list` as the source of truth for available tools. This manifest intentionally does not duplicate the MCP tool catalog.\n",
        agent_did = manifest.node.agent_did,
        data_dir = manifest.node.data_dir,
        control_bind = manifest.network.control_plane_bind,
        control_endpoint = manifest.network.control_plane_endpoint,
        header_name = manifest.auth.header_name,
        header_format = manifest.auth.header_format,
        auth_required = manifest.auth.required,
        token_file = manifest.auth.token_file,
        brain_kind = manifest.brain_provider.kind,
        brain_base_url = manifest
            .brain_provider
            .base_url
            .as_deref()
            .unwrap_or("(not required)"),
        brain_model = manifest
            .brain_provider
            .model
            .as_deref()
            .unwrap_or("(not required)"),
        brain_api_key_env = manifest
            .brain_provider
            .api_key_env
            .as_deref()
            .unwrap_or("(not required)"),
        wattswarm_ui_base_url = manifest
            .network
            .wattswarm_ui_base_url
            .as_deref()
            .unwrap_or("(not configured)"),
        wattswarm_sync_grpc_endpoint = manifest
            .network
            .wattswarm_sync_grpc_endpoint
            .as_deref()
            .unwrap_or("(not configured)"),
        bridge_status = bridge_status,
        mcp_endpoint = manifest.mcp.endpoint,
        mcp_protocol = manifest.mcp.protocol,
        mcp_auth_required = manifest.mcp.auth_required,
        mcp_tools_list_method = manifest.mcp.tools_list_method,
        mcp_tools_call_method = manifest.mcp.tools_call_method,
    )
}

#[cfg(test)]
mod tests {
    use super::{AgentParticipationSurface, write_agent_participation_artifacts};
    use serde_json::Value;
    use std::net::{Ipv4Addr, SocketAddr};
    use tempfile::tempdir;
    use wattetheria_kernel::brain::BrainProviderConfig;
    use wattetheria_kernel::identity::IdentityCompatView;

    #[test]
    fn writes_manifest_with_runtime_values() {
        let dir = tempdir().unwrap();
        let identity = IdentityCompatView {
            agent_did: "did:key:zTest".to_owned(),
            public_key: "pub".to_owned(),
        };
        let bind = SocketAddr::from((Ipv4Addr::UNSPECIFIED, 7777));

        write_agent_participation_artifacts(
            dir.path(),
            &identity,
            &BrainProviderConfig::OpenaiCompatible {
                base_url: "http://127.0.0.1:4000/v1".to_owned(),
                model: "openclaw-agent".to_owned(),
                api_key_env: Some("WATTETHERIA_BRAIN_API_KEY".to_owned()),
                runtime_adapter: None,
            },
            &bind,
            Some("http://127.0.0.1:8042"),
            &AgentParticipationSurface {
                control_plane_endpoint: Some("http://127.0.0.1:7777".to_owned()),
                wattswarm_ui_base_url: Some("http://127.0.0.1:7788".to_owned()),
                wattswarm_sync_grpc_endpoint: Some("http://127.0.0.1:7791".to_owned()),
                host_data_dir: Some("./data/wattetheria".to_owned()),
                mcp_token_auth_required: false,
            },
        )
        .unwrap();

        let manifest_path = dir.path().join(".agent-participation/manifest.json");
        let manifest: Value =
            serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
        assert_eq!(
            manifest["node"]["agent_did"].as_str(),
            Some("did:key:zTest")
        );
        assert_eq!(
            manifest["network"]["control_plane_endpoint"].as_str(),
            Some("http://127.0.0.1:7777")
        );
        assert_eq!(
            manifest["node"]["data_dir"].as_str(),
            Some("./data/wattetheria")
        );
        assert_eq!(
            manifest["auth"]["token_file"].as_str(),
            Some("./data/wattetheria/control.token")
        );
        assert_eq!(
            manifest["brain_provider"]["base_url"].as_str(),
            Some("http://127.0.0.1:4000/v1")
        );
        assert_eq!(
            manifest["mcp"]["endpoint"].as_str(),
            Some("http://127.0.0.1:7777/mcp")
        );
        assert_eq!(manifest["mcp"]["protocol"].as_str(), Some("jsonrpc-http"));
        assert_eq!(manifest["auth"]["required"].as_bool(), Some(false));
        assert_eq!(manifest["mcp"]["auth_required"].as_bool(), Some(false));
        assert_eq!(
            manifest["mcp"]["tools_list_method"].as_str(),
            Some("tools/list")
        );
        assert_eq!(
            manifest["mcp"]["tools_call_method"].as_str(),
            Some("tools/call")
        );
        assert_eq!(
            manifest["network"]["topic_bridge_enabled"].as_bool(),
            Some(true)
        );
        assert!(manifest.get("endpoints").is_none());
        assert!(dir.path().join(".agent-participation/README.md").exists());
    }
}
