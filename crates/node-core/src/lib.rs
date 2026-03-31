mod agent_participation;
mod bootstrap;
pub mod cli;
mod recovery;
mod runtime_loop;

use anyhow::{Context, Result};
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
    ControlPlaneState, DEFAULT_WATTSWARM_SYNC_GRPC_PORT, RateLimiter, StreamEvent,
    run_autonomy_tick_once, serve_control_plane, spawn_wattswarm_sync_bridge,
};
use wattetheria_kernel::audit::AuditLog;
use wattetheria_kernel::brain::BrainEngine;
use wattetheria_kernel::capabilities::CapabilityPolicy;
use wattetheria_kernel::civilization::galaxy::GalaxyState;
use wattetheria_kernel::civilization::identities::{
    ControllerBindingRegistry, PublicIdentityRegistry,
};
use wattetheria_kernel::civilization::missions::MissionBoard;
use wattetheria_kernel::civilization::organizations::OrganizationRegistry;
use wattetheria_kernel::civilization::profiles::CitizenRegistry;
use wattetheria_kernel::civilization::relationships::RelationshipRegistry;
use wattetheria_kernel::civilization::topics::TopicRegistry;
use wattetheria_kernel::event_log::EventLog;
use wattetheria_kernel::governance::GovernanceEngine;
use wattetheria_kernel::identity::IdentityCompatView;
use wattetheria_kernel::local_db::{self, LocalDb};
use wattetheria_kernel::mailbox::CrossSubnetMailbox;
use wattetheria_kernel::map::registry::GalaxyMapRegistry;
use wattetheria_kernel::map::state::{TravelStateRegistry, resolve_anchor_position};
use wattetheria_kernel::online_proof::OnlineProofManager;
use wattetheria_kernel::policy_engine::{PolicyEngine, PolicyState};
use wattetheria_kernel::signing::PayloadSigner;
use wattetheria_kernel::swarm_bridge::{HybridSwarmBridge, SwarmBridge};
use wattetheria_kernel::wallet_identity::WalletSigner;

struct RuntimeState {
    control_bind: SocketAddr,
    identity: IdentityCompatView,
    online_proof: OnlineProofManager,
    control_state: ControlPlaneState,
}

struct CivilizationRuntimeState {
    mission_board: MissionBoard,
    public_identity_registry: PublicIdentityRegistry,
    controller_binding_registry: ControllerBindingRegistry,
    citizen_registry: CitizenRegistry,
    relationship_registry: RelationshipRegistry,
    organization_registry: OrganizationRegistry,
    topic_registry: TopicRegistry,
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

    let local_db = Arc::new(LocalDb::open(cli.data_dir.join("state.db"))?);
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
    let brain_engine = Arc::new(BrainEngine::from_config(&brain_config));

    let mut online_proof: OnlineProofManager = local_db.load_or_migrate(
        local_db::domain::ONLINE_PROOF,
        &cli.data_dir.join("online_proof.json"),
    )?;
    online_proof.create_lease(&identity.agent_did, 300, 20);
    let ledger_path = cli.data_dir.join("ledger.json");
    let legacy_task_engine =
        wattetheria_kernel::task_engine::TaskEngine::new_with_ledger_and_signer(
            event_log.clone(),
            identity.clone(),
            signer.clone(),
            &ledger_path,
        )?;
    let swarm_bridge: Arc<dyn SwarmBridge> = Arc::new(HybridSwarmBridge::new(
        legacy_task_engine,
        ledger_path.clone(),
        cli.wattswarm_ui_base_url.as_deref(),
    ));
    agent_participation::write_agent_participation_artifacts(
        &cli.data_dir,
        &identity,
        &brain_config,
        &control_bind,
        cli.wattswarm_ui_base_url.as_deref(),
        resolve_wattswarm_sync_grpc_endpoint(cli).as_deref(),
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
        brain_engine,
        audit_log,
        local_db,
        stream_tx,
    );

    Ok(RuntimeState {
        control_bind,
        identity,
        online_proof,
        control_state,
    })
}

#[allow(clippy::too_many_arguments)]
fn build_control_state(
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
    brain_engine: Arc<BrainEngine>,
    audit_log: AuditLog,
    local_db: Arc<LocalDb>,
    stream_tx: broadcast::Sender<StreamEvent>,
) -> ControlPlaneState {
    ControlPlaneState {
        agent_did: identity.agent_did.clone(),
        identity: identity.clone(),
        signer,
        started_at: chrono::Utc::now().timestamp(),
        auth_token: control_token,
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
        topic_registry: Arc::new(Mutex::new(civilization_state.topic_registry)),
        galaxy_state: Arc::new(Mutex::new(civilization_state.galaxy_state)),
        galaxy_map_registry: Arc::new(Mutex::new(civilization_state.galaxy_map_registry)),
        travel_state_registry: Arc::new(Mutex::new(civilization_state.travel_state_registry)),
        brain_engine,
        audit_log,
        local_db,
        rate_limiter: Arc::new(RateLimiter::new(cli.control_plane_rate_limit, 60)),
        stream_tx,
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
    let topic_registry: TopicRegistry = local_db.load_or_migrate(
        local_db::domain::TOPIC_REGISTRY,
        &cli.data_dir.join("civilization/topics.json"),
    )?;
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
        topic_registry,
        galaxy_state,
        galaxy_map_registry,
        travel_state_registry,
    })
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

#[cfg(test)]
mod tests {
    use super::derive_grpc_endpoint_from_ui_base;

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
}
