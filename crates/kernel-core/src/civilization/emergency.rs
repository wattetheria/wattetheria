use crate::civilization::galaxy::{DynamicEvent, DynamicEventCategory, GalaxyState};
use crate::civilization::missions::{MissionBoard, MissionDomain, MissionStatus};
use crate::civilization::profiles::{CitizenRegistry, strategy_directive};
use crate::governance::{GovernanceEngine, GovernmentStatus};
use anyhow::Result;
use chrono::Utc;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EmergencyKind {
    GalaxyEvent,
    GovernanceInstability,
    RecallElection,
    Custody,
    SecurityMission,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EmergencyState {
    pub emergency_id: String,
    pub kind: EmergencyKind,
    pub severity: u8,
    pub title: String,
    pub description: String,
    pub agent_id: String,
    pub subnet_id: Option<String>,
    pub zone_id: Option<String>,
    pub requires_human: bool,
    pub created_at: i64,
}

#[must_use]
pub fn evaluate_emergencies(
    agent_id: &str,
    profiles: &CitizenRegistry,
    missions: &MissionBoard,
    governance: &GovernanceEngine,
    galaxy: &GalaxyState,
) -> Vec<EmergencyState> {
    let Some(profile) = profiles.profile(agent_id) else {
        return Vec::new();
    };
    let directive = strategy_directive(&profile.strategy);
    let mut emergencies = Vec::new();

    if let Some(home_subnet_id) = profile.home_subnet_id.as_deref()
        && let Some(planet) = governance.planet(home_subnet_id)
    {
        if planet.government_status == GovernmentStatus::Custody {
            emergencies.push(build_emergency(EmergencySpec {
                agent_id: agent_id.to_string(),
                kind: EmergencyKind::Custody,
                severity: 5,
                title: "Planet placed into custody".to_string(),
                description: "Local sovereignty failed and neutral administration was activated."
                    .to_string(),
                subnet_id: Some(home_subnet_id.to_string()),
                zone_id: profile.home_zone_id.clone(),
                requires_human: true,
            }));
        } else if planet.government_status == GovernmentStatus::Recall {
            emergencies.push(build_emergency(EmergencySpec {
                agent_id: agent_id.to_string(),
                kind: EmergencyKind::RecallElection,
                severity: 4,
                title: "Recall triggered".to_string(),
                description:
                    "Planet stability fell below threshold and leadership is under recall."
                        .to_string(),
                subnet_id: Some(home_subnet_id.to_string()),
                zone_id: profile.home_zone_id.clone(),
                requires_human: true,
            }));
        } else if planet.stability <= 30 {
            emergencies.push(build_emergency(EmergencySpec {
                agent_id: agent_id.to_string(),
                kind: EmergencyKind::GovernanceInstability,
                severity: 4,
                title: "Planet stability critical".to_string(),
                description: "Treasury or public order degraded below safe operating threshold."
                    .to_string(),
                subnet_id: Some(home_subnet_id.to_string()),
                zone_id: profile.home_zone_id.clone(),
                requires_human: directive.emergency_recall_threshold <= 4,
            }));
        }
    }

    if let Some(home_zone_id) = profile.home_zone_id.as_deref() {
        for event in galaxy.events(Some(home_zone_id)) {
            if event.severity < directive.emergency_recall_threshold {
                continue;
            }
            emergencies.push(build_emergency(EmergencySpec {
                agent_id: agent_id.to_string(),
                kind: EmergencyKind::GalaxyEvent,
                severity: event.severity,
                title: event.title,
                description: event.description,
                subnet_id: profile.home_subnet_id.clone(),
                zone_id: Some(event.zone_id),
                requires_human: event.severity >= directive.emergency_recall_threshold,
            }));
        }
    }

    for mission in missions.list(Some(&MissionStatus::Open)) {
        let relevant_subnet = mission.subnet_id.as_ref() == profile.home_subnet_id.as_ref();
        let relevant_zone = mission.zone_id.as_ref() == profile.home_zone_id.as_ref();
        let urgent_domain = matches!(
            mission.domain,
            MissionDomain::Security | MissionDomain::Power
        );
        if urgent_domain && (relevant_subnet || relevant_zone) {
            emergencies.push(build_emergency(EmergencySpec {
                agent_id: agent_id.to_string(),
                kind: EmergencyKind::SecurityMission,
                severity: 3,
                title: mission.title,
                description: "An urgent open mission matches the home zone or subnet.".to_string(),
                subnet_id: mission.subnet_id,
                zone_id: mission.zone_id,
                requires_human: directive.max_auto_actions <= 1,
            }));
        }
    }

    emergencies.sort_by(|a, b| {
        b.severity
            .cmp(&a.severity)
            .then_with(|| a.title.cmp(&b.title))
    });
    emergencies
}

pub fn generate_system_galaxy_events(
    galaxy: &mut GalaxyState,
    governance: &GovernanceEngine,
    missions: &MissionBoard,
    max_events: usize,
) -> Result<Vec<DynamicEvent>> {
    let mut generated = Vec::new();

    for planet in governance.list_planets() {
        if generated.len() >= max_events {
            break;
        }
        if planet.stability <= 30 {
            generated.push(galaxy.publish_event(
                DynamicEventCategory::Political,
                "frontier-belt",
                &format!("Governance crisis on {}", planet.name),
                "Sovereignty stability dropped below safe threshold and emergency legitimacy is under review.",
                7,
                None,
                vec!["governance".to_string(), "crisis".to_string()],
            )?);
        }
    }

    for mission in missions.list(Some(&MissionStatus::Open)) {
        if generated.len() >= max_events {
            break;
        }
        if matches!(
            mission.domain,
            MissionDomain::Security | MissionDomain::Trade
        ) {
            let zone_id = mission.zone_id.as_deref().unwrap_or("frontier-belt");
            if galaxy.zones().iter().any(|zone| zone.zone_id == zone_id) {
                generated.push(galaxy.publish_event(
                    DynamicEventCategory::Spatial,
                    zone_id,
                    &format!("Mission pressure: {}", mission.title),
                    "System generated dispatch created due to unresolved frontier contract pressure.",
                    6,
                    None,
                    vec!["missions".to_string(), "dispatch".to_string()],
                )?);
            }
        }
    }

    Ok(generated)
}

fn build_emergency(spec: EmergencySpec) -> EmergencyState {
    EmergencyState {
        emergency_id: uuid::Uuid::new_v4().to_string(),
        kind: spec.kind,
        severity: spec.severity,
        title: spec.title,
        description: spec.description,
        agent_id: spec.agent_id,
        subnet_id: spec.subnet_id,
        zone_id: spec.zone_id,
        requires_human: spec.requires_human,
        created_at: Utc::now().timestamp(),
    }
}

struct EmergencySpec {
    agent_id: String,
    kind: EmergencyKind,
    severity: u8,
    title: String,
    description: String,
    subnet_id: Option<String>,
    zone_id: Option<String>,
    requires_human: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::civilization::missions::{MissionPublisherKind, MissionReward};
    use crate::civilization::profiles::{Faction, RolePath, StrategyProfile};
    use crate::governance::{GovernanceEngine, PlanetConstitutionTemplate, PlanetCreationRequest};
    use crate::identity::Identity;

    #[test]
    fn emergencies_reflect_profile_governance_and_galaxy() {
        let mut profiles = CitizenRegistry::default();
        profiles.set_profile(
            "agent-a",
            Faction::Order,
            RolePath::Operator,
            StrategyProfile::Conservative,
            Some("planet-a".to_string()),
            Some("genesis-core".to_string()),
        );

        let creator = Identity::new_random();
        let s1 = Identity::new_random();
        let s2 = Identity::new_random();
        let ts = Utc::now().timestamp();
        let mut governance = GovernanceEngine::default();
        governance.issue_license(&creator.agent_id, &creator.agent_id, "proof", 7);
        governance.lock_bond(&creator.agent_id, 100, 30);
        let approvals = vec![
            GovernanceEngine::sign_genesis("planet-a", "Planet A", &creator.agent_id, ts, &s1)
                .unwrap(),
            GovernanceEngine::sign_genesis("planet-a", "Planet A", &creator.agent_id, ts, &s2)
                .unwrap(),
        ];
        governance
            .create_planet(
                &PlanetCreationRequest {
                    subnet_id: "planet-a".to_string(),
                    name: "Planet A".to_string(),
                    creator: creator.agent_id.clone(),
                    created_at: ts,
                    tax_rate: 0.04,
                    constitution_template: PlanetConstitutionTemplate::MigrantCouncil,
                    min_bond: 50,
                    min_approvals: 2,
                },
                &approvals,
            )
            .unwrap();
        governance.adjust_stability("planet-a", -60).unwrap();

        let mut galaxy = GalaxyState::default_with_core_zones();
        galaxy
            .publish_event(
                DynamicEventCategory::Economic,
                "genesis-core",
                "Supply shock",
                "Critical shortage",
                6,
                None,
                vec!["economy".to_string()],
            )
            .unwrap();

        let mut missions = MissionBoard::default();
        missions.publish(
            "Defend orbital yard",
            "Counter hostile incursions.",
            "planet-a",
            MissionPublisherKind::PlanetaryGovernment,
            MissionDomain::Security,
            Some("planet-a".to_string()),
            Some("genesis-core".to_string()),
            Some(RolePath::Enforcer),
            None,
            MissionReward {
                agent_watt: 10,
                reputation: 2,
                capacity: 0,
                treasury_share_watt: 1,
            },
            serde_json::json!({}),
        );

        let emergencies =
            evaluate_emergencies("agent-a", &profiles, &missions, &governance, &galaxy);
        assert!(emergencies.len() >= 2);
        assert!(
            emergencies
                .iter()
                .any(|item| item.kind == EmergencyKind::GalaxyEvent)
        );
        assert!(
            emergencies
                .iter()
                .any(|item| item.kind == EmergencyKind::GovernanceInstability)
        );
    }

    #[test]
    fn system_event_generation_uses_governance_and_missions() {
        let creator = Identity::new_random();
        let s1 = Identity::new_random();
        let s2 = Identity::new_random();
        let ts = Utc::now().timestamp();
        let mut governance = GovernanceEngine::default();
        governance.issue_license(&creator.agent_id, &creator.agent_id, "proof", 7);
        governance.lock_bond(&creator.agent_id, 100, 30);
        let approvals = vec![
            GovernanceEngine::sign_genesis("planet-a", "Planet A", &creator.agent_id, ts, &s1)
                .unwrap(),
            GovernanceEngine::sign_genesis("planet-a", "Planet A", &creator.agent_id, ts, &s2)
                .unwrap(),
        ];
        governance
            .create_planet(
                &PlanetCreationRequest {
                    subnet_id: "planet-a".to_string(),
                    name: "Planet A".to_string(),
                    creator: creator.agent_id.clone(),
                    created_at: ts,
                    tax_rate: 0.04,
                    constitution_template: PlanetConstitutionTemplate::MigrantCouncil,
                    min_bond: 50,
                    min_approvals: 2,
                },
                &approvals,
            )
            .unwrap();
        governance.adjust_stability("planet-a", -70).unwrap();

        let mut missions = MissionBoard::default();
        missions.publish(
            "Escort convoy",
            "Protect freight.",
            "planet-a",
            MissionPublisherKind::PlanetaryGovernment,
            MissionDomain::Trade,
            Some("planet-a".to_string()),
            Some("frontier-belt".to_string()),
            Some(RolePath::Broker),
            None,
            MissionReward {
                agent_watt: 12,
                reputation: 2,
                capacity: 1,
                treasury_share_watt: 1,
            },
            serde_json::json!({}),
        );

        let mut galaxy = GalaxyState::default_with_core_zones();
        let generated =
            generate_system_galaxy_events(&mut galaxy, &governance, &missions, 3).unwrap();
        assert!(!generated.is_empty());
    }
}
