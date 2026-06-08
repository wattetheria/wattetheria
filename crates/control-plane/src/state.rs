use axum::extract::ws::Message;
use axum::http::HeaderMap;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use tokio::sync::{Mutex, RwLock, broadcast};
use tracing::{info, warn};
use wattetheria_kernel::audit::AuditLog;
use wattetheria_kernel::brain::{BrainEngine, BrainProviderConfig};
use wattetheria_kernel::civilization::galaxy::{DynamicEventCategory, GalaxyState};
use wattetheria_kernel::civilization::identities::{
    ControllerBindingRegistry, ControllerKind, OwnershipScope, PublicIdentityRegistry,
};
use wattetheria_kernel::civilization::missions::{
    MissionBoard, MissionDomain, MissionPublisherKind, MissionReward, MissionStatus,
};
use wattetheria_kernel::civilization::organizations::{
    OrganizationKind, OrganizationProposalKind, OrganizationRegistry, OrganizationRole,
};
use wattetheria_kernel::civilization::profiles::{
    CitizenRegistry, Faction, RolePath, StrategyProfile,
};
use wattetheria_kernel::civilization::relationships::{RelationshipKind, RelationshipRegistry};
use wattetheria_kernel::civilization::topics::{HiveRegistry, TopicProjectionKind};
use wattetheria_kernel::event_log::{EventLog, EventRecord};
use wattetheria_kernel::governance::GovernanceEngine;
use wattetheria_kernel::identity::IdentityCompatView;
use wattetheria_kernel::local_db::LocalDb;
use wattetheria_kernel::mailbox::CrossSubnetMailbox;
use wattetheria_kernel::map::registry::GalaxyMapRegistry;
use wattetheria_kernel::map::state::TravelStateRegistry;
use wattetheria_kernel::payments::PaymentLedger;
use wattetheria_kernel::policy_engine::{GrantScope, PolicyEngine};
use wattetheria_kernel::servicenet::ServiceNetClient;
use wattetheria_kernel::signing::{PayloadSigner, sign_payload_with};
use wattetheria_kernel::swarm_bridge::SwarmAgentEnvelope;
use wattetheria_kernel::swarm_bridge::{SwarmBridge, SwarmRelationshipAction};
use wattetheria_social::SocialStore;

#[derive(Debug)]
pub struct RateLimiter {
    max_requests: usize,
    window_sec: i64,
    buckets: Mutex<BTreeMap<String, Vec<i64>>>,
}

impl RateLimiter {
    #[must_use]
    pub fn new(max_requests: usize, window_sec: i64) -> Self {
        Self {
            max_requests,
            window_sec,
            buckets: Mutex::new(BTreeMap::new()),
        }
    }

