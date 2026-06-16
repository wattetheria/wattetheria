mod agent_participation;
mod bootstrap;
pub mod cli;
mod recovery;
mod runtime_loop;

use agent_participation::AgentParticipationSurface;
use anyhow::{Context, Result};
use serde::Deserialize;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::{Mutex, broadcast};
use tokio::time::{Duration, interval};
use tracing::{error, info, warn};

pub use bootstrap::init_tracing;
use bootstrap::{load_or_create_control_token, parse_control_bind, resolve_brain_config};
pub use cli::Cli;
use recovery::startup_recover_events;
use runtime_loop::{LoopContext, run_loop};
use wattetheria_control_plane::{
    ClientExportQuery, ControlPlaneState, DEFAULT_WATTSWARM_SYNC_GRPC_PORT, GatewayEventSequence,
    NodeGeoLocation, RateLimiter, StreamEvent, build_signed_node_event, push_signed_node_event,
    push_signed_snapshot, run_autonomy_tick_once, serve_control_plane,
    spawn_reliability_maintenance_task, spawn_wattswarm_sync_bridge,
};
use wattetheria_kernel::audit::AuditLog;
use wattetheria_kernel::brain::{BrainEngine, BrainProviderConfig};
use wattetheria_kernel::capabilities::CapabilityPolicy;
use wattetheria_kernel::civilization::galaxy::GalaxyState;
use wattetheria_kernel::civilization::identities::{
    ControllerBindingRegistry, PublicIdentityRegistry,
};
use wattetheria_kernel::civilization::missions::MissionBoard;
use wattetheria_kernel::civilization::organizations::OrganizationRegistry;
use wattetheria_kernel::civilization::profiles::CitizenRegistry;
use wattetheria_kernel::civilization::relationships::RelationshipRegistry;
use wattetheria_kernel::civilization::topics::HiveRegistry;
use wattetheria_kernel::economy::{EconomicPolicy, WalletBalanceState};
use wattetheria_kernel::event_log::EventLog;
use wattetheria_kernel::governance::GovernanceEngine;
use wattetheria_kernel::identity::IdentityCompatView;
use wattetheria_kernel::local_db::{self, LocalDb};
use wattetheria_kernel::mailbox::CrossSubnetMailbox;
use wattetheria_kernel::map::registry::GalaxyMapRegistry;
use wattetheria_kernel::map::state::{TravelStateRegistry, resolve_anchor_position};
use wattetheria_kernel::online_proof::OnlineProofManager;
use wattetheria_kernel::payments::PaymentLedger;
use wattetheria_kernel::policy_engine::{PolicyEngine, PolicyState};
use wattetheria_kernel::servicenet::ServiceNetClient;
use wattetheria_kernel::signing::PayloadSigner;
use wattetheria_kernel::swarm_bridge::{HybridSwarmBridge, SwarmBridge};
use wattetheria_kernel::wallet_identity::WalletSigner;
use wattetheria_social::SocialStore;

struct RuntimeState {
    control_bind: SocketAddr,
    identity: IdentityCompatView,
    online_proof: OnlineProofManager,
    control_state: ControlPlaneState,
    brain_config: BrainProviderConfig,
}

struct CivilizationRuntimeState {
    mission_board: MissionBoard,
    public_identity_registry: PublicIdentityRegistry,
    controller_binding_registry: ControllerBindingRegistry,
    citizen_registry: CitizenRegistry,
    relationship_registry: RelationshipRegistry,
    organization_registry: OrganizationRegistry,
    hive_registry: HiveRegistry,
    payment_ledger: PaymentLedger,
    galaxy_state: GalaxyState,
    galaxy_map_registry: GalaxyMapRegistry,
    travel_state_registry: TravelStateRegistry,
}

pub async fn run(cli: Cli) -> Result<()> {
    let mut runtime = setup_runtime(&cli).await?;

    let control_task = spawn_control_plane(runtime.control_state.clone(), runtime.control_bind);
    let autonomy_task = spawn_autonomy_task(&cli, runtime.control_state.clone());
    let wattswarm_sync_task = spawn_wattswarm_sync_bridge(
        runtime.control_state.clone(),
        resolve_wattswarm_sync_grpc_endpoint(&cli),
    );
    let executor_registration_task =
        spawn_wattswarm_executor_registration_task(&cli, &runtime.brain_config);
    let gateway_dispatch_task = spawn_gateway_dispatch_tasks(&cli, &runtime.control_state);
    let reliability_maintenance_task =
        spawn_reliability_maintenance_task(runtime.control_state.clone());

    let run_result = run_loop(LoopContext {
        online_proof: &mut runtime.online_proof,
        identity: &runtime.identity,
        control_state: &runtime.control_state,
    })
    .await;
    control_task.abort();
    if let Some(task) = wattswarm_sync_task {
        task.abort();
    }
    if let Some(task) = executor_registration_task {
        task.abort();
    }
    for task in gateway_dispatch_task {
        task.abort();
    }
    reliability_maintenance_task.abort();
    if let Some(task) = autonomy_task {
        task.abort();
    }
    run_result
}

