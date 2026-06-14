use crate::civilization::profiles::{Faction, RolePath};
use crate::types::PublicGeoPayload;
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

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MissionScope {
    #[default]
    RealWorld,
    InWorld,
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
    #[serde(default)]
    pub scope: MissionScope,
    pub subnet_id: Option<String>,
    pub zone_id: Option<String>,
    pub required_role: Option<RolePath>,
    pub required_faction: Option<Faction>,
    pub reward: MissionReward,
    pub payload: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lat: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lng: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coordinate_source: Option<String>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
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
    pub scope: Option<String>,
    #[serde(default, skip_serializing)]
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
        status: Option<String>,
        metadata: NetworkMissionClaimMetadata,
    ) -> NetworkMissionClaimRecord {
        let record = NetworkMissionClaimRecord {
            mission_id: mission_id.to_string(),
            task_id: task_id.to_string(),
            agent_did: agent_did.to_string(),
            execution_id: execution_id.to_string(),
            claimed_at: Utc::now().timestamp(),
            status,
            metadata,
        };
        self.claims.insert(
            network_claim_key(mission_id, task_id, agent_did),
            record.clone(),
        );
        record
    }

    #[must_use]
    pub fn contains_mission(&self, mission_id: &str) -> bool {
        let mission_id = mission_id.trim();
        self.claims
            .values()
            .any(|record| record.mission_id == mission_id)
    }

    pub fn update_status_by_mission(
        &mut self,
        mission_id: &str,
        status: &str,
    ) -> Option<NetworkMissionClaimRecord> {
        let mission_id = mission_id.trim();
        let status = status.trim();
        if mission_id.is_empty() || status.is_empty() {
            return None;
        }
        let key = self
            .claims
            .iter()
            .find_map(|(key, record)| (record.mission_id == mission_id).then(|| key.clone()))?;
        let record = self.claims.get_mut(&key)?;
        record.status = Some(status.to_owned());
        record.metadata.task_status = Some(status.to_owned());
        Some(record.clone())
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
        self.publish_with_scope(
            title,
            description,
            publisher,
            publisher_kind,
            domain,
            MissionScope::default(),
            subnet_id,
            zone_id,
            required_role,
            required_faction,
            reward,
            payload,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn publish_with_scope(
        &mut self,
        title: &str,
        description: &str,
        publisher: &str,
        publisher_kind: MissionPublisherKind,
        domain: MissionDomain,
        scope: MissionScope,
        subnet_id: Option<String>,
        zone_id: Option<String>,
        required_role: Option<RolePath>,
        required_faction: Option<Faction>,
        reward: MissionReward,
        payload: Value,
    ) -> CivilMission {
        self.publish_with_scope_and_geo(
            title,
            description,
            publisher,
            publisher_kind,
            domain,
            scope,
            subnet_id,
            zone_id,
            required_role,
            required_faction,
            reward,
            payload,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn publish_with_scope_and_geo(
        &mut self,
        title: &str,
        description: &str,
        publisher: &str,
        publisher_kind: MissionPublisherKind,
        domain: MissionDomain,
        scope: MissionScope,
        subnet_id: Option<String>,
        zone_id: Option<String>,
        required_role: Option<RolePath>,
        required_faction: Option<Faction>,
        reward: MissionReward,
        payload: Value,
        public_geo: Option<PublicGeoPayload>,
    ) -> CivilMission {
        let now = Utc::now().timestamp();
        let (lat, lng, coordinate_source) = public_geo.map_or((None, None, None), |geo| {
            (Some(geo.lat), Some(geo.lng), Some(geo.coordinate_source))
        });
        let mission = CivilMission {
            mission_id: uuid::Uuid::new_v4().to_string(),
            title: title.to_string(),
            description: description.to_string(),
            publisher: publisher.to_string(),
            publisher_kind,
            domain,
            scope,
            subnet_id,
            zone_id,
            required_role,
            required_faction,
            reward,
            payload,
            lat,
            lng,
            coordinate_source,
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

    pub fn apply_remote_claim_approved(
        &mut self,
        mission_id: &str,
        agent_did: &str,
    ) -> Option<CivilMission> {
        let mission = self.missions.get_mut(mission_id)?;
        if matches!(
            mission.status,
            MissionStatus::Completed | MissionStatus::Settled | MissionStatus::Cancelled
        ) {
            if mission.claimed_by.is_none() && !agent_did.trim().is_empty() {
                mission.claimed_by = Some(agent_did.to_owned());
            }
            return Some(mission.clone());
        }
        if !agent_did.trim().is_empty() {
            mission.claimed_by = Some(agent_did.to_owned());
        }
        mission.status = MissionStatus::Claimed;
        mission.updated_at = Utc::now().timestamp();
        Some(mission.clone())
    }

    pub fn apply_remote_completed(
        &mut self,
        mission_id: &str,
        agent_did: &str,
        result: Option<Value>,
    ) -> Option<CivilMission> {
        let mission = self.missions.get_mut(mission_id)?;
        if mission.status == MissionStatus::Cancelled {
            return Some(mission.clone());
        }
        if !agent_did.trim().is_empty() {
            mission
                .claimed_by
                .get_or_insert_with(|| agent_did.to_owned());
            mission.completed_by = Some(agent_did.to_owned());
        }
        if result.is_some() || mission.completion_result.is_none() {
            mission.completion_result = result;
        }
        if mission.status != MissionStatus::Settled {
            mission.status = MissionStatus::Completed;
            mission.updated_at = Utc::now().timestamp();
        }
        Some(mission.clone())
    }

    pub fn apply_remote_settled(
        &mut self,
        mission_id: &str,
        agent_did: Option<&str>,
        result: Option<Value>,
    ) -> Option<CivilMission> {
        let mission = self.missions.get_mut(mission_id)?;
        if mission.status == MissionStatus::Cancelled {
            return Some(mission.clone());
        }
        if let Some(agent_did) = agent_did
            .map(str::trim)
            .filter(|agent_did| !agent_did.is_empty())
        {
            mission
                .claimed_by
                .get_or_insert_with(|| agent_did.to_owned());
            mission
                .completed_by
                .get_or_insert_with(|| agent_did.to_owned());
        }
        if result.is_some() || mission.completion_result.is_none() {
            mission.completion_result = result;
        }
        mission.status = MissionStatus::Settled;
        let now = Utc::now().timestamp();
        mission.settled_at.get_or_insert(now);
        mission.updated_at = now;
        Some(mission.clone())
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
    fn mission_board_applies_remote_lifecycle_updates_idempotently() {
        let mut board = MissionBoard::default();
        let mission = board.publish(
            "Remote lifecycle",
            "Publisher receives lifecycle topics.",
            "publisher-public",
            MissionPublisherKind::Player,
            MissionDomain::Trade,
            None,
            None,
            None,
            None,
            MissionReward {
                agent_watt: 10,
                reputation: 1,
                capacity: 0,
                treasury_share_watt: 0,
            },
            serde_json::json!({"objective": "sync"}),
        );

        let claimed = board
            .apply_remote_claim_approved(&mission.mission_id, "agent-worker")
            .unwrap();
        assert_eq!(claimed.status, MissionStatus::Claimed);
        assert_eq!(claimed.claimed_by.as_deref(), Some("agent-worker"));

        let completed = board
            .apply_remote_completed(
                &mission.mission_id,
                "agent-worker",
                Some(serde_json::json!({"ok": true})),
            )
            .unwrap();
        assert_eq!(completed.status, MissionStatus::Completed);
        assert_eq!(completed.completed_by.as_deref(), Some("agent-worker"));

        let settled = board
            .apply_remote_settled(&mission.mission_id, Some("agent-worker"), None)
            .unwrap();
        assert_eq!(settled.status, MissionStatus::Settled);
        assert_eq!(settled.completed_by.as_deref(), Some("agent-worker"));
        assert_eq!(
            settled.completion_result,
            Some(serde_json::json!({"ok": true}))
        );

        let replayed = board
            .apply_remote_completed(
                &mission.mission_id,
                "agent-worker",
                Some(serde_json::json!({"ok": true})),
            )
            .unwrap();
        assert_eq!(replayed.status, MissionStatus::Settled);
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
            Some("published".to_string()),
            NetworkMissionClaimMetadata {
                domain: Some("trade".to_string()),
                publisher_id: Some("publisher-public".to_string()),
                reward_watt: Some(10),
                ..NetworkMissionClaimMetadata::default()
            },
        );

        assert_eq!(record.execution_id, "exec-1");
        assert_eq!(record.status.as_deref(), Some("published"));
        assert_eq!(record.metadata.domain.as_deref(), Some("trade"));
        assert_eq!(record.metadata.reward_watt, Some(10));
        assert!(registry.contains("mission-1", "task-1", "agent-a"));
        assert!(registry.contains_mission("mission-1"));
        let updated = registry
            .update_status_by_mission("mission-1", "approved")
            .unwrap();
        assert_eq!(updated.status.as_deref(), Some("approved"));
        assert_eq!(updated.metadata.task_status.as_deref(), Some("approved"));
        assert!(!registry.contains("mission-1", "task-1", "agent-b"));
    }
}
