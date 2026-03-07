//! CLI tools for bootstrap, diagnostics, policy approvals, and reporting.

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand, ValueEnum};
use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use wattetheria_kernel::brain::{BrainEngine, BrainProviderConfig};
use wattetheria_kernel::capabilities::{CapabilityPolicy, TrustLevel};
use wattetheria_kernel::data_ops::{
    create_snapshot, export_backup, import_backup, migrate_data_dir, recover_if_corrupt,
    recover_if_corrupt_with_sources,
};
use wattetheria_kernel::event_log::{EventLog, EventRecord};
use wattetheria_kernel::identity::Identity;
use wattetheria_kernel::mcp::{
    McpRegistry, McpServerConfig as KernelMcpServerConfig, call_tool, list_tools,
};
use wattetheria_kernel::night_shift::generate_night_shift_report;
use wattetheria_kernel::oracle::OracleRegistry;
use wattetheria_kernel::policy_engine::{CapabilityRequest, DecisionKind, PolicyEngine};
use wattetheria_kernel::skill_package::{InstalledSkill, SkillPackage, SkillRegistry};
use wattetheria_kernel::skill_runtime::{EchoSkill, ProcessSkill, SkillManifest, SkillRuntime};
use wattetheria_kernel::summary::build_signed_summary;
use wattetheria_kernel::types::AgentStats;

#[derive(Debug, Parser)]
#[command(name = "wattetheria")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    Init {
        #[arg(long, default_value = ".wattetheria")]
        data_dir: PathBuf,
    },
    Up {
        #[arg(long, default_value = ".wattetheria")]
        data_dir: PathBuf,
        #[arg(long)]
        control_plane_bind: Option<String>,
        #[arg(long, default_value_t = false)]
        attach: bool,
    },
    Doctor {
        #[arg(long, default_value = ".wattetheria")]
        data_dir: PathBuf,
        #[arg(long)]
        control_plane: Option<String>,
        #[arg(long, default_value_t = false)]
        brain: bool,
    },
    Policy {
        #[arg(long, default_value = ".wattetheria")]
        data_dir: PathBuf,
        #[arg(long)]
        control_plane: Option<String>,
        #[command(subcommand)]
        command: PolicyCommand,
    },
    Governance {
        #[arg(long, default_value = ".wattetheria")]
        data_dir: PathBuf,
        #[arg(long)]
        control_plane: Option<String>,
        #[command(subcommand)]
        command: GovernanceCommand,
    },
    Skill {
        #[arg(long, default_value = ".wattetheria")]
        data_dir: PathBuf,
        #[command(subcommand)]
        command: SkillCommand,
    },
    Mcp {
        #[arg(long, default_value = ".wattetheria")]
        data_dir: PathBuf,
        #[command(subcommand)]
        command: McpCommand,
    },
    Brain {
        #[arg(long, default_value = ".wattetheria")]
        data_dir: PathBuf,
        #[command(subcommand)]
        command: BrainCommand,
    },
    Data {
        #[arg(long, default_value = ".wattetheria")]
        data_dir: PathBuf,
        #[command(subcommand)]
        command: DataCommand,
    },
    Oracle {
        #[arg(long, default_value = ".wattetheria")]
        data_dir: PathBuf,
        #[command(subcommand)]
        command: OracleCommand,
    },
    UpgradeCheck {
        #[arg(long, default_value = env!("CARGO_PKG_VERSION"))]
        current: String,
        #[arg(long)]
        latest: Option<String>,
    },
    NightShift {
        #[arg(long, default_value = ".wattetheria/events.jsonl")]
        event_log: PathBuf,
        #[arg(long, default_value_t = 12)]
        hours: i64,
        #[arg(long)]
        out: Option<PathBuf>,
    },
    PostSummary {
        #[arg(long, default_value = ".wattetheria/identity.json")]
        identity: PathBuf,
        #[arg(long, default_value = ".wattetheria/events.jsonl")]
        events: PathBuf,
        #[arg(long)]
        subnet: Option<String>,
        #[arg(long, default_value = "http://127.0.0.1:8787/api/summaries")]
        endpoint: String,
        #[arg(long, default_value_t = false)]
        dry_run: bool,
    },
}

#[derive(Debug, Subcommand)]
enum PolicyCommand {
    Check {
        #[arg(long)]
        subject: String,
        #[arg(long, value_enum)]
        trust: TrustArg,
        #[arg(long)]
        capability: String,
        #[arg(long)]
        reason: Option<String>,
    },
    Pending,
    Approve {
        #[arg(long)]
        request_id: String,
        #[arg(long)]
        approved_by: String,
        #[arg(long, value_enum)]
        scope: ScopeArg,
    },
}

#[derive(Debug, Subcommand)]
enum GovernanceCommand {
    Planets,
    Proposals {
        #[arg(long)]
        subnet_id: Option<String>,
    },
    Propose {
        #[arg(long)]
        subnet_id: String,
        #[arg(long)]
        kind: String,
        #[arg(long)]
        created_by: String,
        #[arg(long, default_value = "{}")]
        payload: String,
    },
    Vote {
        #[arg(long)]
        proposal_id: String,
        #[arg(long)]
        voter: String,
        #[arg(long)]
        approve: bool,
    },
    Finalize {
        #[arg(long)]
        proposal_id: String,
        #[arg(long, default_value_t = 1)]
        min_votes_for: usize,
    },
}

#[derive(Debug, Subcommand)]
enum SkillCommand {
    Install {
        source: String,
    },
    Enable {
        id: String,
    },
    Disable {
        id: String,
    },
    Perms {
        id: String,
    },
    Test {
        id: String,
        #[arg(long, default_value = "{\"hello\":\"world\"}")]
        input: String,
    },
}

#[derive(Debug, Subcommand)]
enum McpCommand {
    Add {
        config: PathBuf,
    },
    Enable {
        server: String,
    },
    Disable {
        server: String,
    },
    List,
    Test {
        server: String,
        tool: String,
        #[arg(long, default_value = "{}")]
        input: String,
    },
}

#[derive(Debug, Subcommand)]
enum BrainCommand {
    HumanizeNightShift {
        #[arg(long, default_value_t = 12)]
        hours: i64,
    },
    ProposeActions,
    PlanSkillCalls {
        #[arg(long, default_value_t = false)]
        enable: bool,
    },
}