#[allow(clippy::too_many_lines)]
async fn setup_runtime(cli: &Cli) -> Result<RuntimeState> {
    std::fs::create_dir_all(&cli.data_dir).context("create data dir")?;
    let events_path = cli.data_dir.join("events.jsonl");
    let snapshots_path = cli.data_dir.join("snapshots");
    std::fs::create_dir_all(&snapshots_path).context("create snapshots dir")?;

    startup_recover_events(&events_path, &snapshots_path, &cli.recovery_sources).await?;

    let local_db_path = local_db::prepare_primary_db(&cli.data_dir)?;
    let local_db = Arc::new(LocalDb::open(&local_db_path)?);
    let social_store = Arc::new(SocialStore::open(&local_db_path)?);
    social_store.import_legacy_db(local_db::legacy_social_db_path(&cli.data_dir))?;
    let runtime_identity =
        wattetheria_kernel::wallet_identity::load_or_create_wallet_backed_identity(&cli.data_dir)?;
    let signer: Arc<dyn PayloadSigner> = Arc::new(WalletSigner::new(
        &cli.data_dir,
        runtime_identity.compat_view(),
    ));
    let identity = runtime_identity.compat_view();
    let event_log = EventLog::new(events_path)?;
    let audit_log = AuditLog::new(cli.data_dir.join("audit/control_plane.jsonl"))?;
    let control_token = load_or_create_control_token(cli.data_dir.join("control.token"))?;
    let control_bind = parse_control_bind(&cli.control_plane_bind)?;
    let policy_state: PolicyState = local_db.load_or_migrate(
        local_db::domain::POLICY,
        &cli.data_dir.join("policy/state.json"),
    )?;
    let policy_engine = PolicyEngine::new(
        uuid::Uuid::new_v4().to_string(),
        CapabilityPolicy::default(),
        policy_state,
    );

    let brain_config = resolve_brain_config(cli)?;
    let brain_provider_label = brain_provider_label(&brain_config);
    let brain_engine = BrainEngine::from_config(&brain_config);
    let executor_base_url = resolve_executor_base_url(&brain_config);
    let brain_config_arc = Arc::new(tokio::sync::RwLock::new(brain_config.clone()));
    let brain_engine_arc = Arc::new(tokio::sync::RwLock::new(brain_engine));
    let servicenet_base_url = resolve_servicenet_base_url();
    let servicenet_client = Some(Arc::new(ServiceNetClient::new(&servicenet_base_url)?));

    let mut online_proof: OnlineProofManager = local_db.load_or_migrate(
        local_db::domain::ONLINE_PROOF,
        &cli.data_dir.join("online_proof.json"),
    )?;
    online_proof.create_lease(&identity.agent_did, 300, 20);
    let swarm_bridge: Arc<dyn SwarmBridge> = Arc::new(HybridSwarmBridge::new(
        cli.data_dir.join("missions/state.json"),
        cli.wattswarm_ui_base_url.as_deref(),
    ));
    let agent_surface = AgentParticipationSurface {
        control_plane_endpoint: cli.agent_control_plane_endpoint.clone(),
        wattswarm_ui_base_url: cli
            .agent_wattswarm_ui_base_url
            .clone()
            .or_else(|| cli.wattswarm_ui_base_url.clone()),
        wattswarm_sync_grpc_endpoint: cli
            .agent_wattswarm_sync_grpc_endpoint
            .clone()
            .or_else(|| resolve_wattswarm_sync_grpc_endpoint(cli)),
        host_data_dir: cli.agent_host_data_dir.clone(),
        mcp_token_auth_required: cli.mcp_token_auth_required,
    };
    agent_participation::write_agent_participation_artifacts(
        &cli.data_dir,
        &identity,
        &brain_config,
        &control_bind,
        Some(&servicenet_base_url),
        &agent_surface,
    )
    .context("write agent participation artifacts")?;
    let governance_engine: GovernanceEngine = local_db.load_or_migrate(
        local_db::domain::GOVERNANCE,
        &cli.data_dir.join("governance/state.json"),
    )?;
    let governance_engine = Arc::new(Mutex::new(governance_engine));
    let mailbox: CrossSubnetMailbox = local_db.load_or_migrate(
        local_db::domain::MAILBOX,
        &cli.data_dir.join("mailbox/state.json"),
    )?;
    let civilization_state = load_civilization_runtime_state(cli, &identity, &local_db)?;

    info!(agent_did = %identity.agent_did, "kernel started");

    let (stream_tx, _) = broadcast::channel(128);
    let control_state = build_control_state(
        cli,
        &identity,
        signer.clone(),
        &event_log,
        control_token,
        swarm_bridge,
        governance_engine,
        policy_engine,
        mailbox,
        civilization_state,
        brain_config_arc.clone(),
        brain_engine_arc,
        brain_provider_label,
        audit_log,
        local_db,
        social_store,
        servicenet_client,
        executor_base_url,
        Some(resolve_agent_event_callback_base_url(cli)),
        stream_tx,
    )
    .await;

    Ok(RuntimeState {
        control_bind,
        identity,
        online_proof,
        control_state,
        brain_config,
    })
}

