use clap::Parser;
use std::path::PathBuf;

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
    #[arg(long)]
    pub wattswarm_ui_base_url: Option<String>,
    #[arg(long)]
    pub wattswarm_sync_grpc_endpoint: Option<String>,
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
    #[arg(long, default_value_t = false)]
    pub autonomy_enabled: bool,
    #[arg(long, default_value_t = 30)]
    pub autonomy_interval_sec: u64,
}
