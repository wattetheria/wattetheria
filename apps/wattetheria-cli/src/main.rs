//! CLI tools for bootstrap, diagnostics, policy approvals, and reporting.

use anyhow::{Context, Result, anyhow, bail};
use clap::Parser;
mod cli_args;
mod config;
mod doctor;
mod publish;

use crate::cli_args::{
    BrainCommand, Cli, Commands, DataCommand, GovernanceCommand, IdentityCommand, McpCommand,
    OracleCommand, PolicyCommand, ServicenetAgentCardCommand, ServicenetCommand,
};
use crate::config::{
    LocalConfig, ServicenetRegistrationConfig, read_config, read_control_token, run_init, run_up,
    write_config,
};
use crate::doctor::run_doctor;
use semver::Version;
use serde_json::{Value, json};
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
use wattetheria_kernel::local_db::{self, LocalDb};
use wattetheria_kernel::mcp::{
    McpRegistry, McpServerConfig as KernelMcpServerConfig, call_tool, list_tools,
};
use wattetheria_kernel::night_shift::generate_night_shift_report;
use wattetheria_kernel::oracle::OracleRegistry;
use wattetheria_kernel::policy_engine::{
    CapabilityRequest, DecisionKind, PolicyEngine, PolicyState,
};
use wattetheria_kernel::servicenet::{
    attach_servicenet_agent_did_document, normalize_service_address,
};
use wattetheria_kernel::summary::build_signed_summary_for_public_identity;
use wattetheria_kernel::types::AgentStats;
use wattetheria_kernel::wallet_identity::{
    WalletSigner, active_payment_account_binding_proof, load_or_create_wallet_backed_identity,
    load_wallet_backed_identity, open_local_wallet,
};

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
            connect,
        } => run_doctor(&data_dir, control_plane.as_deref(), brain, connect).await?,
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
        Commands::Identity { data_dir, command } => run_identity(&data_dir, &command)?,
        Commands::Servicenet { data_dir, command } => run_servicenet(&data_dir, command).await?,
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
    signer: WalletSigner,
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
            signer: WalletSigner::from_data_dir(data_dir)?,
            provider,
        })
    }

    fn request(&self, operation: &str, input: &Value) -> Result<BrainInvocationAudit> {
        let input_digest = digest_value(input)?;
        self.event_log.append_signed_with_signer(
            "BRAIN_INVOKE_REQUEST",
            serde_json::json!({
                "operation": operation,
                "provider": self.provider,
                "input_digest": input_digest,
            }),
            &self.signer,
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
        self.event_log.append_signed_with_signer(
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
            &self.signer,
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
            runtime_adapter,
        } => serde_json::json!({
            "kind": "agent-runtime",
            "adapter": wattetheria_kernel::brain::AgentRuntimeAdapter::infer(
                base_url,
                model,
                runtime_adapter.as_ref()
            ).key(),
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

fn run_identity(data_dir: &Path, command: &IdentityCommand) -> Result<()> {
    run_init(data_dir)?;
    let identity = load_or_create_wallet_backed_identity(data_dir)
        .context("load or create wallet-backed identity")?;
    match command {
        IdentityCommand::Init => {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "agent_did": identity.agent_did,
                    "public_key": identity.public_key,
                    "data_dir": data_dir.display().to_string(),
                }))?
            );
        }
        IdentityCommand::Show => {
            let identity =
                load_wallet_backed_identity(data_dir).context("load wallet-backed identity")?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "agent_did": identity.agent_did,
                    "public_key": identity.public_key,
                }))?
            );
        }
        IdentityCommand::ExportSeed => {
            let wallet_state = open_local_wallet(data_dir)?;
            let seed = wallet_state
                .wallet
                .export_active_identity_ed25519_seed(&wallet_state.profile)
                .context("export active identity seed")?;
            println!("{}", hex::encode(seed));
        }
    }
    Ok(())
}

