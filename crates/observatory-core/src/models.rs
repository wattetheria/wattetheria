use serde::{Deserialize, Serialize};
use wattetheria_kernel::types::TaskStats;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeatPoint {
    pub subnet_id: String,
    pub active_agents: usize,
    pub total_watt: i64,
    pub total_power: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RankingEntry {
    pub agent_did: String,
    pub subnet_id: String,
    pub metric: String,
    pub value: i64,
    pub power: i64,
    pub watt: i64,
    pub reputation: i64,
    pub capacity: i64,
    pub task_stats: TaskStats,
    pub timestamp: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventStreamEntry {
    pub timestamp: i64,
    pub agent_did: String,
    pub subnet_id: String,
    pub events_digest: String,
    pub watt: i64,
    pub power: i64,
    pub reputation: i64,
    pub capacity: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlanetHealthEntry {
    pub subnet_id: String,
    pub active_agents: usize,
    pub total_watt: i64,
    pub total_power: i64,
    pub total_contribution: i64,
    pub avg_success_rate: f64,
    pub health_score: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreConfig {
    pub max_entries: usize,
    pub max_entry_age_sec: i64,
    pub max_ingest_per_agent_per_minute: usize,
}

impl Default for StoreConfig {
    fn default() -> Self {
        Self {
            max_entries: 10_000,
            max_entry_age_sec: 7 * 24 * 3600,
            max_ingest_per_agent_per_minute: 30,
        }
    }
}

impl StoreConfig {
    #[must_use]
    pub fn from_env() -> Self {
        let default = Self::default();
        let max_entries = std::env::var("OBS_MAX_ENTRIES")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(default.max_entries);

        let max_entry_age_sec = std::env::var("OBS_MAX_AGE_SEC")
            .ok()
            .and_then(|v| v.parse::<i64>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(default.max_entry_age_sec);

        let max_ingest_per_agent_per_minute = std::env::var("OBS_MAX_INGEST_PER_AGENT_MIN")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(default.max_ingest_per_agent_per_minute);

        Self {
            max_entries,
            max_entry_age_sec,
            max_ingest_per_agent_per_minute,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct IngestResponse {
    pub accepted: bool,
    pub total: usize,
    pub ingested: usize,
}
