use crate::civilization::profiles::{Faction, RolePath};
use anyhow::{Context, Result, bail};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MissionDomain {
    Wealth,
    Power,
    Security,
    Trade,
    Culture,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MissionPublisherKind {
    Player,
    Organization,
    PlanetaryGovernment,
    NeutralHub,
    System,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MissionStatus {
    Open,
    Claimed,
    Completed,
    Settled,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MissionReward {
    pub agent_watt: i64,
    pub reputation: i64,
    pub capacity: i64,
    pub treasury_share_watt: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CivilMission {
    pub mission_id: String,
    pub title: String,
    pub description: String,
    pub publisher: String,
    pub publisher_kind: MissionPublisherKind,
    pub domain: MissionDomain,
    pub subnet_id: Option<String>,
    pub zone_id: Option<String>,
    pub required_role: Option<RolePath>,
    pub required_faction: Option<Faction>,
    pub reward: MissionReward,
    pub payload: Value,
    pub created_at: i64,
    #[serde(default)]
    pub updated_at: i64,
    pub claimed_by: Option<String>,
    pub completed_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_result: Option<Value>,
    pub settled_at: Option<i64>,
    pub status: MissionStatus,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MissionBoard {
    missions: BTreeMap<String, CivilMission>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NetworkMissionClaimRecord {
    pub mission_id: String,
    pub task_id: String,
    pub agent_did: String,
    pub execution_id: String,
    pub claimed_at: i64,
    #[serde(default)]
    pub metadata: NetworkMissionClaimMetadata,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct NetworkMissionClaimMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publisher_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publisher_agent_did: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publisher_display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publisher_wattswarm_node_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mission_feed_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mission_scope_hint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reward: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reward_watt: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executor_bounty_watt: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publisher_network_reward_watt: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NetworkMissionClaimRegistry {
    claims: BTreeMap<String, NetworkMissionClaimRecord>,
}

impl NetworkMissionClaimRegistry {
    #[must_use]
    pub fn records(&self) -> Vec<NetworkMissionClaimRecord> {
        self.claims.values().cloned().collect()
    }

    #[must_use]
    pub fn contains(&self, mission_id: &str, task_id: &str, agent_did: &str) -> bool {
        self.claims
            .contains_key(&network_claim_key(mission_id, task_id, agent_did))
    }

    pub fn record(
        &mut self,
        mission_id: &str,
        task_id: &str,
        agent_did: &str,
        execution_id: &str,
        metadata: NetworkMissionClaimMetadata,
    ) -> NetworkMissionClaimRecord {
        let record = NetworkMissionClaimRecord {
            mission_id: mission_id.to_string(),
            task_id: task_id.to_string(),
            agent_did: agent_did.to_string(),
            execution_id: execution_id.to_string(),
            claimed_at: Utc::now().timestamp(),
            metadata,
        };
        self.claims.insert(
            network_claim_key(mission_id, task_id, agent_did),
            record.clone(),
        );
        record
    }
}

fn network_claim_key(mission_id: &str, task_id: &str, agent_did: &str) -> String {
    format!(
        "{}:{}:{}",
        mission_id.trim(),
        task_id.trim(),
        agent_did.trim()
    )
}

impl MissionBoard {
    pub fn load_or_new(path: impl AsRef<Path>) -> Result<Self> {
        if let Some(parent) = path.as_ref().parent() {
            fs::create_dir_all(parent).context("create mission board directory")?;
        }
        if !path.as_ref().exists() {
            return Ok(Self::default());
        }
        let raw = fs::read_to_string(path.as_ref()).context("read mission board")?;
        if raw.trim().is_empty() {
            return Ok(Self::default());
        }
        serde_json::from_str(&raw).context("parse mission board")
    }

    pub fn persist(&self, path: impl AsRef<Path>) -> Result<()> {
        if let Some(parent) = path.as_ref().parent() {
            fs::create_dir_all(parent).context("create mission board directory")?;
        }
        fs::write(path.as_ref(), serde_json::to_string_pretty(self)?).context("write mission board")
    }

    #[allow(clippy::too_many_arguments)]
    pub fn publish(
        &mut self,
        title: &str,
        description: &str,
        publisher: &str,
        publisher_kind: MissionPublisherKind,
        domain: MissionDomain,
        subnet_id: Option<String>,
        zone_id: Option<String>,
        required_role: Option<RolePath>,
        required_faction: Option<Faction>,
        reward: MissionReward,
        payload: Value,
    ) -> CivilMission {
        let now = Utc::now().timestamp();
        let mission = CivilMission {
            mission_id: uuid::Uuid::new_v4().to_string(),
            title: title.to_string(),
            description: description.to_string(),
            publisher: publisher.to_string(),
            publisher_kind,
            domain,
            subnet_id,
            zone_id,
            required_role,
            required_faction,
            reward,
            payload,
            created_at: now,
            updated_at: now,
            claimed_by: None,
            completed_by: None,
            completion_result: None,
            settled_at: None,
            status: MissionStatus::Open,
        };
        self.missions
            .insert(mission.mission_id.clone(), mission.clone());
        mission
    }

    pub fn claim(&mut self, mission_id: &str, agent_did: &str) -> Result<CivilMission> {
        let mission = self
            .missions
            .get_mut(mission_id)
            .context("mission not found")?;
        if mission.status != MissionStatus::Open {
            bail!("mission is not open");
        }
        mission.claimed_by = Some(agent_did.to_string());
        mission.status = MissionStatus::Claimed;
        mission.updated_at = Utc::now().timestamp();
        Ok(mission.clone())
    }

    pub fn complete(
        &mut self,
        mission_id: &str,
        agent_did: &str,
        result: Option<Value>,
    ) -> Result<CivilMission> {
        let mission = self
            .missions
            .get_mut(mission_id)
            .context("mission not found")?;
        if mission.status != MissionStatus::Claimed {
            bail!("mission is not claimed");
        }
        if mission.claimed_by.as_deref() != Some(agent_did) {
            bail!("mission claimed by different agent");
        }
        mission.completed_by = Some(agent_did.to_string());
        mission.completion_result = result;
        mission.status = MissionStatus::Completed;
        mission.updated_at = Utc::now().timestamp();
        Ok(mission.clone())
    }

    pub fn settle(&mut self, mission_id: &str) -> Result<CivilMission> {
        let mission = self
            .missions
            .get_mut(mission_id)
            .context("mission not found")?;
        if mission.status != MissionStatus::Completed {
            bail!("mission is not completed");
        }
        mission.status = MissionStatus::Settled;
        let now = Utc::now().timestamp();
        mission.settled_at = Some(now);
        mission.updated_at = now;
        Ok(mission.clone())
    }

    #[must_use]
    pub fn list(&self, status: Option<&MissionStatus>) -> Vec<CivilMission> {
        self.missions
            .values()
            .filter(|mission| status.is_none_or(|expected| &mission.status == expected))
            .cloned()
            .collect()
    }

    #[must_use]
    pub fn get(&self, mission_id: &str) -> Option<CivilMission> {
        self.missions.get(mission_id).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mission_lifecycle_roundtrip() {
        let mut board = MissionBoard::default();
        let mission = board.publish(
            "Escort convoy",
            "Protect freight into frontier belt.",
            "planet-a",
            MissionPublisherKind::PlanetaryGovernment,
            MissionDomain::Security,
            Some("planet-a".to_string()),
            Some("frontier-belt".to_string()),
            Some(RolePath::Enforcer),
            None,
            MissionReward {
                agent_watt: 25,
                reputation: 4,
                capacity: 1,
                treasury_share_watt: 10,
            },
            serde_json::json!({"route":"f1"}),
        );

        let claimed = board.claim(&mission.mission_id, "agent-z").unwrap();
        assert_eq!(claimed.status, MissionStatus::Claimed);
        let completed = board
            .complete(
                &mission.mission_id,
                "agent-z",
                Some(serde_json::json!({"ok": true})),
            )
            .unwrap();
        assert_eq!(completed.status, MissionStatus::Completed);
        assert_eq!(
            completed.completion_result,
            Some(serde_json::json!({"ok": true}))
        );
        let settled = board.settle(&mission.mission_id).unwrap();
        assert_eq!(settled.status, MissionStatus::Settled);
    }

    #[test]
    fn network_mission_claim_registry_records_claims_by_mission_task_and_agent() {
        let mut registry = NetworkMissionClaimRegistry::default();
        assert!(!registry.contains("mission-1", "task-1", "agent-a"));

        let record = registry.record(
            "mission-1",
            "task-1",
            "agent-a",
            "exec-1",
            NetworkMissionClaimMetadata {
                domain: Some("trade".to_string()),
                publisher_id: Some("publisher-public".to_string()),
                reward_watt: Some(10),
                ..NetworkMissionClaimMetadata::default()
            },
        );

        assert_eq!(record.execution_id, "exec-1");
        assert_eq!(record.metadata.domain.as_deref(), Some("trade"));
        assert_eq!(record.metadata.reward_watt, Some(10));
        assert!(registry.contains("mission-1", "task-1", "agent-a"));
        assert!(!registry.contains("mission-1", "task-1", "agent-b"));
    }
}
