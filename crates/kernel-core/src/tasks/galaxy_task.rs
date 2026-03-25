//! Galaxy-layer task intent model for mapping Wattetheria galaxy-network tasks into Wattswarm contracts.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use wattswarm_protocol::types::{
    Acceptance, Assignment, Budget, BudgetMode, ClaimPolicy, EvidencePolicy, ExploreAssignment,
    ExploreStopPolicy, FeedbackCapabilityPolicy, FinalizeAssignment, MaxConcurrency, PolicyBinding,
    SettlementBadPenalty, SettlementDiminishingReturns, SettlementPolicy, TaskContract, TaskMode,
    VerifyAssignment, VotePolicy,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GalaxyScopeKind {
    GenesisMainnet,
    Planet,
    Subnet,
    Market,
    Route,
    Guild,
    Global,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GalaxyBroadcastMode {
    FullNetwork,
    Scoped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GalaxyTaskScope {
    pub kind: GalaxyScopeKind,
    pub scope_id: String,
    pub broadcast: GalaxyBroadcastMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GalaxyTaskReward {
    pub watt: i64,
    pub reputation: i64,
    pub capacity: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GalaxyTaskBudget {
    pub time_ms: u64,
    pub max_steps: u32,
    pub cost_units: u64,
    pub mode: BudgetMode,
    pub explore_cost_units: u64,
    pub verify_cost_units: u64,
    pub finalize_cost_units: u64,
    pub reuse_verify_time_ms: u64,
    pub reuse_verify_cost_units: u64,
    pub reuse_max_attempts: u32,
    pub expiry_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GalaxyTaskConsensus {
    pub max_proposers: u32,
    pub topk: u32,
    pub no_new_evidence_rounds: u32,
    pub max_verifiers: u32,
    pub max_finalizers: u32,
    pub quorum_threshold: u32,
    pub da_quorum_threshold: u32,
    pub commit_reveal: bool,
    pub reveal_deadline_ms: u64,
    pub settlement_window_ms: u64,
    pub implicit_weight_w: u32,
    pub implicit_weight_k: u32,
    pub implicit_weight: f64,
    pub bad_penalty: i64,
    pub feedback_mode: String,
    pub feedback_authority_pubkey: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GalaxyTaskIntent {
    pub protocol_version: String,
    pub task_id: String,
    pub task_type: String,
    pub objective: String,
    pub scope: GalaxyTaskScope,
    pub galaxy_context: Value,
    pub task_inputs: Value,
    pub output_schema: Value,
    pub verifier_policy: PolicyBinding,
    pub budget: GalaxyTaskBudget,
    pub consensus: GalaxyTaskConsensus,
    pub reward: GalaxyTaskReward,
    pub evidence_policy: EvidencePolicy,
    #[serde(default)]
    pub task_mode: TaskMode,
}

impl GalaxyTaskIntent {
    fn contract_inputs(&self) -> Value {
        let galaxy_metadata = json!({
            "objective": self.objective,
            "scope": self.scope,
            "galaxy_context": self.galaxy_context,
            "reward": self.reward,
        });

        match self.task_inputs.clone() {
            Value::Object(mut map) => {
                map.insert("__wattetheria_galaxy".to_owned(), galaxy_metadata);
                Value::Object(map)
            }
            payload => json!({
                "payload": payload,
                "__wattetheria_galaxy": galaxy_metadata,
            }),
        }
    }

    #[must_use]
    pub fn to_task_contract(&self) -> TaskContract {
        TaskContract {
            protocol_version: self.protocol_version.clone(),
            task_id: self.task_id.clone(),
            task_type: self.task_type.clone(),
            inputs: self.contract_inputs(),
            output_schema: self.output_schema.clone(),
            budget: Budget {
                time_ms: self.budget.time_ms,
                max_steps: self.budget.max_steps,
                cost_units: self.budget.cost_units,
                mode: self.budget.mode,
                explore_cost_units: self.budget.explore_cost_units,
                verify_cost_units: self.budget.verify_cost_units,
                finalize_cost_units: self.budget.finalize_cost_units,
                reuse_verify_time_ms: self.budget.reuse_verify_time_ms,
                reuse_verify_cost_units: self.budget.reuse_verify_cost_units,
                reuse_max_attempts: self.budget.reuse_max_attempts,
            },
            assignment: Assignment {
                mode: "CLAIM".to_owned(),
                claim: ClaimPolicy {
                    lease_ms: self.budget.time_ms.min(self.budget.expiry_ms.max(1)),
                    max_concurrency: MaxConcurrency {
                        propose: 1,
                        verify: 1,
                    },
                },
                explore: ExploreAssignment {
                    max_proposers: self.consensus.max_proposers,
                    topk: self.consensus.topk,
                    stop: ExploreStopPolicy {
                        no_new_evidence_rounds: self.consensus.no_new_evidence_rounds,
                    },
                },
                verify: VerifyAssignment {
                    max_verifiers: self.consensus.max_verifiers,
                },
                finalize: FinalizeAssignment {
                    max_finalizers: self.consensus.max_finalizers,
                },
            },
            acceptance: Acceptance {
                quorum_threshold: self.consensus.quorum_threshold,
                verifier_policy: self.verifier_policy.clone(),
                vote: VotePolicy {
                    commit_reveal: self.consensus.commit_reveal,
                    reveal_deadline_ms: self.consensus.reveal_deadline_ms,
                },
                settlement: SettlementPolicy {
                    window_ms: self.consensus.settlement_window_ms,
                    implicit_weight: self.consensus.implicit_weight,
                    implicit_diminishing_returns: SettlementDiminishingReturns {
                        w: self.consensus.implicit_weight_w,
                        k: self.consensus.implicit_weight_k,
                    },
                    bad_penalty: SettlementBadPenalty {
                        p: self.consensus.bad_penalty,
                    },
                    feedback: FeedbackCapabilityPolicy {
                        mode: self.consensus.feedback_mode.clone(),
                        authority_pubkey: self.consensus.feedback_authority_pubkey.clone(),
                    },
                },
                da_quorum_threshold: self.consensus.da_quorum_threshold,
            },
            task_mode: self.task_mode,
            expiry_ms: self.budget.expiry_ms,
            evidence_policy: self.evidence_policy.clone(),
        }
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn test_market_match_fixture() -> Self {
        Self {
            protocol_version: "v0.1".to_owned(),
            task_id: uuid::Uuid::new_v4().to_string(),
            task_type: "market.match".to_owned(),
            objective:
                "Clear orders for the genesis market and publish a deterministic match result."
                    .to_owned(),
            scope: GalaxyTaskScope {
                kind: GalaxyScopeKind::GenesisMainnet,
                scope_id: "genesis-mainnet".to_owned(),
                broadcast: GalaxyBroadcastMode::FullNetwork,
            },
            galaxy_context: json!({
                "galaxy": "wattetheria",
                "phase": "genesis",
                "market_id": "genesis-market-1"
            }),
            task_inputs: json!({
                "buy_orders": [
                    {"id":"bridge-buy-1", "price":120, "qty":5},
                    {"id":"bridge-buy-2", "price":118, "qty":3}
                ],
                "sell_orders": [
                    {"id":"bridge-sell-1", "price":110, "qty":2},
                    {"id":"bridge-sell-2", "price":112, "qty":6}
                ]
            }),
            output_schema: json!({
                "type":"object",
                "required":["family","trades","cleared_volume","trade_count"],
                "properties":{
                    "family":{"type":"string"},
                    "trades":{"type":"array"},
                    "cleared_volume":{"type":"integer"},
                    "trade_count":{"type":"integer"}
                }
            }),
            verifier_policy: PolicyBinding {
                policy_id: "vp.schema_only.v1".to_owned(),
                policy_version: "1".to_owned(),
                policy_hash: "legacy-bridge".to_owned(),
                policy_params: json!({
                    "scope_kind": "genesis_mainnet",
                    "scope_id": "genesis-mainnet"
                }),
            },
            budget: GalaxyTaskBudget {
                time_ms: 30_000,
                max_steps: 10,
                cost_units: 12,
                mode: BudgetMode::Lifetime,
                explore_cost_units: 4,
                verify_cost_units: 4,
                finalize_cost_units: 4,
                reuse_verify_time_ms: 20_000,
                reuse_verify_cost_units: 2,
                reuse_max_attempts: 1,
                expiry_ms: chrono::Utc::now().timestamp_millis().max(0).cast_unsigned() + 120_000,
            },
            consensus: GalaxyTaskConsensus {
                max_proposers: 1,
                topk: 1,
                no_new_evidence_rounds: 1,
                max_verifiers: 1,
                max_finalizers: 1,
                quorum_threshold: 1,
                da_quorum_threshold: 1,
                commit_reveal: true,
                reveal_deadline_ms: 10_000,
                settlement_window_ms: 86_400_000,
                implicit_weight_w: 10,
                implicit_weight_k: 50,
                implicit_weight: 0.1,
                bad_penalty: 3,
                feedback_mode: "CAPABILITY".to_owned(),
                feedback_authority_pubkey: "ed25519:placeholder".to_owned(),
            },
            reward: GalaxyTaskReward {
                watt: 12,
                reputation: 3,
                capacity: 4,
            },
            evidence_policy: EvidencePolicy {
                max_inline_evidence_bytes: 65_536,
                max_inline_media_bytes: 0,
                inline_mime_allowlist: vec![
                    "application/json".to_owned(),
                    "text/plain".to_owned(),
                    "text/markdown".to_owned(),
                ],
                max_snippet_bytes: 8_192,
                max_snippet_tokens: 2_048,
            },
            task_mode: TaskMode::OneShot,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_market_intent_maps_to_wattswarm_contract() {
        let intent = GalaxyTaskIntent::test_market_match_fixture();
        let contract = intent.to_task_contract();

        assert_eq!(contract.task_id, intent.task_id);
        assert_eq!(contract.task_type, "market.match");
        assert_eq!(contract.acceptance.quorum_threshold, 1);
        assert_eq!(contract.assignment.explore.max_proposers, 1);
        assert_eq!(contract.inputs["buy_orders"][0]["id"], "bridge-buy-1");
        assert_eq!(
            contract.inputs["__wattetheria_galaxy"]["scope"]["scope_id"],
            "genesis-mainnet"
        );
        assert_eq!(
            contract.inputs["__wattetheria_galaxy"]["reward"]["watt"],
            12
        );
    }
}
