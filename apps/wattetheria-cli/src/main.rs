//! CLI tools for bootstrap, diagnostics, policy approvals, and reporting.

use anyhow::{Context, Result, bail};
use clap::Parser;
mod cli_args;
mod config;
mod doctor;

use crate::cli_args::{
    BrainCommand, Cli, Commands, DataCommand, GovernanceCommand, McpCommand, OracleCommand,
    PolicyCommand,
};
use crate::config::{read_config, read_control_token, run_init, run_up};
use crate::doctor::run_doctor;
use semver::Version;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;
use wattetheria_kernel::brain::{BrainEngine, BrainProviderConfig};
use wattetheria_kernel::capabilities::{CapabilityPolicy, TrustLevel};
use wattetheria_kernel::civilization::identities::{
    ControllerBindingRegistry, PublicIdentityRegistry,
};
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
use wattetheria_kernel::summary::build_signed_summary_for_public_identity;
use wattetheria_kernel::types::AgentStats;

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
    let public_id = resolve_public_identity_id(identity_path, &identity);
    let summary =
        build_signed_summary_for_public_identity(&identity, public_id, subnet, &ledger, &events)?;

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

fn resolve_public_identity_id(identity_path: &Path, identity: &Identity) -> Option<String> {
    let Some(data_dir) = identity_path.parent() else {
        return Some(identity.agent_id.clone());
    };
    let public_registry_path = data_dir.join("civilization/public_identities.json");
    let binding_registry_path = data_dir.join("civilization/controller_bindings.json");

    let public_registry = PublicIdentityRegistry::load_or_new(&public_registry_path).ok()?;
    let binding_registry = ControllerBindingRegistry::load_or_new(&binding_registry_path).ok()?;

    binding_registry
        .active_for_controller(&identity.agent_id)
        .and_then(|binding| public_registry.get(&binding.public_id))
        .filter(|public_identity| public_identity.active)
        .or_else(|| public_registry.active_for_legacy_agent(&identity.agent_id))
        .map(|public_identity| public_identity.public_id)
        .or_else(|| Some(identity.agent_id.clone()))
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
