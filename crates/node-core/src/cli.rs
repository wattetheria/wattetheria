use clap::Parser;
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(name = "wattetheria-kernel")]
#[allow(clippy::struct_excessive_bools)]
pub struct Cli {
    #[arg(long, default_value = ".wattetheria")]
    pub data_dir: PathBuf,
    #[arg(long, default_value = "wattetheria.v0.1")]
    pub topic: String,
    #[arg(long, default_value = "/ip4/0.0.0.0/tcp/0")]
    pub listen: String,
    #[arg(long = "bootstrap")]
    pub bootstrap: Vec<String>,
    #[arg(long = "recovery-source")]
    pub recovery_sources: Vec<String>,
    #[arg(long, default_value_t = false)]
    pub run_demo_task: bool,
    #[arg(long, default_value_t = false)]
    pub ignite_demo_planet: bool,
    #[arg(long, default_value_t = false)]
    pub enable_hashcash: bool,
    #[arg(long, default_value_t = false)]
    pub require_hashcash_inbound: bool,
    #[arg(long, default_value_t = false)]
    pub require_hashcash_broadcast: bool,
    #[arg(long, default_value_t = 64)]
    pub p2p_max_peers: usize,
    #[arg(long, default_value_t = 240)]
    pub p2p_peer_rate_limit: usize,
    #[arg(long, default_value_t = 1200)]
    pub p2p_topic_rate_limit: usize,
    #[arg(long, default_value_t = 300)]
    pub p2p_publish_rate_limit: usize,
    #[arg(long, default_value_t = 1)]
    pub p2p_topic_shards: usize,
    #[arg(long, default_value_t = 120)]
    pub p2p_dedupe_ttl_sec: i64,
    #[arg(long, default_value_t = 300)]
    pub p2p_message_ttl_sec: i64,
    #[arg(long, default_value = "127.0.0.1:7777")]
    pub control_plane_bind: String,
    #[arg(long)]
    pub wattswarm_ui_base_url: Option<String>,
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
    #[arg(long = "gateway-url")]
    pub gateway_urls: Vec<String>,
    #[arg(long = "gateway-registry-url")]
    pub gateway_registry_urls: Vec<String>,
    #[arg(long, default_value_t = 30)]
    pub gateway_push_interval_sec: u64,
    #[arg(long, default_value_t = 300)]
    pub gateway_discovery_interval_sec: u64,
}
