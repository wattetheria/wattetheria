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
    pub brain_engine: Arc<BrainEngine>,
    pub autonomy_skill_planner_enabled: bool,
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
pub(crate) struct BrainPlansQuery {
    pub(crate) enable: Option<bool>,
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
    pub enable_skill_planner: Option<bool>,
}

pub(crate) async fn send_stream_text(
    socket: &mut axum::extract::ws::WebSocket,
    payload: String,
) -> bool {
    socket.send(Message::Text(payload.into())).await.is_ok()
}
