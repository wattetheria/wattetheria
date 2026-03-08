mod bootstrap;
pub mod cli;
mod demo;
mod handshake;
mod oracle_sync;
mod recovery;
mod runtime_loop;

use anyhow::{Context, Result};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{Mutex, broadcast};
use tokio::time::{Duration, interval};
use tracing::{error, info, warn};

pub use bootstrap::init_tracing;
use bootstrap::{
    load_or_create_control_token, parse_control_bind, parse_multiaddrs, resolve_brain_config,
};
pub use cli::Cli;
use demo::{ignite_demo_planet, run_demo_task};
use handshake::build_signed_handshake_for_public_identity;
use recovery::startup_recover_events;
use runtime_loop::{LoopContext, log_listeners, run_loop};
use wattetheria_control_plane::{
    ControlPlaneState, RateLimiter, StreamEvent, run_autonomy_tick_once, serve_control_plane,
};
use wattetheria_kernel::admission::{AdmissionConfig, NonceTracker};
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
use wattetheria_kernel::event_log::EventLog;
use wattetheria_kernel::governance::GovernanceEngine;
use wattetheria_kernel::identity::Identity;
use wattetheria_kernel::mailbox::CrossSubnetMailbox;
use wattetheria_kernel::map::registry::GalaxyMapRegistry;
use wattetheria_kernel::map::state::{TravelStateRegistry, resolve_anchor_position};
use wattetheria_kernel::online_proof::OnlineProofManager;
use wattetheria_kernel::oracle::OracleRegistry;
use wattetheria_kernel::policy_engine::PolicyEngine;
use wattetheria_kernel::swarm_bridge::{LegacyTaskEngineBridge, SwarmBridge};
use wattetheria_kernel::trust::{TrustConfig, WebOfTrust};
use wattetheria_p2p_runtime::{P2PConfig, P2PNode};

struct RuntimeState {
    control_bind: SocketAddr,
    identity: Identity,
    event_log: EventLog,
    online_proof: OnlineProofManager,
    online_proof_path: PathBuf,
    web_of_trust: WebOfTrust,
    oracle_registry: OracleRegistry,
    oracle_state_path: PathBuf,
    p2p: P2PNode,
    control_state: ControlPlaneState,
}

struct CivilizationRuntimeState {
    mission_board: MissionBoard,
    mission_board_state_path: PathBuf,
    public_identity_registry: PublicIdentityRegistry,
    public_identity_registry_state_path: PathBuf,
    controller_binding_registry: ControllerBindingRegistry,
    controller_binding_registry_state_path: PathBuf,
    citizen_registry: CitizenRegistry,
    citizen_registry_state_path: PathBuf,
    organization_registry: OrganizationRegistry,
    organization_registry_state_path: PathBuf,
    galaxy_state: GalaxyState,
    galaxy_state_path: PathBuf,
    galaxy_map_registry: GalaxyMapRegistry,
    galaxy_map_registry_state_path: PathBuf,
    travel_state_registry: TravelStateRegistry,
    travel_state_registry_state_path: PathBuf,
}

pub async fn run(cli: Cli) -> Result<()> {
    let mut runtime = setup_runtime(&cli).await?;

    if cli.run_demo_task {
        run_demo_task(
            &runtime.control_state.swarm_bridge,
            &mut runtime.p2p,
            &runtime.identity,
        )
        .await?;
    }

    if cli.ignite_demo_planet {
        let mut governance = runtime.control_state.governance_engine.lock().await;
        ignite_demo_planet(&mut governance, &runtime.identity)?;
        governance.persist(&runtime.control_state.governance_state_path)?;
    }

    let control_task = spawn_control_plane(runtime.control_state.clone(), runtime.control_bind);
    let autonomy_task = spawn_autonomy_task(&cli, runtime.control_state.clone());
    let admission_config = build_admission_config(&cli);
    let mut nonce_tracker = NonceTracker::new(admission_config.max_time_drift_sec * 2);
    let run_result = run_loop(
        &mut runtime.p2p,
        LoopContext {
            online_proof: &mut runtime.online_proof,
            online_proof_path: &runtime.online_proof_path,
            identity: &runtime.identity,
            admission_config: &admission_config,
            nonce_tracker: &mut nonce_tracker,
            web_of_trust: &mut runtime.web_of_trust,
            event_log: &runtime.event_log,
            oracle_registry: &mut runtime.oracle_registry,
            oracle_state_path: &runtime.oracle_state_path,
            enable_hashcash_broadcast: cli.enable_hashcash,
        },
    )
    .await;
    control_task.abort();
    if let Some(task) = autonomy_task {
        task.abort();
    }
    run_result
}