#[allow(clippy::too_many_arguments)]
async fn build_control_state(
    cli: &Cli,
    identity: &IdentityCompatView,
    signer: Arc<dyn PayloadSigner>,
    event_log: &EventLog,
    control_token: String,
    swarm_bridge: Arc<dyn SwarmBridge>,
    governance_engine: Arc<Mutex<GovernanceEngine>>,
    policy_engine: PolicyEngine,
    mailbox: CrossSubnetMailbox,
    civilization_state: CivilizationRuntimeState,
    brain_config: Arc<tokio::sync::RwLock<BrainProviderConfig>>,
    brain_engine: Arc<tokio::sync::RwLock<BrainEngine>>,
    brain_provider_label: String,
    audit_log: AuditLog,
    local_db: Arc<LocalDb>,
    social_store: Arc<SocialStore>,
    servicenet_client: Option<Arc<ServiceNetClient>>,
    agent_executor_base_url: Option<String>,
    agent_event_callback_base_url: Option<String>,
    stream_tx: broadcast::Sender<StreamEvent>,
) -> ControlPlaneState {
    ControlPlaneState {
        data_dir: cli.data_dir.clone(),
        agent_did: identity.agent_did.clone(),
        identity: identity.clone(),
        signer,
        started_at: chrono::Utc::now().timestamp(),
        auth_token: control_token,
        mcp_token_auth_required: cli.mcp_token_auth_required,
        event_log: event_log.clone(),
        swarm_bridge,
        governance_engine,
        policy_engine: Arc::new(Mutex::new(policy_engine)),
        mailbox: Arc::new(Mutex::new(mailbox)),
        mission_board: Arc::new(Mutex::new(civilization_state.mission_board)),
        public_identity_registry: Arc::new(Mutex::new(civilization_state.public_identity_registry)),
        controller_binding_registry: Arc::new(Mutex::new(
            civilization_state.controller_binding_registry,
        )),
        citizen_registry: Arc::new(Mutex::new(civilization_state.citizen_registry)),
        relationship_registry: Arc::new(Mutex::new(civilization_state.relationship_registry)),
        organization_registry: Arc::new(Mutex::new(civilization_state.organization_registry)),
        hive_registry: Arc::new(Mutex::new(civilization_state.hive_registry)),
        payment_ledger: Arc::new(Mutex::new(civilization_state.payment_ledger)),
        galaxy_state: Arc::new(Mutex::new(civilization_state.galaxy_state)),
        galaxy_map_registry: Arc::new(Mutex::new(civilization_state.galaxy_map_registry)),
        travel_state_registry: Arc::new(Mutex::new(civilization_state.travel_state_registry)),
        brain_engine,
        brain_config,
        brain_provider_label,
        audit_log,
        local_db,
        social_store,
        servicenet_client,
        agent_executor_base_url,
        agent_event_callback_base_url,
        agent_topic_bridge_enabled: cli
            .agent_wattswarm_ui_base_url
            .as_deref()
            .or(cli.wattswarm_ui_base_url.as_deref())
            .map(str::trim)
            .is_some_and(|value| !value.is_empty()),
        rate_limiter: Arc::new(RateLimiter::new(cli.control_plane_rate_limit, 60)),
        stream_tx,
        gateway_event_seq: GatewayEventSequence::load_or_seed(&cli.data_dir),
        geo_location: NodeGeoLocation::load_or_fetch(&cli.data_dir, &identity.agent_did).await,
    }
}

fn brain_provider_label(config: &BrainProviderConfig) -> String {
    match config {
        BrainProviderConfig::Rules => "rules".to_string(),
        BrainProviderConfig::Ollama { base_url, model } => {
            format!("ollama model={model} url={base_url}")
        }
        BrainProviderConfig::OpenaiCompatible {
            base_url, model, ..
        } => {
            format!("openai-compatible model={model} url={base_url}")
        }
    }
}

fn load_civilization_runtime_state(
    cli: &Cli,
    identity: &IdentityCompatView,
    local_db: &LocalDb,
) -> Result<CivilizationRuntimeState> {
    let agent_did = &identity.agent_did;

    let mission_board: MissionBoard = local_db.load_or_migrate(
        local_db::domain::MISSION_BOARD,
        &cli.data_dir.join("missions/state.json"),
    )?;
    let mut public_identity_registry: PublicIdentityRegistry = local_db.load_or_migrate(
        local_db::domain::PUBLIC_IDENTITY_REGISTRY,
        &cli.data_dir.join("civilization/public_identities.json"),
    )?;
    let mut controller_binding_registry: ControllerBindingRegistry = local_db.load_or_migrate(
        local_db::domain::CONTROLLER_BINDING_REGISTRY,
        &cli.data_dir.join("civilization/controller_bindings.json"),
    )?;
    let citizen_registry: CitizenRegistry = local_db.load_or_migrate(
        local_db::domain::CITIZEN_REGISTRY,
        &cli.data_dir.join("civilization/profiles.json"),
    )?;
    let relationship_registry: RelationshipRegistry = local_db.load_or_migrate(
        local_db::domain::RELATIONSHIP_REGISTRY,
        &cli.data_dir.join("civilization/relationships.json"),
    )?;
    let organization_registry: OrganizationRegistry = local_db.load_or_migrate(
        local_db::domain::ORGANIZATION_REGISTRY,
        &cli.data_dir.join("civilization/organizations.json"),
    )?;
    let hive_registry = load_or_migrate_hive_registry(local_db, cli)?;
    let payment_ledger: PaymentLedger = local_db.load_or_migrate(
        local_db::domain::PAYMENT_LEDGER,
        &cli.data_dir.join("payments/ledger.json"),
    )?;
    let economic_policy: EconomicPolicy =
        local_db.load_domain_or_default(local_db::domain::ECONOMIC_POLICY)?;
    local_db.save_domain(local_db::domain::ECONOMIC_POLICY, &economic_policy)?;
    let watt_balance_state: WalletBalanceState =
        match local_db.load_domain_or_default(local_db::domain::WATT_BALANCE_STATE) {
            Ok(state) => state,
            Err(error) => {
                warn!("reset invalid watt balance state during startup: {error:#}");
                WalletBalanceState::default()
            }
        };
    local_db.save_domain(local_db::domain::WATT_BALANCE_STATE, &watt_balance_state)?;
    let galaxy_state: GalaxyState = load_or_migrate_galaxy(
        local_db,
        &cli.data_dir.join("galaxy/state.json"),
        &cli.data_dir.join("world/state.json"),
    )?;
    let mut galaxy_map_registry: GalaxyMapRegistry = local_db.load_or_migrate(
        local_db::domain::GALAXY_MAP_REGISTRY,
        &cli.data_dir.join("galaxy/maps.json"),
    )?;
    galaxy_map_registry.ensure_default_genesis_map(&galaxy_state.zones())?;
    local_db.save_domain(local_db::domain::GALAXY_MAP_REGISTRY, &galaxy_map_registry)?;
    let mut travel_state_registry: TravelStateRegistry = local_db.load_or_migrate(
        local_db::domain::TRAVEL_STATE_REGISTRY,
        &cli.data_dir.join("galaxy/travel_state.json"),
    )?;

    let public_identity =
        public_identity_registry.ensure_local_default_for_agent(agent_did, Some(agent_did))?;
    let controller_binding =
        controller_binding_registry.ensure_local_wattswarm(&public_identity.public_id, agent_did);
    if let Some(position) = resolve_anchor_position(
        &galaxy_map_registry.ensure_default_genesis_map(&galaxy_state.zones())?,
        None,
        None,
    ) {
        let _ = travel_state_registry.ensure_position(
            &public_identity.public_id,
            controller_binding
                .controller_node_id
                .as_deref()
                .unwrap_or(agent_did),
            position,
        );
    }
    local_db.save_domain(
        local_db::domain::PUBLIC_IDENTITY_REGISTRY,
        &public_identity_registry,
    )?;
    local_db.save_domain(
        local_db::domain::CONTROLLER_BINDING_REGISTRY,
        &controller_binding_registry,
    )?;
    local_db.save_domain(
        local_db::domain::TRAVEL_STATE_REGISTRY,
        &travel_state_registry,
    )?;

    Ok(CivilizationRuntimeState {
        mission_board,
        public_identity_registry,
        controller_binding_registry,
        citizen_registry,
        relationship_registry,
        organization_registry,
        hive_registry,
        payment_ledger,
        galaxy_state,
        galaxy_map_registry,
        travel_state_registry,
    })
}

