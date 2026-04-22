//! Bridge layer that keeps wattetheria app flows independent from wattswarm transport details.

use crate::civilization::missions::{MissionBoard, MissionStatus};
use crate::swarm_sync::{
    SwarmKnowledgeExportSnapshot, SwarmRunEventsSnapshot, SwarmRunResultSnapshot,
    SwarmTaskDecisionSnapshot, SwarmTaskRunProjectionSnapshot, SwarmTopicActivitySnapshot,
};
use crate::types::AgentStats;
use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SwarmPeerView {
    pub node_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SwarmNetworkStatusView {
    pub running: bool,
    pub mode: String,
    pub peer_protocol_distribution: BTreeMap<String, u64>,
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
pub struct SwarmAgentEnvelope {
    pub protocol: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub capability: Option<String>,
    pub message: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extensions: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
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
}

#[async_trait]
pub trait SwarmBridge: Send + Sync {
    async fn agent_view(&self, agent_did: &str) -> Result<SwarmAgentView>;

    async fn subscribe_topic(
        &self,
        _subscriber_id: &str,
        _feed_key: &str,
        _scope_hint: &str,
        _active: bool,
    ) -> Result<()> {
        Err(anyhow!("wattswarm topic subscriptions are not configured"))
    }

    async fn post_topic_message(
        &self,
        _feed_key: &str,
        _scope_hint: &str,
        _content: Value,
        _reply_to_message_id: Option<String>,
    ) -> Result<()> {
        Err(anyhow!("wattswarm topic messages are not configured"))
    }

    async fn list_topic_messages(
        &self,
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
        _feed_key: &str,
        _subscriber_id: Option<&str>,
    ) -> Result<Option<SwarmTopicCursorView>> {
        Err(anyhow!("wattswarm topic cursors are not configured"))
    }

    async fn network_status(&self) -> Result<SwarmNetworkStatusView> {
        Err(anyhow!("wattswarm network status is not configured"))
    }

    async fn peers(&self) -> Result<Vec<SwarmPeerView>> {
        Err(anyhow!("wattswarm peers are not configured"))
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
        subscriber_id: &str,
        feed_key: &str,
        scope_hint: &str,
        active: bool,
    ) -> Result<()> {
        self.topic_api()?
            .subscribe_topic(subscriber_id, feed_key, scope_hint, active)
            .await
    }

    async fn post_topic_message(
        &self,
        feed_key: &str,
        scope_hint: &str,
        content: Value,
        reply_to_message_id: Option<String>,
    ) -> Result<()> {
        self.topic_api()?
            .post_topic_message(feed_key, scope_hint, content, reply_to_message_id)
            .await
    }

    async fn list_topic_messages(
        &self,
        feed_key: &str,
        scope_hint: &str,
        limit: usize,
        before_created_at: Option<u64>,
        before_message_id: Option<String>,
    ) -> Result<Vec<SwarmTopicMessageView>> {
        self.topic_api()?
            .list_topic_messages(
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
        feed_key: &str,
        subscriber_id: Option<&str>,
    ) -> Result<Option<SwarmTopicCursorView>> {
        self.topic_api()?
            .topic_cursor(feed_key, subscriber_id)
            .await
    }

    async fn network_status(&self) -> Result<SwarmNetworkStatusView> {
        self.topic_api()?.network_status().await
    }

    async fn peers(&self) -> Result<Vec<SwarmPeerView>> {
        self.topic_api()?.peers().await
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
        feed_key: &str,
        scope_hint: &str,
        limit: usize,
        subscriber_node_id: Option<&str>,
    ) -> Result<SwarmTopicActivitySnapshot> {
        self.topic_api()?
            .topic_activity_snapshot(feed_key, scope_hint, limit, subscriber_node_id)
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

    async fn subscribe_topic(
        &self,
        subscriber_id: &str,
        feed_key: &str,
        scope_hint: &str,
        active: bool,
    ) -> Result<()> {
        self.client
            .post(format!("{}/api/topic/subscriptions", self.base_url))
            .json(&json!({
                "subscriber_node_id": subscriber_id,
                "feed_key": feed_key,
                "scope_hint": scope_hint,
                "active": active,
            }))
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }

    async fn post_topic_message(
        &self,
        feed_key: &str,
        scope_hint: &str,
        content: Value,
        reply_to_message_id: Option<String>,
    ) -> Result<()> {
        self.client
            .post(format!("{}/api/topic/messages", self.base_url))
            .json(&json!({
                "feed_key": feed_key,
                "scope_hint": scope_hint,
                "content": content,
                "reply_to_message_id": reply_to_message_id,
            }))
            .send()
            .await?
            .error_for_status()?;
        Ok(())
    }

    async fn list_topic_messages(
        &self,
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
        feed_key: &str,
        subscriber_id: Option<&str>,
    ) -> Result<Option<SwarmTopicCursorView>> {
        let response = self
            .client
            .get(format!("{}/api/topic/cursor", self.base_url))
            .query(&TopicCursorQuery {
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
        let response = self
            .client
            .get(format!("{}/api/node/status", self.base_url))
            .send()
            .await?
            .error_for_status()?
            .json::<NodeStatusResponse>()
            .await?;
        Ok(SwarmNetworkStatusView {
            running: response.running,
            mode: response.mode,
            peer_protocol_distribution: response.peer_protocol_distribution,
        })
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
        Ok(response
            .peers
            .into_iter()
            .map(|node_id| SwarmPeerView { node_id })
            .collect())
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
        feed_key: &str,
        scope_hint: &str,
        limit: usize,
        subscriber_node_id: Option<&str>,
    ) -> Result<SwarmTopicActivitySnapshot> {
        self.client
            .get(format!("{}/api/wattetheria/topic/activity", self.base_url))
            .query(&TopicActivitySnapshotQuery {
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
struct TopicMessagesQuery {
    feed_key: String,
    scope_hint: String,
    limit: usize,
    before_created_at: Option<u64>,
    before_message_id: Option<String>,
}

#[derive(Debug, Serialize)]
struct TopicCursorQuery {
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
struct TopicMessagesResponse {
    messages: Vec<SwarmTopicMessageView>,
}

#[derive(Debug, Deserialize)]
struct TopicCursorResponse {
    cursor: Option<SwarmTopicCursorView>,
}

#[derive(Debug, Deserialize)]
struct NodeStatusResponse {
    running: bool,
    mode: String,
    #[serde(default)]
    peer_protocol_distribution: BTreeMap<String, u64>,
}

#[derive(Debug, Deserialize)]
struct PeersListResponse {
    peers: Vec<String>,
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
        board.complete(&mission.mission_id, "agent-a").unwrap();
        board.settle(&mission.mission_id).unwrap();
        board.persist(&mission_board_path).unwrap();

        let stats = load_agent_stats_from_mission_board(&mission_board_path, "agent-a").unwrap();
        assert_eq!(stats.watt, 15);
        assert_eq!(stats.reputation, 4);
        assert_eq!(stats.capacity, 11);
        assert_eq!(stats.power, 2);
    }
}
