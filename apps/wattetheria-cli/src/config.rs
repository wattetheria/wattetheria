use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::fs;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;
use wattetheria_kernel::brain::BrainProviderConfig;
use wattetheria_kernel::capabilities::CapabilityPolicy;
use wattetheria_kernel::event_log::EventLog;
use wattetheria_kernel::identity::Identity;
use wattetheria_kernel::mcp::McpRegistry;
use wattetheria_kernel::policy_engine::PolicyEngine;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct LocalConfig {
    #[serde(default = "default_control_bind")]
    pub(crate) control_plane_bind: String,
    #[serde(default = "default_control_endpoint")]
    pub(crate) control_plane_endpoint: String,
    #[serde(default = "default_p2p_topic_shards")]
    pub(crate) p2p_topic_shards: usize,
    #[serde(default)]
    pub(crate) recovery_sources: Vec<String>,
    #[serde(default)]
    pub(crate) brain_provider: BrainProviderConfig,
    #[serde(default)]
    pub(crate) autonomy_enabled: bool,
    #[serde(default = "default_autonomy_interval_sec")]
    pub(crate) autonomy_interval_sec: u64,
}

fn default_control_bind() -> String {
    "127.0.0.1:7777".to_string()
}

fn default_control_endpoint() -> String {
    format!("http://{}", default_control_bind())
}

fn default_p2p_topic_shards() -> usize {
    1
}

fn default_autonomy_interval_sec() -> u64 {
    30
}

impl Default for LocalConfig {
    fn default() -> Self {
        let bind = default_control_bind();
        Self {
            control_plane_endpoint: default_control_endpoint(),
            control_plane_bind: bind,
            p2p_topic_shards: default_p2p_topic_shards(),
            recovery_sources: Vec::new(),
            brain_provider: BrainProviderConfig::Rules,
            autonomy_enabled: false,
            autonomy_interval_sec: default_autonomy_interval_sec(),
        }
    }
}

pub(crate) fn run_init(data_dir: &Path) -> Result<()> {
    fs::create_dir_all(data_dir).context("create data directory")?;
    fs::create_dir_all(data_dir.join("audit"))?;
    fs::create_dir_all(data_dir.join("snapshots"))?;
    fs::create_dir_all(data_dir.join("mcp"))?;
    fs::create_dir_all(data_dir.join("policy"))?;

    let identity = Identity::load_or_create(data_dir.join("identity.json"))?;
    let _ = EventLog::new(data_dir.join("events.jsonl"))?;
    let token = load_or_create_control_token(data_dir.join("control.token"))?;

    let config_path = data_dir.join("config.json");
    if !config_path.exists() {
        let config = LocalConfig::default();
        fs::write(&config_path, serde_json::to_string_pretty(&config)?).context("write config")?;
    }

    let schema_version = data_dir.join("schema.version");
    if !schema_version.exists() {
        fs::write(&schema_version, "0.2.0").context("write schema version")?;
    }

    let _ = McpRegistry::load_or_new(data_dir.join("mcp/servers.json"))?;
    let _ = PolicyEngine::load_or_new(
        data_dir.join("policy/state.json"),
        "cli-bootstrap",
        CapabilityPolicy::default(),
    )?;

    let response = serde_json::json!({
        "status": "ok",
        "agent_id": identity.agent_id,
        "data_dir": data_dir,
        "control_plane_endpoint": read_config(data_dir)?.control_plane_endpoint,
        "token_file": data_dir.join("control.token"),
        "token_preview": format!("{}...", &token.chars().take(8).collect::<String>()),
    });

    let rendered = serde_json::to_string_pretty(&response)?;
    println!("{rendered}");
    Ok(())
}

pub(crate) async fn run_up(
    data_dir: &Path,
    bind_override: Option<String>,
    attach: bool,
) -> Result<()> {
    run_init(data_dir)?;

    let mut config = read_config(data_dir)?;
    if let Some(bind) = bind_override {
        config.control_plane_bind.clone_from(&bind);
        config.control_plane_endpoint = format!("http://{bind}");
        write_config(data_dir, &config)?;
    }

    let token = load_or_create_control_token(data_dir.join("control.token"))?;
    let mut command = kernel_command(data_dir, &config);

    if attach {
        let status = command.status().context("run kernel in attach mode")?;
        if !status.success() {
            bail!("kernel exited with status: {status}");
        }
        return Ok(());
    }

    let log_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(data_dir.join("daemon.log"))
        .context("open daemon log")?;
    let log_file_err = log_file.try_clone().context("clone daemon log handle")?;

    let child = command
        .stdout(Stdio::from(log_file))
        .stderr(Stdio::from(log_file_err))
        .spawn()
        .context("spawn kernel daemon")?;

    fs::write(data_dir.join("daemon.pid"), child.id().to_string()).context("write daemon pid")?;

    wait_for_control_plane(&config.control_plane_endpoint, &token, 60).await?;

    let response = serde_json::json!({
        "status": "ok",
        "pid": child.id(),
        "control_plane_endpoint": config.control_plane_endpoint,
        "log_file": data_dir.join("daemon.log"),
    });
    println!("{}", serde_json::to_string_pretty(&response)?);
    Ok(())
}