fn load_or_migrate_hive_registry(local_db: &LocalDb, cli: &Cli) -> Result<HiveRegistry> {
    if let Some(registry) = local_db.load_domain::<HiveRegistry>(local_db::domain::HIVE_REGISTRY)? {
        return Ok(registry);
    }
    local_db.load_or_migrate(
        local_db::domain::HIVE_REGISTRY,
        &cli.data_dir.join("civilization/hives.json"),
    )
}

fn load_or_migrate_galaxy(
    local_db: &LocalDb,
    primary_path: &Path,
    legacy_path: &Path,
) -> Result<GalaxyState> {
    if let Some(value) = local_db.load_domain::<GalaxyState>(local_db::domain::GALAXY_STATE)? {
        return Ok(value);
    }
    let json_path = if primary_path.exists() {
        primary_path
    } else if legacy_path.exists() {
        legacy_path
    } else {
        primary_path
    };
    let mut value = if json_path.exists() {
        let raw = std::fs::read_to_string(json_path).context("read legacy galaxy json")?;
        if raw.trim().is_empty() {
            GalaxyState::default_with_core_zones()
        } else {
            serde_json::from_str(&raw).context("parse legacy galaxy json")?
        }
    } else {
        GalaxyState::default_with_core_zones()
    };
    if value.zones().is_empty() {
        value = GalaxyState::default_with_core_zones();
    }
    local_db.save_domain(local_db::domain::GALAXY_STATE, &value)?;
    Ok(value)
}

fn resolve_wattswarm_sync_grpc_endpoint(cli: &Cli) -> Option<String> {
    if let Some(endpoint) = &cli.wattswarm_sync_grpc_endpoint {
        let endpoint = endpoint.trim();
        if !endpoint.is_empty() {
            return Some(endpoint.to_string());
        }
    }
    cli.wattswarm_ui_base_url
        .as_deref()
        .and_then(derive_grpc_endpoint_from_ui_base)
}

fn derive_grpc_endpoint_from_ui_base(base_url: &str) -> Option<String> {
    let mut url = reqwest::Url::parse(base_url).ok()?;
    let host = url.host_str()?.to_string();
    let scheme = if url.scheme() == "https" {
        "https"
    } else {
        "http"
    };
    url.set_scheme(scheme).ok()?;
    url.set_host(Some(&host)).ok()?;
    url.set_port(Some(DEFAULT_WATTSWARM_SYNC_GRPC_PORT)).ok()?;
    url.set_path("");
    url.set_query(None);
    url.set_fragment(None);
    Some(url.to_string().trim_end_matches('/').to_string())
}

fn spawn_control_plane(
    control_state: ControlPlaneState,
    control_bind: SocketAddr,
) -> tokio::task::JoinHandle<()> {
    info!(bind = %control_bind, "starting control plane");
    tokio::spawn(async move {
        if let Err(error) = serve_control_plane(control_state, control_bind).await {
            error!(%error, "control plane terminated");
        }
    })
}

