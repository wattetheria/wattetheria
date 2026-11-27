use crate::civilization::profiles::{Faction, RolePath};
use crate::types::PublicGeoPayload;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

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
    /// The place the coordinates stand for, when the publishing node knew it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub city: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
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
    /// Unix seconds after which an `Open` or `Claimed` mission ends unsettled. `None` never
    /// expires.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
    /// DID of the one agent the publisher named to take the mission. `None` leaves it open to
    /// anyone. The name is resolved once, at publishing, so a later rename changes nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee_did: Option<String>,
    /// The named agent's display name as the publisher wrote it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee_display_name: Option<String>,
}

impl CivilMission {
    /// A new mission as its publisher drafts it. It has no id yet: whoever records it names it,
    /// the gateway for a mission published to the network, the local board for one kept here.
    #[allow(clippy::too_many_arguments)]
    #[must_use]
    pub fn draft(
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
    ) -> Self {
        let now = Utc::now().timestamp();
        let (lat, lng, coordinate_source, city, country) =
            public_geo.map_or((None, None, None, None, None), |geo| {
                (
                    Some(geo.lat),
                    Some(geo.lng),
                    Some(geo.coordinate_source),
                    geo.city,
                    geo.country,
                )
            });
        Self {
            mission_id: String::new(),
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
            city,
            country,
            created_at: now,
            updated_at: now,
            claimed_by: None,
            claimed_by_display_name: None,
            completed_by: None,
            completion_result: None,
            settled_at: None,
            status: MissionStatus::Open,
            expires_at: None,
            assignee_did: None,
            assignee_display_name: None,
        }
    }

    /// Whether `agent_did` may take the mission: anyone, unless the publisher named one agent.
    #[must_use]
    pub fn allows_claimer(&self, agent_did: &str) -> bool {
        self.assignee_did
            .as_deref()
            .is_none_or(|assignee| assignee == agent_did.trim())
    }

    /// True when the mission has an expiry that `now` (unix seconds) has reached.
    #[must_use]
    pub fn is_expired(&self, now: i64) -> bool {
        self.expires_at.is_some_and(|expires_at| expires_at <= now)
    }
}

/// Missions as the gateway records them, gathered for one read. The node keeps no missions of
/// its own: every view is built from the gateway's records and dropped after use.
#[derive(Debug, Clone, Default)]
pub struct MissionBoard {
    missions: BTreeMap<String, CivilMission>,
    /// What the gateway added up for one agent, by its DID. A view lists only the newest
    /// missions, so an agent's whole record cannot be counted from the list.
    completed: Option<(String, CompletedMissions)>,
}

/// What the settled missions an agent carried out add up to.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CompletedTotals {
    pub missions: usize,
    pub watt: i64,
    pub reputation: i64,
    pub capacity: i64,
}

impl CompletedTotals {
    fn add(&mut self, other: Self) {
        self.missions += other.missions;
        self.watt += other.watt;
        self.reputation += other.reputation;
        self.capacity += other.capacity;
    }
}

/// Every settled mission one agent carried out, added up per domain.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CompletedMissions {
    by_domain: Vec<(MissionDomain, CompletedTotals)>,
}

impl CompletedMissions {
    pub fn add(&mut self, domain: MissionDomain, totals: CompletedTotals) {
        match self
            .by_domain
            .iter_mut()
            .find(|(known, _)| *known == domain)
        {
            Some((_, sum)) => sum.add(totals),
            None => self.by_domain.push((domain, totals)),
        }
    }

    /// The totals of one domain.
    #[must_use]
    pub fn domain(&self, domain: &MissionDomain) -> CompletedTotals {
        self.by_domain
            .iter()
            .find(|(known, _)| known == domain)
            .map(|(_, totals)| *totals)
            .unwrap_or_default()
    }

    /// The totals of every domain together.
    #[must_use]
    pub fn all(&self) -> CompletedTotals {
        let mut all = CompletedTotals::default();
        for (_, totals) in &self.by_domain {
            all.add(*totals);
        }
        all
    }
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
    pub scope: Option<String>,
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

/// How this node reaches the publishers of the missions its agent asked to take: the wattswarm
/// task and execution each claim runs on. Where a mission stands is the gateway's to say.
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

    /// Drops what this node kept about `agent_did`'s claims on the mission, once the agent
    /// gave the mission up.
    pub fn forget(&mut self, mission_id: &str, agent_did: &str) {
        self.claims
            .retain(|_, record| record.mission_id != mission_id || record.agent_did != agent_did);
    }