async fn setup_runtime(cli: &Cli) -> Result<RuntimeState> {
    std::fs::create_dir_all(&cli.data_dir).context("create data dir")?;
    let identity_path = cli.data_dir.join("identity.json");
    let events_path = cli.data_dir.join("events.jsonl");
    let snapshots_path = cli.data_dir.join("snapshots");
    std::fs::create_dir_all(&snapshots_path).context("create snapshots dir")?;

    startup_recover_events(&events_path, &snapshots_path, &cli.recovery_sources).await?;

    let identity = Identity::load_or_create(identity_path)?;
    let event_log = EventLog::new(events_path)?;
    let audit_log = AuditLog::new(cli.data_dir.join("audit/control_plane.jsonl"))?;
    let control_token = load_or_create_control_token(cli.data_dir.join("control.token"))?;
    let control_bind = parse_control_bind(&cli.control_plane_bind)?;
    let policy_state_path = cli.data_dir.join("policy/state.json");
    let policy_engine = PolicyEngine::load_or_new(
        policy_state_path,
        uuid::Uuid::new_v4().to_string(),
        CapabilityPolicy::default(),
    )?;

    let brain_config = resolve_brain_config(cli)?;
    let brain_engine = Arc::new(BrainEngine::from_config(&brain_config));

    let online_proof_path = cli.data_dir.join("online_proof.json");
    let mut online_proof = OnlineProofManager::load_or_new(&online_proof_path).unwrap_or_default();
    online_proof.create_lease(&identity.agent_id, 300, 20);
    let web_of_trust = WebOfTrust::new(TrustConfig {
        blacklist_weight_threshold: 3,
    });

    let oracle_registry = OracleRegistry::load_or_new(cli.data_dir.join("oracle/state.json"))?;
    let oracle_state_path = cli.data_dir.join("oracle/state.json");

    let ledger_path = cli.data_dir.join("ledger.json");
    let legacy_task_engine = wattetheria_kernel::task_engine::TaskEngine::new_with_ledger(
        event_log.clone(),
        identity.clone(),
        &ledger_path,
    )?;
    let swarm_bridge: Arc<dyn SwarmBridge> = Arc::new(LegacyTaskEngineBridge::new(
        legacy_task_engine,
        ledger_path.clone(),
    ));
    let governance_state_path = cli.data_dir.join("governance/state.json");
    let governance_engine = Arc::new(Mutex::new(GovernanceEngine::load_or_new(
        &governance_state_path,
    )?));
    let mailbox_state_path = cli.data_dir.join("mailbox/state.json");
    let mailbox = CrossSubnetMailbox::load_or_new(&mailbox_state_path)?;
    let civilization_state = load_civilization_runtime_state(cli, &identity.agent_id)?;

    let (listen_addr, bootstrap_addrs) = parse_multiaddrs(cli)?;
    let p2p_config = P2PConfig {
        max_connected_peers: cli.p2p_max_peers,
        per_peer_msgs_per_minute: cli.p2p_peer_rate_limit,
        per_topic_msgs_per_minute: cli.p2p_topic_rate_limit,
        per_topic_publish_per_minute: cli.p2p_publish_rate_limit,
        topic_shards: cli.p2p_topic_shards.max(1),
        dedupe_ttl_sec: cli.p2p_dedupe_ttl_sec,
        message_ttl_sec: cli.p2p_message_ttl_sec,
        ..P2PConfig::default()
    };
    let mut p2p = P2PNode::new_with_config(&cli.topic, listen_addr, &bootstrap_addrs, p2p_config)?;

    let public_id = resolve_public_identity_id(&civilization_state, &identity);
    let handshake = build_signed_handshake_for_public_identity(
        &identity,
        Some(public_id.as_str()),
        &online_proof,
        cli.enable_hashcash,
    )?;
    p2p.publish_json(&handshake)?;

    info!(agent_id = %identity.agent_id, "kernel started");
    log_listeners(&p2p);

    let (stream_tx, _) = broadcast::channel(128);
    let control_state = build_control_state(
        cli,
        &identity,
        &event_log,
        control_token,
        swarm_bridge,
        governance_engine,
        governance_state_path,
        policy_engine,
        mailbox,
        mailbox_state_path,
        civilization_state,
        brain_engine,
        audit_log,
        stream_tx,
    );

    Ok(RuntimeState {
        control_bind,
        identity,
        event_log,
        online_proof,
        online_proof_path,
        web_of_trust,
        oracle_registry,
        oracle_state_path,
        p2p,
        control_state,
    })
}

