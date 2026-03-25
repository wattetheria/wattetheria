mod bootstrap;
pub mod cli;
mod handshake;
mod oracle_sync;
mod recovery;
mod runtime_loop;
mod snapshot_broadcast;

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
use handshake::build_signed_handshake_for_identity_and_signer;
use recovery::startup_recover_events;
use runtime_loop::{LoopContext, log_listeners, run_loop};
use snapshot_broadcast::{
    gateway_push_interval_sec, gateway_startup_jitter_secs, gateway_sync_enabled,
};
use wattetheria_control_plane::{
    ControlPlaneState, DEFAULT_WATTSWARM_SYNC_GRPC_PORT, RateLimiter, StreamEvent,
    run_autonomy_tick_once, serve_control_plane, spawn_wattswarm_sync_bridge,
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
use wattetheria_kernel::civilization::relationships::RelationshipRegistry;
use wattetheria_kernel::civilization::topics::TopicRegistry;
use wattetheria_kernel::event_log::EventLog;
use wattetheria_kernel::governance::GovernanceEngine;
use wattetheria_kernel::identity::IdentityCompatView;
use wattetheria_kernel::local_db::LocalDb;
use wattetheria_kernel::mailbox::CrossSubnetMailbox;
use wattetheria_kernel::map::registry::GalaxyMapRegistry;
use wattetheria_kernel::map::state::{TravelStateRegistry, resolve_anchor_position};
use wattetheria_kernel::online_proof::OnlineProofManager;
use wattetheria_kernel::oracle::OracleRegistry;
use wattetheria_kernel::policy_engine::PolicyEngine;
use wattetheria_kernel::signing::PayloadSigner;
use wattetheria_kernel::swarm_bridge::{HybridSwarmBridge, SwarmBridge};
use wattetheria_kernel::trust::{TrustConfig, WebOfTrust};
use wattetheria_kernel::wallet_identity::WalletSigner;
use wattetheria_p2p_runtime::{P2PConfig, P2PNode};

struct RuntimeState {
    control_bind: SocketAddr,
    identity: IdentityCompatView,
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
    relationship_registry: RelationshipRegistry,
    relationship_registry_state_path: PathBuf,
    organization_registry: OrganizationRegistry,
    organization_registry_state_path: PathBuf,
    topic_registry: TopicRegistry,
    topic_registry_state_path: PathBuf,
    galaxy_state: GalaxyState,
    galaxy_state_path: PathBuf,
    galaxy_map_registry: GalaxyMapRegistry,
    galaxy_map_registry_state_path: PathBuf,
    travel_state_registry: TravelStateRegistry,
    travel_state_registry_state_path: PathBuf,
}

pub async fn run(cli: Cli) -> Result<()> {
    let mut runtime = setup_runtime(&cli).await?;

    let control_task = spawn_control_plane(runtime.control_state.clone(), runtime.control_bind);
    let autonomy_task = spawn_autonomy_task(&cli, runtime.control_state.clone());
    let wattswarm_sync_task = spawn_wattswarm_sync_bridge(
        runtime.control_state.clone(),
        resolve_wattswarm_sync_grpc_endpoint(&cli),
    );
    let gateway_publish_enabled = gateway_sync_enabled(&cli);
    let gateway_push_interval_sec = gateway_push_interval_sec(&cli);
    let gateway_startup_jitter_sec =
        gateway_startup_jitter_secs(&runtime.identity.agent_did, gateway_push_interval_sec);
    if gateway_publish_enabled {
        warn!(
            "gateway snapshot publication now rides wattswarm gossip; direct HTTP gateway push is disabled"
        );
    }
    let admission_config = build_admission_config(&cli);
    let mut nonce_tracker = NonceTracker::new(admission_config.max_time_drift_sec * 2);
    let run_result = run_loop(
        &mut runtime.p2p,
        LoopContext {
            online_proof: &mut runtime.online_proof,
            online_proof_path: &runtime.online_proof_path,
            identity: &runtime.identity,
            control_state: &runtime.control_state,
            admission_config: &admission_config,
            nonce_tracker: &mut nonce_tracker,
            web_of_trust: &mut runtime.web_of_trust,
            event_log: &runtime.event_log,
            oracle_registry: &mut runtime.oracle_registry,
            oracle_state_path: &runtime.oracle_state_path,
            gateway_publish_enabled,
            gateway_push_interval_sec,
            gateway_startup_jitter_sec,
            enable_hashcash_broadcast: cli.enable_hashcash,
        },
    )
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
    online_proof.create_lease(&identity.agent_did, 300, 20);
    let web_of_trust = WebOfTrust::new(TrustConfig {
        blacklist_weight_threshold: 3,
    });

    let oracle_registry = OracleRegistry::load_or_new(cli.data_dir.join("oracle/state.json"))?;
    let oracle_state_path = cli.data_dir.join("oracle/state.json");

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
    let governance_state_path = cli.data_dir.join("governance/state.json");
    let governance_engine = Arc::new(Mutex::new(GovernanceEngine::load_or_new(
        &governance_state_path,
    )?));
    let mailbox_state_path = cli.data_dir.join("mailbox/state.json");
    let mailbox = CrossSubnetMailbox::load_or_new(&mailbox_state_path)?;
    let civilization_state = load_civilization_runtime_state(cli, &identity)?;

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
    let handshake = build_signed_handshake_for_identity_and_signer(
        &identity,
        signer.as_ref(),
        Some(public_id.as_str()),
        &online_proof,
        cli.enable_hashcash,
    )?;
    p2p.publish_json(&handshake)?;

    info!(agent_did = %identity.agent_did, "kernel started");
    log_listeners(&p2p);

    let (stream_tx, _) = broadcast::channel(128);
    let control_state = build_control_state(
        cli,
        &identity,
        signer.clone(),
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
        local_db,
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
    identity: &IdentityCompatView,
) -> String {
    civilization_state
        .controller_binding_registry
        .active_for_controller(&identity.agent_did)
        .and_then(|binding| {
            civilization_state
                .public_identity_registry
                .get(&binding.public_id)
                .filter(|public_identity| public_identity.active)
        })
        .or_else(|| {
            civilization_state
                .public_identity_registry
                .active_for_agent_did(&identity.agent_did)
        })
        .map_or_else(
            || identity.agent_did.clone(),
            |public_identity| public_identity.public_id,
        )
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
    governance_state_path: PathBuf,
    policy_engine: PolicyEngine,
    mailbox: CrossSubnetMailbox,
    mailbox_state_path: PathBuf,
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
        relationship_registry: Arc::new(Mutex::new(civilization_state.relationship_registry)),
        relationship_registry_state_path: civilization_state.relationship_registry_state_path,
        organization_registry: Arc::new(Mutex::new(civilization_state.organization_registry)),
        organization_registry_state_path: civilization_state.organization_registry_state_path,
        topic_registry: Arc::new(Mutex::new(civilization_state.topic_registry)),
        topic_registry_state_path: civilization_state.topic_registry_state_path,
        galaxy_state: Arc::new(Mutex::new(civilization_state.galaxy_state)),
        galaxy_state_path: civilization_state.galaxy_state_path,
        galaxy_map_registry: Arc::new(Mutex::new(civilization_state.galaxy_map_registry)),
        galaxy_map_registry_state_path: civilization_state.galaxy_map_registry_state_path,
        travel_state_registry: Arc::new(Mutex::new(civilization_state.travel_state_registry)),
        travel_state_registry_state_path: civilization_state.travel_state_registry_state_path,
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
) -> Result<CivilizationRuntimeState> {
    let agent_did = &identity.agent_did;
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
    let relationship_registry_state_path = cli.data_dir.join("civilization/relationships.json");
    let relationship_registry =
        RelationshipRegistry::load_or_new(&relationship_registry_state_path)?;
    let organization_registry_state_path = cli.data_dir.join("civilization/organizations.json");
    let organization_registry =
        OrganizationRegistry::load_or_new(&organization_registry_state_path)?;
    let topic_registry_state_path = cli.data_dir.join("civilization/topics.json");
    let topic_registry = TopicRegistry::load_or_new(&topic_registry_state_path)?;
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
    let public_identity =
        public_identity_registry.ensure_local_default_for_agent(agent_did, Some(agent_did));
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
        relationship_registry,
        relationship_registry_state_path,
        organization_registry,
        organization_registry_state_path,
        topic_registry,
        topic_registry_state_path,
        galaxy_state,
        galaxy_state_path,
        galaxy_map_registry,
        galaxy_map_registry_state_path,
        travel_state_registry,
        travel_state_registry_state_path,
    })
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