#[derive(Debug, Subcommand)]
enum DataCommand {
    SnapshotCreate,
    Recover {
        #[arg(long = "source")]
        source: Vec<PathBuf>,
    },
    Migrate {
        #[arg(long, default_value = "0.2.0")]
        to: String,
    },
    BackupExport {
        out: PathBuf,
    },
    BackupImport {
        input: PathBuf,
    },
}

#[derive(Debug, Subcommand)]
enum OracleCommand {
    Publish {
        feed_id: String,
        #[arg(long, default_value = "{}")]
        payload: String,
        #[arg(long, default_value_t = 1)]
        price_watt: i64,
    },
    Subscribe {
        feed_id: String,
        #[arg(long, default_value_t = 1)]
        max_price_watt: i64,
    },
    Credit {
        #[arg(long)]
        agent: Option<String>,
        #[arg(long)]
        watt: i64,
    },
    Balance {
        #[arg(long)]
        agent: Option<String>,
    },
    Pull {
        feed_id: String,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum TrustArg {
    Trusted,
    Verified,
    Untrusted,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ScopeArg {
    Once,
    Session,
    Permanent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct LocalConfig {
    #[serde(default = "default_control_bind")]
    control_plane_bind: String,
    #[serde(default = "default_control_endpoint")]
    control_plane_endpoint: String,
    #[serde(default = "default_p2p_topic_shards")]
    p2p_topic_shards: usize,
    #[serde(default)]
    recovery_sources: Vec<String>,
    #[serde(default)]
    brain_provider: BrainProviderConfig,
    #[serde(default)]
    autonomy_enabled: bool,
    #[serde(default = "default_autonomy_interval_sec")]
    autonomy_interval_sec: u64,
    #[serde(default)]
    autonomy_skill_planner_enabled: bool,
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
            autonomy_skill_planner_enabled: false,
        }
    }
}

#[derive(Debug, Serialize)]
struct DoctorCheck {
    name: String,
    status: String,
    detail: String,
}

#[derive(Debug, Serialize)]
struct DoctorReport {
    data_dir: String,
    overall: String,
    checks: Vec<DoctorCheck>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Init { data_dir } => run_init(&data_dir)?,
        Commands::Up {
            data_dir,
            control_plane_bind,
            attach,
        } => run_up(&data_dir, control_plane_bind, attach).await?,
        Commands::Doctor {
            data_dir,
            control_plane,
            brain,
        } => run_doctor(&data_dir, control_plane.as_deref(), brain).await?,
        Commands::Policy {
            data_dir,
            control_plane,
            command,
        } => run_policy(&data_dir, control_plane.as_deref(), command).await?,
        Commands::Governance {
            data_dir,
            control_plane,
            command,
        } => run_governance(&data_dir, control_plane.as_deref(), command).await?,
        Commands::Skill { data_dir, command } => run_skill(&data_dir, command).await?,
        Commands::Mcp { data_dir, command } => run_mcp(&data_dir, command).await?,
        Commands::Brain { data_dir, command } => run_brain(&data_dir, command).await?,
        Commands::Data { data_dir, command } => run_data(&data_dir, command)?,
        Commands::Oracle { data_dir, command } => run_oracle(&data_dir, command)?,
        Commands::UpgradeCheck { current, latest } => {
            run_upgrade_check(&current, latest.as_deref())?;
        }
        Commands::NightShift {
            event_log,
            hours,
            out,
        } => run_night_shift(&event_log, hours, out)?,
        Commands::PostSummary {
            identity,
            events,
            subnet,
            endpoint,
            dry_run,
        } => post_summary(&identity, &events, subnet, &endpoint, dry_run).await?,
    }
    Ok(())
}

fn run_init(data_dir: &Path) -> Result<()> {
    fs::create_dir_all(data_dir).context("create data directory")?;
    fs::create_dir_all(data_dir.join("audit"))?;
    fs::create_dir_all(data_dir.join("snapshots"))?;
    fs::create_dir_all(data_dir.join("skills"))?;
    fs::create_dir_all(data_dir.join("skills/store"))?;
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

    let _ = SkillRegistry::load_or_new(data_dir.join("skills/registry.json"))?;
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

async fn run_up(data_dir: &Path, bind_override: Option<String>, attach: bool) -> Result<()> {
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

    // CI can be noticeably slower here because `cargo test` shells out to `cargo run`
    // for the kernel, which may wait on target/package locks before the daemon is healthy.
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
    if config.autonomy_skill_planner_enabled {
        command.arg("--autonomy-skill-planner-enabled");
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

    let mut command = Command::new("cargo");
    command
        .arg("run")
        .arg("-p")
        .arg("wattetheria-kernel")
        .arg("--");
    append_kernel_runtime_args(&mut command, data_dir, config);
    command
}
async fn run_doctor(
    data_dir: &Path,
    endpoint_override: Option<&str>,
    brain_check: bool,
) -> Result<()> {
    let config = read_config(data_dir).unwrap_or_default();
    let endpoint = endpoint_override.map_or_else(
        || config.control_plane_endpoint.clone(),
        ToString::to_string,
    );

    let mut checks = Vec::new();

    push_check(
        &mut checks,
        "identity",
        Identity::load(data_dir.join("identity.json")).is_ok(),
        "identity file and keypair are valid",
        "identity file missing or invalid",
    );
    append_signing_check(&mut checks, data_dir.join("identity.json"));
    append_network_config_check(&mut checks, &config);

    append_event_log_check(&mut checks, data_dir.join("events.jsonl"));

    push_check(
        &mut checks,
        "storage",
        can_write_storage(data_dir).is_ok(),
        "data directory writable",
        "cannot write to data directory",
    );

    let token = read_control_token(data_dir.join("control.token"));
    push_check(
        &mut checks,
        "control_token",
        token.is_ok(),
        "control token is available",
        "control token missing",
    );

    append_control_plane_checks(&mut checks, &endpoint, token).await;
    append_mcp_registry_check(&mut checks, data_dir);
    append_provider_checks(&mut checks, &config, brain_check).await;
    finalize_doctor_report(data_dir, checks)
}

fn append_mcp_registry_check(checks: &mut Vec<DoctorCheck>, data_dir: &Path) {
    let registry_path = data_dir.join("mcp/servers.json");
    match McpRegistry::load_or_new(registry_path) {
        Ok(registry) => {
            let total = registry.list().len();
            checks.push(DoctorCheck {
                name: "mcp_registry".to_string(),
                status: if total == 0 {
                    "warn".to_string()
                } else {
                    "ok".to_string()
                },
                detail: format!("configured MCP servers: {total}"),
            });
        }
        Err(error) => checks.push(DoctorCheck {
            name: "mcp_registry".to_string(),
            status: "fail".to_string(),
            detail: error.to_string(),
        }),
    }
}

fn append_signing_check(checks: &mut Vec<DoctorCheck>, identity_path: PathBuf) {
    match Identity::load(identity_path) {
        Ok(identity) => {
            let probe = serde_json::json!({"probe":"doctor_signing"});
            match wattetheria_kernel::signing::sign_payload(&probe, &identity).and_then(
                |signature| {
                    wattetheria_kernel::signing::verify_payload(
                        &probe,
                        &signature,
                        &identity.agent_id,
                    )
                },
            ) {
                Ok(true) => checks.push(DoctorCheck {
                    name: "signing".to_string(),
                    status: "ok".to_string(),
                    detail: "sign + verify roundtrip passed".to_string(),
                }),
                Ok(false) => checks.push(DoctorCheck {
                    name: "signing".to_string(),
                    status: "fail".to_string(),
                    detail: "signature verification returned false".to_string(),
                }),
                Err(error) => checks.push(DoctorCheck {
                    name: "signing".to_string(),
                    status: "fail".to_string(),
                    detail: error.to_string(),
                }),
            }
        }
        Err(error) => checks.push(DoctorCheck {
            name: "signing".to_string(),
            status: "fail".to_string(),
            detail: error.to_string(),
        }),
    }
}

fn append_network_config_check(checks: &mut Vec<DoctorCheck>, config: &LocalConfig) {
    let endpoint_ok = reqwest::Url::parse(&config.control_plane_endpoint).is_ok();
    checks.push(DoctorCheck {
        name: "network_endpoint".to_string(),
        status: if endpoint_ok {
            "ok".to_string()
        } else {
            "fail".to_string()
        },
        detail: if endpoint_ok {
            format!(
                "control plane endpoint is valid: {}",
                config.control_plane_endpoint
            )
        } else {
            format!(
                "invalid control plane endpoint: {}",
                config.control_plane_endpoint
            )
        },
    });

    let bind = config.control_plane_bind.trim();
    let status = if bind.starts_with("127.") || bind.starts_with("localhost") {
        "warn"
    } else {
        "ok"
    };
    let detail = if status == "warn" {
        format!("control plane bind is local-only ({bind}); NAT reachability is limited")
    } else {
        format!("control plane bind allows remote reachability checks ({bind})")
    };
    checks.push(DoctorCheck {
        name: "nat_reachability_hint".to_string(),
        status: status.to_string(),
        detail,
    });
}

fn append_event_log_check(checks: &mut Vec<DoctorCheck>, event_path: PathBuf) {
    match EventLog::new(event_path) {
        Ok(log) => match log.verify_chain() {
            Ok((true, _)) => checks.push(DoctorCheck {
                name: "event_log".to_string(),
                status: "ok".to_string(),
                detail: "hash chain verified".to_string(),
            }),
            Ok((false, reason)) => checks.push(DoctorCheck {
                name: "event_log".to_string(),
                status: "fail".to_string(),
                detail: reason.unwrap_or_else(|| "hash chain invalid".to_string()),
            }),
            Err(error) => checks.push(DoctorCheck {
                name: "event_log".to_string(),
                status: "fail".to_string(),
                detail: error.to_string(),
            }),
        },
        Err(error) => checks.push(DoctorCheck {
            name: "event_log".to_string(),
            status: "fail".to_string(),
            detail: error.to_string(),
        }),
    }
}

async fn append_control_plane_checks(
    checks: &mut Vec<DoctorCheck>,
    endpoint: &str,
    token: Result<String>,
) {
    match token {
        Ok(token) => {
            if let Err(error) = check_control_plane(endpoint, &token).await {
                checks.push(DoctorCheck {
                    name: "control_plane_health".to_string(),
                    status: "fail".to_string(),
                    detail: error.to_string(),
                });
                return;
            }

            checks.push(DoctorCheck {
                name: "control_plane_health".to_string(),
                status: "ok".to_string(),
                detail: format!("reachable at {endpoint}"),
            });

            if let Ok(server_ts) = fetch_server_timestamp(endpoint).await {
                let drift = (chrono::Utc::now().timestamp() - server_ts).abs();
                checks.push(DoctorCheck {
                    name: "time_drift".to_string(),
                    status: if drift <= 120 {
                        "ok".to_string()
                    } else {
                        "fail".to_string()
                    },
                    detail: format!("clock drift: {drift}s"),
                });
            }
        }
        Err(_) => checks.push(DoctorCheck {
            name: "control_plane_health".to_string(),
            status: "fail".to_string(),
            detail: "token unavailable, skipping health check".to_string(),
        }),
    }
}

async fn append_provider_checks(
    checks: &mut Vec<DoctorCheck>,
    config: &LocalConfig,
    brain_check: bool,
) {
    let provider_name = match &config.brain_provider {
        BrainProviderConfig::Rules => "rules".to_string(),
        BrainProviderConfig::Ollama { base_url, model } => {
            format!("ollama model={model} url={base_url}")
        }
        BrainProviderConfig::OpenaiCompatible {
            base_url, model, ..
        } => {
            format!("openai-compatible model={model} url={base_url}")
        }
    };

    checks.push(DoctorCheck {
        name: "brain_provider".to_string(),
        status: "ok".to_string(),
        detail: provider_name,
    });

    if brain_check {
        let engine = BrainEngine::from_config(&config.brain_provider);
        match engine.doctor().await {
            Ok(status) => checks.push(DoctorCheck {
                name: "brain_health".to_string(),
                status: "ok".to_string(),
                detail: status,
            }),
            Err(error) => checks.push(DoctorCheck {
                name: "brain_health".to_string(),
                status: "fail".to_string(),
                detail: error.to_string(),
            }),
        }
    }
}

fn finalize_doctor_report(data_dir: &Path, checks: Vec<DoctorCheck>) -> Result<()> {
    let has_fail = checks.iter().any(|check| check.status == "fail");
    let report = DoctorReport {
        data_dir: data_dir.display().to_string(),
        overall: if has_fail {
            "fail".to_string()
        } else {
            "ok".to_string()
        },
        checks,
    };

    println!("{}", serde_json::to_string_pretty(&report)?);

    if has_fail {
        bail!("doctor detected failing checks");
    }

    Ok(())
}

async fn run_policy(
    data_dir: &Path,
    endpoint_override: Option<&str>,
    command: PolicyCommand,
) -> Result<()> {
    let config = read_config(data_dir).unwrap_or_default();
    let endpoint =
        endpoint_override.map_or_else(|| config.control_plane_endpoint, ToString::to_string);
    let token = read_control_token(data_dir.join("control.token"))?;
    let client = reqwest::Client::new();

    match command {
        PolicyCommand::Check {
            subject,
            trust,
            capability,
            reason,
        } => {
            let payload = serde_json::json!({
                "subject": subject,
                "trust": trust.as_str(),
                "capability": capability,
                "reason": reason,
            });
            let res = client
                .post(format!("{endpoint}/v1/policy/check"))
                .header("authorization", format!("Bearer {token}"))
                .json(&payload)
                .send()
                .await
                .context("call policy check")?;
            let status = res.status().as_u16();
            let body: Value = res.json().await.unwrap_or(Value::Null);
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "status": status,
                    "body": body,
                }))?
            );
        }
        PolicyCommand::Pending => {
            let res = client
                .get(format!("{endpoint}/v1/policy/pending"))
                .header("authorization", format!("Bearer {token}"))
                .send()
                .await
                .context("call policy pending")?;
            let body: Value = res.json().await.unwrap_or(Value::Null);
            println!("{}", serde_json::to_string_pretty(&body)?);
        }
        PolicyCommand::Approve {
            request_id,
            approved_by,
            scope,
        } => {
            let payload = serde_json::json!({
                "request_id": request_id,
                "approved_by": approved_by,
                "scope": scope.as_str(),
            });
            let res = client
                .post(format!("{endpoint}/v1/policy/approve"))
                .header("authorization", format!("Bearer {token}"))
                .json(&payload)
                .send()
                .await
                .context("call policy approve")?;
            let status = res.status().as_u16();
            let body: Value = res.json().await.unwrap_or(Value::Null);
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "status": status,
                    "body": body,
                }))?
            );
        }
    }

    Ok(())
}