fn append_kernel_runtime_args(command: &mut Command, data_dir: &Path, config: &LocalConfig) {
    command
        .arg("--data-dir")
        .arg(data_dir)
        .arg("--control-plane-bind")
        .arg(&config.control_plane_bind)
        .arg("--p2p-topic-shards")
        .arg(config.p2p_topic_shards.max(1).to_string());

    if config.autonomy_enabled {
        command.arg("--autonomy-enabled");
    }
    command
        .arg("--autonomy-interval-sec")
        .arg(config.autonomy_interval_sec.max(5).to_string());

    match &config.brain_provider {
        BrainProviderConfig::Rules => {
            command.arg("--brain-provider-kind").arg("rules");
        }
        BrainProviderConfig::Ollama { base_url, model } => {
            command
                .arg("--brain-provider-kind")
                .arg("ollama")
                .arg("--brain-base-url")
                .arg(base_url)
                .arg("--brain-model")
                .arg(model);
        }
        BrainProviderConfig::OpenaiCompatible {
            base_url,
            model,
            api_key_env,
        } => {
            command
                .arg("--brain-provider-kind")
                .arg("openai-compatible")
                .arg("--brain-base-url")
                .arg(base_url)
                .arg("--brain-model")
                .arg(model);
            if let Some(name) = api_key_env {
                command.arg("--brain-api-key-env").arg(name);
            }
        }
    }

    for source in &config.recovery_sources {
        command.arg("--recovery-source").arg(source);
    }
}

fn kernel_command(data_dir: &Path, config: &LocalConfig) -> Command {
    if let Ok(kernel_bin) = std::env::var("WATTETHERIA_KERNEL_BIN") {
        let mut command = Command::new(kernel_bin);
        append_kernel_runtime_args(&mut command, data_dir, config);
        return command;
    }

    let mut command = Command::new("cargo");
    command
        .arg("run")
        .arg("-p")
        .arg("wattetheria-kernel")
        .arg("--");
    append_kernel_runtime_args(&mut command, data_dir, config);
    command
}

pub(crate) fn read_config(data_dir: &Path) -> Result<LocalConfig> {
    let path = data_dir.join("config.json");
    if !path.exists() {
        return Ok(LocalConfig::default());
    }
    let raw = fs::read_to_string(path).context("read config")?;
    serde_json::from_str(&raw).context("parse config")
}

fn write_config(data_dir: &Path, config: &LocalConfig) -> Result<()> {
    let path = data_dir.join("config.json");
    fs::write(path, serde_json::to_string_pretty(config)?).context("write config")
}

pub(crate) fn load_or_create_control_token(path: PathBuf) -> Result<String> {
    if path.exists() {
        let token = fs::read_to_string(&path).context("read control token")?;
        let token = token.trim().to_string();
        if !token.is_empty() {
            return Ok(token);
        }
    }

    let token = uuid::Uuid::new_v4().to_string();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).context("create token directory")?;
    }
    fs::write(path, &token).context("write control token")?;
    Ok(token)
}

pub(crate) fn read_control_token(path: PathBuf) -> Result<String> {
    let token = fs::read_to_string(path).context("read control token")?;
    let token = token.trim().to_string();
    if token.is_empty() {
        bail!("control token is empty");
    }
    Ok(token)
}

pub(crate) fn can_write_storage(data_dir: &Path) -> Result<()> {
    let path = data_dir.join(".doctor_write_test");
    fs::write(&path, "ok").context("write storage probe")?;
    fs::remove_file(path).context("remove storage probe")
}

pub(crate) async fn check_control_plane(endpoint: &str, token: &str) -> Result<()> {
    let response = reqwest::Client::new()
        .get(format!("{endpoint}/v1/state"))
        .header("authorization", format!("Bearer {token}"))
        .timeout(Duration::from_secs(3))
        .send()
        .await
        .context("request control plane state")?;

    if !response.status().is_success() {
        bail!("state endpoint status {}", response.status());
    }
    Ok(())
}

pub(crate) async fn fetch_server_timestamp(endpoint: &str) -> Result<i64> {
    let response = reqwest::Client::new()
        .get(format!("{endpoint}/v1/health"))
        .timeout(Duration::from_secs(3))
        .send()
        .await
        .context("request control plane health")?;
    let body: serde_json::Value = response.json().await.context("parse health response")?;
    body["timestamp"]
        .as_i64()
        .context("health response missing timestamp")
}

async fn wait_for_control_plane(endpoint: &str, token: &str, timeout_seconds: u64) -> Result<()> {
    let mut attempts = 0_u64;
    loop {
        attempts += 1;
        if check_control_plane(endpoint, token).await.is_ok() {
            return Ok(());
        }
        if attempts >= timeout_seconds {
            bail!("control plane did not become healthy within timeout");
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}
