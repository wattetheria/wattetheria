use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::fs;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;
use wattetheria_kernel::brain::BrainProviderConfig;
use wattetheria_kernel::event_log::EventLog;
use wattetheria_kernel::local_db::LocalDb;
use wattetheria_kernel::mcp::McpRegistry;
use wattetheria_kernel::wallet_identity::load_or_create_wallet_backed_identity;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct LocalConfig {
    #[serde(default = "default_control_bind")]
    pub(crate) control_plane_bind: String,
    #[serde(default = "default_control_endpoint")]
    pub(crate) control_plane_endpoint: String,
    #[serde(default)]
    pub(crate) recovery_sources: Vec<String>,
    #[serde(default)]
    pub(crate) brain_provider: BrainProviderConfig,
    #[serde(default)]
    pub(crate) wattswarm_ui_base_url: Option<String>,
    #[serde(default)]
    pub(crate) wattswarm_sync_grpc_endpoint: Option<String>,
    #[serde(default)]
    pub(crate) servicenet_base_url: Option<String>,
    #[serde(default)]
    pub(crate) autonomy_enabled: bool,
    #[serde(default = "default_autonomy_interval_sec")]
    pub(crate) autonomy_interval_sec: u64,
    #[serde(default = "default_observatory_port")]
    pub(crate) observatory_port: u16,
    #[serde(default)]
    pub(crate) wattswarm_compose_dir: Option<String>,
}

fn default_control_bind() -> String {
    "127.0.0.1:7777".to_string()
}

fn default_control_endpoint() -> String {
    format!("http://{}", default_control_bind())
}

fn default_autonomy_interval_sec() -> u64 {
    30
}

fn default_observatory_port() -> u16 {
    8787
}

impl Default for LocalConfig {
    fn default() -> Self {
        let bind = default_control_bind();
        Self {
            control_plane_endpoint: default_control_endpoint(),
            control_plane_bind: bind,
            recovery_sources: Vec::new(),
            brain_provider: BrainProviderConfig::Rules,
            wattswarm_ui_base_url: None,
            wattswarm_sync_grpc_endpoint: None,
            servicenet_base_url: None,
            autonomy_enabled: false,
            autonomy_interval_sec: default_autonomy_interval_sec(),
            observatory_port: default_observatory_port(),
            wattswarm_compose_dir: None,
        }
    }
}