#[allow(clippy::too_many_lines)]
async fn run_governance(
    data_dir: &Path,
    endpoint_override: Option<&str>,
    command: GovernanceCommand,
) -> Result<()> {
    let config = read_config(data_dir).unwrap_or_default();
    let endpoint =
        endpoint_override.map_or_else(|| config.control_plane_endpoint, ToString::to_string);
    let token = read_control_token(data_dir.join("control.token"))?;
    let client = reqwest::Client::new();

    match command {
        GovernanceCommand::Planets => {
            let res = client
                .get(format!("{endpoint}/v1/governance/planets"))
                .header("authorization", format!("Bearer {token}"))
                .send()
                .await
                .context("call governance planets")?;
            let body: Value = res.json().await.unwrap_or(Value::Null);
            println!("{}", serde_json::to_string_pretty(&body)?);
        }
        GovernanceCommand::Proposals { subnet_id } => {
            let mut req = client
                .get(format!("{endpoint}/v1/governance/proposals"))
                .header("authorization", format!("Bearer {token}"));
            if let Some(subnet_id) = subnet_id {
                req = req.query(&[("subnet_id", subnet_id)]);
            }
            let res = req.send().await.context("call governance proposals")?;
            let body: Value = res.json().await.unwrap_or(Value::Null);
            println!("{}", serde_json::to_string_pretty(&body)?);
        }
        GovernanceCommand::Propose {
            subnet_id,
            kind,
            created_by,
            payload,
        } => {
            let payload: Value = serde_json::from_str(&payload).context("parse --payload JSON")?;
            let res = client
                .post(format!("{endpoint}/v1/governance/proposals"))
                .header("authorization", format!("Bearer {token}"))
                .json(&serde_json::json!({
                    "subnet_id": subnet_id,
                    "kind": kind,
                    "payload": payload,
                    "created_by": created_by,
                }))
                .send()
                .await
                .context("call governance propose")?;
            let status = res.status().as_u16();
            let body: Value = res.json().await.unwrap_or(Value::Null);
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({"status": status, "body": body}))?
            );
        }
        GovernanceCommand::Vote {
            proposal_id,
            voter,
            approve,
        } => {
            let res = client
                .post(format!("{endpoint}/v1/governance/proposals/vote"))
                .header("authorization", format!("Bearer {token}"))
                .json(&serde_json::json!({
                    "proposal_id": proposal_id,
                    "voter": voter,
                    "approve": approve,
                }))
                .send()
                .await
                .context("call governance vote")?;
            let status = res.status().as_u16();
            let body: Value = res.json().await.unwrap_or(Value::Null);
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({"status": status, "body": body}))?
            );
        }
        GovernanceCommand::Finalize {
            proposal_id,
            min_votes_for,
        } => {
            let res = client
                .post(format!("{endpoint}/v1/governance/proposals/finalize"))
                .header("authorization", format!("Bearer {token}"))
                .json(&serde_json::json!({
                    "proposal_id": proposal_id,
                    "min_votes_for": min_votes_for,
                }))
                .send()
                .await
                .context("call governance finalize")?;
            let status = res.status().as_u16();
            let body: Value = res.json().await.unwrap_or(Value::Null);
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({"status": status, "body": body}))?
            );
        }
    }

    Ok(())
}