fn resolve_public_identity_id(
    civilization_state: &CivilizationRuntimeState,
    identity: &Identity,
) -> String {
    civilization_state
        .controller_binding_registry
        .active_for_controller(&identity.agent_id)
        .and_then(|binding| {
            civilization_state
                .public_identity_registry
                .get(&binding.public_id)
                .filter(|public_identity| public_identity.active)
        })
        .or_else(|| {
            civilization_state
                .public_identity_registry
                .active_for_legacy_agent(&identity.agent_id)
        })
        .map_or_else(
            || identity.agent_id.clone(),
            |public_identity| public_identity.public_id,
        )
}

#[allow(clippy::too_many_arguments)]
fn build_control_state(
    cli: &Cli,
    identity: &Identity,
    event_log: &EventLog,
    control_token: String,
    swarm_bridge: Arc<dyn SwarmBridge>,
    governance_engine: Arc<Mutex<GovernanceEngine>>,
    governance_state_path: PathBuf,
    policy_engine: PolicyEngine,
    mailbox: CrossSubnetMailbox,
    mailbox_state_path: PathBuf,
    civilization_state: CivilizationRuntimeState,
    brain_engine: Arc<BrainEngine>,
    audit_log: AuditLog,
    stream_tx: broadcast::Sender<StreamEvent>,
) -> ControlPlaneState {
    ControlPlaneState {
        agent_id: identity.agent_id.clone(),
        identity: identity.clone(),
        started_at: chrono::Utc::now().timestamp(),
        auth_token: control_token,
        event_log: event_log.clone(),
        swarm_bridge,
        governance_engine,
        governance_state_path,
        policy_engine: Arc::new(Mutex::new(policy_engine)),
        mailbox: Arc::new(Mutex::new(mailbox)),
        mailbox_state_path,
        mission_board: Arc::new(Mutex::new(civilization_state.mission_board)),
        mission_board_state_path: civilization_state.mission_board_state_path,
        public_identity_registry: Arc::new(Mutex::new(civilization_state.public_identity_registry)),
        public_identity_registry_state_path: civilization_state.public_identity_registry_state_path,
        controller_binding_registry: Arc::new(Mutex::new(
            civilization_state.controller_binding_registry,
        )),
        controller_binding_registry_state_path: civilization_state
            .controller_binding_registry_state_path,
        citizen_registry: Arc::new(Mutex::new(civilization_state.citizen_registry)),
        citizen_registry_state_path: civilization_state.citizen_registry_state_path,
        organization_registry: Arc::new(Mutex::new(civilization_state.organization_registry)),
        organization_registry_state_path: civilization_state.organization_registry_state_path,
        galaxy_state: Arc::new(Mutex::new(civilization_state.galaxy_state)),
        galaxy_state_path: civilization_state.galaxy_state_path,
        galaxy_map_registry: Arc::new(Mutex::new(civilization_state.galaxy_map_registry)),
        galaxy_map_registry_state_path: civilization_state.galaxy_map_registry_state_path,
        travel_state_registry: Arc::new(Mutex::new(civilization_state.travel_state_registry)),
        travel_state_registry_state_path: civilization_state.travel_state_registry_state_path,
        brain_engine,
        audit_log,
        rate_limiter: Arc::new(RateLimiter::new(cli.control_plane_rate_limit, 60)),
        stream_tx,
    }
}

