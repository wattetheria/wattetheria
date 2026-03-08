use crate::civilization::missions::{CivilMission, MissionBoard, MissionStatus};
use crate::civilization::profiles::CitizenProfile;
use crate::governance::{GovernanceEngine, GovernmentStatus};
use crate::map::model::GalaxyMap;
use crate::map::state::{TravelPosition, TravelSession};
use crate::map::travel::TravelRiskLevel;
use chrono::Utc;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TravelMissionHighlight {
    pub mission_id: String,
    pub title: String,
    pub domain: crate::civilization::missions::MissionDomain,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TravelMissionImpact {
    pub open_local_count: usize,
    pub eligible_local_count: usize,
    pub highlighted: Vec<TravelMissionHighlight>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TravelGovernanceImpact {
    pub subnet_id: String,
    pub planet_name: String,
    pub government_status: GovernmentStatus,
    pub stability: i64,
    pub treasury_watt: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TravelConsequence {
    pub consequence_id: String,
    pub public_id: String,
    pub map_id: String,
    pub system_id: String,
    pub planet_id: Option<String>,
    pub zone_id: Option<String>,
    pub route_risk_level: TravelRiskLevel,
    pub route_total_risk: u32,
    pub warning_codes: Vec<String>,
    pub mission_impact: TravelMissionImpact,
    pub governance_impact: Option<TravelGovernanceImpact>,
    pub summary: String,
    pub created_at: i64,
}

#[must_use]
pub fn evaluate_arrival_consequence(
    public_id: &str,
    map: &GalaxyMap,
    position: &TravelPosition,
    session: &TravelSession,
    profile: Option<&CitizenProfile>,
    missions: &MissionBoard,
    governance: &GovernanceEngine,
) -> TravelConsequence {
    let local_open = missions
        .list(Some(&MissionStatus::Open))
        .into_iter()
        .filter(|mission| mission_matches_destination(mission, position))
        .collect::<Vec<_>>();
    let eligible_local = local_open
        .iter()
        .filter(|mission| mission_matches_profile(mission, profile))
        .collect::<Vec<_>>();
    let governance_impact = position
        .planet_id
        .as_deref()
        .and_then(|planet_id| find_subnet_for_planet(map, planet_id))
        .and_then(|subnet_id| governance.planet(subnet_id))
        .map(|planet| TravelGovernanceImpact {
            subnet_id: planet.subnet_id.clone(),
            planet_name: planet.name.clone(),
            government_status: planet.government_status.clone(),
            stability: planet.stability,
            treasury_watt: planet.treasury_watt,
        });
    let highlighted = eligible_local
        .iter()
        .take(3)
        .map(|mission| TravelMissionHighlight {
            mission_id: mission.mission_id.clone(),
            title: mission.title.clone(),
            domain: mission.domain.clone(),
        })
        .collect::<Vec<_>>();
    let mission_impact = TravelMissionImpact {
        open_local_count: local_open.len(),
        eligible_local_count: eligible_local.len(),
        highlighted,
    };
    let warning_codes = session
        .plan
        .warnings
        .iter()
        .map(|warning| warning.code.clone())
        .collect::<Vec<_>>();
    let summary = build_summary(
        map,
        position,
        &session.plan.risk_level,
        &mission_impact,
        governance_impact.as_ref(),
    );

    TravelConsequence {
        consequence_id: format!("travel-effect-{public_id}-{}", Utc::now().timestamp()),
        public_id: public_id.to_string(),
        map_id: position.map_id.clone(),
        system_id: position.system_id.clone(),
        planet_id: position.planet_id.clone(),
        zone_id: position.zone_id.clone(),
        route_risk_level: session.plan.risk_level.clone(),
        route_total_risk: session.plan.total_risk,
        warning_codes,
        mission_impact,
        governance_impact,
        summary,
        created_at: Utc::now().timestamp(),
    }
}

fn mission_matches_destination(mission: &CivilMission, position: &TravelPosition) -> bool {
    mission.subnet_id.as_deref() == position.planet_id.as_deref()
        || mission.zone_id.as_deref() == position.zone_id.as_deref()
}

fn mission_matches_profile(mission: &CivilMission, profile: Option<&CitizenProfile>) -> bool {
    let role_ok = mission
        .required_role
        .as_ref()
        .is_none_or(|required| profile.is_some_and(|profile| &profile.role == required));
    let faction_ok = mission
        .required_faction
        .as_ref()
        .is_none_or(|required| profile.is_some_and(|profile| &profile.faction == required));
    role_ok && faction_ok
}

fn find_subnet_for_planet<'a>(map: &'a GalaxyMap, planet_id: &str) -> Option<&'a str> {
    map.systems
        .iter()
        .flat_map(|system| system.planets.iter())
        .find(|planet| planet.planet_id == planet_id)
        .and_then(|planet| planet.subnet_id.as_deref())
}

fn build_summary(
    map: &GalaxyMap,
    position: &TravelPosition,
    risk_level: &TravelRiskLevel,
    mission_impact: &TravelMissionImpact,
    governance_impact: Option<&TravelGovernanceImpact>,
) -> String {
    let system_name = map
        .systems
        .iter()
        .find(|system| system.system_id == position.system_id)
        .map_or_else(|| position.system_id.clone(), |system| system.name.clone());
    let risk_label = match risk_level {
        TravelRiskLevel::Stable => "stable",
        TravelRiskLevel::Guarded => "guarded",
        TravelRiskLevel::Contested => "contested",
        TravelRiskLevel::Volatile => "volatile",
    };
    let mission_clause = format!(
        "{} eligible local missions are now reachable in {}",
        mission_impact.eligible_local_count, system_name
    );
    let governance_clause = governance_impact.map_or_else(
        || "no governed subnet is anchored to the current landing point".to_string(),
        |impact| {
            format!(
                "{} is {} with stability {}",
                impact.planet_name,
                government_status_label(&impact.government_status),
                impact.stability
            )
        },
    );
    format!("{mission_clause}; route conditions were {risk_label}; {governance_clause}.")
}

fn government_status_label(status: &GovernmentStatus) -> &'static str {
    match status {
        GovernmentStatus::Active => "active",
        GovernmentStatus::Recall => "under recall",
        GovernmentStatus::Custody => "in custody",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::civilization::galaxy::GalaxyState;
    use crate::civilization::missions::{MissionDomain, MissionPublisherKind, MissionReward};
    use crate::civilization::profiles::{CitizenRegistry, Faction, RolePath, StrategyProfile};
    use crate::governance::{GovernanceEngine, PlanetConstitutionTemplate, PlanetCreationRequest};
    use crate::identity::Identity;
    use crate::map::model::default_genesis_map;
    use crate::map::state::resolve_system_position;
    use crate::map::travel::travel_plan;

    #[test]
    #[allow(clippy::too_many_lines)]
    fn arrival_consequence_summarizes_local_missions_and_governance() {
        let map = default_genesis_map();
        let mut missions = MissionBoard::default();
        missions.publish(
            "Frontier liquidity",
            "Stabilize the frontier exchange",
            "planet-test",
            MissionPublisherKind::PlanetaryGovernment,
            MissionDomain::Trade,
            Some("planet-test".to_string()),
            Some("frontier-belt".to_string()),
            Some(RolePath::Broker),
            Some(Faction::Freeport),
            MissionReward {
                agent_watt: 10,
                reputation: 2,
                capacity: 1,
                treasury_share_watt: 1,
            },
            serde_json::json!({}),
        );
        let mut governance = GovernanceEngine::default();
        let creator = Identity::new_random();
        let signer_a = Identity::new_random();
        let signer_b = Identity::new_random();
        let now = Utc::now().timestamp();
        governance.issue_license(&creator.agent_id, &creator.agent_id, "proof", 7);
        governance.lock_bond(&creator.agent_id, 100, 30);
        let approvals = vec![
            GovernanceEngine::sign_genesis(
                "planet-test",
                "Planet Test",
                &creator.agent_id,
                now,
                &signer_a,
            )
            .unwrap(),
            GovernanceEngine::sign_genesis(
                "planet-test",
                "Planet Test",
                &creator.agent_id,
                now,
                &signer_b,
            )
            .unwrap(),
        ];
        governance
            .create_planet(
                &PlanetCreationRequest {
                    subnet_id: "planet-test".to_string(),
                    name: "Planet Test".to_string(),
                    creator: creator.agent_id.clone(),
                    created_at: now,
                    tax_rate: 0.04,
                    constitution_template: PlanetConstitutionTemplate::CorporateCharter,
                    min_bond: 50,
                    min_approvals: 2,
                },
                &approvals,
            )
            .unwrap();
        let mut profiles = CitizenRegistry::default();
        profiles.set_profile(
            "agent-a",
            Faction::Freeport,
            RolePath::Broker,
            StrategyProfile::Balanced,
            Some("planet-test".to_string()),
            Some("frontier-belt".to_string()),
        );
        let profile = profiles.profile("agent-a");
        let session = TravelSession {
            session_id: "travel-1".to_string(),
            map_id: map.map_id.clone(),
            from_system_id: "genesis-prime".to_string(),
            to_system_id: "frontier-gate".to_string(),
            plan: travel_plan(
                &map,
                &GalaxyState::default_with_core_zones(),
                "genesis-prime",
                "frontier-gate",
            )
            .unwrap(),
            status: crate::map::state::TravelSessionStatus::InTransit,
            departed_at: now,
            updated_at: now,
        };
        let position = resolve_system_position(&map, "frontier-gate").unwrap();

        let consequence = evaluate_arrival_consequence(
            "captain-aurora",
            &map,
            &position,
            &session,
            profile.as_ref(),
            &missions,
            &governance,
        );

        assert_eq!(consequence.system_id, "frontier-gate");
        assert_eq!(consequence.mission_impact.eligible_local_count, 1);
        assert_eq!(
            consequence
                .governance_impact
                .as_ref()
                .map(|impact| impact.subnet_id.as_str()),
            Some("planet-test")
        );
        assert!(consequence.summary.contains("eligible local missions"));
    }
}