async fn run_skill(data_dir: &Path, command: SkillCommand) -> Result<()> {
    run_init(data_dir)?;
    let mut registry = SkillRegistry::load_or_new(data_dir.join("skills/registry.json"))?;

    match command {
        SkillCommand::Install { source } => {
            let (holder, source_path, resolved_source) =
                resolve_skill_source(data_dir, &source).await?;
            let package = SkillPackage::load(&source_path)?;
            let installed =
                registry.install(&package, data_dir.join("skills/store"), &resolved_source)?;
            let _keep_alive = holder;
            println!("{}", serde_json::to_string_pretty(&installed)?);
        }
        SkillCommand::Enable { id } => {
            let updated = registry.set_enabled(&id, true)?;
            println!("{}", serde_json::to_string_pretty(&updated)?);
        }
        SkillCommand::Disable { id } => {
            let updated = registry.set_enabled(&id, false)?;
            println!("{}", serde_json::to_string_pretty(&updated)?);
        }
        SkillCommand::Perms { id } => {
            let skill = registry.get(&id)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "id": skill.id,
                    "version": skill.version,
                    "enabled": skill.enabled,
                    "trust": skill.trust,
                    "required_caps": skill.required_caps,
                    "source": skill.source,
                }))?
            );
        }
        SkillCommand::Test { id, input } => {
            let skill = registry.get(&id)?;
            let payload: Value = serde_json::from_str(&input).context("parse --input JSON")?;
            run_skill_test(data_dir, &skill, &payload)?;
        }
    }

    Ok(())
}