async fn run_servicenet(data_dir: &Path, command: ServicenetCommand) -> Result<()> {
    match command {
        ServicenetCommand::Register { card } => {
            run_servicenet_provider_register(data_dir, &card).await
        }
        ServicenetCommand::AgentCard { command } => match command {
            ServicenetAgentCardCommand::Init { out } => run_servicenet_agent_card_init(out),
        },
        ServicenetCommand::Publish {
            agent_id,
            version,
            risk_level,
            ttl_minutes,
            dry_run,
        } => {
            run_servicenet_publish(
                data_dir,
                &agent_id,
                &version,
                &risk_level,
                ttl_minutes,
                dry_run,
            )
            .await
        }
    }
}

fn run_servicenet_agent_card_init(out: Option<PathBuf>) -> Result<()> {
    let output_dir = match out {
        Some(path) => path,
        None => std::env::current_dir().context("resolve current directory")?,
    };
    if output_dir.exists() && !output_dir.is_dir() {
        bail!(
            "agent card output path is not a directory: {}",
            output_dir.display()
        );
    }
    fs::create_dir_all(&output_dir).with_context(|| {
        format!(
            "create agent card output directory `{}`",
            output_dir.display()
        )
    })?;
    let card_path = output_dir.join("agent-card.jsonc");
    if card_path.exists() {
        bail!("agent card already exists: {}", card_path.display());
    }
    fs::write(&card_path, agent_card_template_jsonc())
        .with_context(|| format!("write agent card template `{}`", card_path.display()))?;
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "status": "ok",
            "card": card_path,
            "next": [
                "Edit agent-card.jsonc with your agent details.",
                "Run `wattetheria servicenet register` from the directory that contains the file.",
                "Run `wattetheria servicenet publish <agent-id>` with the returned agent_id."
            ],
        }))?
    );
    Ok(())
}