fn spawn_autonomy_task(
    cli: &Cli,
    control_state: ControlPlaneState,
) -> Option<tokio::task::JoinHandle<()>> {
    if !cli.autonomy_enabled {
        return None;
    }

    let interval_sec = cli.autonomy_interval_sec.max(5);
    Some(tokio::spawn(async move {
        let mut ticker = interval(Duration::from_secs(interval_sec));
        loop {
            ticker.tick().await;
            if let Err(error) = run_autonomy_tick_once(&control_state, 12).await {
                warn!(%error, "autonomy tick failed");
            }
        }
    }))
}

const CORE_AGENT_EXECUTOR_NAME: &str = "core-agent";

#[derive(Debug, Clone, PartialEq, Eq)]
struct WattswarmExecutorRegistration {
    endpoint_url: String,
    executor_name: String,
    executor_base_url: String,
    agent_event_callback_base_url: String,
    commit_plane_endpoint: String,
    commit_plane_token_file: String,
}

#[derive(Debug, serde::Serialize)]
struct ExecutorAddRequest<'a> {
    name: &'a str,
    base_url: &'a str,
    agent_event_callback_base_url: Option<&'a str>,
    remote: bool,
    commit_plane_endpoint: Option<&'a str>,
    commit_plane_token_file: Option<&'a str>,
}

fn spawn_wattswarm_executor_registration_task(
    cli: &Cli,
    brain_config: &BrainProviderConfig,
) -> Option<tokio::task::JoinHandle<()>> {
    let registration = resolve_wattswarm_executor_registration(cli, brain_config)?;
    Some(tokio::spawn(async move {
        let client = reqwest::Client::new();
        let mut retry = interval(Duration::from_secs(5));
        loop {
            retry.tick().await;
            match register_executor_once(&client, &registration).await {
                Ok(()) => {
                    info!(
                        executor = %registration.executor_name,
                        endpoint = %registration.endpoint_url,
                        "registered local executor with wattswarm"
                    );
                    break;
                }
                Err(error) => {
                    warn!(
                        executor = %registration.executor_name,
                        endpoint = %registration.endpoint_url,
                        %error,
                        "register local executor with wattswarm failed; retrying"
                    );
                }
            }
        }
    }))
}

fn resolve_wattswarm_executor_registration(
    cli: &Cli,
    brain_config: &BrainProviderConfig,
) -> Option<WattswarmExecutorRegistration> {
    let wattswarm_ui_base_url = cli
        .wattswarm_ui_base_url
        .as_deref()
        .or(cli.agent_wattswarm_ui_base_url.as_deref())
        .and_then(trim_base_url)?;
    let executor_base_url = resolve_executor_base_url(brain_config)?;
    let agent_event_callback_base_url = resolve_agent_event_callback_base_url(cli);
    Some(WattswarmExecutorRegistration {
        endpoint_url: format!("{wattswarm_ui_base_url}/api/executors/add"),
        executor_name: CORE_AGENT_EXECUTOR_NAME.to_owned(),
        executor_base_url,
        agent_event_callback_base_url: agent_event_callback_base_url.clone(),
        commit_plane_endpoint: agent_event_callback_base_url,
        commit_plane_token_file: cli
            .agent_host_data_dir
            .as_deref()
            .map_or_else(
                || cli.data_dir.join("control.token"),
                |dir| std::path::Path::new(dir).join("control.token"),
            )
            .display()
            .to_string(),
    })
}

fn trim_base_url(value: &str) -> Option<String> {
    let trimmed = value.trim().trim_end_matches('/');
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn resolve_executor_base_url(brain_config: &BrainProviderConfig) -> Option<String> {
    match brain_config {
        BrainProviderConfig::Rules => None,
        BrainProviderConfig::Ollama { base_url, .. }
        | BrainProviderConfig::OpenaiCompatible { base_url, .. } => {
            let base_url = base_url.trim().trim_end_matches('/');
            (!base_url.is_empty()).then(|| base_url.to_owned())
        }
    }
}

fn resolve_agent_event_callback_base_url(cli: &Cli) -> String {
    cli.wattswarm_agent_event_callback_base_url
        .as_deref()
        .and_then(trim_base_url)
        .or_else(|| {
            cli.agent_control_plane_endpoint
                .as_deref()
                .and_then(trim_base_url)
        })
        .unwrap_or_else(|| format!("http://{}", cli.control_plane_bind))
}

async fn register_executor_once(
    client: &reqwest::Client,
    registration: &WattswarmExecutorRegistration,
) -> Result<()> {
    client
        .post(&registration.endpoint_url)
        .json(&ExecutorAddRequest {
            name: &registration.executor_name,
            base_url: &registration.executor_base_url,
            agent_event_callback_base_url: Some(&registration.agent_event_callback_base_url),
            remote: false,
            commit_plane_endpoint: Some(&registration.commit_plane_endpoint),
            commit_plane_token_file: Some(&registration.commit_plane_token_file),
        })
        .send()
        .await
        .with_context(|| {
            format!(
                "POST wattswarm executor registration {}",
                registration.endpoint_url
            )
        })?
        .error_for_status()
        .context("wattswarm executor registration returned error status")?;
    Ok(())
}

fn spawn_gateway_dispatch_tasks(
    cli: &Cli,
    control_state: &ControlPlaneState,
) -> Vec<tokio::task::JoinHandle<()>> {
    let gateway_urls = resolve_gateway_urls(cli);
    if gateway_urls.is_empty() {
        return Vec::new();
    }

    let snapshot_urls = gateway_urls.clone();
    let snapshot_state = control_state.clone();
    let snapshot_interval_sec = cli.gateway_snapshot_interval_sec.max(15);
    let snapshot_task = tokio::spawn(async move {
        let client = reqwest::Client::new();
        let mut ticker = interval(Duration::from_secs(snapshot_interval_sec));
        loop {
            ticker.tick().await;
            for gateway_url in &snapshot_urls {
                if let Err(error) = push_signed_snapshot(
                    &client,
                    gateway_url,
                    &snapshot_state,
                    &ClientExportQuery::default(),
                )
                .await
                {
                    warn!(gateway_url, %error, "gateway snapshot push failed");
                }
            }
        }
    });

    let event_urls = gateway_urls;
    let event_state = control_state.clone();
    let event_task = tokio::spawn(async move {
        let client = reqwest::Client::new();
        let mut receiver = event_state.stream_tx.subscribe();
        loop {
            match receiver.recv().await {
                Ok(event) => match build_signed_node_event(&event_state, &event) {
                    Ok(Some(signed_event)) => {
                        for gateway_url in &event_urls {
                            if let Err(error) =
                                push_signed_node_event(&client, gateway_url, &signed_event).await
                            {
                                warn!(gateway_url, %error, kind = %event.kind, "gateway event push failed");
                            }
                        }
                    }
                    Ok(None) => {}
                    Err(error) => {
                        warn!(kind = %event.kind, %error, "build gateway node event failed");
                    }
                },
                Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                    warn!(skipped, "gateway dispatch stream lagged");
                }
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            }
        }
    });

    vec![snapshot_task, event_task]
}