async fn run_mcp(data_dir: &Path, command: McpCommand) -> Result<()> {
    run_init(data_dir)?;
    let mut registry = McpRegistry::load_or_new(data_dir.join("mcp/servers.json"))?;

    match command {
        McpCommand::Add { config } => {
            let raw = fs::read_to_string(&config)
                .with_context(|| format!("read mcp config {}", config.display()))?;
            let server: KernelMcpServerConfig =
                serde_json::from_str(&raw).context("parse mcp server config")?;
            let added = registry.add_server(server)?;
            println!("{}", serde_json::to_string_pretty(&added)?);
        }
        McpCommand::Enable { server } => {
            let updated = registry.set_enabled(&server, true)?;
            println!("{}", serde_json::to_string_pretty(&updated)?);
        }
        McpCommand::Disable { server } => {
            let updated = registry.set_enabled(&server, false)?;
            println!("{}", serde_json::to_string_pretty(&updated)?);
        }
        McpCommand::List => {
            println!("{}", serde_json::to_string_pretty(&registry.list())?);
        }
        McpCommand::Test {
            server,
            tool,
            input,
        } => {
            let config = registry.get(&server)?;
            let tools = list_tools(&config).await?;
            if !tools.iter().any(|descriptor| descriptor.name == tool) {
                bail!("tool {tool} not exposed by mcp server {server}");
            }
            let payload: Value = serde_json::from_str(&input).context("parse --input JSON")?;
            run_mcp_test(data_dir, &config, &tool, payload).await?;
        }
    }

    Ok(())
}

async fn run_brain(data_dir: &Path, command: BrainCommand) -> Result<()> {
    run_init(data_dir)?;
    let config = read_config(data_dir).unwrap_or_default();
    let engine = BrainEngine::from_config(&config.brain_provider);
    let audit =
        BrainAuditContext::new(data_dir, brain_provider_descriptor(&config.brain_provider))?;

    match command {
        BrainCommand::HumanizeNightShift { hours } => {
            run_brain_humanize(data_dir, &engine, &audit, hours).await?;
        }
        BrainCommand::ProposeActions => {
            run_brain_propose_actions(data_dir, &engine, &audit).await?;
        }
        BrainCommand::PlanSkillCalls { enable } => {
            run_brain_plan_skill_calls(data_dir, &engine, &audit, enable).await?;
        }
    }

    Ok(())
}

struct BrainAuditContext {
    event_log: EventLog,
    identity: Identity,
    provider: Value,
}

struct BrainInvocationAudit {
    operation: String,
    input_digest: String,
    started: Instant,
}

impl BrainAuditContext {
    fn new(data_dir: &Path, provider: Value) -> Result<Self> {
        Ok(Self {
            event_log: EventLog::new(data_dir.join("events.jsonl"))?,
            identity: Identity::load_or_create(data_dir.join("identity.json"))?,
            provider,
        })
    }

    fn request(&self, operation: &str, input: &Value) -> Result<BrainInvocationAudit> {
        let input_digest = digest_value(input)?;
        self.event_log.append_signed(
            "BRAIN_INVOKE_REQUEST",
            serde_json::json!({
                "operation": operation,
                "provider": self.provider,
                "input_digest": input_digest,
            }),
            &self.identity,
        )?;

        Ok(BrainInvocationAudit {
            operation: operation.to_string(),
            input_digest,
            started: Instant::now(),
        })
    }

