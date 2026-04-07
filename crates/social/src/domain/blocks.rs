use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SocialBlock {
    pub block_id: String,
    pub owner_public_id: String,
    pub blocked_public_id: String,
    pub blocked_node_id: Option<String>,
    pub reason: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}
