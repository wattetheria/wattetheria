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
    #[serde(default)]
    pub reputation: i64,
    #[serde(default)]
    pub capacity: i64,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reward: Option<MissionReward>,
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
    /// Display name of the claimer as known when the claim was recorded, so the ledger entry
    /// that pays the bounty can name the executor without a registry lookup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claimed_by_display_name: Option<String>,
    pub completed_by: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_result: Option<Value>,
    pub settled_at: Option<i64>,
    pub status: MissionStatus,
    /// Gateway-ledger escrow holding the Watt bounty. `None` means no Watt is funded: the
    /// mission predates the ledger or carries no bounty. On a `Cancelled` mission a value means
    /// the refund is still pending (the lock outcome was unconfirmed or the refund failed).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub escrow_id: Option<String>,
    /// True once the ledger confirmed the escrow lock. The id is recorded *before* the lock
    /// request goes out, so a crash between the two leaves an unconfirmed escrow that the sweep
    /// resolves against the ledger instead of stranding locked Watt.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub escrow_confirmed: bool,
    /// Unix seconds after which an `Open` or `Claimed` mission is cancelled and its bounty
    /// refunded. `None` never expires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
}

impl CivilMission {
    /// True when the mission has an expiry that `now` (unix seconds) has reached.
    #[must_use]
    pub fn is_expired(&self, now: i64) -> bool {
        self.expires_at.is_some_and(|expires_at| expires_at <= now)
    }

    /// True while an escrow is recorded whose lock the ledger has not confirmed.
    #[must_use]
    pub fn has_unconfirmed_escrow(&self) -> bool {
        self.escrow_id.is_some() && !self.escrow_confirmed
    }
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

    #[must_use]
    pub fn get(
        &self,
        mission_id: &str,
        task_id: &str,
        agent_did: &str,
    ) -> Option<NetworkMissionClaimRecord> {
        self.claims
            .get(&network_claim_key(mission_id, task_id, agent_did))
            .cloned()
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

    pub fn update_status(
        &mut self,
        mission_id: &str,
        task_id: &str,
        agent_did: &str,
        status: &str,
    ) -> Option<NetworkMissionClaimRecord> {
        let status = status.trim();
        if status.is_empty() {
            return None;
        }
        let record = self
            .claims
            .get_mut(&network_claim_key(mission_id, task_id, agent_did))?;
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
            Some(reward),
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
        reward: Option<MissionReward>,
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
            claimed_by_display_name: None,
            completed_by: None,
            completion_result: None,
            settled_at: None,
            status: MissionStatus::Open,
            escrow_id: None,
            escrow_confirmed: false,
            expires_at: None,
        };
        self.missions
            .insert(mission.mission_id.clone(), mission.clone());
        mission
    }

    /// Sets the mission's expiry (unix seconds).
    pub fn set_expiry(
        &mut self,
        mission_id: &str,
        expires_at: Option<i64>,
    ) -> Result<CivilMission> {
        let mission = self
            .missions
            .get_mut(mission_id)
            .context("mission not found")?;
        mission.expires_at = expires_at;
        mission.updated_at = Utc::now().timestamp();
        Ok(mission.clone())
    }

    /// Open or claimed missions whose expiry has passed.
    #[must_use]
    pub fn expired_missions(&self, now: i64) -> Vec<CivilMission> {
        self.missions
            .values()
            .filter(|mission| {
                matches!(mission.status, MissionStatus::Open | MissionStatus::Claimed)
                    && mission.is_expired(now)
            })
            .cloned()
            .collect()
    }

    /// Missions (other than cancelled ones, whose escrow is a pending refund) that recorded an
    /// escrow the ledger never confirmed.
    #[must_use]
    pub fn unconfirmed_escrows(&self) -> Vec<CivilMission> {
        self.missions
            .values()
            .filter(|mission| {
                mission.status != MissionStatus::Cancelled && mission.has_unconfirmed_escrow()
            })
            .cloned()
            .collect()
    }

