use axum::extract::ws::Message;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{Mutex, broadcast};
use wattetheria_kernel::audit::AuditLog;
use wattetheria_kernel::brain::BrainEngine;
use wattetheria_kernel::civilization::identities::{
    ControllerBindingRegistry, ControllerKind, OwnershipScope, PublicIdentityRegistry,
};
use wattetheria_kernel::civilization::missions::{
    MissionBoard, MissionDomain, MissionPublisherKind, MissionReward, MissionStatus,
};
use wattetheria_kernel::civilization::profiles::{
    CitizenRegistry, Faction, RolePath, StrategyProfile,
};
use wattetheria_kernel::civilization::world::{DynamicEventCategory, WorldState};
use wattetheria_kernel::event_log::EventLog;
use wattetheria_kernel::governance::GovernanceEngine;
use wattetheria_kernel::identity::Identity;
use wattetheria_kernel::mailbox::CrossSubnetMailbox;
use wattetheria_kernel::policy_engine::{GrantScope, PolicyEngine};
use wattetheria_kernel::swarm_bridge::SwarmBridge;

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

#[derive(Clone)]
pub struct ControlPlaneState {
    pub agent_id: String,
    pub identity: Identity,
    pub started_at: i64,
    pub auth_token: String,
    pub event_log: EventLog,
    pub swarm_bridge: Arc<dyn SwarmBridge>,
    pub governance_engine: Arc<Mutex<GovernanceEngine>>,
    pub governance_state_path: PathBuf,
    pub policy_engine: Arc<Mutex<PolicyEngine>>,
    pub mailbox: Arc<Mutex<CrossSubnetMailbox>>,
    pub mailbox_state_path: PathBuf,
    pub mission_board: Arc<Mutex<MissionBoard>>,
    pub mission_board_state_path: PathBuf,
    pub public_identity_registry: Arc<Mutex<PublicIdentityRegistry>>,
    pub public_identity_registry_state_path: PathBuf,
    pub controller_binding_registry: Arc<Mutex<ControllerBindingRegistry>>,
    pub controller_binding_registry_state_path: PathBuf,
    pub citizen_registry: Arc<Mutex<CitizenRegistry>>,
    pub citizen_registry_state_path: PathBuf,
    pub world_state: Arc<Mutex<WorldState>>,
    pub world_state_path: PathBuf,
    pub brain_engine: Arc<BrainEngine>,
    pub audit_log: AuditLog,
    pub rate_limiter: Arc<RateLimiter>,
    pub stream_tx: broadcast::Sender<StreamEvent>,
}

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
}

#[derive(Debug, Deserialize)]
pub struct MissionClaimBody {
    pub mission_id: String,
    pub agent_id: String,
}

#[derive(Debug, Deserialize)]
pub struct MissionSettleBody {
    pub mission_id: String,
}

#[derive(Debug, Deserialize)]
pub struct CitizenProfileQuery {
    pub agent_id: Option<String>,
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
    pub legacy_agent_id: Option<String>,
    pub active: Option<bool>,
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
    pub agent_id: String,
    pub faction: Faction,
    pub role: RolePath,
    pub strategy: StrategyProfile,
    pub home_subnet_id: Option<String>,
    pub home_zone_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct MetricsQuery {
    pub agent_id: Option<String>,
    pub public_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct WorldEventsQuery {
    pub zone_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct EmergencyQuery {
    pub agent_id: Option<String>,
    pub public_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CharacterBootstrapBody {
    pub public_id: String,
    pub display_name: String,
    pub legacy_agent_id: Option<String>,
    pub faction: Faction,
    pub role: RolePath,
    pub strategy: StrategyProfile,
    pub home_subnet_id: Option<String>,
    pub home_zone_id: Option<String>,
    pub controller_kind: Option<ControllerKind>,
    pub controller_ref: Option<String>,
    pub controller_node_id: Option<String>,
    pub ownership_scope: Option<OwnershipScope>,
    pub active: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct WorldEventBody {
    pub category: DynamicEventCategory,
    pub zone_id: String,
    pub title: String,
    pub description: String,
    pub severity: u8,
    pub expires_at: Option<i64>,
    pub tags: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct WorldGenerateBody {
    pub max_events: Option<usize>,
}

pub(crate) async fn send_stream_text(
    socket: &mut axum::extract::ws::WebSocket,
    payload: String,
) -> bool {
    socket.send(Message::Text(payload.into())).await.is_ok()
}
