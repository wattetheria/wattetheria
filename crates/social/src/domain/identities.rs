use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalIdentityContext {
    pub public_id: String,
    pub agent_did: String,
    pub display_name: String,
    pub description: Option<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub skills: Vec<String>,
    pub active: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteIdentityProfile {
    pub public_id: String,
    pub agent_did: String,
    pub display_name: String,
    pub description: Option<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub skills: Vec<String>,
    pub did_document_json: Option<serde_json::Value>,
    pub active: bool,
    pub last_profile_fetched_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
}