async fn run_servicenet_provider_register(data_dir: &Path, card_path: &Path) -> Result<()> {
    ensure_servicenet_data_dir(data_dir)?;
    let _ = load_or_create_wallet_backed_identity(data_dir)?;
    let config = read_config(data_dir)?;
    let servicenet = resolve_servicenet_base_url();
    let (agent_card, card_raw, card_path) = load_agent_card(card_path)?;
    publish::validate_agent_card(&agent_card)?;
    let endpoint = agent_card_url(&agent_card)?.to_owned();
    publish::validate_endpoint(&endpoint)?;

    let wallet = publish::open_wallet_or_explain(data_dir)?;
    let identity = wallet
        .wallet
        .active_identity(&wallet.profile)
        .context("resolve active identity")?;
    let identity_did = identity.did.to_string();

    let client = publish::ServicenetClient::new(&servicenet);
    let challenge = client
        .create_ownership_challenge(&identity_did, "register")
        .await?;

    let signature_b64 = publish::sign_with_identity_b64(&wallet, challenge.challenge.as_bytes())?;
    let record = client
        .register_provider(
            &challenge.provider_id,
            &identity_did,
            agent_card.get("name").and_then(Value::as_str),
            challenge.challenge_id,
            &signature_b64,
        )
        .await?;
    let servicenet_namespace = record["provider_id"]
        .as_str()
        .unwrap_or(&challenge.provider_id)
        .to_owned();
    let attester_identity = record["provider_did"]
        .as_str()
        .unwrap_or(&identity_did)
        .to_owned();
    let agent_id = derive_agent_id(&agent_card, &servicenet_namespace)?;
    let service_address = agent_card_service_address(&agent_card)?;
    let card_hash = hash_agent_card(&card_raw);

    let mut config = config;
    let registration = ServicenetRegistrationConfig {
        provider_id: servicenet_namespace.clone(),
        provider_did: attester_identity.clone(),
        agent_id: agent_id.clone(),
        service_address: service_address.clone(),
        card_path: card_path.display().to_string(),
        card_hash: card_hash.clone(),
    };
    config
        .servicenet_registrations
        .retain(|existing| existing.agent_id != agent_id);
    config.servicenet_registrations.push(registration);
    write_config(data_dir, &config)?;

    println!(
        "{}",
        serde_json::to_string_pretty(&serde_json::json!({
            "status": "ok",
            "provider_id": &servicenet_namespace,
            "provider_did": &attester_identity,
            "agent_id": &agent_id,
            "service_address": service_address,
            "card": card_path,
            "card_hash": &card_hash,
        }))?
    );
    Ok(())
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
async fn run_servicenet_publish(
    data_dir: &Path,
    agent_id: &str,
    version: &str,
    risk_level: &str,
    ttl_minutes: u64,
    dry_run: bool,
) -> Result<()> {
    ensure_servicenet_data_dir(data_dir)?;
    let _ = load_or_create_wallet_backed_identity(data_dir)?;
    let config = read_config(data_dir)?;
    let registration = config
        .servicenet_registrations
        .iter()
        .find(|registration| registration.agent_id == agent_id)
        .ok_or_else(|| {
        anyhow!(
            "no ServiceNet registration found for agent `{agent_id}`; run `wattetheria servicenet register` first"
        )
    })?;
    let servicenet = resolve_servicenet_base_url();

    let card_path = PathBuf::from(&registration.card_path);
    let (mut agent_card, card_raw, _) = load_agent_card(&card_path)?;
    publish::validate_agent_card(&agent_card)?;
    let endpoint = agent_card_url(&agent_card)?.to_owned();
    publish::validate_endpoint(&endpoint)?;
    let card_hash = hash_agent_card(&card_raw);
    if card_hash != registration.card_hash {
        bail!(
            "agent card changed since ServiceNet registration; run `wattetheria servicenet register --card {}` again",
            card_path.display()
        );
    }
    let service_address = registration_service_address(registration.service_address.as_deref())?;
    strip_agent_card_submission_metadata(&mut agent_card);

    let wallet = publish::open_wallet_or_explain(data_dir)?;
    let identity = wallet
        .wallet
        .active_identity(&wallet.profile)
        .context("resolve active identity")?;
    let identity_did = identity.did.to_string();
    let payment_account_binding = active_payment_account_binding_proof(data_dir)?
        .map(serde_json::to_value)
        .transpose()
        .context("serialize active payment account binding proof")?
        .unwrap_or(Value::Null);
    attach_servicenet_agent_did_document(
        &mut agent_card,
        &identity_did,
        &registration.agent_id,
        service_address.as_deref(),
        Some(&payment_account_binding),
    );

    let deployment = serde_json::json!({
        "runtime": "remote_http",
        "endpoint": {
            "url": endpoint,
            "protocol_binding": "JSONRPC",
            "protocol_version": "1.0",
            "interaction_protocol": "google_a2a",
        },
    });
    let review = serde_json::json!({
        "risk_level": risk_level,
        "human_approval_required": false,
    });
    let artifacts = serde_json::json!({});

    let issued_at_ms = now_ms();
    let expires_at_ms = issued_at_ms.saturating_add(ttl_minutes.saturating_mul(60_000));
    let nonce = uuid::Uuid::new_v4().to_string();

    // Mirror server-side `build_agent_attestation_payload`: signature is NOT
    // part of the signed bytes; the CLI signs the submission semantics and
    // freshness window.
    let attestation_payload_value = serde_json::json!({
        "provider_id": &registration.provider_id,
        "agent_id": &registration.agent_id,
        "service_address": &service_address,
        "version": version,
        "agent_card": agent_card,
        "deployment": deployment,
        "review": review,
        "artifacts": artifacts,
        "provider_attester_did": &identity_did,
        "delegation_token": Value::Null,
        "source_commit": Value::Null,
        "build_digest": Value::Null,
        "payment_account_binding": payment_account_binding.clone(),
        "nonce": nonce,
        "issued_at_ms": issued_at_ms,
        "expires_at_ms": expires_at_ms,
    });
    let attestation_bytes =
        serde_jcs::to_vec(&attestation_payload_value).context("canonicalize attestation")?;
    let signature_b64 = publish::sign_with_identity_b64(&wallet, &attestation_bytes)?;

    let attestations = serde_json::json!({
        "attestation_signature": signature_b64,
        "provider_attester_did": &identity_did,
        "nonce": nonce,
        "issued_at_ms": issued_at_ms,
        "expires_at_ms": expires_at_ms,
    });
    let request = serde_json::json!({
        "provider_id": &registration.provider_id,
        "agent_id": &registration.agent_id,
        "service_address": &service_address,
        "version": version,
        "agent_card": agent_card,
        "deployment": deployment,
        "review": review,
        "artifacts": artifacts,
        "payment_account_binding": payment_account_binding,
        "attestations": attestations,
    });

    if dry_run {
        println!("{}", serde_json::to_string_pretty(&request)?);
        return Ok(());
    }

    let client = publish::ServicenetClient::new(&servicenet);
    let record = client.submit_agent(request).await?;
    println!("{}", serde_json::to_string_pretty(&record)?);
    Ok(())
}

const DEFAULT_SERVICENET_BASE_URL: &str = "https://servicenet.wattetheria.com";

fn ensure_servicenet_data_dir(data_dir: &Path) -> Result<()> {
    fs::create_dir_all(data_dir).context("create data directory")?;
    if !data_dir.join("config.json").exists() {
        write_config(data_dir, &LocalConfig::default())?;
    }
    Ok(())
}

fn resolve_servicenet_base_url() -> String {
    DEFAULT_SERVICENET_BASE_URL.to_owned()
}

fn load_agent_card(card_path: &Path) -> Result<(Value, String, PathBuf)> {
    let path = fs::canonicalize(card_path)
        .with_context(|| format!("resolve agent card `{}`", card_path.display()))?;
    let raw = fs::read_to_string(&path)
        .with_context(|| format!("read agent card `{}`", path.display()))?;
    let mut json_source = raw.clone();
    json_strip_comments::strip(&mut json_source).context("strip agent card JSONC comments")?;
    let card: Value = serde_json::from_str(&json_source).context("parse agent card JSON/JSONC")?;
    Ok((card, raw, path))
}

fn agent_card_service_address(agent_card: &Value) -> Result<Option<String>> {
    for field in ["serviceAddress", "service_address"] {
        if let Some(value) = agent_card.get(field).and_then(Value::as_str) {
            return normalize_service_address(value);
        }
    }
    Ok(None)
}

fn registration_service_address(value: Option<&str>) -> Result<Option<String>> {
    match value {
        Some(value) => normalize_service_address(value),
        None => Ok(None),
    }
}

fn strip_agent_card_submission_metadata(agent_card: &mut Value) {
    if let Some(object) = agent_card.as_object_mut() {
        object.remove("serviceAddress");
        object.remove("service_address");
    }
}

fn agent_card_template_jsonc() -> &'static str {
    r#"{
  "name": "",
  // Optional unique ServiceNet alias. Use <name>@wattetheria.
  // This is stored outside agent_card during publish.
  "serviceAddress": "",
  "description": "",
  "url": "",
  "preferredTransport": "JSONRPC",
  "protocolVersion": "1.0",

  // UI Scope:
  // "real_world" = real-world ServiceNet agent.
  // "wattetheria_native" = Wattetheria-native published agent.
  "scope": "real_world",

  // UI Origin:
  // real_world: "established_service" or "custom_built".
  // wattetheria_native: "native_published".
  "origin": "custom_built",

  // UI Domain:
  // real_world: GENERAL, TRANSPORTATION, FOOD, CLOTHING, HOUSING, PAYMENTS,
  // COMMERCE, MEDIA, HEALTH, EDUCATION, TRAVEL.
  // wattetheria_native: GENERAL, GOVERNANCE, PRODUCTION, TRADING, AUTOMATION,
  // SECURITY, EXPLORATION, DISCOVERY, SERVICENET.
  "domain": "GENERAL",

  // UI Cost. User-set amount charged for invoking this agent.
  "cost": 18,
  "currency": "USDC",

  // Optional A2A x402 payment discovery. This is static/default settlement info.
  // The callee agent can still request per-invocation payment through the A2A task flow.
  // payTo is the callee/merchant settlement receiving address, not the caller wallet.
  // asset is optional; when omitted, compatible clients may resolve it from network + currency.
  // resource names the paid ServiceNet resource; use this card's agent name, not a registry agent_id.
  // payment_account_bindings and didDocument are filled from the local wallet during publish.
  "payment_account_bindings": [],
  "didDocument": {
    "payment_account_bindings": []
  },
  // "capabilities": {
  //   "extensions": [
  //     {
  //       "uri": "https://github.com/google-a2a/a2a-x402/v0.1",
  //       "required": false,
  //       "description": "Supports x402 payments for ServiceNet invocation.",
  //       "params": {
  //         "accepts": [
  //           {
  //             "scheme": "exact",
  //             "network": "base",
  //             "payTo": "0x0000000000000000000000000000000000000000",
  //             "maxAmountRequired": "0",
  //             "resource": "servicenet:agent:<agent_name>",
  //             "description": "ServiceNet agent invocation",
  //             "maxTimeoutSeconds": 600
  //           }
  //         ]
  //       }
  //     }
  //   ]
  // },

  // A2A task support:
  // true = SendMessage may return a Task, caller can poll with GetTask.
  // false = SendMessage normally returns a Message.
  "supportsTask": false,

  "skills": [
    {
      "name": "",
      "description": ""
    }
  ],
  "securitySchemes": {
    "none": {
      "type": "none"
    }
  },
  "security": [
    {
      "none": []
    }
  ]
}
"#
}

