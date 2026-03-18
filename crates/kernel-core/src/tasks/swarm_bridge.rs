//! Bridge layer that keeps wattetheria app flows independent from the current legacy task engine.

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use tokio::sync::Mutex;
use wattswarm_protocol::types::{
    Acceptance, Assignment, Budget, BudgetMode, ClaimPayload, ClaimPolicy, ClaimRole, EventPayload,
    EvidencePolicy, ExploreAssignment, ExploreStopPolicy, FeedbackCapabilityPolicy,
    FinalizeAssignment, MaxConcurrency, PolicyBinding, SettlementBadPenalty,
    SettlementDiminishingReturns, SettlementPolicy, TaskContract, TaskTerminalState, TaskView,
    VerifyAssignment, VotePolicy,
};

use crate::galaxy_task::GalaxyTaskIntent;
use crate::task_engine::TaskEngine;
use crate::types::{AgentStats, Reward, Sla, Task, VerificationMode, VerificationSpec};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SwarmTaskReceipt {
    pub task_id: String,
    pub accepted_by: String,
    pub created_event: EventPayload,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SwarmTaskProjectionView {
    pub task_id: String,
    pub task_type: String,
    pub epoch: u64,
    pub terminal_state: String,
    pub committed_candidate_id: Option<String>,
    pub finalized_candidate_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SwarmAgentView {
    pub agent_id: String,
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

#[async_trait]
pub trait SwarmBridge: Send + Sync {
    async fn submit_task_contract(
        &self,
        submitter_id: &str,
        contract: TaskContract,
    ) -> Result<SwarmTaskReceipt>;

    async fn task_projection(&self, task_id: &str) -> Result<Option<SwarmTaskProjectionView>>;

    async fn task_events(&self, task_id: &str) -> Result<Vec<EventPayload>>;

    async fn run_task_contract(
        &self,
        worker_id: &str,
        contract: TaskContract,
    ) -> Result<SwarmTaskProjectionView>;

    async fn agent_view(&self, agent_id: &str) -> Result<SwarmAgentView>;

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

    async fn submit_galaxy_task(
        &self,
        submitter_id: &str,
        intent: GalaxyTaskIntent,
    ) -> Result<SwarmTaskReceipt> {
        self.submit_task_contract(submitter_id, intent.to_task_contract())
            .await
    }

    async fn run_galaxy_task(
        &self,
        worker_id: &str,
        intent: GalaxyTaskIntent,
    ) -> Result<SwarmTaskProjectionView> {
        self.run_task_contract(worker_id, intent.to_task_contract())
            .await
    }
}

pub struct LegacyTaskEngineBridge {
    engine: Mutex<TaskEngine>,
    ledger_path: PathBuf,
}

pub struct HybridSwarmBridge {
    task_bridge: LegacyTaskEngineBridge,
    topic_api: Option<HttpWattswarmApi>,
}

impl LegacyTaskEngineBridge {
    #[must_use]
    pub fn new(engine: TaskEngine, ledger_path: PathBuf) -> Self {
        Self {
            engine: Mutex::new(engine),
            ledger_path,
        }
    }

    pub fn load_ledger(path: impl AsRef<Path>) -> Result<HashMap<String, AgentStats>> {
        TaskEngine::load_ledger(path)
    }
}

impl HybridSwarmBridge {
    #[must_use]
    pub fn new(
        engine: TaskEngine,
        ledger_path: PathBuf,
        wattswarm_ui_base_url: Option<&str>,
    ) -> Self {
        Self {
            task_bridge: LegacyTaskEngineBridge::new(engine, ledger_path),
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
    async fn submit_task_contract(
        &self,
        submitter_id: &str,
        contract: TaskContract,
    ) -> Result<SwarmTaskReceipt> {
        self.task_bridge
            .submit_task_contract(submitter_id, contract)
            .await
    }

    async fn task_projection(&self, task_id: &str) -> Result<Option<SwarmTaskProjectionView>> {
        self.task_bridge.task_projection(task_id).await
    }

    async fn task_events(&self, task_id: &str) -> Result<Vec<EventPayload>> {
        self.task_bridge.task_events(task_id).await
    }

    async fn run_task_contract(
        &self,
        worker_id: &str,
        contract: TaskContract,
    ) -> Result<SwarmTaskProjectionView> {
        self.task_bridge
            .run_task_contract(worker_id, contract)
            .await
    }

    async fn agent_view(&self, agent_id: &str) -> Result<SwarmAgentView> {
        self.task_bridge.agent_view(agent_id).await
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
}

#[async_trait]
impl SwarmBridge for LegacyTaskEngineBridge {
    async fn submit_task_contract(
        &self,
        submitter_id: &str,
        contract: TaskContract,
    ) -> Result<SwarmTaskReceipt> {
        let mut engine = self.engine.lock().await;
        let task = engine.publish_task(
            &contract.task_type,
            "wattswarm-bridge",
            contract.inputs.clone(),
            VerificationSpec {
                mode: VerificationMode::Deterministic,
                witnesses: None,
            },
            Reward {
                watt: i64::try_from(contract.budget.cost_units).unwrap_or(i64::MAX),
                reputation: 0,
                capacity: 0,
            },
            Sla {
                timeout_sec: (contract.budget.time_ms / 1_000).max(1),
            },
        )?;

        Ok(SwarmTaskReceipt {
            task_id: task.task_id,
            accepted_by: submitter_id.to_string(),
            created_event: EventPayload::TaskCreated(contract),
        })
    }

    async fn task_projection(&self, task_id: &str) -> Result<Option<SwarmTaskProjectionView>> {
        let engine = self.engine.lock().await;
        Ok(engine.get_task(task_id).map(map_task_projection))
    }

    async fn task_events(&self, task_id: &str) -> Result<Vec<EventPayload>> {
        let engine = self.engine.lock().await;
        let Some(task) = engine.get_task(task_id) else {
            return Ok(Vec::new());
        };

        let mut events = vec![EventPayload::TaskCreated(task_contract_from_legacy_task(
            &task,
        ))];
        if let Some(claimer) = &task.claimed_by {
            events.push(EventPayload::TaskClaimed(ClaimPayload {
                task_id: task.task_id.clone(),
                role: ClaimRole::Propose,
                claimer_node_id: claimer.clone(),
                execution_id: format!("legacy-exec-{}", task.task_id),
                lease_until: chrono::Utc::now().timestamp_millis().max(0).cast_unsigned() + 5_000,
            }));
        }
        Ok(events)
    }

    async fn run_task_contract(
        &self,
        worker_id: &str,
        contract: TaskContract,
    ) -> Result<SwarmTaskProjectionView> {
        let receipt = self.submit_task_contract(worker_id, contract).await?;
        let mut engine = self.engine.lock().await;
        engine.claim_task(&receipt.task_id, worker_id)?;
        let result = engine.execute_task(&receipt.task_id)?;
        engine.submit_task_result(&receipt.task_id, &result, worker_id)?;
        let verified = engine.verify_task(&receipt.task_id)?;
        if verified {
            let _ = engine.settle_task(&receipt.task_id)?;
        }
        engine.persist_ledger(&self.ledger_path)?;

        let task = engine
            .get_task(&receipt.task_id)
            .context("bridge task missing after execution")?;
        Ok(map_task_projection(task))
    }

    async fn agent_view(&self, agent_id: &str) -> Result<SwarmAgentView> {
        let engine = self.engine.lock().await;
        Ok(SwarmAgentView {
            agent_id: agent_id.to_string(),
            stats: engine.get_ledger(agent_id),
        })
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

fn map_task_projection(task: Task) -> SwarmTaskProjectionView {
    let terminal_state = match task.status.as_deref() {
        Some("SETTLED" | "VERIFIED") => TaskTerminalState::Finalized,
        Some("REJECTED") => TaskTerminalState::Suspended,
        _ => TaskTerminalState::Open,
    };
    let view = TaskView {
        contract: task_contract_from_legacy_task(&task),
        epoch: 0,
        terminal_state,
        committed_candidate_id: task.claimed_by.clone(),
        finalized_candidate_id: task
            .status
            .as_deref()
            .filter(|status| *status == "SETTLED" || *status == "VERIFIED")
            .map(|_| task.task_id.clone()),
    };

    SwarmTaskProjectionView {
        task_id: task.task_id,
        task_type: view.contract.task_type,
        epoch: view.epoch,
        terminal_state: task_terminal_state_label(&view.terminal_state).to_owned(),
        committed_candidate_id: view.committed_candidate_id,
        finalized_candidate_id: view.finalized_candidate_id,
    }
}

fn task_contract_from_legacy_task(task: &Task) -> TaskContract {
    TaskContract {
        protocol_version: "v0.1".to_owned(),
        task_id: task.task_id.clone(),
        task_type: task.task_family.clone(),
        inputs: task.input_spec.clone(),
        output_schema: json!({}),
        budget: Budget {
            time_ms: task.sla.timeout_sec.saturating_mul(1_000),
            max_steps: 1,
            cost_units: u64::try_from(task.reward.watt.max(0)).unwrap_or_default(),
            mode: BudgetMode::Lifetime,
            explore_cost_units: 0,
            verify_cost_units: 0,
            finalize_cost_units: 0,
            reuse_verify_time_ms: 0,
            reuse_verify_cost_units: 0,
            reuse_max_attempts: 0,
        },
        assignment: Assignment {
            mode: "CLAIM".to_owned(),
            claim: ClaimPolicy {
                lease_ms: task.sla.timeout_sec.saturating_mul(1_000),
                max_concurrency: MaxConcurrency {
                    propose: 1,
                    verify: 1,
                },
            },
            explore: ExploreAssignment {
                max_proposers: 1,
                topk: 1,
                stop: ExploreStopPolicy {
                    no_new_evidence_rounds: 1,
                },
            },
            verify: VerifyAssignment { max_verifiers: 1 },
            finalize: FinalizeAssignment { max_finalizers: 1 },
        },
        acceptance: Acceptance {
            quorum_threshold: 1,
            verifier_policy: PolicyBinding {
                policy_id: "legacy-bridge".to_owned(),
                policy_version: "1".to_owned(),
                policy_hash: "legacy-bridge".to_owned(),
                policy_params: json!({}),
            },
            vote: VotePolicy {
                commit_reveal: false,
                reveal_deadline_ms: 0,
            },
            settlement: SettlementPolicy {
                window_ms: 0,
                implicit_weight: 0.0,
                implicit_diminishing_returns: SettlementDiminishingReturns { w: 0, k: 0 },
                bad_penalty: SettlementBadPenalty { p: 0 },
                feedback: FeedbackCapabilityPolicy {
                    mode: "NONE".to_owned(),
                    authority_pubkey: String::new(),
                },
            },
            da_quorum_threshold: 1,
        },
        task_mode: wattswarm_protocol::types::TaskMode::OneShot,
        expiry_ms: chrono::Utc::now().timestamp_millis().max(0).cast_unsigned()
            + task.sla.timeout_sec.saturating_mul(1_000),
        evidence_policy: EvidencePolicy {
            max_inline_evidence_bytes: 0,
            max_inline_media_bytes: 0,
            inline_mime_allowlist: Vec::new(),
            max_snippet_bytes: 0,
            max_snippet_tokens: 0,
        },
    }
}

fn task_terminal_state_label(state: &TaskTerminalState) -> &'static str {
    match state {
        TaskTerminalState::Open => "open",
        TaskTerminalState::Expired => "expired",
        TaskTerminalState::Finalized => "finalized",
        TaskTerminalState::Stopped => "stopped",
        TaskTerminalState::Suspended => "suspended",
        TaskTerminalState::Killed => "killed",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event_log::EventLog;
    use crate::identity::Identity;
    use tempfile::tempdir;

    #[tokio::test]
    async fn legacy_bridge_exposes_local_node_task_view() {
        let dir = tempdir().unwrap();
        let identity = Identity::new_random();
        let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
        let engine = TaskEngine::new(event_log, identity.clone());
        let bridge = LegacyTaskEngineBridge::new(engine, dir.path().join("ledger.json"));

        let task = bridge
            .run_galaxy_task(&identity.agent_id, GalaxyTaskIntent::demo_market_match())
            .await
            .unwrap();

        assert_eq!(task.task_type, "market.match");
        assert_eq!(task.terminal_state, "finalized");
    }

    #[tokio::test]
    async fn legacy_bridge_reports_agent_stats_without_invented_consensus_flags() {
        let dir = tempdir().unwrap();
        let identity = Identity::new_random();
        let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
        let engine = TaskEngine::new(event_log, identity.clone());
        let bridge = LegacyTaskEngineBridge::new(engine, dir.path().join("ledger.json"));

        let agent = bridge.agent_view(&identity.agent_id).await.unwrap();
        assert_eq!(agent.agent_id, identity.agent_id);
        assert_eq!(agent.stats, AgentStats::default());
    }

    #[tokio::test]
    async fn legacy_bridge_submission_returns_wattswarm_task_created_event() {
        let dir = tempdir().unwrap();
        let identity = Identity::new_random();
        let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
        let engine = TaskEngine::new(event_log, identity.clone());
        let bridge = LegacyTaskEngineBridge::new(engine, dir.path().join("ledger.json"));

        let ack = bridge
            .submit_galaxy_task(&identity.agent_id, GalaxyTaskIntent::demo_market_match())
            .await
            .unwrap();

        match ack.created_event {
            EventPayload::TaskCreated(contract) => {
                assert_eq!(contract.task_type, "market.match");
            }
            other => panic!("unexpected event payload: {other:?}"),
        }
    }
}
