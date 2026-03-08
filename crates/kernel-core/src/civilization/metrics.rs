use crate::civilization::galaxy::{DynamicEventCategory, GalaxyState};
use crate::civilization::missions::{MissionBoard, MissionDomain, MissionStatus};
use crate::civilization::profiles::{CitizenRegistry, Faction, RolePath};
use crate::governance::GovernanceEngine;
use crate::types::AgentStats;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CivilizationScores {
    pub wealth: i64,
    pub power: i64,
    pub security: i64,
    pub trade: i64,
    pub culture: i64,
    pub total_influence: i64,
}

#[must_use]
pub fn compute_scores(
    agent_id: &str,
    stats: &AgentStats,
    missions: &MissionBoard,
    profiles: &CitizenRegistry,
    governance: &GovernanceEngine,
    galaxy: &GalaxyState,
) -> CivilizationScores {
    let settled_status = MissionStatus::Settled;
    let settled = missions.list(Some(&settled_status));
    let by_agent: Vec<_> = settled
        .into_iter()
        .filter(|mission| mission.completed_by.as_deref() == Some(agent_id))
        .collect();

    let domain_score = |domain: MissionDomain| -> i64 {
        by_agent
            .iter()
            .filter(|mission| mission.domain == domain)
            .map(|mission| mission.reward.agent_watt + mission.reward.reputation * 2)
            .sum()
    };

    let planets = governance.list_planets();
    let created_planets = i64::try_from(
        planets
            .iter()
            .filter(|planet| planet.creator == agent_id)
            .count(),
    )
    .unwrap_or(i64::MAX);
    let validator_planets = i64::try_from(
        planets
            .iter()
            .filter(|planet| planet.validators.contains(agent_id))
            .count(),
    )
    .unwrap_or(i64::MAX);

    let profile = profiles.profile(agent_id);
    let role_bonus = match profile.as_ref().map(|profile| &profile.role) {
        Some(RolePath::Operator | RolePath::Broker) => 10,
        Some(RolePath::Enforcer) => 12,
        Some(RolePath::Artificer) => 8,
        None => 0,
    };
    let faction_bonus = match profile.as_ref().map(|profile| &profile.faction) {
        Some(Faction::Order | Faction::Freeport | Faction::Raider) => 8,
        None => 0,
    };

    let event_pressure = galaxy
        .events(None)
        .into_iter()
        .filter(|event| event.severity >= 6)
        .fold((0_i64, 0_i64, 0_i64), |mut acc, event| {
            match event.category {
                DynamicEventCategory::Economic => acc.1 += i64::from(event.severity),
                DynamicEventCategory::Spatial => acc.0 += i64::from(event.severity),
                DynamicEventCategory::Political => acc.2 += i64::from(event.severity),
            }
            acc
        });

    let wealth =
        stats.watt + stats.capacity * 4 + stats.power * 10 + domain_score(MissionDomain::Wealth);
    let power = stats.power * 20
        + stats.reputation * 5
        + created_planets * 40
        + validator_planets * 20
        + event_pressure.2
        + role_bonus;
    let security = domain_score(MissionDomain::Security)
        + validator_planets * 10
        + event_pressure.0
        + faction_bonus;
    let trade = domain_score(MissionDomain::Trade) + stats.watt / 4 + role_bonus + event_pressure.1;
    let culture = domain_score(MissionDomain::Culture)
        + stats.reputation * 4
        + created_planets * 5
        + faction_bonus;
    let total_influence = wealth + power + security + trade + culture;

    CivilizationScores {
        wealth,
        power,
        security,
        trade,
        culture,
        total_influence,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::civilization::galaxy::{DynamicEventCategory, GalaxyState};
    use crate::civilization::missions::{MissionPublisherKind, MissionReward};
    use crate::civilization::profiles::{CitizenRegistry, StrategyProfile};

    #[test]
    fn metrics_include_profiles_missions_and_governance() {
        let mut missions = MissionBoard::default();
        let mission = missions.publish(
            "Run exchange",
            "Stabilize local market.",
            "planet-a",
            MissionPublisherKind::Organization,
            MissionDomain::Trade,
            Some("planet-a".to_string()),
            Some("genesis-core".to_string()),
            Some(RolePath::Broker),
            Some(Faction::Freeport),
            MissionReward {
                agent_watt: 20,
                reputation: 3,
                capacity: 0,
                treasury_share_watt: 5,
            },
            serde_json::json!({}),
        );
        missions.claim(&mission.mission_id, "agent-a").unwrap();
        missions.complete(&mission.mission_id, "agent-a").unwrap();
        missions.settle(&mission.mission_id).unwrap();

        let mut profiles = CitizenRegistry::default();
        profiles.set_profile(
            "agent-a",
            Faction::Freeport,
            RolePath::Broker,
            StrategyProfile::Balanced,
            Some("planet-a".to_string()),
            Some("genesis-core".to_string()),
        );

        let mut governance = GovernanceEngine::default();
        governance.issue_license("agent-a", "agent-a", "proof", 7);
        governance.lock_bond("agent-a", 100, 30);

        let mut galaxy = GalaxyState::default_with_core_zones();
        galaxy
            .publish_event(
                DynamicEventCategory::Economic,
                "genesis-core",
                "Liquidity crunch",
                "Markets tightened.",
                7,
                None,
                vec!["trade".to_string()],
            )
            .unwrap();

        let scores = compute_scores(
            "agent-a",
            &AgentStats {
                power: 5,
                watt: 100,
                reputation: 10,
                capacity: 8,
            },
            &missions,
            &profiles,
            &governance,
            &galaxy,
        );

        assert!(scores.wealth > 0);
        assert!(scores.trade > 0);
        assert!(scores.total_influence >= scores.wealth);
    }
}