fn agent_card_url(agent_card: &Value) -> Result<&str> {
    agent_card
        .get("url")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("agent card `url` missing"))
}

fn hash_agent_card(card_raw: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(card_raw.as_bytes()))
}

fn derive_agent_id(agent_card: &Value, provider_id: &str) -> Result<String> {
    let name = agent_card
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("agent card `name` missing"))?;
    let url = agent_card_url(agent_card)?;
    let slug = slugify_agent_name(name);
    let digest = Sha256::digest(format!("{provider_id}:{url}").as_bytes());
    let suffix = format!("{digest:x}");
    Ok(format!("{slug}-{}", &suffix[..8]))
}

fn slugify_agent_name(name: &str) -> String {
    let mut slug = String::new();
    let mut last_was_dash = false;
    for ch in name.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            last_was_dash = false;
        } else if !last_was_dash && !slug.is_empty() {
            slug.push('-');
            last_was_dash = true;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    if slug.is_empty() {
        "agent".to_owned()
    } else {
        slug
    }
}

#[allow(clippy::too_many_lines)]
fn run_oracle(data_dir: &Path, command: OracleCommand) -> Result<()> {
    run_init(data_dir)?;
    let db = LocalDb::open(local_db::prepare_primary_db(data_dir)?)?;
    let mut oracle: OracleRegistry = db.load_or_migrate(
        wattetheria_kernel::local_db::domain::ORACLE_REGISTRY,
        &data_dir.join("oracle/state.json"),
    )?;
    let identity = load_or_create_wallet_backed_identity(data_dir)?;
    let signer = WalletSigner::from_data_dir(data_dir)?;
    let identity_view = identity.compat_view();
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
                &format!("oracle:publisher:{}", identity.agent_did),
                TrustLevel::Trusted,
                &[String::from("oracle.publish")],
                Some("oracle.publish"),
                Some(&payload),
            )?;
            let feed = oracle.publish_with_signer(
                &feed_id,
                payload,
                price_watt,
                &identity_view,
                &signer,
                Some(&event_log),
            )?;
            db.save_domain(
                wattetheria_kernel::local_db::domain::ORACLE_REGISTRY,
                &oracle,
            )?;
            println!("{}", serde_json::to_string_pretty(&feed)?);
        }
        OracleCommand::Subscribe {
            feed_id,
            max_price_watt,
        } => {
            ensure_capabilities_allowed(
                data_dir,
                &format!("oracle:subscriber:{}", identity.agent_did),
                TrustLevel::Verified,
                &[String::from("oracle.subscribe")],
                Some("oracle.subscribe"),
                None,
            )?;
            let subscription = oracle.subscribe_with_signer(
                &identity.agent_did,
                &feed_id,
                max_price_watt,
                &identity_view,
                &signer,
                Some(&event_log),
            )?;
            db.save_domain(
                wattetheria_kernel::local_db::domain::ORACLE_REGISTRY,
                &oracle,
            )?;
            println!("{}", serde_json::to_string_pretty(&subscription)?);
        }
        OracleCommand::Credit { agent, watt } => {
            let target = agent.unwrap_or_else(|| identity.agent_did.clone());
            let balance = oracle.credit(&target, watt)?;
            db.save_domain(
                wattetheria_kernel::local_db::domain::ORACLE_REGISTRY,
                &oracle,
            )?;
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "agent_did": target,
                    "balance": balance,
                }))?
            );
        }
        OracleCommand::Balance { agent } => {
            let target = agent.unwrap_or_else(|| identity.agent_did.clone());
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "agent_did": target,
                    "balance": oracle.balance_of(&target),
                }))?
            );
        }
        OracleCommand::Pull { feed_id } => {
            let (feeds, settlement) = oracle.pull_for_subscriber_settled_with_signer(
                &identity.agent_did,
                &feed_id,
                &identity_view,
                &signer,
                Some(&event_log),
            )?;
            db.save_domain(
                wattetheria_kernel::local_db::domain::ORACLE_REGISTRY,
                &oracle,
            )?;
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

    let signer = load_wallet_signer(data_dir)?;
    let event_log = EventLog::new(data_dir.join("events.jsonl"))?;
    let input_digest = digest_value(&input)?;

    event_log.append_signed_with_signer(
        "MCP_CALL_REQUEST",
        serde_json::json!({
            "server": server.name,
            "tool": tool,
            "input_digest": input_digest,
        }),
        &signer,
    )?;

    let output = call_tool(server, data_dir.join("mcp/usage.json"), tool, input).await?;

    event_log.append_signed_with_signer(
        "MCP_CALL_RESULT",
        serde_json::json!({
            "server": server.name,
            "tool": tool,
            "ok": true,
            "output_digest": digest_value(&output)?,
        }),
        &signer,
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

fn load_wallet_signer(data_dir: &Path) -> Result<WalletSigner> {
    WalletSigner::from_data_dir(data_dir)
}

fn ensure_capabilities_allowed(
    data_dir: &Path,
    subject: &str,
    trust: TrustLevel,
    capabilities: &[String],
    reason: Option<&str>,
    payload: Option<&Value>,
) -> Result<()> {
    let (mut engine, db) = open_policy_engine(data_dir)?;
    for capability in capabilities {
        let decision = engine.evaluate(CapabilityRequest {
            request_id: String::new(),
            timestamp: 0,
            subject: subject.to_string(),
            trust,
            capability: capability.clone(),
            reason: reason.map(ToString::to_string),
            input_digest: payload.map(digest_value).transpose()?,
        });

        if decision.decision != DecisionKind::Allowed {
            db.save_domain(wattetheria_kernel::local_db::domain::POLICY, engine.state())?;
            bail!(
                "capability approval required for {capability}; request_id={}",
                decision.request_id.unwrap_or_else(|| "unknown".to_string())
            );
        }
    }
    db.save_domain(wattetheria_kernel::local_db::domain::POLICY, engine.state())?;
    Ok(())
}

fn open_policy_engine(data_dir: &Path) -> Result<(PolicyEngine, LocalDb)> {
    let db = LocalDb::open(local_db::prepare_primary_db(data_dir)?)?;
    let state: PolicyState = db.load_or_migrate(
        wattetheria_kernel::local_db::domain::POLICY,
        &data_dir.join("policy/state.json"),
    )?;
    let engine = PolicyEngine::new("cli-session", CapabilityPolicy::default(), state);
    Ok((engine, db))
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
    let pending = open_policy_engine(data_dir)?.0.list_pending().len();
    Ok(serde_json::json!({
        "events": events.len(),
        "pending_policy_requests": pending,
    }))
}

fn now_ms() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
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
    let identity = if let Some(data_dir) = identity_path
        .parent()
        .filter(|dir| dir.join(".watt-wallet").exists())
    {
        load_wallet_backed_identity(data_dir)?
    } else {
        Identity::load(identity_path)?
    };
    let events = read_events(events_path)?;

    let ledger = latest_stats(&events).unwrap_or_default();
    let data_dir = identity_path.parent().unwrap_or(identity_path.as_path());
    let public_id = resolve_public_identity_id(data_dir, &identity);
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

fn resolve_public_identity_id(data_dir: &Path, identity: &Identity) -> Option<String> {
    let db = match local_db::prepare_primary_db(data_dir).and_then(LocalDb::open) {
        Ok(db) => db,
        Err(error) => {
            eprintln!("open local db for public identity: {error:#}");
            return Some(identity.agent_did.clone());
        }
    };
    let public_registry: PublicIdentityRegistry = match db.load_or_migrate(
        wattetheria_kernel::local_db::domain::PUBLIC_IDENTITY_REGISTRY,
        &data_dir.join("civilization/public_identities.json"),
    ) {
        Ok(registry) => registry,
        Err(error) => {
            eprintln!("load public identity registry: {error:#}");
            return Some(identity.agent_did.clone());
        }
    };
    let binding_registry: ControllerBindingRegistry = match db.load_or_migrate(
        wattetheria_kernel::local_db::domain::CONTROLLER_BINDING_REGISTRY,
        &data_dir.join("civilization/controller_bindings.json"),
    ) {
        Ok(registry) => registry,
        Err(error) => {
            eprintln!("load controller binding registry: {error:#}");
            return Some(identity.agent_did.clone());
        }
    };

    binding_registry
        .active_for_controller(&identity.agent_did)
        .and_then(|binding| public_registry.get(&binding.public_id))
        .filter(|public_identity| public_identity.active)
        .or_else(|| public_registry.active_for_agent_did(&identity.agent_did))
        .map(|public_identity| public_identity.public_id)
        .or_else(|| Some(identity.agent_did.clone()))
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

#[cfg(test)]
mod tests {
    use super::{agent_card_template_jsonc, load_agent_card};
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn load_agent_card_accepts_jsonc_comments() {
        let dir = tempdir().expect("create temp dir");
        let card_path = dir.path().join("agent-card.jsonc");
        fs::write(
            &card_path,
            r#"{
  // UI metadata must survive JSONC parsing.
  "name": "Alice",
  "scope": "real_world",
  "origin": "custom_built",
  "domain": "GENERAL"
}"#,
        )
        .expect("write card");

        let (card, raw, path) = load_agent_card(&card_path).expect("load JSONC card");

        assert_eq!(path, card_path.canonicalize().expect("canonicalize card"));
        assert!(raw.contains("// UI metadata"));
        assert_eq!(card["name"], "Alice");
        assert_eq!(card["scope"], "real_world");
        assert_eq!(card["origin"], "custom_built");
        assert_eq!(card["domain"], "GENERAL");
    }

    #[test]
    fn agent_card_template_is_valid_jsonc() {
        let dir = tempdir().expect("create temp dir");
        let card_path = dir.path().join("agent-card.jsonc");
        fs::write(&card_path, agent_card_template_jsonc()).expect("write template");

        let (card, _, _) = load_agent_card(&card_path).expect("load template");

        assert_eq!(card["scope"], "real_world");
        assert_eq!(card["origin"], "custom_built");
        assert_eq!(card["domain"], "GENERAL");
        assert_eq!(card["cost"], 18);
        assert_eq!(card["currency"], "USDC");
        assert_eq!(card["supportsTask"], false);
        assert_eq!(card["serviceAddress"].as_str(), Some(""));
        assert!(
            card["payment_account_bindings"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert!(
            card["didDocument"]["payment_account_bindings"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        let skill = card["skills"][0]
            .as_object()
            .expect("template skill object");
        let mut skill_keys = skill.keys().map(String::as_str).collect::<Vec<_>>();
        skill_keys.sort_unstable();
        assert_eq!(skill_keys, vec!["description", "name"]);
        assert!(agent_card_template_jsonc().contains("servicenet:agent:<agent_name>"));
        assert!(!agent_card_template_jsonc().contains("servicenet:agent:<agent_id>"));
    }
}