#[derive(Debug, Deserialize, Default)]
struct GatewayDispatchConfig {
    #[serde(default)]
    gateway_urls: Vec<String>,
}

fn resolve_gateway_urls(cli: &Cli) -> Vec<String> {
    let cli_urls = normalize_gateway_urls(&cli.gateway_urls);
    if !cli_urls.is_empty() {
        return cli_urls;
    }
    cli.gateway_config_path
        .as_deref()
        .map(load_gateway_urls_from_config)
        .unwrap_or_default()
}

const DEFAULT_SERVICENET_BASE_URL: &str = "https://servicenet.wattetheria.com";

fn resolve_servicenet_base_url() -> String {
    DEFAULT_SERVICENET_BASE_URL.to_owned()
}

fn load_gateway_urls_from_config(path: &Path) -> Vec<String> {
    let Ok(bytes) = std::fs::read(path) else {
        return Vec::new();
    };
    match serde_json::from_slice::<GatewayDispatchConfig>(&bytes) {
        Ok(config) => normalize_gateway_urls(&config.gateway_urls),
        Err(error) => {
            warn!(
                path = %path.display(),
                %error,
                "failed to parse gateway config; skipping startup-config gateway URLs"
            );
            Vec::new()
        }
    }
}

fn normalize_gateway_urls(values: &[String]) -> Vec<String> {
    let mut normalized = Vec::new();
    for value in values {
        let trimmed = value.trim().trim_end_matches('/');
        if trimmed.is_empty() || normalized.iter().any(|existing| existing == trimmed) {
            continue;
        }
        normalized.push(trimmed.to_owned());
    }
    normalized
}

#[cfg(test)]
mod tests {
    use super::{
        CORE_AGENT_EXECUTOR_NAME, Cli, derive_grpc_endpoint_from_ui_base, register_executor_once,
        resolve_gateway_urls, resolve_servicenet_base_url, resolve_wattswarm_executor_registration,
    };
    use axum::{Json, Router, routing::post};
    use serde_json::{Value, json};
    use std::sync::{Arc, Mutex};
    use wattetheria_kernel::brain::BrainProviderConfig;

    #[test]
    fn derive_grpc_endpoint_from_ui_base_rewrites_port() {
        assert_eq!(
            derive_grpc_endpoint_from_ui_base("http://127.0.0.1:7788").as_deref(),
            Some("http://127.0.0.1:7791")
        );
        assert_eq!(
            derive_grpc_endpoint_from_ui_base("https://wattswarm.internal/ui").as_deref(),
            Some("https://wattswarm.internal:7791")
        );
    }

    #[test]
    fn resolve_wattswarm_executor_registration_skips_rules_provider() {
        let cli = Cli {
            data_dir: ".wattetheria".into(),
            recovery_sources: Vec::new(),
            control_plane_bind: "127.0.0.1:7777".to_owned(),
            wattswarm_ui_base_url: Some("http://127.0.0.1:7788".to_owned()),
            wattswarm_sync_grpc_endpoint: None,
            wattswarm_agent_event_callback_base_url: None,
            agent_control_plane_endpoint: None,
            agent_wattswarm_ui_base_url: None,
            agent_wattswarm_sync_grpc_endpoint: None,
            agent_host_data_dir: None,
            mcp_token_auth_required: false,
            gateway_urls: Vec::new(),
            gateway_config_path: None,
            gateway_snapshot_interval_sec: 45,
            control_plane_rate_limit: 60,
            brain_provider_kind: "rules".to_owned(),
            brain_base_url: "http://127.0.0.1:11434".to_owned(),
            brain_model: "model".to_owned(),
            brain_api_key_env: None,
            autonomy_enabled: false,
            autonomy_interval_sec: 30,
        };
        assert!(
            resolve_wattswarm_executor_registration(&cli, &BrainProviderConfig::Rules).is_none()
        );
    }

