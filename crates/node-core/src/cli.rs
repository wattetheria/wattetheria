use clap::Parser;
use std::path::PathBuf;
use wattetheria_control_plane::mcp_events::AgentEventMode;

#[derive(Debug, Parser)]
#[command(name = "wattetheria-kernel")]
#[allow(clippy::struct_excessive_bools)]
pub struct Cli {
    #[arg(long, default_value = ".wattetheria")]
    pub data_dir: PathBuf,
    #[arg(long = "recovery-source")]
    pub recovery_sources: Vec<String>,
    #[arg(long, default_value = "127.0.0.1:7777")]
    pub control_plane_bind: String,
    #[arg(long, env = "WATTETHERIA_MCP_PUBLIC_BIND")]
    pub mcp_public_bind: Option<String>,
    #[arg(long)]
    pub wattswarm_ui_base_url: Option<String>,
    #[arg(long)]
    pub wattswarm_sync_grpc_endpoint: Option<String>,
    #[arg(long)]
    pub wattswarm_agent_event_callback_base_url: Option<String>,
    #[arg(long, default_value = "api_runtime")]
    pub agent_event_mode: AgentEventMode,
    #[arg(long)]
    pub agent_control_plane_endpoint: Option<String>,
    #[arg(long)]
    pub agent_wattswarm_ui_base_url: Option<String>,
    #[arg(long)]
    pub agent_wattswarm_sync_grpc_endpoint: Option<String>,
    #[arg(long)]
    pub agent_host_data_dir: Option<String>,
    #[arg(long, default_value_t = false)]
    pub mcp_token_auth_required: bool,
    #[arg(long = "gateway-url")]
    pub gateway_urls: Vec<String>,
    #[arg(long = "gateway-config-path")]
    pub gateway_config_path: Option<PathBuf>,
    #[arg(long, default_value_t = 45)]
    pub gateway_snapshot_interval_sec: u64,
    #[arg(long, default_value_t = 60)]
    pub control_plane_rate_limit: usize,
    #[arg(long, default_value = "rules")]
    pub brain_provider_kind: String,
    #[arg(long, default_value = "http://127.0.0.1:11434")]
    pub brain_base_url: String,
    #[arg(long, default_value = "qwen2.5:7b-instruct")]
    pub brain_model: String,
    #[arg(long)]
    pub brain_api_key_env: Option<String>,
    #[arg(long)]
    pub brain_runtime_adapter: Option<String>,
    #[arg(long)]
    pub brain_session_header_name: Option<String>,
    #[arg(long, default_value = "stable_per_scope")]
    pub brain_runtime_session_mode: String,
    #[arg(long, default_value_t = false)]
    pub autonomy_enabled: bool,
    #[arg(long, default_value_t = 30)]
    pub autonomy_interval_sec: u64,
}

#[cfg(test)]
mod tests {
    use super::Cli;
    use clap::Parser;

    #[test]
    fn agent_event_mode_is_explicit_and_defaults_to_api_runtime() {
        use wattetheria_control_plane::mcp_events::AgentEventMode;

        assert_eq!(
            Cli::try_parse_from(["kernel"]).unwrap().agent_event_mode,
            AgentEventMode::ApiRuntime
        );
        assert_eq!(
            Cli::try_parse_from(["kernel", "--agent-event-mode", "mcp_events"])
                .unwrap()
                .agent_event_mode,
            AgentEventMode::McpEvents
        );
        assert!(Cli::try_parse_from(["kernel", "--agent-event-mode", "other"]).is_err());
    }

    #[test]
    fn runtime_session_mode_defaults_to_scoped_stable() {
        let cli = Cli::try_parse_from(["wattetheria-kernel"]).expect("default CLI");

        assert_eq!(cli.brain_runtime_session_mode, "stable_per_scope");
    }
}
