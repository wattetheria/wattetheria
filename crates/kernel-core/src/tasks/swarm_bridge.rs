//! Bridge layer that keeps wattetheria app flows independent from the current legacy task engine.

use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
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