    #[must_use]
    pub fn contains_mission(&self, mission_id: &str) -> bool {
        let mission_id = mission_id.trim();
        self.claims
            .values()
            .any(|record| record.mission_id == mission_id)
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
    #[must_use]
    pub fn from_missions(missions: impl IntoIterator<Item = CivilMission>) -> Self {
        Self {
            missions: missions
                .into_iter()
                .map(|mission| (mission.mission_id.clone(), mission))
                .collect(),
            completed: None,
        }
    }

    /// Keeps the gateway's totals of everything `agent_did` completed, which then answer
    /// [`Self::completed_by`] for that agent in place of the listed missions.
    #[must_use]
    pub fn with_completed(mut self, agent_did: &str, completed: CompletedMissions) -> Self {
        self.completed = Some((agent_did.to_owned(), completed));
        self
    }

    /// What the settled missions `agent_did` carried out add up to: the gateway's totals when
    /// this view holds them for that agent, else the listed missions added up.
    #[must_use]
    pub fn completed_by(&self, agent_did: &str) -> CompletedMissions {
        if let Some((known, completed)) = &self.completed
            && known == agent_did
        {
            return completed.clone();
        }
        let mut completed = CompletedMissions::default();
        for mission in self.missions.values().filter(|mission| {
            mission.status == MissionStatus::Settled
                && mission.completed_by.as_deref() == Some(agent_did)
        }) {
            let reward = mission.reward.clone().unwrap_or(MissionReward {
                agent_watt: 0,
                reputation: 0,
                capacity: 0,
            });
            completed.add(
                mission.domain.clone(),
                CompletedTotals {
                    missions: 1,
                    watt: reward.agent_watt,
                    reputation: reward.reputation,
                    capacity: reward.capacity,
                },
            );
        }
        completed
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

/// Builders for tests: they put missions into a view the way the gateway would record them.
#[cfg(any(test, feature = "test-fixtures"))]
impl MissionBoard {
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
        let mut mission = CivilMission::draft(
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
        );
        mission.mission_id = uuid::Uuid::new_v4().to_string();
        self.insert(mission)
    }

    pub fn insert(&mut self, mission: CivilMission) -> CivilMission {
        self.missions
            .insert(mission.mission_id.clone(), mission.clone());
        mission
    }

    fn update(
        &mut self,
        mission_id: &str,
        change: impl FnOnce(&mut CivilMission),
    ) -> Option<CivilMission> {
        let mission = self.missions.get_mut(mission_id)?;
        change(mission);
        mission.updated_at = Utc::now().timestamp();
        Some(mission.clone())
    }

    pub fn set_assignee(
        &mut self,
        mission_id: &str,
        assignee_did: &str,
        display_name: Option<&str>,
    ) -> Option<CivilMission> {
        self.update(mission_id, |mission| {
            mission.assignee_did = Some(assignee_did.trim().to_owned());
            mission.assignee_display_name = display_name.map(str::to_owned);
        })
    }

    pub fn claim(&mut self, mission_id: &str, agent_did: &str) -> Option<CivilMission> {
        self.claim_as(mission_id, agent_did, None)
    }

    pub fn claim_as(
        &mut self,
        mission_id: &str,
        agent_did: &str,
        display_name: Option<&str>,
    ) -> Option<CivilMission> {
        self.update(mission_id, |mission| {
            mission.claimed_by = Some(agent_did.to_owned());
            mission.claimed_by_display_name = display_name.map(str::to_owned);
            mission.status = MissionStatus::Claimed;
        })
    }

    pub fn complete(
        &mut self,
        mission_id: &str,
        agent_did: &str,
        result: Option<Value>,
    ) -> Option<CivilMission> {
        self.update(mission_id, |mission| {
            mission.completed_by = Some(agent_did.to_owned());
            mission.completion_result = result;
            mission.status = MissionStatus::Completed;
        })
    }

    pub fn cancel(&mut self, mission_id: &str) -> Option<CivilMission> {
        self.update(mission_id, |mission| {
            mission.status = MissionStatus::Cancelled;
        })
    }

    pub fn settle(&mut self, mission_id: &str) -> Option<CivilMission> {
        self.update(mission_id, |mission| {
            mission.status = MissionStatus::Settled;
            mission.settled_at = Some(Utc::now().timestamp());
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_draft_carries_the_place_its_publishing_node_named() {
        let placed = CivilMission::draft(
            "placed",
            "desc",
            "publisher",
            MissionPublisherKind::Player,
            MissionDomain::Trade,
            MissionScope::InWorld,
            None,
            None,
            None,
            None,
            None,
            serde_json::json!({}),
            Some(crate::types::PublicGeoPayload {
                lat: -33.8399,
                lng: 151.0583,
                coordinate_source: "ip_api".to_owned(),
                city: Some("Sydney".to_owned()),
                country: Some("Australia".to_owned()),
            }),
        );
        assert_eq!(placed.status, MissionStatus::Open);
        let serialized = serde_json::to_value(&placed).unwrap();
        assert_eq!(serialized["city"], "Sydney", "{serialized}");
        assert_eq!(serialized["country"], "Australia");
        assert!(serialized.get("escrow_id").is_none(), "{serialized}");
    }

    #[test]
    fn a_mission_reserved_for_one_agent_allows_only_that_agent() {
        let mut board = MissionBoard::default();
        let open = board.publish(
            "open",
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
        );
        assert!(open.allows_claimer("agent-a"));
        let reserved = board
            .set_assignee(&open.mission_id, " agent-b ", Some("Bea"))
            .unwrap();
        assert!(!reserved.allows_claimer("agent-a"));
        assert!(reserved.allows_claimer("agent-b"));
        assert_eq!(MissionBoard::from_missions([reserved]).list(None).len(), 1);
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
        let exact = registry.get("mission-1", "task-1", "agent-a").unwrap();
        assert_eq!(exact.execution_id, "exec-1");
        assert!(registry.contains_mission("mission-1"));
        assert!(!registry.contains("mission-1", "task-1", "agent-b"));
    }
}