    /// Forgets the ledger escrow once its Watt has been returned to the publisher (or it turned
    /// out never to have been locked).
    pub fn clear_bounty_escrow(&mut self, mission_id: &str) -> Result<CivilMission> {
        let mission = self
            .missions
            .get_mut(mission_id)
            .context("mission not found")?;
        mission.escrow_id = None;
        mission.escrow_confirmed = false;
        mission.updated_at = Utc::now().timestamp();
        Ok(mission.clone())
    }

    /// Records the escrow about to be requested from the ledger, before the request is sent, so
    /// the escrow is never unknown to the board while the ledger may hold it.
    pub fn record_bounty_escrow_intent(
        &mut self,
        mission_id: &str,
        escrow_id: &str,
    ) -> Result<CivilMission> {
        let mission = self
            .missions
            .get_mut(mission_id)
            .context("mission not found")?;
        mission.escrow_id = Some(escrow_id.to_string());
        mission.escrow_confirmed = false;
        mission.updated_at = Utc::now().timestamp();
        Ok(mission.clone())
    }

    /// Records the ledger escrow that funds the mission bounty as confirmed locked.
    pub fn mark_bounty_escrowed(
        &mut self,
        mission_id: &str,
        escrow_id: &str,
    ) -> Result<CivilMission> {
        let mission = self
            .missions
            .get_mut(mission_id)
            .context("mission not found")?;
        mission.escrow_id = Some(escrow_id.to_string());
        mission.escrow_confirmed = true;
        mission.updated_at = Utc::now().timestamp();
        Ok(mission.clone())
    }

    pub fn claim(&mut self, mission_id: &str, agent_did: &str) -> Result<CivilMission> {
        self.claim_as(mission_id, agent_did, None)
    }

