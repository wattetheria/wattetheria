//! Bridge layer that keeps wattetheria app flows independent from wattswarm transport details.

use crate::civilization::missions::{MissionBoard, MissionStatus};
use crate::swarm_sync::{
    SwarmKnowledgeExportSnapshot, SwarmRunEventsSnapshot, SwarmRunResultSnapshot,
    SwarmTaskDecisionSnapshot, SwarmTaskRunProjectionSnapshot, SwarmTopicActivitySnapshot,
};
use crate::types::AgentStats;
use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use wattswarm_protocol::types::{ArtifactRef, ClaimRole, InlineEvidence, TaskContract};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SwarmAgentView {
    pub agent_did: String,
    pub stats: AgentStats,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SwarmTopicMessageView {
    pub message_id: String,
    pub network_id: String,
    pub feed_key: String,
    pub scope_hint: String,
    pub author_node_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_envelope: Option<SwarmAgentEnvelope>,
    pub content: Value,
    pub reply_to_message_id: Option<String>,
    pub created_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SwarmTopicCursorView {
    pub subscriber_node_id: String,
    pub feed_key: String,
    pub scope_hint: String,
    pub last_event_seq: u64,
    pub updated_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SwarmPeerView {
    pub node_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connected: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recently_seen: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stale: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_seen_age_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discovery: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relationship: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SwarmNetworkStatusView {
    pub running: bool,
    pub mode: String,
    pub peer_protocol_distribution: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SwarmDiagnosticsQuery {
    pub limit: Option<usize>,
    pub level: Option<String>,
    pub component: Option<String>,
    pub category: Option<String>,
    pub phase: Option<String>,
    pub event_id: Option<String>,
    pub object_id: Option<String>,
    pub source_node_id: Option<String>,
    pub search: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SwarmDiagnosticsSnapshot {
    #[serde(default)]
    pub ok: bool,
    pub generated_at: String,
    #[serde(default)]
    pub network_service_started: bool,
    #[serde(default)]
    pub snapshot: Option<Value>,
    #[serde(default)]
    pub diagnostics: Vec<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SwarmRelationshipAction {
    Request,
    Accept,
    Reject,
    Cancel,
    Remove,
    Block,
    Unblock,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SwarmSourceAgentCard {
    pub agent_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_id: Option<String>,
    pub card_hash: String,
    pub issued_at: u64,
    pub card: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SwarmAgentEnvelope {
    pub protocol: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport_profile: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_node_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_node_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_agent_card: Option<SwarmSourceAgentCard>,
    #[serde(
        default,
        alias = "message_json",
        deserialize_with = "deserialize_envelope_json"
    )]
    pub message: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(
        alias = "extensions_json",
        deserialize_with = "deserialize_optional_envelope_json"
    )]
    pub extensions: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
}

fn deserialize_envelope_json<'de, D>(deserializer: D) -> std::result::Result<Value, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<Value>::deserialize(deserializer)?;
    match value {
        Some(Value::String(raw)) if raw.trim().is_empty() => Ok(Value::Null),
        Some(Value::String(raw)) => serde_json::from_str(&raw).map_err(serde::de::Error::custom),
        Some(value) => Ok(value),
        None => Ok(Value::Null),
    }
}

fn deserialize_optional_envelope_json<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<Value>, D::Error>
where
    D: Deserializer<'de>,
{
    let value = Option::<Value>::deserialize(deserializer)?;
    match value {
        Some(Value::Null) | None => Ok(None),
        Some(Value::String(raw)) if raw.trim().is_empty() => Ok(None),
        Some(Value::String(raw)) => serde_json::from_str(&raw)
            .map(Some)
            .map_err(serde::de::Error::custom),
        Some(value) => Ok(Some(value)),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SwarmRunSubmitCommand {
    pub spec: Value,
    pub kickoff: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SwarmPeerRelationshipView {
    pub remote_node_id: String,
    pub relationship_state: String,
    pub last_action: String,
    pub initiated_by: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_envelope: Option<SwarmAgentEnvelope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub responded_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleared_at: Option<u64>,
    pub updated_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SwarmPeerDmThreadView {
    pub remote_node_id: String,
    pub thread_id: String,
    pub thread_kind: String,
    pub session_state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relationship_established_at: Option<u64>,
    pub created_at: u64,
    pub updated_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_message_at: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SwarmPeerDmMessageView {
    pub thread_id: String,
    pub message_id: String,
    pub remote_node_id: String,
    pub message_kind: String,
    pub direction: String,
    pub delivery_state: String,
    pub a2a_protocol: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_envelope: Option<SwarmAgentEnvelope>,
    pub content: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encrypted_body: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_encoding: Option<String>,
    pub created_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acknowledged_at: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SwarmRelationshipActionCommand {
    pub remote_node_id: String,
    pub action: SwarmRelationshipAction,
    pub agent_envelope: SwarmAgentEnvelope,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SwarmDirectMessageCommand {
    pub remote_node_id: String,
    pub agent_envelope: SwarmAgentEnvelope,
    pub content: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SwarmAgentPaymentCommand {
    pub remote_node_id: String,
    pub message_kind: String,
    pub payment: Value,
    pub agent_envelope: SwarmAgentEnvelope,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SwarmTaskAnnounceCommand {
    pub task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub announcement_id: Option<String>,
    pub feed_key: String,
    pub scope_hint: String,
    pub summary: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail_ref: Option<ArtifactRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_envelope: Option<SwarmAgentEnvelope>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SwarmTaskClaimCommand {
    pub task_id: String,
    pub role: ClaimRole,
    pub execution_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_ms: Option<u64>,
    pub agent_envelope: SwarmAgentEnvelope,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SwarmTaskClaimDecisionCommand {
    pub task_id: String,
    pub execution_id: String,
    pub claimer_node_id: String,
    pub approved: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub agent_envelope: SwarmAgentEnvelope,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SwarmTaskCompleteCommand {
    pub task_id: String,
    pub execution_id: String,
    pub output: Value,
    pub agent_envelope: SwarmAgentEnvelope,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SwarmTaskCompletionDecisionCommand {
    pub task_id: String,
    pub execution_id: String,
    pub approved: bool,
    #[serde(default)]
    pub retry_requested: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub agent_envelope: SwarmAgentEnvelope,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SwarmTaskSettleCommand {
    pub task_id: String,
    pub execution_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt: Option<Value>,
    pub agent_envelope: SwarmAgentEnvelope,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SwarmTaskProposeCandidateCommand {
    pub task_id: String,
    pub execution_id: String,
    pub candidate_id: String,
    pub output: Value,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_inline: Vec<InlineEvidence>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_refs: Vec<ArtifactRef>,
    pub agent_envelope: SwarmAgentEnvelope,
}

#[async_trait]
pub trait SwarmBridge: Send + Sync {
    async fn agent_view(&self, agent_did: &str) -> Result<SwarmAgentView>;

    async fn subscribe_topic(
        &self,
        _network_id: Option<&str>,
        _subscriber_id: &str,
        _feed_key: &str,
        _scope_hint: &str,
        _active: bool,
        _agent_envelope: Option<SwarmAgentEnvelope>,
    ) -> Result<()> {
        Err(anyhow!("wattswarm topic subscriptions are not configured"))
    }

    async fn post_topic_message(
        &self,
        _network_id: Option<&str>,
        _feed_key: &str,
        _scope_hint: &str,
        _content: Value,
        _reply_to_message_id: Option<String>,
        _agent_envelope: Option<SwarmAgentEnvelope>,
    ) -> Result<()> {
        Err(anyhow!("wattswarm topic messages are not configured"))
    }

    async fn list_topic_messages(
        &self,
        _network_id: Option<&str>,
        _feed_key: &str,
        _scope_hint: &str,
        _limit: usize,
        _before_created_at: Option<u64>,
        _before_message_id: Option<String>,
    ) -> Result<Vec<SwarmTopicMessageView>> {
        Err(anyhow!("wattswarm topic history is not configured"))
    }

    async fn topic_cursor(
        &self,
        _network_id: Option<&str>,
        _feed_key: &str,
        _subscriber_id: Option<&str>,
    ) -> Result<Option<SwarmTopicCursorView>> {
        Err(anyhow!("wattswarm topic cursors are not configured"))
    }

    async fn network_status(&self) -> Result<SwarmNetworkStatusView> {
        Err(anyhow!("wattswarm network status is not configured"))
    }

    async fn current_network_id(&self) -> Result<String> {
        Err(anyhow!("wattswarm current network ID is not configured"))
    }

    async fn local_node_id(&self) -> Result<String> {
        Err(anyhow!("wattswarm local node id is not configured"))
    }

    async fn peers(&self) -> Result<Vec<SwarmPeerView>> {
        Err(anyhow!("wattswarm peers are not configured"))
    }

    async fn diagnostics(&self, _query: SwarmDiagnosticsQuery) -> Result<SwarmDiagnosticsSnapshot> {
        Err(anyhow!("wattswarm diagnostics are not configured"))
    }

    async fn list_peer_relationships(&self) -> Result<Vec<SwarmPeerRelationshipView>> {
        Err(anyhow!("wattswarm peer relationships are not configured"))
    }

    async fn send_peer_relationship_action(
        &self,
        _command: SwarmRelationshipActionCommand,
    ) -> Result<Value> {
        Err(anyhow!("wattswarm peer relationships are not configured"))
    }

    async fn list_peer_dm_threads(&self) -> Result<Vec<SwarmPeerDmThreadView>> {
        Err(anyhow!(
            "wattswarm peer direct message threads are not configured"
        ))
    }

    async fn list_peer_dm_messages(&self, _thread_id: &str) -> Result<Vec<SwarmPeerDmMessageView>> {
        Err(anyhow!("wattswarm peer direct messages are not configured"))
    }

    async fn send_peer_direct_message(&self, _command: SwarmDirectMessageCommand) -> Result<Value> {
        Err(anyhow!("wattswarm peer direct messages are not configured"))
    }

    async fn publish_agent_payment_message(
        &self,
        _command: SwarmAgentPaymentCommand,
    ) -> Result<Value> {
        Err(anyhow!("wattswarm agent payments are not configured"))
    }

    async fn sample_task_contract(&self, _task_id: &str) -> Result<TaskContract> {
        Err(anyhow!("wattswarm task sample is not configured"))
    }

    async fn submit_task(&self, _contract: TaskContract) -> Result<Value> {
        Err(anyhow!("wattswarm task submit is not configured"))
    }

    async fn submit_run(&self, _command: SwarmRunSubmitCommand) -> Result<Value> {
        Err(anyhow!("wattswarm run submit is not configured"))
    }

    async fn import_task_contract(&self, _contract: TaskContract) -> Result<Value> {
        Err(anyhow!("wattswarm task contract import is not configured"))
    }

    async fn announce_task(&self, _command: SwarmTaskAnnounceCommand) -> Result<Value> {
        Err(anyhow!("wattswarm task announce is not configured"))
    }

    async fn claim_task(&self, _command: SwarmTaskClaimCommand) -> Result<Value> {
        Err(anyhow!("wattswarm task claim is not configured"))
    }

    async fn decide_task_claim(&self, _command: SwarmTaskClaimDecisionCommand) -> Result<Value> {
        Err(anyhow!("wattswarm task claim decision is not configured"))
    }

    async fn complete_task(&self, _command: SwarmTaskCompleteCommand) -> Result<Value> {
        Err(anyhow!("wattswarm task complete is not configured"))
    }

    async fn decide_task_completion(
        &self,
        _command: SwarmTaskCompletionDecisionCommand,
    ) -> Result<Value> {
        Err(anyhow!(
            "wattswarm task completion decision is not configured"
        ))
    }

    async fn settle_task(&self, _command: SwarmTaskSettleCommand) -> Result<Value> {
        Err(anyhow!("wattswarm task settle is not configured"))
    }

    async fn propose_task_candidate(
        &self,
        _command: SwarmTaskProposeCandidateCommand,
    ) -> Result<Value> {
        Err(anyhow!(
            "wattswarm task candidate proposal is not configured"
        ))
    }

    async fn accept_and_finalize_task(
        &self,
        _task_id: &str,
        _candidate_id: &str,
        _agent_envelope: Option<SwarmAgentEnvelope>,
    ) -> Result<Value> {
        Err(anyhow!("wattswarm task accept result is not configured"))
    }

    async fn task_run_projection(
        &self,
        _task_limit: usize,
        _run_limit: usize,
    ) -> Result<SwarmTaskRunProjectionSnapshot> {
        Err(anyhow!("wattswarm task/run projection is not configured"))
    }

    async fn task_decision_snapshot(&self, _task_id: &str) -> Result<SwarmTaskDecisionSnapshot> {
        Err(anyhow!(
            "wattswarm task decision snapshot is not configured"
        ))
    }

    async fn run_result_snapshot(&self, _run_id: &str) -> Result<SwarmRunResultSnapshot> {
        Err(anyhow!("wattswarm run result snapshot is not configured"))
    }

    async fn run_events_snapshot(
        &self,
        _run_id: &str,
        _limit: usize,
    ) -> Result<SwarmRunEventsSnapshot> {
        Err(anyhow!("wattswarm run events snapshot is not configured"))
    }

    async fn topic_activity_snapshot(
        &self,
        _network_id: Option<&str>,
        _feed_key: &str,
        _scope_hint: &str,
        _limit: usize,
        _subscriber_node_id: Option<&str>,
    ) -> Result<SwarmTopicActivitySnapshot> {
        Err(anyhow!(
            "wattswarm topic activity snapshot is not configured"
        ))
    }

    async fn knowledge_export_snapshot(
        &self,
        _task_type: Option<&str>,
        _task_id: Option<&str>,
    ) -> Result<SwarmKnowledgeExportSnapshot> {
        Err(anyhow!("wattswarm knowledge export is not configured"))
    }
}

pub struct HybridSwarmBridge {
    mission_board_path: PathBuf,
    topic_api: Option<HttpWattswarmApi>,
}

impl HybridSwarmBridge {
    #[must_use]
    pub fn new(mission_board_path: PathBuf, wattswarm_ui_base_url: Option<&str>) -> Self {
        Self {
            mission_board_path,
            topic_api: wattswarm_ui_base_url.map(HttpWattswarmApi::new),
        }
    }

    fn topic_api(&self) -> Result<&HttpWattswarmApi> {
        self.topic_api
            .as_ref()
            .ok_or_else(|| anyhow!("wattswarm UI base URL is not configured"))
    }
}

#[async_trait]
impl SwarmBridge for HybridSwarmBridge {
    async fn agent_view(&self, agent_did: &str) -> Result<SwarmAgentView> {
        Ok(SwarmAgentView {
            agent_did: agent_did.to_owned(),
            stats: load_agent_stats_from_mission_board(&self.mission_board_path, agent_did)?,
        })
    }

    async fn subscribe_topic(
        &self,
        network_id: Option<&str>,
        subscriber_id: &str,
        feed_key: &str,
        scope_hint: &str,
        active: bool,
        agent_envelope: Option<SwarmAgentEnvelope>,
    ) -> Result<()> {
        self.topic_api()?
            .subscribe_topic(
                network_id,
                subscriber_id,
                feed_key,
                scope_hint,
                active,
                agent_envelope,
            )
            .await
    }

    async fn post_topic_message(
        &self,
        network_id: Option<&str>,
        feed_key: &str,
        scope_hint: &str,
        content: Value,
        reply_to_message_id: Option<String>,
        agent_envelope: Option<SwarmAgentEnvelope>,
    ) -> Result<()> {
        self.topic_api()?
            .post_topic_message(
                network_id,
                feed_key,
                scope_hint,
                content,
                reply_to_message_id,
                agent_envelope,
            )
            .await
    }

    async fn list_topic_messages(
        &self,
        network_id: Option<&str>,
        feed_key: &str,
        scope_hint: &str,
        limit: usize,
        before_created_at: Option<u64>,
        before_message_id: Option<String>,
    ) -> Result<Vec<SwarmTopicMessageView>> {
        self.topic_api()?
            .list_topic_messages(
                network_id,
                feed_key,
                scope_hint,
                limit,
                before_created_at,
                before_message_id,
            )
            .await
    }

    async fn topic_cursor(
        &self,
        network_id: Option<&str>,
        feed_key: &str,
        subscriber_id: Option<&str>,
    ) -> Result<Option<SwarmTopicCursorView>> {
        self.topic_api()?
            .topic_cursor(network_id, feed_key, subscriber_id)
            .await
    }

    async fn network_status(&self) -> Result<SwarmNetworkStatusView> {
        self.topic_api()?.network_status().await
    }

    async fn current_network_id(&self) -> Result<String> {
        self.topic_api()?.current_network_id().await
    }

    async fn local_node_id(&self) -> Result<String> {
        self.topic_api()?.local_node_id().await
    }

    async fn peers(&self) -> Result<Vec<SwarmPeerView>> {
        self.topic_api()?.peers().await
    }

    async fn diagnostics(&self, query: SwarmDiagnosticsQuery) -> Result<SwarmDiagnosticsSnapshot> {
        self.topic_api()?.diagnostics(query).await
    }

    async fn list_peer_relationships(&self) -> Result<Vec<SwarmPeerRelationshipView>> {
        self.topic_api()?.list_peer_relationships().await
    }

    async fn send_peer_relationship_action(
        &self,
        command: SwarmRelationshipActionCommand,
    ) -> Result<Value> {
        self.topic_api()?
            .send_peer_relationship_action(command)
            .await
    }

    async fn list_peer_dm_threads(&self) -> Result<Vec<SwarmPeerDmThreadView>> {
        self.topic_api()?.list_peer_dm_threads().await
    }

    async fn list_peer_dm_messages(&self, thread_id: &str) -> Result<Vec<SwarmPeerDmMessageView>> {
        self.topic_api()?.list_peer_dm_messages(thread_id).await
    }

    async fn send_peer_direct_message(&self, command: SwarmDirectMessageCommand) -> Result<Value> {
        self.topic_api()?.send_peer_direct_message(command).await
    }

    async fn publish_agent_payment_message(
        &self,
        command: SwarmAgentPaymentCommand,
    ) -> Result<Value> {
        self.topic_api()?
            .publish_agent_payment_message(command)
            .await
    }

    async fn sample_task_contract(&self, task_id: &str) -> Result<TaskContract> {
        self.topic_api()?.sample_task_contract(task_id).await
    }

    async fn submit_task(&self, contract: TaskContract) -> Result<Value> {
        self.topic_api()?.submit_task(contract).await
    }

    async fn submit_run(&self, command: SwarmRunSubmitCommand) -> Result<Value> {
        self.topic_api()?.submit_run(command).await
    }

    async fn import_task_contract(&self, contract: TaskContract) -> Result<Value> {
        self.topic_api()?.import_task_contract(contract).await
    }

    async fn announce_task(&self, command: SwarmTaskAnnounceCommand) -> Result<Value> {
        self.topic_api()?.announce_task(command).await
    }

    async fn claim_task(&self, command: SwarmTaskClaimCommand) -> Result<Value> {
        self.topic_api()?.claim_task(command).await
    }

    async fn decide_task_claim(&self, command: SwarmTaskClaimDecisionCommand) -> Result<Value> {
        self.topic_api()?.decide_task_claim(command).await
    }

    async fn complete_task(&self, command: SwarmTaskCompleteCommand) -> Result<Value> {
        self.topic_api()?.complete_task(command).await
    }

    async fn decide_task_completion(
        &self,
        command: SwarmTaskCompletionDecisionCommand,
    ) -> Result<Value> {
        self.topic_api()?.decide_task_completion(command).await
    }

    async fn settle_task(&self, command: SwarmTaskSettleCommand) -> Result<Value> {
        self.topic_api()?.settle_task(command).await
    }

    async fn propose_task_candidate(
        &self,
        command: SwarmTaskProposeCandidateCommand,
    ) -> Result<Value> {
        self.topic_api()?.propose_task_candidate(command).await
    }

    async fn accept_and_finalize_task(
        &self,
        task_id: &str,
        candidate_id: &str,
        agent_envelope: Option<SwarmAgentEnvelope>,
    ) -> Result<Value> {
        self.topic_api()?
            .accept_and_finalize_task(task_id, candidate_id, agent_envelope)
            .await
    }

    async fn task_run_projection(
        &self,
        task_limit: usize,
        run_limit: usize,
    ) -> Result<SwarmTaskRunProjectionSnapshot> {
        self.topic_api()?
            .task_run_projection(task_limit, run_limit)
            .await
    }

    async fn task_decision_snapshot(&self, task_id: &str) -> Result<SwarmTaskDecisionSnapshot> {
        self.topic_api()?.task_decision_snapshot(task_id).await
    }

    async fn run_result_snapshot(&self, run_id: &str) -> Result<SwarmRunResultSnapshot> {
        self.topic_api()?.run_result_snapshot(run_id).await
    }

    async fn run_events_snapshot(
        &self,
        run_id: &str,
        limit: usize,
    ) -> Result<SwarmRunEventsSnapshot> {
        self.topic_api()?.run_events_snapshot(run_id, limit).await
    }

    async fn topic_activity_snapshot(
        &self,
        network_id: Option<&str>,
        feed_key: &str,
        scope_hint: &str,
        limit: usize,
        subscriber_node_id: Option<&str>,
    ) -> Result<SwarmTopicActivitySnapshot> {
        self.topic_api()?
            .topic_activity_snapshot(network_id, feed_key, scope_hint, limit, subscriber_node_id)
            .await
    }

    async fn knowledge_export_snapshot(
        &self,
        task_type: Option<&str>,
        task_id: Option<&str>,
    ) -> Result<SwarmKnowledgeExportSnapshot> {
        self.topic_api()?
            .knowledge_export_snapshot(task_type, task_id)
            .await
    }
}

struct HttpWattswarmApi {
    base_url: String,
    client: reqwest::Client,
}

impl HttpWattswarmApi {
    fn new(base_url: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_owned(),
            client: reqwest::Client::new(),
        }
    }

    async fn node_status_response(&self) -> Result<NodeStatusResponse> {
        self.client
            .get(format!("{}/api/node/status", self.base_url))
            .send()
            .await?
            .error_for_status()?
            .json::<NodeStatusResponse>()
            .await
            .context("decode wattswarm node status response")
    }

    async fn subscribe_topic(
        &self,
        network_id: Option<&str>,
        subscriber_id: &str,
        feed_key: &str,
        scope_hint: &str,
        active: bool,
        agent_envelope: Option<SwarmAgentEnvelope>,
    ) -> Result<()> {
        self.client
            .post(format!("{}/api/topic/subscriptions", self.base_url))
            .json(&json!({
                "network_id": network_id,
                "subscriber_node_id": subscriber_id,
                "feed_key": feed_key,
                "scope_hint": scope_hint,
                "active": active,
                "agent_envelope": agent_envelope,
            }))
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }

    async fn post_topic_message(
        &self,
        network_id: Option<&str>,
        feed_key: &str,
        scope_hint: &str,
        content: Value,
        reply_to_message_id: Option<String>,
        agent_envelope: Option<SwarmAgentEnvelope>,
    ) -> Result<()> {
        self.client
            .post(format!("{}/api/topic/messages", self.base_url))
            .json(&json!({
                "network_id": network_id,
                "feed_key": feed_key,
                "scope_hint": scope_hint,
                "content": content,
                "reply_to_message_id": reply_to_message_id,
                "agent_envelope": agent_envelope,
            }))
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }

    async fn list_topic_messages(
        &self,
        network_id: Option<&str>,
        feed_key: &str,
        scope_hint: &str,
        limit: usize,
        before_created_at: Option<u64>,
        before_message_id: Option<String>,
    ) -> Result<Vec<SwarmTopicMessageView>> {
        let response = self
            .client
            .get(format!("{}/api/topic/messages", self.base_url))
            .query(&TopicMessagesQuery {
                network_id: network_id.map(ToOwned::to_owned),
                feed_key: feed_key.to_owned(),
                scope_hint: scope_hint.to_owned(),
                limit,
                before_created_at,
                before_message_id,
            })
            .send()
            .await?
            .error_for_status()?
            .json::<TopicMessagesResponse>()
            .await?;
        Ok(response.messages)
    }

    async fn topic_cursor(
        &self,
        network_id: Option<&str>,
        feed_key: &str,
        subscriber_id: Option<&str>,
    ) -> Result<Option<SwarmTopicCursorView>> {
        let response = self
            .client
            .get(format!("{}/api/topic/cursor", self.base_url))
            .query(&TopicCursorQuery {
                network_id: network_id.map(ToOwned::to_owned),
                feed_key: feed_key.to_owned(),
                subscriber_node_id: subscriber_id.map(ToOwned::to_owned),
            })
            .send()
            .await?
            .error_for_status()?
            .json::<TopicCursorResponse>()
            .await?;
        Ok(response.cursor)
    }

    async fn network_status(&self) -> Result<SwarmNetworkStatusView> {
        let response = self.node_status_response().await?;
        Ok(SwarmNetworkStatusView {
            running: response.running,
            mode: response.mode,
            peer_protocol_distribution: response.peer_protocol_distribution,
        })
    }

    async fn current_network_id(&self) -> Result<String> {
        let response = self
            .client
            .get(format!(
                "{}/api/wattetheria/network/snapshot",
                self.base_url
            ))
            .send()
            .await?
            .error_for_status()?
            .json::<NetworkSnapshotResponse>()
            .await
            .context("decode wattswarm network snapshot")?;
        let network_id = response.network_id.trim();
        if network_id.is_empty() {
            return Err(anyhow!(
                "wattswarm network snapshot did not include network_id"
            ));
        }
        Ok(network_id.to_owned())
    }

    async fn local_node_id(&self) -> Result<String> {
        let response = self.node_status_response().await?;
        let node_id = response.node_id.trim();
        if node_id.is_empty() {
            return Err(anyhow!("wattswarm node status did not include node_id"));
        }
        Ok(node_id.to_owned())
    }

    async fn peers(&self) -> Result<Vec<SwarmPeerView>> {
        let response = self
            .client
            .get(format!("{}/api/peers/list", self.base_url))
            .send()
            .await?
            .error_for_status()?
            .json::<PeersListResponse>()
            .await?;
        Ok(wattswarm_peer_views(response))
    }

    async fn diagnostics(&self, query: SwarmDiagnosticsQuery) -> Result<SwarmDiagnosticsSnapshot> {
        self.client
            .get(format!("{}/api/diagnostics", self.base_url))
            .query(&query)
            .send()
            .await?
            .error_for_status()?
            .json::<SwarmDiagnosticsSnapshot>()
            .await
            .context("decode wattswarm diagnostics snapshot")
    }

    async fn list_peer_relationships(&self) -> Result<Vec<SwarmPeerRelationshipView>> {
        Ok(self
            .client
            .get(format!("{}/api/peers/relationships", self.base_url))
            .send()
            .await?
            .error_for_status()?
            .json::<PeerRelationshipsResponse>()
            .await?
            .relationships)
    }

    async fn send_peer_relationship_action(
        &self,
        command: SwarmRelationshipActionCommand,
    ) -> Result<Value> {
        self.client
            .post(format!("{}/api/peers/relationships", self.base_url))
            .json(&command)
            .send()
            .await?
            .error_for_status()?
            .json::<Value>()
            .await
            .context("decode wattswarm peer relationship action response")
    }

    async fn list_peer_dm_threads(&self) -> Result<Vec<SwarmPeerDmThreadView>> {
        Ok(self
            .client
            .get(format!("{}/api/peers/dm/threads", self.base_url))
            .send()
            .await?
            .error_for_status()?
            .json::<PeerDmThreadsResponse>()
            .await?
            .threads)
    }

    async fn list_peer_dm_messages(&self, thread_id: &str) -> Result<Vec<SwarmPeerDmMessageView>> {
        Ok(self
            .client
            .get(format!("{}/api/peers/dm/messages", self.base_url))
            .query(&PeerDmMessagesQuery {
                thread_id: thread_id.to_owned(),
            })
            .send()
            .await?
            .error_for_status()?
            .json::<PeerDmMessagesResponse>()
            .await?
            .messages)
    }

    async fn send_peer_direct_message(&self, command: SwarmDirectMessageCommand) -> Result<Value> {
        self.client
            .post(format!("{}/api/peers/dm/messages", self.base_url))
            .json(&command)
            .send()
            .await?
            .error_for_status()?
            .json::<Value>()
            .await
            .context("decode wattswarm peer direct message response")
    }

    async fn publish_agent_payment_message(
        &self,
        command: SwarmAgentPaymentCommand,
    ) -> Result<Value> {
        self.client
            .post(format!("{}/api/payments/messages", self.base_url))
            .json(&command)
            .send()
            .await?
            .error_for_status()?
            .json::<Value>()
            .await
            .context("decode wattswarm agent payment response")
    }

    async fn sample_task_contract(&self, task_id: &str) -> Result<TaskContract> {
        self.client
            .get(format!("{}/api/task/sample", self.base_url))
            .query(&TaskSampleQuery {
                task_id: task_id.to_owned(),
            })
            .send()
            .await?
            .error_for_status()?
            .json::<TaskSampleResponse>()
            .await
            .context("decode wattswarm task sample response")
            .map(|response| response.contract)
    }

    async fn submit_task(&self, contract: TaskContract) -> Result<Value> {
        let task_id = contract.task_id.clone();
        let response = self
            .client
            .post(format!("{}/api/task/submit", self.base_url))
            .json(&json!({ "contract": contract }))
            .send()
            .await?;
        let status = response.status();
        let body = response
            .text()
            .await
            .context("read wattswarm task submit response")?;
        if status.is_success() {
            return serde_json::from_str::<Value>(&body)
                .context("decode wattswarm task submit response");
        }
        if body.contains("task already exists") {
            return Ok(json!({
                "ok": true,
                "task_id": task_id,
                "already_exists": true,
            }));
        }
        Err(anyhow!(
            "wattswarm task submit failed with status {status}: {body}"
        ))
    }

    async fn submit_run(&self, command: SwarmRunSubmitCommand) -> Result<Value> {
        let response = self
            .client
            .post(format!("{}/api/run/submit", self.base_url))
            .json(&command)
            .send()
            .await?;
        let status = response.status();
        let body = response
            .text()
            .await
            .context("read wattswarm run submit response")?;
        if status.is_success() {
            return serde_json::from_str::<Value>(&body)
                .context("decode wattswarm run submit response");
        }
        Err(anyhow!(
            "wattswarm run submit failed with status {status}: {body}"
        ))
    }

    async fn import_task_contract(&self, contract: TaskContract) -> Result<Value> {
        self.client
            .post(format!("{}/api/task/import-contract", self.base_url))
            .json(&json!({ "contract": contract }))
            .send()
            .await?
            .error_for_status()?
            .json::<Value>()
            .await
            .context("decode wattswarm task contract import response")
    }

    async fn announce_task(&self, command: SwarmTaskAnnounceCommand) -> Result<Value> {
        self.client
            .post(format!("{}/api/task/announce", self.base_url))
            .json(&command)
            .send()
            .await?
            .error_for_status()?
            .json::<Value>()
            .await
            .context("decode wattswarm task announce response")
    }

    async fn claim_task(&self, command: SwarmTaskClaimCommand) -> Result<Value> {
        let response = self
            .client
            .post(format!("{}/api/task/claim", self.base_url))
            .json(&command)
            .send()
            .await?;
        let status = response.status();
        let body = response
            .text()
            .await
            .context("read wattswarm task claim response")?;
        if status.is_success() {
            return serde_json::from_str::<Value>(&body)
                .context("decode wattswarm task claim response");
        }
        Err(anyhow!(
            "wattswarm task claim failed with status {status}: {body}"
        ))
    }

    async fn decide_task_claim(&self, command: SwarmTaskClaimDecisionCommand) -> Result<Value> {
        self.client
            .post(format!("{}/api/task/claim-decision", self.base_url))
            .json(&command)
            .send()
            .await?
            .error_for_status()?
            .json::<Value>()
            .await
            .context("decode wattswarm task claim decision response")
    }

    async fn complete_task(&self, command: SwarmTaskCompleteCommand) -> Result<Value> {
        self.client
            .post(format!("{}/api/task/complete", self.base_url))
            .json(&command)
            .send()
            .await?
            .error_for_status()?
            .json::<Value>()
            .await
            .context("decode wattswarm task complete response")
    }

    async fn decide_task_completion(
        &self,
        command: SwarmTaskCompletionDecisionCommand,
    ) -> Result<Value> {
        self.client
            .post(format!("{}/api/task/completion-decision", self.base_url))
            .json(&command)
            .send()
            .await?
            .error_for_status()?
            .json::<Value>()
            .await
            .context("decode wattswarm task completion decision response")
    }

    async fn settle_task(&self, command: SwarmTaskSettleCommand) -> Result<Value> {
        self.client
            .post(format!("{}/api/task/settle", self.base_url))
            .json(&command)
            .send()
            .await?
            .error_for_status()?
            .json::<Value>()
            .await
            .context("decode wattswarm task settle response")
    }

    async fn propose_task_candidate(
        &self,
        command: SwarmTaskProposeCandidateCommand,
    ) -> Result<Value> {
        self.client
            .post(format!("{}/api/task/propose-candidate", self.base_url))
            .json(&command)
            .send()
            .await?
            .error_for_status()?
            .json::<Value>()
            .await
            .context("decode wattswarm task candidate proposal response")
    }

    async fn accept_and_finalize_task(
        &self,
        task_id: &str,
        candidate_id: &str,
        agent_envelope: Option<SwarmAgentEnvelope>,
    ) -> Result<Value> {
        self.client
            .post(format!("{}/api/task/accept-result", self.base_url))
            .json(&json!({
                "task_id": task_id,
                "candidate_id": candidate_id,
                "agent_envelope": agent_envelope,
            }))
            .send()
            .await?
            .error_for_status()?
            .json::<Value>()
            .await
            .context("decode wattswarm task accept result response")
    }

    async fn task_run_projection(
        &self,
        task_limit: usize,
        run_limit: usize,
    ) -> Result<SwarmTaskRunProjectionSnapshot> {
        self.client
            .get(format!(
                "{}/api/wattetheria/task-run/snapshot",
                self.base_url
            ))
            .query(&TaskRunSnapshotQuery {
                task_limit,
                run_limit,
            })
            .send()
            .await?
            .error_for_status()?
            .json::<SwarmTaskRunProjectionSnapshot>()
            .await
            .context("decode wattswarm task/run projection")
    }

    async fn task_decision_snapshot(&self, task_id: &str) -> Result<SwarmTaskDecisionSnapshot> {
        self.client
            .get(format!(
                "{}/api/wattetheria/task/decision/{}",
                self.base_url, task_id
            ))
            .send()
            .await?
            .error_for_status()?
            .json::<SwarmTaskDecisionSnapshot>()
            .await
            .context("decode wattswarm task decision snapshot")
    }

    async fn run_result_snapshot(&self, run_id: &str) -> Result<SwarmRunResultSnapshot> {
        self.client
            .get(format!(
                "{}/api/wattetheria/run/result/{}",
                self.base_url, run_id
            ))
            .send()
            .await?
            .error_for_status()?
            .json::<SwarmRunResultSnapshot>()
            .await
            .context("decode wattswarm run result snapshot")
    }

    async fn run_events_snapshot(
        &self,
        run_id: &str,
        limit: usize,
    ) -> Result<SwarmRunEventsSnapshot> {
        self.client
            .get(format!(
                "{}/api/wattetheria/run/events/{}",
                self.base_url, run_id
            ))
            .query(&RunEventsSnapshotQuery {
                limit: i64::try_from(limit).unwrap_or(i64::MAX),
            })
            .send()
            .await?
            .error_for_status()?
            .json::<SwarmRunEventsSnapshot>()
            .await
            .context("decode wattswarm run events snapshot")
    }

    async fn topic_activity_snapshot(
        &self,
        network_id: Option<&str>,
        feed_key: &str,
        scope_hint: &str,
        limit: usize,
        subscriber_node_id: Option<&str>,
    ) -> Result<SwarmTopicActivitySnapshot> {
        self.client
            .get(format!("{}/api/wattetheria/topic/activity", self.base_url))
            .query(&TopicActivitySnapshotQuery {
                network_id: network_id.map(ToOwned::to_owned),
                feed_key: feed_key.to_owned(),
                scope_hint: scope_hint.to_owned(),
                limit,
                subscriber_node_id: subscriber_node_id.map(ToOwned::to_owned),
            })
            .send()
            .await?
            .error_for_status()?
            .json::<SwarmTopicActivitySnapshot>()
            .await
            .context("decode wattswarm topic activity snapshot")
    }

    async fn knowledge_export_snapshot(
        &self,
        task_type: Option<&str>,
        task_id: Option<&str>,
    ) -> Result<SwarmKnowledgeExportSnapshot> {
        self.client
            .post(format!(
                "{}/api/wattetheria/knowledge/export",
                self.base_url
            ))
            .json(&KnowledgeExportRequest {
                task_type: task_type.map(ToOwned::to_owned),
                task_id: task_id.map(ToOwned::to_owned),
            })
            .send()
            .await?
            .error_for_status()?
            .json::<SwarmKnowledgeExportSnapshot>()
            .await
            .context("decode wattswarm knowledge export snapshot")
    }
}

#[derive(Debug, Serialize)]
struct TaskSampleQuery {
    task_id: String,
}

#[derive(Debug, Serialize)]
struct TopicMessagesQuery {
    network_id: Option<String>,
    feed_key: String,
    scope_hint: String,
    limit: usize,
    before_created_at: Option<u64>,
    before_message_id: Option<String>,
}

#[derive(Debug, Serialize)]
struct TopicCursorQuery {
    network_id: Option<String>,
    feed_key: String,
    subscriber_node_id: Option<String>,
}

#[derive(Debug, Serialize)]
struct TaskRunSnapshotQuery {
    task_limit: usize,
    run_limit: usize,
}

#[derive(Debug, Serialize)]
struct RunEventsSnapshotQuery {
    limit: i64,
}

#[derive(Debug, Serialize)]
struct TopicActivitySnapshotQuery {
    network_id: Option<String>,
    feed_key: String,
    scope_hint: String,
    limit: usize,
    subscriber_node_id: Option<String>,
}

#[derive(Debug, Serialize)]
struct KnowledgeExportRequest {
    task_type: Option<String>,
    task_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TaskSampleResponse {
    contract: TaskContract,
}

#[derive(Debug, Deserialize)]
struct TopicMessagesResponse {
    messages: Vec<SwarmTopicMessageView>,
}

#[derive(Debug, Deserialize)]
struct TopicCursorResponse {
    cursor: Option<SwarmTopicCursorView>,
}

#[derive(Debug, Deserialize)]
struct NetworkSnapshotResponse {
    network_id: String,
}

#[derive(Debug, Deserialize)]
struct NodeStatusResponse {
    running: bool,
    #[serde(default)]
    node_id: String,
    mode: String,
    #[serde(default)]
    peer_protocol_distribution: BTreeMap<String, u64>,
}

#[derive(Debug, Deserialize)]
struct PeersListResponse {
    #[serde(default)]
    peers: Vec<String>,
    #[serde(default)]
    records: Vec<Value>,
}

fn wattswarm_peer_views(response: PeersListResponse) -> Vec<SwarmPeerView> {
    let mut peers = BTreeMap::<String, SwarmPeerView>::new();
    for node_id in response.peers {
        peers.insert(
            node_id.clone(),
            SwarmPeerView {
                node_id,
                connected: Some(true),
                recently_seen: Some(true),
                stale: Some(false),
                last_seen_age_ms: None,
                discovery: None,
                metadata: None,
                relationship: None,
            },
        );
    }
    for record in response.records {
        let Some(node_id) = record
            .get("node_id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
        else {
            continue;
        };
        let connected = record.get("connected").and_then(Value::as_bool);
        let recently_seen = record.get("recently_seen").and_then(Value::as_bool);
        let stale = record.get("stale").and_then(Value::as_bool);
        let last_seen_age_ms = record.get("last_seen_age_ms").and_then(Value::as_u64);
        let peer = peers
            .entry(node_id.clone())
            .or_insert_with(|| SwarmPeerView {
                node_id,
                connected,
                recently_seen,
                stale,
                last_seen_age_ms,
                discovery: None,
                metadata: None,
                relationship: None,
            });
        if peer.connected.is_none() {
            peer.connected = connected;
        }
        if peer.recently_seen.is_none() {
            peer.recently_seen = recently_seen;
        }
        if peer.stale.is_none() {
            peer.stale = stale;
        }
        if peer.last_seen_age_ms.is_none() {
            peer.last_seen_age_ms = last_seen_age_ms;
        }
        peer.discovery = record
            .get("discovery")
            .filter(|value| !value.is_null())
            .cloned();
        peer.metadata = record
            .get("metadata")
            .filter(|value| !value.is_null())
            .cloned();
        peer.relationship = record
            .get("relationship")
            .filter(|value| !value.is_null())
            .cloned();
    }
    peers.into_values().collect()
}

#[derive(Debug, Deserialize)]
struct PeerRelationshipsResponse {
    relationships: Vec<SwarmPeerRelationshipView>,
}

#[derive(Debug, Deserialize)]
struct PeerDmThreadsResponse {
    threads: Vec<SwarmPeerDmThreadView>,
}

#[derive(Debug, Deserialize)]
struct PeerDmMessagesResponse {
    messages: Vec<SwarmPeerDmMessageView>,
}

#[derive(Debug, Serialize)]
struct PeerDmMessagesQuery {
    thread_id: String,
}

fn load_agent_stats_from_mission_board(
    mission_board_path: impl AsRef<Path>,
    agent_did: &str,
) -> Result<AgentStats> {
    let missions = MissionBoard::load_or_new(mission_board_path)?;
    let settled = missions.list(Some(&MissionStatus::Settled));
    let mut stats = AgentStats::default();

    for mission in settled
        .into_iter()
        .filter(|mission| mission.completed_by.as_deref() == Some(agent_did))
    {
        stats.watt += mission.reward.agent_watt;
        stats.reputation += mission.reward.reputation;
        stats.capacity += mission.reward.capacity;
    }

    if stats.watt != 0 || stats.reputation != 0 || stats.capacity != 0 {
        stats.power = (1 + (stats.capacity / 10)).max(1);
    }

    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::civilization::missions::{
        MissionBoard, MissionDomain, MissionPublisherKind, MissionReward,
    };
    use tempfile::tempdir;

    #[test]
    fn mission_board_agent_stats_roundtrip_matches_settled_rewards() {
        let dir = tempdir().unwrap();
        let mission_board_path = dir.path().join("missions/state.json");
        let mut board = MissionBoard::default();
        let mission = board.publish(
            "Stabilize relay",
            "Recover relay uptime.",
            "planet-a",
            MissionPublisherKind::PlanetaryGovernment,
            MissionDomain::Security,
            Some("planet-a".to_owned()),
            Some("zone-a".to_owned()),
            None,
            None,
            MissionReward {
                agent_watt: 15,
                reputation: 4,
                capacity: 11,
                treasury_share_watt: 2,
            },
            json!({}),
        );
        board.claim(&mission.mission_id, "agent-a").unwrap();
        board
            .complete(&mission.mission_id, "agent-a", None)
            .unwrap();
        board.settle(&mission.mission_id).unwrap();
        board.persist(&mission_board_path).unwrap();

        let stats = load_agent_stats_from_mission_board(&mission_board_path, "agent-a").unwrap();
        assert_eq!(stats.watt, 15);
        assert_eq!(stats.reputation, 4);
        assert_eq!(stats.capacity, 11);
        assert_eq!(stats.power, 2);
    }

    #[test]
    fn wattswarm_peer_views_preserve_local_peer_records() {
        let peers = wattswarm_peer_views(PeersListResponse {
            peers: vec!["peer-a".to_owned()],
            records: vec![
                json!({
                    "node_id": "peer-a",
                    "connected": true,
                    "recently_seen": true,
                    "stale": false,
                    "last_seen_age_ms": 100,
                    "discovery": {
                        "endpoint_id": "iroh-endpoint-a",
                        "source_kind": "bootstrap"
                    },
                    "metadata": {
                        "endpoint_id": "iroh-endpoint-a",
                        "handshake_status": "identified"
                    },
                    "relationship": {
                        "relationship_state": "friend",
                        "last_action": "accept"
                    }
                }),
                json!({
                    "node_id": "peer-b",
                    "connected": false,
                    "recently_seen": false,
                    "stale": true,
                    "last_seen_age_ms": 181_000
                }),
            ],
        });

        assert_eq!(peers.len(), 2);
        assert_eq!(peers[0].node_id, "peer-a");
        assert_eq!(peers[0].connected, Some(true));
        assert_eq!(peers[0].recently_seen, Some(true));
        assert_eq!(peers[0].stale, Some(false));
        assert_eq!(peers[0].last_seen_age_ms, Some(100));
        assert_eq!(
            peers[0]
                .discovery
                .as_ref()
                .and_then(|value| value.get("source_kind"))
                .and_then(Value::as_str),
            Some("bootstrap")
        );
        assert_eq!(
            peers[0]
                .relationship
                .as_ref()
                .and_then(|value| value.get("relationship_state"))
                .and_then(Value::as_str),
            Some("friend")
        );
        assert_eq!(peers[1].node_id, "peer-b");
        assert_eq!(peers[1].connected, Some(false));
        assert_eq!(peers[1].recently_seen, Some(false));
        assert_eq!(peers[1].stale, Some(true));
        assert_eq!(peers[1].last_seen_age_ms, Some(181_000));
    }
}
