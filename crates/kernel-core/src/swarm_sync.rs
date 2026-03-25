use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::swarm_bridge::{SwarmTopicCursorView, SwarmTopicMessageView};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SwarmTaskProjectionSummary {
    pub task_id: String,
    pub task_type: String,
    pub epoch: u64,
    pub terminal_state: String,
    pub committed_candidate_id: Option<String>,
    pub finalized_candidate_id: Option<String>,
    pub retry_attempt: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SwarmTaskRunProjectionSnapshot {
    pub generated_at: u64,
    pub recent_tasks: Vec<SwarmTaskProjectionSummary>,
    pub recent_runs: Vec<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SwarmTaskDecisionSnapshot {
    pub ok: bool,
    pub task_id: String,
    pub committed_candidate_id: Option<String>,
    pub finalized_candidate_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SwarmRunResultSnapshot {
    pub ok: bool,
    pub result: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SwarmRunEventsSnapshot {
    pub ok: bool,
    pub events: Vec<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SwarmTopicActivitySnapshot {
    pub generated_at: u64,
    pub subscriber_node_id: String,
    pub feed_key: String,
    pub scope_hint: String,
    pub messages: Vec<SwarmTopicMessageView>,
    pub cursor: Option<SwarmTopicCursorView>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SwarmKnowledgeExportSnapshot {
    pub ok: bool,
    pub knowledge: Value,
}