pub(crate) fn run_init(data_dir: &Path) -> Result<()> {
    fs::create_dir_all(data_dir).context("create data directory")?;
    fs::create_dir_all(data_dir.join("audit"))?;
    fs::create_dir_all(data_dir.join("snapshots"))?;
    fs::create_dir_all(data_dir.join("mcp"))?;
    fs::create_dir_all(data_dir.join("policy"))?;

    let identity = load_or_create_wallet_backed_identity(data_dir)?;
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
    let _ = LocalDb::open(data_dir.join("state.db"))?;

    let response = serde_json::json!({
        "status": "ok",
        "agent_did": identity.agent_did,
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

    // --- wattswarm (Docker) ---
    let wattswarm_started = start_wattswarm_docker(&config);
    if let Err(error) = &wattswarm_started {
        eprintln!("wattswarm docker: {error:#}");
    }

    // --- kernel ---
    let mut command = kernel_command(data_dir, &config);

    if attach {
        let obs_child = spawn_observatory(data_dir, &config).ok();
        let status = command.status().context("run kernel in attach mode")?;
        if let Some(mut child) = obs_child {
            let _ = child.kill();
        }
        if !status.success() {
            bail!("kernel exited with status: {status}");
        }
        return Ok(());
    }

    let kernel_log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(data_dir.join("daemon.log"))
        .context("open kernel daemon log")?;
    let kernel_log_err = kernel_log.try_clone().context("clone kernel log handle")?;

    let kernel_child = command
        .stdout(Stdio::from(kernel_log))
        .stderr(Stdio::from(kernel_log_err))
        .spawn()
        .context("spawn kernel daemon")?;

    fs::write(data_dir.join("daemon.pid"), kernel_child.id().to_string())
        .context("write kernel daemon pid")?;

    // --- observatory ---
    let observatory_pid = match spawn_observatory(data_dir, &config) {
        Ok(child) => {
            let pid = child.id();
            fs::write(data_dir.join("observatory.pid"), pid.to_string())
                .context("write observatory pid")?;
            Some(pid)
        }
        Err(error) => {
            eprintln!("observatory: {error:#}");
            None
        }
    };

    wait_for_control_plane(&config.control_plane_endpoint, &token, 300).await?;

    let response = serde_json::json!({
        "status": "ok",
        "kernel_pid": kernel_child.id(),
        "observatory_pid": observatory_pid,
        "observatory_port": config.observatory_port,
        "wattswarm_docker": wattswarm_started.is_ok(),
        "control_plane_endpoint": config.control_plane_endpoint,
        "log_file": data_dir.join("daemon.log"),
    });
    println!("{}", serde_json::to_string_pretty(&response)?);
    Ok(())
}

fn spawn_observatory(data_dir: &Path, config: &LocalConfig) -> Result<std::process::Child> {
    let mut command = observatory_command(config);
    let log_file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(data_dir.join("observatory.log"))
        .context("open observatory log")?;
    let log_file_err = log_file
        .try_clone()
        .context("clone observatory log handle")?;
    command
        .stdout(Stdio::from(log_file))
        .stderr(Stdio::from(log_file_err))
        .spawn()
        .context("spawn observatory daemon")
}

fn observatory_command(config: &LocalConfig) -> Command {
    if let Ok(bin) = std::env::var("WATTETHERIA_OBSERVATORY_BIN") {
        let mut command = Command::new(bin);
        command.env("PORT", config.observatory_port.to_string());
        return command;
    }
    if let Some(bin) = discover_observatory_bin() {
        let mut command = Command::new(bin);
        command.env("PORT", config.observatory_port.to_string());
        return command;
    }
    let mut command = Command::new("cargo");
    command
        .arg("run")
        .arg("-p")
        .arg("wattetheria-observatory")
        .arg("--");
    command.env("PORT", config.observatory_port.to_string());
    command
}

fn discover_observatory_bin() -> Option<PathBuf> {
    let current = std::env::current_exe().ok()?;
    let dir = current.parent()?;
    let candidate = dir.join(format!(
        "wattetheria-observatory{}",
        std::env::consts::EXE_SUFFIX
    ));
    if candidate.exists() {
        return Some(candidate);
    }
    None
}

fn start_wattswarm_docker(config: &LocalConfig) -> Result<()> {
    let compose_dir = config
        .wattswarm_compose_dir
        .as_deref()
        .context("wattswarm_compose_dir not configured in config.json")?;
    let dir = Path::new(compose_dir);
    if !dir.join("docker-compose.yml").exists() {
        bail!("docker-compose.yml not found in {}", dir.display());
    }

    let mut args = vec!["compose", "-f", "docker-compose.yml"];

    // Include the wattetheria overlay to expose gRPC sync port.
    let has_wattetheria_overlay = dir.join("docker-compose.wattetheria.yml").exists();
    if has_wattetheria_overlay {
        args.extend(["-f", "docker-compose.wattetheria.yml"]);

        // Ensure watt-net Docker network exists.
        let _ = Command::new("docker")
            .args(["network", "create", "watt-net"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }

    args.extend(["up", "-d"]);

    let status = Command::new("docker")
        .args(&args)
        .current_dir(dir)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .context("run docker compose for wattswarm")?;
    if !status.success() {
        bail!("docker compose up failed with status: {status}");
    }
    Ok(())
}

fn append_kernel_runtime_args(command: &mut Command, data_dir: &Path, config: &LocalConfig) {
    command
        .arg("--data-dir")
        .arg(data_dir)
        .arg("--control-plane-bind")
        .arg(&config.control_plane_bind);

    if config.autonomy_enabled {
        command.arg("--autonomy-enabled");
    }
    command
        .arg("--autonomy-interval-sec")
        .arg(config.autonomy_interval_sec.max(5).to_string());
    if let Some(base_url) = &config.wattswarm_ui_base_url {
        command.arg("--wattswarm-ui-base-url").arg(base_url);
    }
    if let Some(endpoint) = &config.wattswarm_sync_grpc_endpoint {
        command.arg("--wattswarm-sync-grpc-endpoint").arg(endpoint);
    }
    if let Some(base_url) = &config.servicenet_base_url {
        command.arg("--servicenet-base-url").arg(base_url);
    }
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

    if let Some(kernel_bin) = discover_kernel_bin() {
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

fn discover_kernel_bin() -> Option<PathBuf> {
    let current = std::env::current_exe().ok()?;
    let dir = current.parent()?;
    let candidate = dir.join(format!(
        "wattetheria-kernel{}",
        std::env::consts::EXE_SUFFIX
    ));
    if candidate.exists() {
        return Some(candidate);
    }
    None
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

#[cfg(test)]
mod tests {
    use super::{LocalConfig, append_kernel_runtime_args};
    use std::path::Path;
    use std::process::Command;

    #[test]
    fn kernel_runtime_args_include_autonomy_settings() {
        let mut command = Command::new("echo");
        let config = LocalConfig {
            autonomy_enabled: true,
            servicenet_base_url: Some("http://127.0.0.1:8042".to_string()),
            ..LocalConfig::default()
        };

        append_kernel_runtime_args(&mut command, Path::new("/tmp/wattetheria"), &config);

        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();

        assert!(args.contains(&"--autonomy-enabled".to_string()));
        assert!(
            args.windows(2).any(
                |pair| pair[0] == "--servicenet-base-url" && pair[1] == "http://127.0.0.1:8042"
            )
        );
        assert!(
            args.windows(2)
                .any(|pair| { pair[0] == "--autonomy-interval-sec" })
        );
    }
}