    #[test]
    fn resolve_wattswarm_executor_registration_uses_core_agent_and_trimmed_urls() {
        let cli = Cli {
            data_dir: ".wattetheria".into(),
            recovery_sources: Vec::new(),
            control_plane_bind: "127.0.0.1:7777".to_owned(),
            wattswarm_ui_base_url: Some("http://wattswarm-kernel:7788/".to_owned()),
            wattswarm_sync_grpc_endpoint: None,
            wattswarm_agent_event_callback_base_url: Some(
                " http://wattetheria-kernel:7777/ ".to_owned(),
            ),
            agent_control_plane_endpoint: Some("http://127.0.0.1:7777".to_owned()),
            agent_wattswarm_ui_base_url: Some("http://127.0.0.1:7788/".to_owned()),
            agent_wattswarm_sync_grpc_endpoint: None,
            agent_host_data_dir: None,
            mcp_token_auth_required: false,
            gateway_urls: Vec::new(),
            gateway_config_path: None,
            gateway_snapshot_interval_sec: 45,
            control_plane_rate_limit: 60,
            brain_provider_kind: "openai-compatible".to_owned(),
            brain_base_url: "http://127.0.0.1:8787/v1/".to_owned(),
            brain_model: "model".to_owned(),
            brain_api_key_env: None,
            autonomy_enabled: false,
            autonomy_interval_sec: 30,
        };
        let registration = resolve_wattswarm_executor_registration(
            &cli,
            &BrainProviderConfig::OpenaiCompatible {
                base_url: "http://127.0.0.1:8787/v1/".to_owned(),
                model: "model".to_owned(),
                api_key_env: None,
            },
        )
        .expect("registration");
        assert_eq!(registration.executor_name, CORE_AGENT_EXECUTOR_NAME);
        assert_eq!(
            registration.endpoint_url,
            "http://wattswarm-kernel:7788/api/executors/add"
        );
        assert_eq!(registration.executor_base_url, "http://127.0.0.1:8787/v1");
        assert_eq!(
            registration.agent_event_callback_base_url,
            "http://wattetheria-kernel:7777"
        );
        assert_eq!(
            registration.commit_plane_endpoint,
            "http://wattetheria-kernel:7777"
        );
    }

    #[test]
    fn resolve_wattswarm_executor_registration_falls_back_to_agent_wattswarm_url() {
        let cli = Cli {
            data_dir: ".wattetheria".into(),
            recovery_sources: Vec::new(),
            control_plane_bind: "127.0.0.1:7777".to_owned(),
            wattswarm_ui_base_url: None,
            wattswarm_sync_grpc_endpoint: None,
            wattswarm_agent_event_callback_base_url: None,
            agent_control_plane_endpoint: Some("http://127.0.0.1:7777".to_owned()),
            agent_wattswarm_ui_base_url: Some("http://127.0.0.1:7788/".to_owned()),
            agent_wattswarm_sync_grpc_endpoint: None,
            agent_host_data_dir: None,
            mcp_token_auth_required: false,
            gateway_urls: Vec::new(),
            gateway_config_path: None,
            gateway_snapshot_interval_sec: 45,
            control_plane_rate_limit: 60,
            brain_provider_kind: "openai-compatible".to_owned(),
            brain_base_url: "http://127.0.0.1:8787/v1/".to_owned(),
            brain_model: "model".to_owned(),
            brain_api_key_env: None,
            autonomy_enabled: false,
            autonomy_interval_sec: 30,
        };

        let registration = resolve_wattswarm_executor_registration(
            &cli,
            &BrainProviderConfig::OpenaiCompatible {
                base_url: "http://127.0.0.1:8787/v1/".to_owned(),
                model: "model".to_owned(),
                api_key_env: None,
            },
        )
        .expect("registration");

        assert_eq!(
            registration.endpoint_url,
            "http://127.0.0.1:7788/api/executors/add"
        );
        assert_eq!(
            registration.agent_event_callback_base_url,
            "http://127.0.0.1:7777"
        );
    }

    #[test]
    fn resolve_wattswarm_executor_registration_uses_agent_host_data_dir_for_commit_token() {
        let cli = Cli {
            data_dir: ".wattetheria".into(),
            recovery_sources: Vec::new(),
            control_plane_bind: "127.0.0.1:7777".to_owned(),
            wattswarm_ui_base_url: Some("http://wattswarm-kernel:7788".to_owned()),
            wattswarm_sync_grpc_endpoint: None,
            wattswarm_agent_event_callback_base_url: Some("http://kernel:7777".to_owned()),
            agent_control_plane_endpoint: Some("http://127.0.0.1:7777".to_owned()),
            agent_wattswarm_ui_base_url: None,
            agent_wattswarm_sync_grpc_endpoint: None,
            agent_host_data_dir: Some("/var/lib/wattetheria".to_owned()),
            mcp_token_auth_required: false,
            gateway_urls: Vec::new(),
            gateway_config_path: None,
            gateway_snapshot_interval_sec: 45,
            control_plane_rate_limit: 60,
            brain_provider_kind: "openai-compatible".to_owned(),
            brain_base_url: "http://127.0.0.1:8787/v1/".to_owned(),
            brain_model: "model".to_owned(),
            brain_api_key_env: None,
            autonomy_enabled: false,
            autonomy_interval_sec: 30,
        };

        let registration = resolve_wattswarm_executor_registration(
            &cli,
            &BrainProviderConfig::OpenaiCompatible {
                base_url: "http://127.0.0.1:8787/v1/".to_owned(),
                model: "model".to_owned(),
                api_key_env: None,
            },
        )
        .expect("registration");

        assert_eq!(
            registration.commit_plane_token_file,
            "/var/lib/wattetheria/control.token"
        );
    }

