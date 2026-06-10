use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeferredAgentEvent {
    pub event_id: String,
    pub local_public_id: String,
    pub remote_public_id: String,
    pub remote_node_id: Option<String>,
    pub source_agent_id: Option<String>,
    pub status: String,
    pub event_json: Value,
    pub reason: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub replayed_at: Option<i64>,
}