    fn result(
        &self,
        invocation: &BrainInvocationAudit,
        output: Option<&Value>,
        error: Option<&str>,
    ) -> Result<()> {
        let duration_ms =
            u64::try_from(invocation.started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let output_digest = output.map(digest_value).transpose()?;
        self.event_log.append_signed(
            "BRAIN_INVOKE_RESULT",
            serde_json::json!({
                "operation": invocation.operation,
                "provider": self.provider,
                "input_digest": invocation.input_digest,
                "output_digest": output_digest,
                "duration_ms": duration_ms,
                "status": if error.is_some() { "error" } else { "ok" },
                "error": error,
            }),
            &self.identity,
        )?;
        Ok(())
    }
}

async fn run_brain_humanize(
    data_dir: &Path,
    engine: &BrainEngine,
    audit: &BrainAuditContext,
    hours: i64,
) -> Result<()> {
    let report = build_night_shift_value(data_dir, hours)?;
    let invocation = audit.request("humanize_night_shift", &report)?;

    let human = match engine.humanize_night_shift(&report).await {
        Ok(human) => human,
        Err(error) => {
            audit.result(&invocation, None, Some(&error.to_string()))?;
            return Err(error);
        }
    };

    if let Err(error) = validate_schema_file(
        &schema_file_path("human_report.json"),
        &serde_json::to_value(&human)?,
    ) {
        audit.result(&invocation, None, Some(&error.to_string()))?;
        return Err(error);
    }

    let output = serde_json::to_value(&human)?;
    audit.result(&invocation, Some(&output), None)?;
    println!("{}", serde_json::to_string_pretty(&human)?);
    Ok(())
}

async fn run_brain_propose_actions(
    data_dir: &Path,
    engine: &BrainEngine,
    audit: &BrainAuditContext,
) -> Result<()> {
    let state = build_local_state_value(data_dir)?;
    let invocation = audit.request("propose_actions", &state)?;

    let proposals = match engine.propose_actions(&state).await {
        Ok(proposals) => proposals,
        Err(error) => {
            audit.result(&invocation, None, Some(&error.to_string()))?;
            return Err(error);
        }
    };

    for proposal in &proposals {
        if let Err(error) = validate_schema_file(
            &schema_file_path("action_proposal.json"),
            &serde_json::to_value(proposal)?,
        ) {
            audit.result(&invocation, None, Some(&error.to_string()))?;
            return Err(error);
        }
    }

    let output = serde_json::to_value(&proposals)?;
    audit.result(&invocation, Some(&output), None)?;
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

async fn run_brain_plan_skill_calls(
    data_dir: &Path,
    engine: &BrainEngine,
    audit: &BrainAuditContext,
    enable: bool,
) -> Result<()> {
    let mut state = build_local_state_value(data_dir)?;
    state["skill_planner_enabled"] = Value::Bool(enable);

    let report = build_night_shift_value(data_dir, 12)?;
    state["latest_report_digest"] = Value::String(digest_value(&report)?);

    let invocation = audit.request("plan_skill_calls", &state)?;

    let plans = match engine.plan_skill_calls(&state).await {
        Ok(plans) => plans,
        Err(error) => {
            audit.result(&invocation, None, Some(&error.to_string()))?;
            return Err(error);
        }
    };

    for plan in &plans {
        if let Err(error) = validate_schema_file(
            &schema_file_path("skill_call_plan.json"),
            &serde_json::to_value(plan)?,
        ) {
            audit.result(&invocation, None, Some(&error.to_string()))?;
            return Err(error);
        }
    }

    let output = serde_json::to_value(&plans)?;
    audit.result(&invocation, Some(&output), None)?;
    println!("{}", serde_json::to_string_pretty(&output)?);
    Ok(())
}

fn brain_provider_descriptor(provider: &BrainProviderConfig) -> Value {
    match provider {
        BrainProviderConfig::Rules => serde_json::json!({"kind": "rules"}),
        BrainProviderConfig::Ollama { base_url, model } => {
            serde_json::json!({"kind": "ollama", "base_url": base_url, "model": model})
        }
        BrainProviderConfig::OpenaiCompatible {
            base_url,
            model,
            api_key_env,
        } => serde_json::json!({
            "kind": "openai-compatible",
            "base_url": base_url,
            "model": model,
            "api_key_env": api_key_env,
        }),
    }
}
fn run_data(data_dir: &Path, command: DataCommand) -> Result<()> {
    run_init(data_dir)?;
    let events = data_dir.join("events.jsonl");
    let snapshots = data_dir.join("snapshots");

    match command {
        DataCommand::SnapshotCreate => {
            let meta = create_snapshot(&events, &snapshots)?;
            println!("{}", serde_json::to_string_pretty(&meta)?);
        }
        DataCommand::Recover { source } => {
            let recovered = if source.is_empty() {
                recover_if_corrupt(&events, &snapshots)?
            } else {
                recover_if_corrupt_with_sources(&events, &snapshots, &source)?
            };
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "recovered": recovered.is_some(),
                    "snapshot": recovered,
                }))?
            );
        }
        DataCommand::Migrate { to } => {
            let report = migrate_data_dir(data_dir, &to)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        DataCommand::BackupExport { out } => {
            export_backup(data_dir, &out)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "status": "ok",
                    "archive": out,
                }))?
            );
        }
        DataCommand::BackupImport { input } => {
            import_backup(&input, data_dir)?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "status": "ok",
                    "source": input,
                    "data_dir": data_dir,
                }))?
            );
        }
    }

    Ok(())
}

fn run_oracle(data_dir: &Path, command: OracleCommand) -> Result<()> {
    run_init(data_dir)?;
    let mut oracle = OracleRegistry::load_or_new(data_dir.join("oracle/state.json"))?;
    let identity = Identity::load_or_create(data_dir.join("identity.json"))?;
    let event_log = EventLog::new(data_dir.join("events.jsonl"))?;

    match command {
        OracleCommand::Publish {
            feed_id,
            payload,
            price_watt,
        } => {
            let payload: Value = serde_json::from_str(&payload).context("parse --payload JSON")?;
            ensure_capabilities_allowed(
                data_dir,
                &format!("oracle:publisher:{}", identity.agent_id),
                TrustLevel::Trusted,
                &[String::from("oracle.publish")],
                Some("oracle.publish"),
                Some(&payload),
            )?;
            let feed =
                oracle.publish(&feed_id, payload, price_watt, &identity, Some(&event_log))?;
            oracle.persist(data_dir.join("oracle/state.json"))?;
            println!("{}", serde_json::to_string_pretty(&feed)?);
        }
        OracleCommand::Subscribe {
            feed_id,
            max_price_watt,
        } => {
            ensure_capabilities_allowed(
                data_dir,
                &format!("oracle:subscriber:{}", identity.agent_id),
                TrustLevel::Verified,
                &[String::from("oracle.subscribe")],
                Some("oracle.subscribe"),
                None,
            )?;
            let subscription = oracle.subscribe(
                &identity.agent_id,
                &feed_id,
                max_price_watt,
                &identity,
                Some(&event_log),
            )?;
            oracle.persist(data_dir.join("oracle/state.json"))?;
            println!("{}", serde_json::to_string_pretty(&subscription)?);
        }
        OracleCommand::Credit { agent, watt } => {
            let target = agent.unwrap_or_else(|| identity.agent_id.clone());
            let balance = oracle.credit(&target, watt)?;
            oracle.persist(data_dir.join("oracle/state.json"))?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "agent_id": target,
                    "balance": balance,
                }))?
            );
        }
        OracleCommand::Balance { agent } => {
            let target = agent.unwrap_or_else(|| identity.agent_id.clone());
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "agent_id": target,
                    "balance": oracle.balance_of(&target),
                }))?
            );
        }
        OracleCommand::Pull { feed_id } => {
            let (feeds, settlement) = oracle.pull_for_subscriber_settled(
                &identity.agent_id,
                &feed_id,
                &identity,
                Some(&event_log),
            )?;
            oracle.persist(data_dir.join("oracle/state.json"))?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "settlement": settlement,
                    "feeds": feeds,
                }))?
            );
        }
    }

    Ok(())
}

fn run_upgrade_check(current: &str, latest: Option<&str>) -> Result<()> {
    let current = Version::parse(current).context("parse current version")?;

    let result = if let Some(latest_raw) = latest {
        let latest = Version::parse(latest_raw).context("parse latest version")?;
        if latest > current {
            serde_json::json!({
                "status": "update_available",
                "current": current.to_string(),
                "latest": latest.to_string(),
                "instructions": "Install the newer release package or run cargo install --force from the release source.",
            })
        } else {
            serde_json::json!({
                "status": "up_to_date",
                "current": current.to_string(),
                "latest": latest.to_string(),
            })
        }
    } else {
        serde_json::json!({
            "status": "unknown",
            "current": current.to_string(),
            "message": "No latest version source configured. Provide --latest <version>.",
        })
    };

    println!("{}", serde_json::to_string_pretty(&result)?);
    Ok(())
}