    #[test]
    fn resolve_gateway_urls_reads_config_file_when_cli_urls_missing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_path = dir.path().join("startup_config.json");
        std::fs::write(
            &config_path,
            serde_json::to_vec(&json!({
                "gateway_urls": [
                    " http://52.91.11.113:8080/ ",
                    "http://52.91.11.113:8080",
                    "https://gw.example.com"
                ]
            }))
            .expect("serialize config"),
        )
        .expect("write config");
        let cli = Cli {
            data_dir: ".wattetheria".into(),
            recovery_sources: Vec::new(),
            control_plane_bind: "127.0.0.1:7777".to_owned(),
            wattswarm_ui_base_url: None,
            wattswarm_sync_grpc_endpoint: None,
            wattswarm_agent_event_callback_base_url: None,
            agent_control_plane_endpoint: None,
            agent_wattswarm_ui_base_url: None,
            agent_wattswarm_sync_grpc_endpoint: None,
            agent_host_data_dir: None,
            mcp_token_auth_required: false,
            gateway_urls: Vec::new(),
            gateway_config_path: Some(config_path),
            gateway_snapshot_interval_sec: 45,
            control_plane_rate_limit: 60,
            brain_provider_kind: "rules".to_owned(),
            brain_base_url: "http://127.0.0.1:11434".to_owned(),
            brain_model: "model".to_owned(),
            brain_api_key_env: None,
            autonomy_enabled: false,
            autonomy_interval_sec: 30,
        };

        assert_eq!(
            resolve_gateway_urls(&cli),
            vec![
                "http://52.91.11.113:8080".to_owned(),
                "https://gw.example.com".to_owned()
            ]
        );
    }

    #[test]
    fn resolve_gateway_urls_prefers_explicit_cli_urls_over_config_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let config_path = dir.path().join("startup_config.json");
        std::fs::write(
            &config_path,
            serde_json::to_vec(&json!({
                "gateway_urls": ["https://gw.example.com"]
            }))
            .expect("serialize config"),
        )
        .expect("write config");
        let cli = Cli {
            data_dir: ".wattetheria".into(),
            recovery_sources: Vec::new(),
            control_plane_bind: "127.0.0.1:7777".to_owned(),
            wattswarm_ui_base_url: None,
            wattswarm_sync_grpc_endpoint: None,
            wattswarm_agent_event_callback_base_url: None,
            agent_control_plane_endpoint: None,
            agent_wattswarm_ui_base_url: None,
            agent_wattswarm_sync_grpc_endpoint: None,
            agent_host_data_dir: None,
            mcp_token_auth_required: false,
            gateway_urls: vec!["http://primary-gateway:8080/".to_owned()],
            gateway_config_path: Some(config_path),
            gateway_snapshot_interval_sec: 45,
            control_plane_rate_limit: 60,
            brain_provider_kind: "rules".to_owned(),
            brain_base_url: "http://127.0.0.1:11434".to_owned(),
            brain_model: "model".to_owned(),
            brain_api_key_env: None,
            autonomy_enabled: false,
            autonomy_interval_sec: 30,
        };

        assert_eq!(
            resolve_gateway_urls(&cli),
            vec!["http://primary-gateway:8080".to_owned()]
        );
    }

    #[test]
    fn resolve_servicenet_base_url_is_fixed_official_endpoint() {
        assert_eq!(
            resolve_servicenet_base_url(),
            "https://servicenet.wattetheria.com"
        );
    }

    #[tokio::test]
    async fn register_executor_once_posts_executor_add_request() {
        let seen = Arc::new(Mutex::new(Vec::<Value>::new()));
        let seen_clone = Arc::clone(&seen);
        let app = Router::new().route(
            "/api/executors/add",
            post(move |Json(payload): Json<Value>| {
                let seen = Arc::clone(&seen_clone);
                async move {
                    seen.lock().expect("lock").push(payload);
                    Json(json!({"ok": true}))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind listener");
        let addr = listener.local_addr().expect("listener addr");
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve app");
        });
        let registration = super::WattswarmExecutorRegistration {
            endpoint_url: format!("http://{addr}/api/executors/add"),
            executor_name: CORE_AGENT_EXECUTOR_NAME.to_owned(),
            executor_base_url: "http://127.0.0.1:8787".to_owned(),
            agent_event_callback_base_url: "http://127.0.0.1:7777".to_owned(),
            commit_plane_endpoint: "http://127.0.0.1:7791".to_owned(),
            commit_plane_token_file: "/tmp/wattetheria-token".to_owned(),
        };

        register_executor_once(&reqwest::Client::new(), &registration)
            .await
            .expect("register executor");

        let seen = seen.lock().expect("lock");
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0]["name"].as_str(), Some(CORE_AGENT_EXECUTOR_NAME));
        assert_eq!(seen[0]["base_url"].as_str(), Some("http://127.0.0.1:8787"));
        assert_eq!(
            seen[0]["agent_event_callback_base_url"].as_str(),
            Some("http://127.0.0.1:7777")
        );
        assert_eq!(seen[0]["remote"].as_bool(), Some(false));
        assert_eq!(
            seen[0]["commit_plane_endpoint"].as_str(),
            Some("http://127.0.0.1:7791")
        );
        assert_eq!(
            seen[0]["commit_plane_token_file"].as_str(),
            Some("/tmp/wattetheria-token")
        );
        server.abort();
    }
}