    pub async fn allow(&self, key: &str) -> bool {
        let mut buckets = self.buckets.lock().await;
        let now = Utc::now().timestamp();
        let window_start = now - self.window_sec;
        let entries = buckets.entry(key.to_string()).or_default();
        entries.retain(|timestamp| *timestamp >= window_start);
        if entries.len() >= self.max_requests {
            return false;
        }
        entries.push(now);
        true
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct StreamEvent {
    pub kind: String,
    pub timestamp: i64,
    pub payload: Value,
}

#[derive(Debug, Clone)]
pub struct AgentCommitContext {
    pub event_id: String,
    pub decision_id: String,
}

pub fn agent_commit_context_from_headers(headers: &HeaderMap) -> Option<AgentCommitContext> {
    let event_id = headers
        .get("x-agent-event-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())?
        .to_owned();
    let decision_id = headers
        .get("x-agent-decision-id")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())?
        .to_owned();
    Some(AgentCommitContext {
        event_id,
        decision_id,
    })
}

#[derive(Clone)]
pub struct ControlPlaneState {
    pub data_dir: PathBuf,
    pub agent_did: String,
    pub identity: IdentityCompatView,
    pub signer: Arc<dyn PayloadSigner>,
    pub started_at: i64,
    pub auth_token: String,
    pub mcp_token_auth_required: bool,
    pub event_log: EventLog,
    pub swarm_bridge: Arc<dyn SwarmBridge>,
    pub governance_engine: Arc<Mutex<GovernanceEngine>>,
    pub policy_engine: Arc<Mutex<PolicyEngine>>,
    pub mailbox: Arc<Mutex<CrossSubnetMailbox>>,
    pub mission_board: Arc<Mutex<MissionBoard>>,
    pub public_identity_registry: Arc<Mutex<PublicIdentityRegistry>>,
    pub controller_binding_registry: Arc<Mutex<ControllerBindingRegistry>>,
    pub citizen_registry: Arc<Mutex<CitizenRegistry>>,
    pub relationship_registry: Arc<Mutex<RelationshipRegistry>>,
    pub organization_registry: Arc<Mutex<OrganizationRegistry>>,
    pub hive_registry: Arc<Mutex<HiveRegistry>>,
    pub payment_ledger: Arc<Mutex<PaymentLedger>>,
    pub galaxy_state: Arc<Mutex<GalaxyState>>,
    pub galaxy_map_registry: Arc<Mutex<GalaxyMapRegistry>>,
    pub travel_state_registry: Arc<Mutex<TravelStateRegistry>>,
    pub brain_engine: Arc<RwLock<BrainEngine>>,
    pub brain_config: Arc<RwLock<BrainProviderConfig>>,
    pub brain_provider_label: String,
    pub audit_log: AuditLog,
    pub local_db: Arc<LocalDb>,
    pub social_store: Arc<SocialStore>,
    pub servicenet_client: Option<Arc<ServiceNetClient>>,
    pub agent_executor_base_url: Option<String>,
    pub agent_event_callback_base_url: Option<String>,
    pub agent_topic_bridge_enabled: bool,
    pub rate_limiter: Arc<RateLimiter>,
    pub stream_tx: broadcast::Sender<StreamEvent>,
    pub gateway_event_seq: Arc<GatewayEventSequence>,
    pub geo_location: Arc<NodeGeoLocation>,
}

impl ControlPlaneState {
    pub fn sign_payload(&self, payload: &impl Serialize) -> anyhow::Result<String> {
        sign_payload_with(payload, self.signer.as_ref())
    }

    pub fn append_signed_event(
        &self,
        event_type: impl Into<String>,
        payload: Value,
    ) -> anyhow::Result<EventRecord> {
        self.event_log
            .append_signed_with_signer(event_type, payload, self.signer.as_ref())
    }

    #[must_use]
    pub fn next_gateway_event_seq(&self) -> u64 {
        self.gateway_event_seq.next()
    }
}

#[derive(Debug)]
pub struct GatewayEventSequence {
    path: PathBuf,
    next_seq: StdMutex<u64>,
}

impl GatewayEventSequence {
    const DEFAULT_STATE_FILE: &str = "last_seq.json";

    #[must_use]
    pub fn load_or_seed(data_dir: &std::path::Path) -> Arc<Self> {
        let path = data_dir.join("gateway").join(Self::DEFAULT_STATE_FILE);
        let seed = read_persisted_gateway_seq(&path)
            .and_then(|last_seq| last_seq.checked_add(1))
            .unwrap_or_else(|| Utc::now().timestamp_millis().max(0).cast_unsigned());
        Arc::new(Self {
            path,
            next_seq: StdMutex::new(seed),
        })
    }

    #[must_use]
    pub fn next(&self) -> u64 {
        let mut guard = self
            .next_seq
            .lock()
            .expect("gateway event sequence mutex poisoned");
        let current = *guard;
        *guard = guard.saturating_add(1);
        if let Err(error) = persist_gateway_seq(&self.path, current) {
            warn!(
                path = %self.path.display(),
                seq = current,
                "persist gateway event sequence failed: {error:#}"
            );
        }
        current
    }
}

fn read_persisted_gateway_seq(path: &std::path::Path) -> Option<u64> {
    let raw = fs::read_to_string(path).ok()?;
    serde_json::from_str::<PersistedGatewayEventSequence>(&raw)
        .ok()
        .map(|persisted| persisted.last_seq)
}

fn persist_gateway_seq(path: &std::path::Path, last_seq: u64) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let payload = serde_json::to_vec_pretty(&PersistedGatewayEventSequence { last_seq })?;
    fs::write(path, payload)?;
    Ok(())
}

#[derive(Debug, Serialize, Deserialize)]
struct PersistedGatewayEventSequence {
    last_seq: u64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct NodeGeoLocation {
    pub lat: f64,
    pub lng: f64,
    pub source: GeoSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum GeoSource {
    #[serde(rename = "ip_api")]
    IpApi,
    #[serde(rename = "cached")]
    Cached,
    #[serde(rename = "derived")]
    Derived,
}

impl NodeGeoLocation {
    pub async fn load_or_fetch(_data_dir: &std::path::Path, fallback_id: &str) -> Arc<Self> {
        match fetch_geo_from_ip_api().await {
            Ok(geo) => {
                info!(
                    lat = geo.lat,
                    lng = geo.lng,
                    "resolved geo location via ip-api.com"
                );
                Arc::new(geo)
            }
            Err(error) => {
                warn!("ip-api.com geo lookup failed, using derived fallback: {error:#}");
                let (lat, lng) = super::routes::network::derived_geo(fallback_id);
                Arc::new(Self {
                    lat,
                    lng,
                    source: GeoSource::Derived,
                })
            }
        }
    }

    pub fn load_or_fetch_blocking(_data_dir: &std::path::Path, fallback_id: &str) -> Arc<Self> {
        match fetch_geo_from_ip_api_blocking() {
            Ok(geo) => {
                info!(
                    lat = geo.lat,
                    lng = geo.lng,
                    "resolved geo location via ip-api.com"
                );
                Arc::new(geo)
            }
            Err(error) => {
                warn!("ip-api.com geo lookup failed, using derived fallback: {error:#}");
                let (lat, lng) = super::routes::network::derived_geo(fallback_id);
                Arc::new(Self {
                    lat,
                    lng,
                    source: GeoSource::Derived,
                })
            }
        }
    }
}

#[derive(Debug, Deserialize)]
struct IpApiResponse {
    status: String,
    lat: Option<f64>,
    lon: Option<f64>,
}

async fn fetch_geo_from_ip_api() -> anyhow::Result<NodeGeoLocation> {
    let response: IpApiResponse = reqwest::get("http://ip-api.com/json/?fields=status,lat,lon")
        .await
        .context("ip-api.com request failed")?
        .json()
        .await
        .context("ip-api.com response parse failed")?;

    if response.status != "success" {
        anyhow::bail!("ip-api.com returned status: {}", response.status);
    }

    let lat = response.lat.context("ip-api.com missing lat")?;
    let lon = response.lon.context("ip-api.com missing lon")?;

    Ok(NodeGeoLocation {
        lat,
        lng: lon,
        source: GeoSource::IpApi,
    })
}

fn fetch_geo_from_ip_api_blocking() -> anyhow::Result<NodeGeoLocation> {
    let response: IpApiResponse =
        reqwest::blocking::get("http://ip-api.com/json/?fields=status,lat,lon")
            .context("ip-api.com request failed")?
            .json()
            .context("ip-api.com response parse failed")?;

    if response.status != "success" {
        anyhow::bail!("ip-api.com returned status: {}", response.status);
    }

    let lat = response.lat.context("ip-api.com missing lat")?;
    let lon = response.lon.context("ip-api.com missing lon")?;

    Ok(NodeGeoLocation {
        lat,
        lng: lon,
        source: GeoSource::IpApi,
    })
}

use anyhow::Context as _;

#[derive(Debug, Deserialize)]
pub(crate) struct EventsQuery {
    pub(crate) since: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct EventsExportQuery {
    pub(crate) since: Option<i64>,
    pub(crate) limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct NightShiftQuery {
    pub(crate) hours: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AuditQuery {
    pub(crate) limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct AuthQuery {
    pub(crate) token: String,
}

#[derive(Debug, Deserialize)]
pub struct ActionRequest {
    pub action: String,
}

#[derive(Debug, Deserialize)]
pub struct PolicyCheckBody {
    pub subject: String,
    pub trust: wattetheria_kernel::capabilities::TrustLevel,
    pub capability: String,
    pub reason: Option<String>,
    pub input_digest: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct PolicyApproveBody {
    pub request_id: String,
    pub approved_by: String,
    pub scope: GrantScope,
}

#[derive(Debug, Deserialize)]
pub struct PolicyRevokeBody {
    pub grant_id: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct GovernanceProposalsQuery {
    pub(crate) subnet_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ProposalCreateBody {
    pub subnet_id: String,
    pub kind: String,
    pub payload: Value,
    pub created_by: String,
}

#[derive(Debug, Deserialize)]
pub struct ProposalVoteBody {
    pub proposal_id: String,
    pub voter: String,
    pub approve: bool,
}

#[derive(Debug, Deserialize)]
pub struct ProposalFinalizeBody {
    pub proposal_id: String,
    pub min_votes_for: usize,
}

#[derive(Debug, Deserialize)]
pub struct GovernanceTreasuryBody {
    pub subnet_id: String,
    pub amount_watt: i64,
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct GovernanceStabilityBody {
    pub subnet_id: String,
    pub delta: i64,
}

#[derive(Debug, Deserialize)]
pub struct GovernanceRecallBody {
    pub subnet_id: String,
    pub initiated_by: String,
    pub reason: String,
    pub threshold: i64,
}

#[derive(Debug, Deserialize)]
pub struct GovernanceSuccessorBody {
    pub subnet_id: String,
    pub successor: String,
    pub min_bond: i64,
}

#[derive(Debug, Deserialize)]
pub struct GovernanceCustodyBody {
    pub subnet_id: String,
    pub reason: String,
    pub managed_by: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct GovernanceCustodyReleaseBody {
    pub subnet_id: String,
    pub successor: Option<String>,
    pub min_bond: i64,
}

#[derive(Debug, Deserialize)]
pub struct GovernanceTakeoverBody {
    pub subnet_id: String,
    pub challenger: String,
    pub reason: String,
    pub min_bond: i64,
}

#[derive(Debug, Deserialize)]
pub struct MailboxSendBody {
    pub to_agent: String,
    pub from_subnet: String,
    pub to_subnet: String,
    pub payload: Value,
}

#[derive(Debug, Deserialize)]
pub struct MailboxFetchQuery {
    pub subnet_id: String,
}

#[derive(Debug, Deserialize)]
pub struct MailboxAckBody {
    pub subnet_id: String,
    pub message_id: String,
}

#[derive(Debug, Deserialize)]
pub struct TopicsQuery {
    pub network_id: Option<String>,
    pub organization_id: Option<String>,
    pub mission_id: Option<String>,
    pub projection_kind: Option<TopicProjectionKind>,
    pub include_inactive: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct TopicCreateBody {
    pub public_id: Option<String>,
    pub network_id: Option<String>,
    pub feed_key: String,
    pub scope_hint: String,
    pub display_name: String,
    pub summary: Option<String>,
    pub projection_kind: TopicProjectionKind,
    pub organization_id: Option<String>,
    pub mission_id: Option<String>,
    #[serde(default)]
    pub participant_public_ids: Vec<String>,
    pub why_this_exists: Option<String>,
    pub initial_message: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub struct TopicMessagesQuery {
    pub network_id: Option<String>,
    pub feed_key: String,
    pub scope_hint: String,
    pub limit: Option<usize>,
    pub before_created_at: Option<u64>,
    pub before_message_id: Option<String>,
    pub subscriber_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct HiveMessagesQuery {
    pub network_id: Option<String>,
    pub feed_key: Option<String>,
    pub scope_hint: Option<String>,
    pub limit: Option<usize>,
    pub before_created_at: Option<u64>,
    pub before_message_id: Option<String>,
    pub subscriber_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct HiveSubscriptionBody {
    pub public_id: Option<String>,
    pub network_id: Option<String>,
    pub feed_key: Option<String>,
    pub scope_hint: Option<String>,
    pub display_name: Option<String>,
    pub summary: Option<String>,
    pub projection_kind: Option<TopicProjectionKind>,
    pub organization_id: Option<String>,
    pub mission_id: Option<String>,
    pub why_this_exists: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct TopicMessageBody {
    pub public_id: Option<String>,
    pub network_id: Option<String>,
    pub feed_key: String,
    pub scope_hint: String,
    pub content: Value,
    pub reply_to_message_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct HiveMessageBody {
    pub public_id: Option<String>,
    pub network_id: Option<String>,
    pub feed_key: Option<String>,
    pub scope_hint: Option<String>,
    pub content: Value,
    pub reply_to_message_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AgentActionCommitBody {
    pub event: AgentActionCommitEvent,
    pub decision: AgentActionDecision,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct AgentActionCommitEvent {
    pub event_id: String,
    pub event_type: String,
    pub source_kind: String,
    #[serde(default)]
    pub source_node_id: Option<String>,
    #[serde(default)]
    pub target_agent_id: Option<String>,
    #[serde(default)]
    pub agent_envelope: Option<SwarmAgentEnvelope>,
    pub payload: Value,
    #[serde(default)]
    pub requires_commit: bool,
}

#[derive(Debug, Deserialize)]
pub struct AgentActionDecision {
    pub decision_id: String,
    pub action: String,
    pub route: String,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub payload: Value,
}

#[derive(Debug, Deserialize)]
pub struct NetworkPeersQuery {
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct ClientIdentityQuery {
    pub agent_did: Option<String>,
    pub public_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ClientListQuery {
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct ClientRpcLogsQuery {
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct ClientLeaderboardQuery {
    pub category: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct ClientExportQuery {
    pub agent_did: Option<String>,
    pub public_id: Option<String>,
    pub node_limit: Option<usize>,
    pub peer_limit: Option<usize>,
    pub task_limit: Option<usize>,
    pub organization_limit: Option<usize>,
    pub rpc_log_limit: Option<usize>,
    pub leaderboard_limit: Option<usize>,
    pub leaderboard_category: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AutonomyTickBody {
    pub hours: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct MissionsQuery {
    pub status: Option<MissionStatus>,
}

#[derive(Debug, Deserialize)]
pub struct MissionPublishBody {
    pub title: String,
    pub description: String,
    pub publisher: String,
    pub publisher_kind: MissionPublisherKind,
    pub domain: MissionDomain,
    pub subnet_id: Option<String>,
    pub zone_id: Option<String>,
    pub required_role: Option<RolePath>,
    pub required_faction: Option<Faction>,
    pub reward: MissionReward,
    pub payload: Value,
    #[serde(default)]
    pub settlement_delegation: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub struct MissionClaimBody {
    pub mission_id: String,
    pub agent_did: String,
    pub task_id: Option<String>,
    pub mission_feed_key: Option<String>,
    pub mission_scope_hint: Option<String>,
    pub publisher_wattswarm_node_id: Option<String>,
    pub claim_route: Option<Value>,
    pub result: Option<Value>,
}

impl MissionClaimBody {
    pub fn local(mission_id: String, agent_did: String) -> Self {
        Self {
            mission_id,
            agent_did,
            task_id: None,
            mission_feed_key: None,
            mission_scope_hint: None,
            publisher_wattswarm_node_id: None,
            claim_route: None,
            result: None,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct MissionSettleBody {
    pub mission_id: String,
    pub task_id: Option<String>,
    pub agent_did: Option<String>,
    pub candidate_id: Option<String>,
    #[serde(default)]
    pub claim_route: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub struct CitizenProfileQuery {
    pub agent_did: Option<String>,
    pub public_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct PublicIdentityQuery {
    pub public_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ControllerBindingQuery {
    pub public_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct PublicIdentityBody {
    pub public_id: String,
    pub display_name: String,
    pub agent_did: Option<String>,
    pub active: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct PublicIdentityDisplayNameBody {
    pub public_id: String,
    pub display_name: String,
}

#[derive(Debug, Deserialize)]
pub struct ControllerBindingBody {
    pub public_id: String,
    pub controller_kind: ControllerKind,
    pub controller_ref: String,
    pub controller_node_id: Option<String>,
    pub ownership_scope: OwnershipScope,
    pub active: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct CitizenProfileBody {
    pub agent_did: String,
    pub faction: Faction,
    pub role: RolePath,
    pub strategy: StrategyProfile,
    pub home_subnet_id: Option<String>,
    pub home_zone_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RelationshipQuery {
    pub public_id: Option<String>,
    pub counterpart_public_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct RelationshipBody {
    pub public_id: Option<String>,
    pub counterpart_public_id: String,
    pub kind: RelationshipKind,
    pub active: bool,
}

#[derive(Debug, Deserialize)]
pub struct AgentRelationshipActionBody {
    pub public_id: Option<String>,
    #[serde(default)]
    pub counterpart_public_id: Option<String>,
    #[serde(default)]
    pub remote_node_id: Option<String>,
    #[serde(default)]
    pub target_agent_did: Option<String>,
    pub action: SwarmRelationshipAction,
    #[serde(default)]
    pub message: Option<Value>,
    #[serde(default)]
    pub extensions: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub struct AgentDmThreadsQuery {
    pub public_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AgentDmMessagesQuery {
    pub public_id: Option<String>,
    #[serde(rename = "counterpart_public_id")]
    pub counterpart: Option<String>,
    #[serde(rename = "thread_id")]
    pub thread: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct AgentDmSendBody {
    pub public_id: Option<String>,
    pub counterpart_public_id: String,
    pub content: Value,
    #[serde(default)]
    pub extensions: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub struct AgentPaymentsQuery {
    pub public_id: Option<String>,
    pub counterpart_public_id: Option<String>,
    pub status: Option<wattetheria_kernel::payments::PaymentStatus>,
    pub role: Option<String>,
    pub rail: Option<String>,
    pub limit: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct AgentPaymentProposeBody {
    pub public_id: Option<String>,
    #[serde(default)]
    pub counterpart_public_id: Option<String>,
    #[serde(default)]
    pub agent_id: Option<String>,
    pub amount: String,
    pub currency: String,
    pub rail: String,
    #[serde(default)]
    pub layer: wattetheria_kernel::payments::SettlementLayer,
    #[serde(default)]
    pub network: Option<String>,
    #[serde(default)]
    pub recipient_address: Option<String>,
    #[serde(default)]
    pub mission_id: Option<String>,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub metadata: Option<Value>,
    #[serde(default)]
    pub expires_at: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct AgentPaymentAuthorizeBody {
    #[serde(default)]
    pub sender_address: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct AgentPaymentSubmitBody {
    #[serde(default)]
    pub settlement_receipt: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub struct AgentPaymentSettleBody {
    pub settlement_receipt: Value,
}

#[derive(Debug, Deserialize)]
pub struct AgentPaymentRejectBody {
    pub reject_reason: String,
}

#[derive(Debug, Deserialize)]
pub struct WalletBindWeb3PaymentAccountBody {
    pub address: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub rail: Option<String>,
    #[serde(default)]
    pub network: Option<String>,
    #[serde(default)]
    pub chain_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct WalletCreatePaymentAccountBody {
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub rail: Option<String>,
    #[serde(default)]
    pub network: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct MetricsQuery {
    pub agent_did: Option<String>,
    pub public_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct GalaxyEventsQuery {
    pub zone_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct GalaxyMapQuery {
    pub map_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct GalaxyTravelOptionsQuery {
    #[serde(rename = "map_id")]
    pub map: Option<String>,
    #[serde(rename = "public_id")]
    pub public_identity: Option<String>,
    #[serde(rename = "agent_did")]
    pub controller: Option<String>,
    #[serde(rename = "from_system_id")]
    pub from_system: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct GalaxyTravelPlanQuery {
    #[serde(rename = "map_id")]
    pub map: Option<String>,
    #[serde(rename = "public_id")]
    pub public_identity: Option<String>,
    #[serde(rename = "agent_did")]
    pub controller: Option<String>,
    #[serde(rename = "from_system_id")]
    pub from_system: Option<String>,
    #[serde(rename = "to_system_id")]
    pub destination: String,
}

#[derive(Debug, Deserialize)]
pub struct GalaxyTravelStateQuery {
    #[serde(rename = "public_id")]
    pub public_identity: Option<String>,
    #[serde(rename = "agent_did")]
    pub controller: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct GalaxyTravelDepartBody {
    #[serde(rename = "map_id")]
    pub map: Option<String>,
    #[serde(rename = "public_id")]
    pub public_identity: Option<String>,
    #[serde(rename = "agent_did")]
    pub controller: Option<String>,
    #[serde(rename = "to_system_id")]
    pub destination: String,
}

#[derive(Debug, Deserialize)]
pub struct GalaxyTravelArriveBody {
    #[serde(rename = "public_id")]
    pub public_identity: Option<String>,
    #[serde(rename = "agent_did")]
    pub controller: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct EmergencyQuery {
    pub agent_did: Option<String>,
    pub public_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct BootstrapIdentityBody {
    pub public_id: Option<String>,
    pub display_name: String,
    pub agent_did: Option<String>,
    pub faction: Option<Faction>,
    pub role: Option<RolePath>,
    pub strategy: Option<StrategyProfile>,
    pub home_subnet_id: Option<String>,
    pub home_zone_id: Option<String>,
    pub controller_kind: Option<ControllerKind>,
    pub controller_ref: Option<String>,
    pub controller_node_id: Option<String>,
    pub ownership_scope: Option<OwnershipScope>,
    pub active: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct OrganizationsQuery {
    pub agent_did: Option<String>,
    pub public_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct OrganizationCreateBody {
    pub public_id: Option<String>,
    pub organization_id: String,
    pub name: String,
    pub kind: OrganizationKind,
    pub summary: String,
    pub faction_alignment: Option<Faction>,
    pub home_subnet_id: Option<String>,
    pub home_zone_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct OrganizationMemberBody {
    pub organization_id: String,
    pub actor_public_id: Option<String>,
    pub public_id: String,
    pub role: OrganizationRole,
    pub title: Option<String>,
    pub active: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct OrganizationTreasuryBody {
    pub organization_id: String,
    pub actor_public_id: Option<String>,
    pub amount_watt: i64,
    pub reason: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct OrganizationMissionPublishBody {
    pub organization_id: String,
    pub actor_public_id: Option<String>,
    pub title: String,
    pub description: String,
    pub domain: MissionDomain,
    pub subnet_id: Option<String>,
    pub zone_id: Option<String>,
    pub required_role: Option<RolePath>,
    pub required_faction: Option<Faction>,
    pub reward: MissionReward,
    pub treasury_commit_watt: Option<i64>,
    pub payload: Value,
}

#[derive(Debug, Deserialize)]
pub struct OrganizationProposalsQuery {
    pub organization_id: String,
    pub public_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct OrganizationProposalCreateBody {
    pub organization_id: String,
    pub actor_public_id: Option<String>,
    pub kind: OrganizationProposalKind,
    pub title: String,
    pub summary: String,
    pub proposed_subnet_id: Option<String>,
    pub proposed_subnet_name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct OrganizationProposalVoteBody {
    pub proposal_id: String,
    pub actor_public_id: Option<String>,
    pub approve: bool,
}

#[derive(Debug, Deserialize)]
pub struct OrganizationProposalFinalizeBody {
    pub proposal_id: String,
    pub actor_public_id: Option<String>,
    pub min_votes_for: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct OrganizationCharterApplicationBody {
    pub proposal_id: String,
    pub actor_public_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct GalaxyEventBody {
    pub category: DynamicEventCategory,
    pub zone_id: String,
    pub title: String,
    pub description: String,
    pub severity: u8,
    pub expires_at: Option<i64>,
    pub tags: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct GalaxyGenerateBody {
    pub max_events: Option<usize>,
}

#[derive(Debug, Deserialize)]
pub struct DashboardHomeQuery {
    pub agent_did: Option<String>,
    pub public_id: Option<String>,
    pub hours: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct MyMissionsQuery {
    pub agent_did: Option<String>,
    pub public_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct MyGovernanceQuery {
    pub agent_did: Option<String>,
    pub public_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct MyOrganizationsQuery {
    pub agent_did: Option<String>,
    pub public_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct BootstrapCatalogQuery {}

#[derive(Debug, Deserialize)]
pub struct GameStatusQuery {
    pub agent_did: Option<String>,
    pub public_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct GameActionBody {
    pub agent_did: Option<String>,
    pub public_id: Option<String>,
}

pub(crate) async fn send_stream_text(
    socket: &mut axum::extract::ws::WebSocket,
    payload: String,
) -> bool {
    socket.send(Message::Text(payload.into())).await.is_ok()
}

#[cfg(test)]
mod tests {
    use super::GatewayEventSequence;

    #[test]
    fn gateway_event_sequence_resumes_from_persisted_last_seq() {
        let dir = tempfile::tempdir().unwrap();
        let sequence = GatewayEventSequence::load_or_seed(dir.path());
        let first = sequence.next();
        let second = sequence.next();
        assert_eq!(second, first + 1);

        let reloaded = GatewayEventSequence::load_or_seed(dir.path());
        assert_eq!(reloaded.next(), second + 1);
    }
}
