use crate::policy::rules::PolicyScope;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyDecision {
    Allow,
    Deny,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyDecisionLog {
    pub decision_id: String,
    pub owner_public_id: String,
    pub scope: PolicyScope,
    pub target_public_id: String,
    pub target_node_id: Option<String>,
    pub rule_id: Option<String>,
    pub decision: PolicyDecision,
    pub reason: String,
    pub context_json: Value,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyEvaluation {
    pub decision: PolicyDecision,
    pub matched_rule_id: Option<String>,
    pub reason: String,
    pub context_json: Value,
}
