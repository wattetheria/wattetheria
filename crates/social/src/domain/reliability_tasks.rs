use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReliabilityTask {
    pub object_kind: String,
    pub object_id: String,
    pub status: String,
    pub attempt_count: i64,
    pub last_attempt_at: Option<i64>,
    pub next_attempt_at: i64,
    pub last_error: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}
