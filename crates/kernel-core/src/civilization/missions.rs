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
    pub claimed_by: Option<String>,
    pub completed_by: Option<String>,
    pub settled_at: Option<i64>,
    pub status: MissionStatus,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MissionBoard {
    missions: BTreeMap<String, CivilMission>,
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
            created_at: Utc::now().timestamp(),
            claimed_by: None,
            completed_by: None,
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
        Ok(mission.clone())
    }

    pub fn complete(&mut self, mission_id: &str, agent_did: &str) -> Result<CivilMission> {
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
        mission.status = MissionStatus::Completed;
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
        mission.settled_at = Some(Utc::now().timestamp());
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
        let completed = board.complete(&mission.mission_id, "agent-z").unwrap();
        assert_eq!(completed.status, MissionStatus::Completed);
        let settled = board.settle(&mission.mission_id).unwrap();
        assert_eq!(settled.status, MissionStatus::Settled);
    }
}