fn run_skill_test(data_dir: &Path, skill: &InstalledSkill, input: &Value) -> Result<()> {
    if !skill.enabled {
        bail!("skill {} is disabled", skill.id);
    }

    ensure_capabilities_allowed(
        data_dir,
        &format!("skill:{}@{}", skill.id, skill.version),
        skill.trust,
        &skill.required_caps,
        Some("skill.test"),
        Some(input),
    )?;

    let identity = Identity::load_or_create(data_dir.join("identity.json"))?;
    let event_log = EventLog::new(data_dir.join("events.jsonl"))?;
    let input_digest = digest_value(input)?;

    event_log.append_signed(
        "SKILL_CALL_REQUEST",
        serde_json::json!({
            "skill_id": skill.id,
            "version": skill.version,
            "required_caps": skill.required_caps,
            "input_digest": input_digest,
        }),
        &identity,
    )?;

    let mut runtime = SkillRuntime::new(CapabilityPolicy::default());
    register_skill_handler(&mut runtime, skill)?;

    let output = runtime.invoke(&skill.id, &skill.version, skill.trust, input.clone())?;

    event_log.append_signed(
        "SKILL_CALL_RESULT",
        serde_json::json!({
            "skill_id": skill.id,
            "version": skill.version,
            "ok": true,
            "input_digest": input_digest,
            "output_digest": digest_value(&output)?,
        }),
        &identity,
    )?;

    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "status": "ok",
            "skill_id": skill.id,
            "output": output,
        }))?
    );
    Ok(())
}

async fn run_mcp_test(
    data_dir: &Path,
    server: &KernelMcpServerConfig,
    tool: &str,
    input: Value,
) -> Result<()> {
    ensure_capabilities_allowed(
        data_dir,
        &format!("mcp:{}", server.name),
        TrustLevel::Verified,
        &[format!("mcp.call:{tool}")],
        Some("mcp.test"),
        Some(&input),
    )?;

    let identity = Identity::load_or_create(data_dir.join("identity.json"))?;
    let event_log = EventLog::new(data_dir.join("events.jsonl"))?;
    let input_digest = digest_value(&input)?;

    event_log.append_signed(
        "MCP_CALL_REQUEST",
        serde_json::json!({
            "server": server.name,
            "tool": tool,
            "input_digest": input_digest,
        }),
        &identity,
    )?;

    let output = call_tool(server, data_dir.join("mcp/usage.json"), tool, input).await?;

    event_log.append_signed(
        "MCP_CALL_RESULT",
        serde_json::json!({
            "server": server.name,
            "tool": tool,
            "ok": true,
            "output_digest": digest_value(&output)?,
        }),
        &identity,
    )?;

    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "status": "ok",
            "server": server.name,
            "tool": tool,
            "output": output,
        }))?
    );
    Ok(())
}

fn ensure_capabilities_allowed(
    data_dir: &Path,
    subject: &str,
    trust: TrustLevel,
    capabilities: &[String],
    reason: Option<&str>,
    payload: Option<&Value>,
) -> Result<()> {
    let mut engine = open_policy_engine(data_dir)?;
    for capability in capabilities {
        let decision = engine.evaluate(CapabilityRequest {
            request_id: String::new(),
            timestamp: 0,
            subject: subject.to_string(),
            trust,
            capability: capability.clone(),
            reason: reason.map(ToString::to_string),
            input_digest: payload.map(digest_value).transpose()?,
        })?;

        if decision.decision != DecisionKind::Allowed {
            bail!(
                "capability approval required for {capability}; request_id={}",
                decision.request_id.unwrap_or_else(|| "unknown".to_string())
            );
        }
    }
    Ok(())
}

fn open_policy_engine(data_dir: &Path) -> Result<PolicyEngine> {
    PolicyEngine::load_or_new(
        data_dir.join("policy/state.json"),
        "cli-session",
        CapabilityPolicy::default(),
    )
}

fn register_skill_handler(runtime: &mut SkillRuntime, skill: &InstalledSkill) -> Result<()> {
    let manifest = SkillManifest {
        name: skill.id.clone(),
        version: skill.version.clone(),
        required_capabilities: skill.required_caps.clone(),
    };

    if skill.entry == "builtin:echo" {
        runtime.register(manifest, EchoSkill);
        return Ok(());
    }

    if let Some(rel) = skill.entry.strip_prefix("process:") {
        if rel.trim().is_empty() {
            bail!("process skill entry must include a relative path");
        }

        let install_root = Path::new(&skill.install_path);
        let install_root_normalized = install_root
            .canonicalize()
            .with_context(|| format!("resolve skill install root {}", install_root.display()))?;
        let resolved = install_root
            .join(rel)
            .canonicalize()
            .with_context(|| format!("resolve process skill entry {rel}"))?;

        if !resolved.starts_with(&install_root_normalized) {
            bail!("process skill entry escapes install root");
        }
        if !resolved.is_file() {
            bail!("process skill entry is not a file");
        }

        runtime.register(manifest, ProcessSkill::new(resolved));
        return Ok(());
    }

    bail!(
        "unsupported skill entry {} (supported: builtin:echo, process:<relative>)",
        skill.entry
    )
}

fn digest_value(value: &Value) -> Result<String> {
    let bytes = serde_json::to_vec(value).context("serialize value for digest")?;
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    Ok(hex::encode(hasher.finalize()))
}

fn build_night_shift_value(data_dir: &Path, hours: i64) -> Result<Value> {
    let log = EventLog::new(data_dir.join("events.jsonl"))?;
    let now = chrono::Utc::now().timestamp();
    let report = generate_night_shift_report(&log.get_all()?, now - hours.max(1) * 3600, now);
    serde_json::to_value(report).context("serialize night shift report")
}

fn build_local_state_value(data_dir: &Path) -> Result<Value> {
    let log = EventLog::new(data_dir.join("events.jsonl"))?;
    let events = log.get_all()?;
    let pending = open_policy_engine(data_dir)?.list_pending().len();
    Ok(serde_json::json!({
        "events": events.len(),
        "pending_policy_requests": pending,
    }))
}

fn schema_file_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("schemas")
        .join(name)
}

fn validate_schema_file(schema_path: &Path, payload: &Value) -> Result<()> {
    let raw = fs::read_to_string(schema_path)
        .with_context(|| format!("read schema {}", schema_path.display()))?;
    let schema: Value = serde_json::from_str(&raw).context("parse schema")?;
    let validator = jsonschema::validator_for(&schema).context("compile schema")?;
    if let Err(error) = validator.validate(payload) {
        bail!("schema validation failed: {error}");
    }
    Ok(())
}