    /// `claim`, also recording the claimer's display name when the caller knows it.
    pub fn claim_as(
        &mut self,
        mission_id: &str,
        agent_did: &str,
        display_name: Option<&str>,
    ) -> Result<CivilMission> {
        let mission = self
            .missions
            .get_mut(mission_id)
            .context("mission not found")?;
        if mission.status != MissionStatus::Open {
            bail!("mission is not open");
        }
        if mission.is_expired(Utc::now().timestamp()) {
            bail!("mission has expired");
        }
        if mission.has_unconfirmed_escrow() {
            bail!("mission bounty escrow is not confirmed");
        }
        mission.claimed_by = Some(agent_did.to_string());
        mission.claimed_by_display_name = display_name
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_owned);
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
        if mission.is_expired(Utc::now().timestamp()) {
            bail!("mission has expired");
        }
        if mission.has_unconfirmed_escrow() {
            bail!("mission bounty escrow is not confirmed");
        }
        mission.completed_by = Some(agent_did.to_string());
        mission.completion_result = result;
        mission.status = MissionStatus::Completed;
        mission.updated_at = Utc::now().timestamp();
        Ok(mission.clone())
    }

    /// Cancels a mission that has not been completed: by its publisher, or because the bounty
    /// escrow could not be locked after publishing, so an unfunded mission never stays open.
    pub fn cancel(&mut self, mission_id: &str) -> Result<CivilMission> {
        let mission = self
            .missions
            .get_mut(mission_id)
            .context("mission not found")?;
        if matches!(
            mission.status,
            MissionStatus::Completed | MissionStatus::Settled
        ) {
            bail!("mission can no longer be cancelled");
        }
        mission.status = MissionStatus::Cancelled;
        mission.updated_at = Utc::now().timestamp();
        Ok(mission.clone())
    }

    /// Cancels a mission whose bounty escrow was recorded but never confirmed by the ledger,
    /// whatever its lifecycle state short of `Settled`: the bounty it promised does not exist,
    /// so it cannot be claimed, completed or settled. The escrow id stays on the mission as a
    /// pending refund until the ledger is checked once more.
    pub fn cancel_unfunded(&mut self, mission_id: &str) -> Result<CivilMission> {
        let mission = self
            .missions
            .get_mut(mission_id)
            .context("mission not found")?;
        if !mission.has_unconfirmed_escrow() {
            bail!("mission escrow is confirmed or absent");
        }
        if mission.status == MissionStatus::Settled {
            bail!("mission is already settled");
        }
        mission.status = MissionStatus::Cancelled;
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

    /// A remote lifecycle update for an open or claimed mission whose expiry has passed cancels
    /// it locally (the escrow id stays on the mission as a pending refund) and reports the
    /// cancelled mission, so an expired bounty can no longer be claimed or paid out.
    fn cancel_if_expired(mission: &mut CivilMission) -> Option<CivilMission> {
        let now = Utc::now().timestamp();
        if matches!(mission.status, MissionStatus::Open | MissionStatus::Claimed)
            && mission.is_expired(now)
        {
            mission.status = MissionStatus::Cancelled;
            mission.updated_at = now;
            return Some(mission.clone());
        }
        None
    }

    /// A claim approval relayed from the network. Only an open mission (or one already claimed
    /// by the same agent) changes; a mission claimed by someone else rejects the update so a
    /// relayed envelope cannot reassign the claim. Completed, settled and cancelled missions are
    /// returned unchanged.
    pub fn apply_remote_claim_approved(
        &mut self,
        mission_id: &str,
        agent_did: &str,
    ) -> Result<Option<CivilMission>> {
        let Some(mission) = self.missions.get_mut(mission_id) else {
            return Ok(None);
        };
        if let Some(cancelled) = Self::cancel_if_expired(mission) {
            return Ok(Some(cancelled));
        }
        let agent_did = agent_did.trim();
        match mission.status {
            MissionStatus::Open => {
                if !agent_did.is_empty() {
                    mission.claimed_by = Some(agent_did.to_owned());
                }
                mission.status = MissionStatus::Claimed;
                mission.updated_at = Utc::now().timestamp();
            }
            MissionStatus::Claimed => Self::ensure_same_claimer(mission, agent_did)?,
            MissionStatus::Completed | MissionStatus::Settled | MissionStatus::Cancelled => {}
        }
        Ok(Some(mission.clone()))
    }

    /// A completion relayed from the network. The reporting agent must be the one that claimed
    /// the mission (an unclaimed mission is claimed by it); anyone else is rejected, so a relayed
    /// envelope cannot make itself the payee. The result is recorded once; a mission that is
    /// already completed or settled keeps its executor and result.
    pub fn apply_remote_completed(
        &mut self,
        mission_id: &str,
        agent_did: &str,
        result: Option<Value>,
    ) -> Result<Option<CivilMission>> {
        let Some(mission) = self.missions.get_mut(mission_id) else {
            return Ok(None);
        };
        if let Some(cancelled) = Self::cancel_if_expired(mission) {
            return Ok(Some(cancelled));
        }
        let agent_did = agent_did.trim();
        match mission.status {
            MissionStatus::Cancelled => {}
            MissionStatus::Completed | MissionStatus::Settled => {
                Self::ensure_same_executor(mission, agent_did)?;
            }
            MissionStatus::Open | MissionStatus::Claimed => {
                Self::ensure_same_claimer(mission, agent_did)?;
                if !agent_did.is_empty() {
                    mission
                        .claimed_by
                        .get_or_insert_with(|| agent_did.to_owned());
                    mission.completed_by = Some(agent_did.to_owned());
                }
                if result.is_some() || mission.completion_result.is_none() {
                    mission.completion_result = result;
                }
                mission.status = MissionStatus::Completed;
                mission.updated_at = Utc::now().timestamp();
            }
        }
        Ok(Some(mission.clone()))
    }

    fn ensure_same_claimer(mission: &CivilMission, agent_did: &str) -> Result<()> {
        match mission.claimed_by.as_deref() {
            Some(claimer) if !agent_did.is_empty() && claimer != agent_did => bail!(
                "mission {} is claimed by {claimer}, not by {agent_did}",
                mission.mission_id
            ),
            _ => Ok(()),
        }
    }

    fn ensure_same_executor(mission: &CivilMission, agent_did: &str) -> Result<()> {
        match mission.completed_by.as_deref() {
            Some(executor) if !agent_did.is_empty() && executor != agent_did => bail!(
                "mission {} was completed by {executor}, not by {agent_did}",
                mission.mission_id
            ),
            _ => Ok(()),
        }
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

    fn publish_simple(board: &mut MissionBoard, title: &str) -> CivilMission {
        board.publish(
            title,
            "desc",
            "publisher",
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
            },
            serde_json::json!({}),
        )
    }

    #[test]
    fn claim_as_records_the_claimer_display_name_and_claim_leaves_it_empty() {
        let mut board = MissionBoard::default();
        let named = publish_simple(&mut board, "named");
        let claimed = board
            .claim_as(&named.mission_id, "agent-a", Some("  Ada  "))
            .unwrap();
        assert_eq!(claimed.claimed_by.as_deref(), Some("agent-a"));
        assert_eq!(claimed.claimed_by_display_name.as_deref(), Some("Ada"));
        let serialized = serde_json::to_value(&claimed).unwrap();
        assert_eq!(serialized["claimed_by_display_name"], "Ada");

        let blank = publish_simple(&mut board, "blank");
        let claimed = board
            .claim_as(&blank.mission_id, "agent-b", Some("   "))
            .unwrap();
        assert_eq!(claimed.claimed_by_display_name, None);
        let plain = publish_simple(&mut board, "plain");
        let claimed = board.claim(&plain.mission_id, "agent-c").unwrap();
        assert_eq!(claimed.claimed_by_display_name, None);
        let serialized = serde_json::to_value(&claimed).unwrap();
        assert!(serialized.get("claimed_by_display_name").is_none());
    }

    #[test]
    fn expired_missions_cannot_be_claimed_or_completed() {
        let mut board = MissionBoard::default();
        let past = Utc::now().timestamp() - 60;
        let open = publish_simple(&mut board, "open-expired");
        board.set_expiry(&open.mission_id, Some(past)).unwrap();
        assert!(
            board
                .claim(&open.mission_id, "agent-a")
                .unwrap_err()
                .to_string()
                .contains("expired")
        );

        let claimed = publish_simple(&mut board, "claimed-expired");
        board.claim(&claimed.mission_id, "agent-b").unwrap();
        board.set_expiry(&claimed.mission_id, Some(past)).unwrap();
        assert!(
            board
                .complete(&claimed.mission_id, "agent-b", None)
                .unwrap_err()
                .to_string()
                .contains("expired")
        );
        assert_eq!(board.expired_missions(Utc::now().timestamp()).len(), 2);

        let future = publish_simple(&mut board, "still-valid");
        board
            .set_expiry(&future.mission_id, Some(Utc::now().timestamp() + 600))
            .unwrap();
        board.claim(&future.mission_id, "agent-c").unwrap();
        board.complete(&future.mission_id, "agent-c", None).unwrap();
    }

    #[test]
    fn remote_lifecycle_updates_cancel_an_expired_mission_and_keep_its_escrow() {
        let mut board = MissionBoard::default();
        let past = Utc::now().timestamp() - 60;
        for (title, kind) in [("claim", 0), ("complete", 1)] {
            let mission = publish_simple(&mut board, title);
            board
                .mark_bounty_escrowed(&mission.mission_id, "escrow-1")
                .unwrap();
            board.set_expiry(&mission.mission_id, Some(past)).unwrap();
            let updated = match kind {
                0 => board.apply_remote_claim_approved(&mission.mission_id, "agent-x"),
                _ => board.apply_remote_completed(&mission.mission_id, "agent-x", None),
            }
            .unwrap()
            .unwrap();
            assert_eq!(updated.status, MissionStatus::Cancelled, "{title}");
            assert_eq!(updated.escrow_id.as_deref(), Some("escrow-1"), "{title}");
            assert!(updated.completed_by.is_none(), "{title}");
        }

        // A mission completed before its expiry still settles.
        let done = publish_simple(&mut board, "done-in-time");
        board.claim(&done.mission_id, "agent-y").unwrap();
        board.complete(&done.mission_id, "agent-y", None).unwrap();
        board.set_expiry(&done.mission_id, Some(past)).unwrap();
        let settled = board.settle(&done.mission_id).unwrap();
        assert_eq!(settled.status, MissionStatus::Settled);
    }

    #[test]
    fn escrow_intent_is_unconfirmed_until_the_ledger_confirms_it() {
        let mut board = MissionBoard::default();
        let mission = publish_simple(&mut board, "escrowed");
        let intent = board
            .record_bounty_escrow_intent(&mission.mission_id, "escrow-a")
            .unwrap();
        assert!(intent.has_unconfirmed_escrow());
        assert_eq!(board.unconfirmed_escrows().len(), 1);

        let confirmed = board
            .mark_bounty_escrowed(&mission.mission_id, "escrow-a")
            .unwrap();
        assert!(confirmed.escrow_confirmed);
        assert_eq!(board.unconfirmed_escrows(), Vec::<CivilMission>::new());

        let cleared = board.clear_bounty_escrow(&mission.mission_id).unwrap();
        assert!(cleared.escrow_id.is_none());
        assert!(!cleared.escrow_confirmed);
        assert!(
            board
                .cancel_unfunded(&mission.mission_id)
                .unwrap_err()
                .to_string()
                .contains("confirmed or absent")
        );

        // Serialized form omits the flag while false and restores it as false.
        let json = serde_json::to_value(&cleared).unwrap();
        assert!(json.get("escrow_confirmed").is_none());
        let restored: CivilMission = serde_json::from_value(json).unwrap();
        assert!(!restored.escrow_confirmed);
    }

    #[test]
    fn unconfirmed_escrow_blocks_claim_and_complete_until_confirmed_or_cancelled() {
        let mut board = MissionBoard::default();
        let mission = publish_simple(&mut board, "unfunded");
        board
            .record_bounty_escrow_intent(&mission.mission_id, "escrow-u")
            .unwrap();
        assert!(
            board
                .claim(&mission.mission_id, "agent-a")
                .unwrap_err()
                .to_string()
                .contains("not confirmed")
        );
        board
            .mark_bounty_escrowed(&mission.mission_id, "escrow-u")
            .unwrap();
        board.claim(&mission.mission_id, "agent-a").unwrap();
        board
            .record_bounty_escrow_intent(&mission.mission_id, "escrow-u")
            .unwrap();
        assert!(
            board
                .complete(&mission.mission_id, "agent-a", None)
                .unwrap_err()
                .to_string()
                .contains("not confirmed")
        );

        // A completed mission (synced remotely) whose escrow never existed can still be
        // cancelled as unfunded; a settled one cannot.
        let completed = board
            .apply_remote_completed(&mission.mission_id, "agent-a", None)
            .unwrap()
            .unwrap();
        assert_eq!(completed.status, MissionStatus::Completed);
        let cancelled = board.cancel_unfunded(&mission.mission_id).unwrap();
        assert_eq!(cancelled.status, MissionStatus::Cancelled);
        assert_eq!(cancelled.escrow_id.as_deref(), Some("escrow-u"));

        let settled = publish_simple(&mut board, "settled");
        board.claim(&settled.mission_id, "agent-b").unwrap();
        board
            .complete(&settled.mission_id, "agent-b", None)
            .unwrap();
        board.settle(&settled.mission_id).unwrap();
        board
            .record_bounty_escrow_intent(&settled.mission_id, "escrow-s")
            .unwrap();
        assert!(board.cancel_unfunded(&settled.mission_id).is_err());
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
            },
            serde_json::json!({"objective": "sync"}),
        );

        let claimed = board
            .apply_remote_claim_approved(&mission.mission_id, "agent-worker")
            .unwrap()
            .unwrap();
        assert_eq!(claimed.status, MissionStatus::Claimed);
        assert_eq!(claimed.claimed_by.as_deref(), Some("agent-worker"));

        let completed = board
            .apply_remote_completed(
                &mission.mission_id,
                "agent-worker",
                Some(serde_json::json!({"ok": true})),
            )
            .unwrap()
            .unwrap();
        assert_eq!(completed.status, MissionStatus::Completed);
        assert_eq!(completed.completed_by.as_deref(), Some("agent-worker"));

        // Settlement only ever happens locally (it releases the bounty); relayed updates
        // afterwards are idempotent.
        let settled = board.settle(&mission.mission_id).unwrap();
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
            .unwrap()
            .unwrap();
        assert_eq!(replayed.status, MissionStatus::Settled);
        let replayed = board
            .apply_remote_claim_approved(&mission.mission_id, "agent-worker")
            .unwrap()
            .unwrap();
        assert_eq!(replayed.status, MissionStatus::Settled);
    }

    #[test]
    fn relayed_lifecycle_updates_from_another_agent_are_rejected() {
        let mut board = MissionBoard::default();
        let mission = publish_simple(&mut board, "guarded");
        board
            .apply_remote_claim_approved(&mission.mission_id, "agent-worker")
            .unwrap();

        // Someone else cannot take over the claim or report the completion.
        assert!(
            board
                .apply_remote_claim_approved(&mission.mission_id, "agent-intruder")
                .is_err()
        );
        assert!(
            board
                .apply_remote_completed(&mission.mission_id, "agent-intruder", None)
                .is_err()
        );
        let stored = board.get(&mission.mission_id).unwrap();
        assert_eq!(stored.status, MissionStatus::Claimed);
        assert_eq!(stored.claimed_by.as_deref(), Some("agent-worker"));
        assert!(stored.completed_by.is_none());

        // The claimer completes; a later report by someone else is rejected and the executor
        // and result are kept.
        board
            .apply_remote_completed(
                &mission.mission_id,
                "agent-worker",
                Some(serde_json::json!({"ok": true})),
            )
            .unwrap();
        assert!(
            board
                .apply_remote_completed(
                    &mission.mission_id,
                    "agent-intruder",
                    Some(serde_json::json!({"ok": false})),
                )
                .is_err()
        );
        let stored = board.get(&mission.mission_id).unwrap();
        assert_eq!(stored.completed_by.as_deref(), Some("agent-worker"));
        assert_eq!(
            stored.completion_result,
            Some(serde_json::json!({"ok": true}))
        );

        // An unknown mission is simply not applied.
        assert!(
            board
                .apply_remote_completed("missing", "agent-worker", None)
                .unwrap()
                .is_none()
        );
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
        let exact = registry.get("mission-1", "task-1", "agent-a").unwrap();
        assert_eq!(exact.execution_id, "exec-1");
        assert!(registry.contains_mission("mission-1"));
        let updated = registry
            .update_status_by_mission("mission-1", "approved")
            .unwrap();
        assert_eq!(updated.status.as_deref(), Some("approved"));
        assert_eq!(updated.metadata.task_status.as_deref(), Some("approved"));
        let updated = registry
            .update_status("mission-1", "task-1", "agent-a", "completed")
            .unwrap();
        assert_eq!(updated.status.as_deref(), Some("completed"));
        assert_eq!(updated.metadata.task_status.as_deref(), Some("completed"));
        assert!(!registry.contains("mission-1", "task-1", "agent-b"));
    }
}