fn load_civilization_runtime_state(cli: &Cli, agent_id: &str) -> Result<CivilizationRuntimeState> {
    let mission_board_state_path = cli.data_dir.join("missions/state.json");
    let mission_board = MissionBoard::load_or_new(&mission_board_state_path)?;
    let public_identity_registry_state_path =
        cli.data_dir.join("civilization/public_identities.json");
    let mut public_identity_registry =
        PublicIdentityRegistry::load_or_new(&public_identity_registry_state_path)?;
    let controller_binding_registry_state_path =
        cli.data_dir.join("civilization/controller_bindings.json");
    let mut controller_binding_registry =
        ControllerBindingRegistry::load_or_new(&controller_binding_registry_state_path)?;
    let citizen_registry_state_path = cli.data_dir.join("civilization/profiles.json");
    let citizen_registry = CitizenRegistry::load_or_new(&citizen_registry_state_path)?;
    let organization_registry_state_path = cli.data_dir.join("civilization/organizations.json");
    let organization_registry =
        OrganizationRegistry::load_or_new(&organization_registry_state_path)?;
    let galaxy_state_path = cli.data_dir.join("galaxy/state.json");
    let legacy_galaxy_state_path = cli.data_dir.join("world/state.json");
    let galaxy_state = if galaxy_state_path.exists() {
        GalaxyState::load_or_new(&galaxy_state_path)?
    } else if legacy_galaxy_state_path.exists() {
        GalaxyState::load_or_new(&legacy_galaxy_state_path)?
    } else {
        GalaxyState::load_or_new(&galaxy_state_path)?
    };
    let galaxy_map_registry_state_path = cli.data_dir.join("galaxy/maps.json");
    let mut galaxy_map_registry = GalaxyMapRegistry::load_or_new(&galaxy_map_registry_state_path)?;
    galaxy_map_registry.ensure_default_genesis_map(&galaxy_state.zones())?;
    galaxy_map_registry.persist(&galaxy_map_registry_state_path)?;
    let travel_state_registry_state_path = cli.data_dir.join("galaxy/travel_state.json");
    let mut travel_state_registry =
        TravelStateRegistry::load_or_new(&travel_state_registry_state_path)?;
    let public_identity = public_identity_registry.ensure_local_default(agent_id);
    let controller_binding =
        controller_binding_registry.ensure_local_wattswarm(&public_identity.public_id, agent_id);
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
                .unwrap_or(agent_id),
            position,
        );
    }
    public_identity_registry.persist(&public_identity_registry_state_path)?;
    controller_binding_registry.persist(&controller_binding_registry_state_path)?;
    travel_state_registry.persist(&travel_state_registry_state_path)?;

    Ok(CivilizationRuntimeState {
        mission_board,
        mission_board_state_path,
        public_identity_registry,
        public_identity_registry_state_path,
        controller_binding_registry,
        controller_binding_registry_state_path,
        citizen_registry,
        citizen_registry_state_path,
        organization_registry,
        organization_registry_state_path,
        galaxy_state,
        galaxy_state_path,
        galaxy_map_registry,
        galaxy_map_registry_state_path,
        travel_state_registry,
        travel_state_registry_state_path,
    })
}

fn build_admission_config(cli: &Cli) -> AdmissionConfig {
    AdmissionConfig {
        max_time_drift_sec: 180,
        min_hashcash_bits: 12,
        require_hashcash_for_handshake: cli.require_hashcash_inbound,
        require_hashcash_for_broadcast: cli.require_hashcash_broadcast,
    }
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
