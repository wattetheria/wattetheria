use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportKind {
    Wattswarm,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteTransportBinding {
    pub public_id: String,
    pub agent_did: Option<String>,
    pub transport_kind: TransportKind,
    pub transport_node_id: String,
    pub binding_source: String,
    pub binding_confidence: i32,
    pub binding_proof_json: Option<serde_json::Value>,
    pub binding_verified: bool,
    pub binding_verified_at: Option<i64>,
    pub updated_at: i64,
}
