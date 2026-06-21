use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSkill {
    pub skill_id: String,
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub visible: bool,
    pub source: String,
    pub sort_order: i64,
    pub created_at: i64,
    pub updated_at: i64,
}