async fn resolve_skill_source(
    data_dir: &Path,
    source: &str,
) -> Result<(Option<tempfile::TempDir>, PathBuf, String)> {
    let resolved_source = if let Some(registry_id) = source.strip_prefix("registry:") {
        let catalog = read_skill_catalog(data_dir.join("skills/catalog.json"))?;
        catalog
            .get(registry_id)
            .cloned()
            .with_context(|| format!("registry skill id not found: {registry_id}"))?
    } else {
        source.to_string()
    };

    if resolved_source.starts_with("http://") || resolved_source.starts_with("https://") {
        let bytes = reqwest::Client::new()
            .get(&resolved_source)
            .send()
            .await
            .context("download skill package")?
            .bytes()
            .await
            .context("read downloaded skill package bytes")?;

        let tempdir = tempfile::tempdir().context("create temp directory for skill url")?;
        let archive_path = tempdir.path().join("skill.tar.gz");
        fs::write(&archive_path, &bytes).context("write downloaded skill archive")?;

        let unpack_dir = tempdir.path().join("unpack");
        fs::create_dir_all(&unpack_dir)?;
        let reader = fs::File::open(&archive_path).context("open downloaded skill archive")?;
        let decoder = flate2::read::GzDecoder::new(reader);
        let mut archive = tar::Archive::new(decoder);
        archive
            .unpack(&unpack_dir)
            .context("unpack skill archive")?;

        let package_root = detect_skill_root(&unpack_dir)?;
        return Ok((Some(tempdir), package_root, resolved_source));
    }
    Ok((None, PathBuf::from(&resolved_source), resolved_source))
}

fn read_skill_catalog(path: PathBuf) -> Result<BTreeMap<String, String>> {
    if !path.exists() {
        bail!(
            "skill catalog not found at {} (expected registry:id mapping)",
            path.display()
        );
    }
    let raw = fs::read_to_string(path).context("read skill catalog")?;
    serde_json::from_str(&raw).context("parse skill catalog")
}

fn detect_skill_root(unpack_dir: &Path) -> Result<PathBuf> {
    let entries: Vec<PathBuf> = fs::read_dir(unpack_dir)
        .context("read unpack directory")?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .collect();

    if entries.len() == 1 && entries[0].is_dir() {
        Ok(entries[0].clone())
    } else {
        Ok(unpack_dir.to_path_buf())
    }
}

fn run_night_shift(event_log: &PathBuf, hours: i64, out: Option<PathBuf>) -> Result<()> {
    let log = EventLog::new(event_log)?;
    let now = chrono::Utc::now().timestamp();
    let report = generate_night_shift_report(&log.get_all()?, now - hours * 3600, now);
    let output = serde_json::to_string_pretty(&report)?;
    if let Some(path) = out {
        fs::write(path, &output)?;
    }
    println!("{output}");
    Ok(())
}

async fn post_summary(
    identity_path: &PathBuf,
    events_path: &PathBuf,
    subnet: Option<String>,
    endpoint: &str,
    dry_run: bool,
) -> Result<()> {
    let identity = Identity::load(identity_path)?;
    let events = read_events(events_path)?;

    let ledger = latest_stats(&events).unwrap_or_default();
    let summary = build_signed_summary(&identity, subnet, &ledger, &events)?;

    if dry_run {
        let rendered = serde_json::to_string_pretty(&summary)?;
        println!("{rendered}");
        return Ok(());
    }

    let client = reqwest::Client::new();
    let res = client.post(endpoint).json(&summary).send().await?;
    let status = res.status();
    let body: Value = res.json().await.unwrap_or(Value::Null);
    let response = serde_json::to_string_pretty(
        &serde_json::json!({"status": status.as_u16(), "body": body}),
    )?;
    println!("{response}");
    Ok(())
}

fn read_events(path: &PathBuf) -> Result<Vec<EventRecord>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let raw = fs::read_to_string(path).context("read events jsonl")?;
    if raw.trim().is_empty() {
        return Ok(Vec::new());
    }
    raw.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).context("parse event"))
        .collect()
}

fn latest_stats(events: &[EventRecord]) -> Option<AgentStats> {
    // Use the latest settlement event as the local ledger snapshot.
    events.iter().rev().find_map(|event| {
        if event.event_type == "TASK_SETTLED" {
            serde_json::from_value(event.payload["new_stats"].clone()).ok()
        } else {
            None
        }
    })
}

fn read_config(data_dir: &Path) -> Result<LocalConfig> {
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

fn load_or_create_control_token(path: PathBuf) -> Result<String> {
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

fn read_control_token(path: PathBuf) -> Result<String> {
    let token = fs::read_to_string(path).context("read control token")?;
    let token = token.trim().to_string();
    if token.is_empty() {
        bail!("control token is empty");
    }
    Ok(token)
}

fn can_write_storage(data_dir: &Path) -> Result<()> {
    let path = data_dir.join(".doctor_write_test");
    fs::write(&path, "ok").context("write storage probe")?;
    fs::remove_file(path).context("remove storage probe")
}

async fn check_control_plane(endpoint: &str, token: &str) -> Result<()> {
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

async fn fetch_server_timestamp(endpoint: &str) -> Result<i64> {
    let response = reqwest::Client::new()
        .get(format!("{endpoint}/v1/health"))
        .timeout(Duration::from_secs(3))
        .send()
        .await
        .context("request control plane health")?;
    let body: Value = response.json().await.context("parse health response")?;
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

fn push_check(
    checks: &mut Vec<DoctorCheck>,
    name: &str,
    condition: bool,
    ok_detail: &str,
    fail_detail: &str,
) {
    checks.push(DoctorCheck {
        name: name.to_string(),
        status: if condition {
            "ok".to_string()
        } else {
            "fail".to_string()
        },
        detail: if condition {
            ok_detail.to_string()
        } else {
            fail_detail.to_string()
        },
    });
}

impl TrustArg {
    fn as_str(self) -> &'static str {
        match self {
            Self::Trusted => "trusted",
            Self::Verified => "verified",
            Self::Untrusted => "untrusted",
        }
    }
}

impl ScopeArg {
    fn as_str(self) -> &'static str {
        match self {
            Self::Once => "once",
            Self::Session => "session",
            Self::Permanent => "permanent",
        }
    }
}
