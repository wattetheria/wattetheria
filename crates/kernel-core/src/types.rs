//! Shared protocol domain types used across kernel, cli, and observatory.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentStats {
    pub power: i64,
    pub watt: i64,
    pub reputation: i64,
    pub capacity: i64,
}

impl Default for AgentStats {
    fn default() -> Self {
        Self {
            power: 1,
            watt: 0,
            reputation: 0,
            capacity: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Reward {
    pub watt: i64,
    pub reputation: i64,
    pub capacity: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum VerificationMode {
    Deterministic,
    Witness,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VerificationSpec {
    pub mode: VerificationMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub witnesses: Option<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Sla {
    pub timeout_sec: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Task {
    pub task_id: String,
    pub task_family: String,
    pub tier: String,
    pub input_spec: Value,
    pub verification: VerificationSpec,
    pub reward: Reward,
    pub sla: Sla,
    pub signature: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub claimed_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskStats {
    pub completed: u64,
    pub success_rate: f64,
    pub contribution: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SignedSummary {
    pub agent_did: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub controller_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_id: Option<String>,
    pub timestamp: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subnet_id: Option<String>,
    pub power: i64,
    pub watt: i64,
    pub reputation: i64,
    pub capacity: i64,
    pub task_stats: TaskStats,
    pub events_digest: String,
    pub signature: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HandshakePayload {
    pub version: String,
    pub agent_did: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub controller_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub public_id: Option<String>,
    pub nonce: String,
    pub timestamp: i64,
    pub capabilities_summary: Value,
    pub online_proof: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hashcash: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ActionEnvelope {
    pub r#type: String,
    pub version: String,
    pub action: String,
    pub action_id: String,
    pub timestamp: i64,
    pub sender: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recipient: Option<String>,
    pub payload: Value,
    pub signature: String,
}
