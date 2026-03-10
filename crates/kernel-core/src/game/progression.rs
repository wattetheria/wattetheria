use serde::{Deserialize, Serialize};

use crate::civilization::metrics::CivilizationScores;
use crate::civilization::missions::{MissionBoard, MissionStatus};
use crate::civilization::profiles::{CitizenProfile, RolePath};
use crate::governance::GovernanceEngine;
use crate::map::GalaxyMapRegistry;
use crate::types::AgentStats;

use super::qualification::QualificationTrack;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GameStage {
    Survival,
    Foothold,
    Influence,
    Expansion,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProgressionTier {
    Initiate,
    Specialist,
    Coordinator,
    Sovereign,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GameObjective {
    pub key: String,
    pub title: String,
    pub complete: bool,
    pub progress_pct: u8,
    pub hint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HomeAnchor {
    pub map_id: String,
    pub map_name: String,
    pub system_id: String,
    pub system_name: String,
    pub planet_id: String,
    pub planet_name: String,
    pub zone_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GameStatus {
    pub stage: GameStage,
    pub tier: ProgressionTier,
    pub headline: String,
    pub summary: String,
    pub total_influence: i64,
    pub settled_missions: usize,
    pub active_missions: usize,
    pub governed_planets: usize,
    pub can_enter_governance: bool,
    pub home_anchor: Option<HomeAnchor>,
    pub recommended_actions: Vec<String>,
    pub objectives: Vec<GameObjective>,
    pub qualifications: Vec<QualificationTrack>,
    pub governance_journey: GovernanceJourney,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GovernanceGate {
    pub key: String,
    pub title: String,
    pub complete: bool,
    pub hint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GovernanceJourney {
    pub eligible_now: bool,
    pub current_status: String,
    pub next_gate: String,
    pub gates: Vec<GovernanceGate>,
}

pub struct GameComputation<'a> {
    pub stats: &'a AgentStats,
    pub scores: &'a CivilizationScores,
    pub missions: &'a MissionBoard,
    pub governance: &'a GovernanceEngine,
    pub maps: &'a GalaxyMapRegistry,
    pub qualifications: Vec<QualificationTrack>,
}

#[must_use]
pub fn compute_status(
    controller_id: &str,
    profile: Option<&CitizenProfile>,
    context: GameComputation<'_>,
) -> GameStatus {
    let settled_missions = settled_mission_count(controller_id, context.missions);
    let active_missions = active_mission_count(controller_id, context.missions);
    let governed_planets = governed_planet_count(controller_id, context.governance);
    let created_planets = created_planet_count(controller_id, context.governance);
    let can_enter_governance = context.governance.has_valid_license(controller_id)
        && context.governance.has_active_bond(controller_id, 1);
    let home_anchor = locate_home_anchor(profile, context.maps);
    let stage = determine_stage(
        settled_missions,
        active_missions,
        governed_planets,
        created_planets,
        can_enter_governance,
        home_anchor.is_some(),
        context.scores.total_influence,
    );
    let tier = determine_tier(context.scores.total_influence);

    let objectives = objectives_for_stage(
        &stage,
        settled_missions,
        active_missions,
        home_anchor.is_some(),
        can_enter_governance,
        governed_planets,
        context.scores.total_influence,
    );
    let recommended_actions = recommended_actions(
        profile.map(|profile| &profile.role),
        &stage,
        active_missions,
        home_anchor.is_some(),
    );
    let governance_journey = compute_governance_journey(
        controller_id,
        home_anchor.is_some(),
        context.scores.total_influence,
        context.governance,
    );
    let headline = match stage {
        GameStage::Survival => "Establish your first foothold".to_string(),
        GameStage::Foothold => "Convert activity into durable position".to_string(),
        GameStage::Influence => "Shape policy, markets, and route outcomes".to_string(),
        GameStage::Expansion => "Push the galaxy outward from your existing power base".to_string(),
    };
    let summary = format!(
        "Influence {} with watt {}, power {}, reputation {}, and capacity {}.",
        context.scores.total_influence,
        context.stats.watt,
        context.stats.power,
        context.stats.reputation,
        context.stats.capacity
    );

    GameStatus {
        stage,
        tier,
        headline,
        summary,
        total_influence: context.scores.total_influence,
        settled_missions,
        active_missions,
        governed_planets,
        can_enter_governance,
        home_anchor,
        recommended_actions,
        objectives,
        qualifications: context.qualifications,
        governance_journey,
    }
}

fn settled_mission_count(controller_id: &str, missions: &MissionBoard) -> usize {
    missions
        .list(Some(&MissionStatus::Settled))
        .into_iter()
        .filter(|mission| mission.completed_by.as_deref() == Some(controller_id))
        .count()
}

fn active_mission_count(controller_id: &str, missions: &MissionBoard) -> usize {
    missions
        .list(None)
        .into_iter()
        .filter(|mission| {
            mission.claimed_by.as_deref() == Some(controller_id)
                && matches!(
                    mission.status,
                    MissionStatus::Claimed | MissionStatus::Completed
                )
        })
        .count()
}

fn governed_planet_count(controller_id: &str, governance: &GovernanceEngine) -> usize {
    governance
        .list_planets()
        .into_iter()
        .filter(|planet| {
            planet.creator == controller_id || planet.validators.contains(controller_id)
        })
        .count()
}

fn created_planet_count(controller_id: &str, governance: &GovernanceEngine) -> usize {
    governance
        .list_planets()
        .into_iter()
        .filter(|planet| planet.creator == controller_id)
        .count()
}

fn determine_stage(
    settled_missions: usize,
    active_missions: usize,
    governed_planets: usize,
    created_planets: usize,
    can_enter_governance: bool,
    has_home_anchor: bool,
    total_influence: i64,
) -> GameStage {
    if created_planets > 0 || (governed_planets > 0 && total_influence >= 900) {
        GameStage::Expansion
    } else if governed_planets > 0 || can_enter_governance || total_influence >= 400 {
        GameStage::Influence
    } else if settled_missions >= 2
        || active_missions > 0
        || has_home_anchor
        || total_influence >= 150
    {
        GameStage::Foothold
    } else {
        GameStage::Survival
    }
}

fn determine_tier(total_influence: i64) -> ProgressionTier {
    match total_influence {
        influence if influence >= 1000 => ProgressionTier::Sovereign,
        influence if influence >= 500 => ProgressionTier::Coordinator,
        influence if influence >= 200 => ProgressionTier::Specialist,
        _ => ProgressionTier::Initiate,
    }
}

#[must_use]
pub fn compute_governance_journey(
    controller_id: &str,
    has_home_anchor: bool,
    total_influence: i64,
    governance: &GovernanceEngine,
) -> GovernanceJourney {
    let has_license = governance.has_valid_license(controller_id);
    let has_bond = governance.has_active_bond(controller_id, 1);
    let influence_ready = total_influence >= 400;
    let eligible_now = has_license && has_bond;
    let gates = vec![
        GovernanceGate {
            key: "home_anchor".to_string(),
            title: "Hold a home anchor".to_string(),
            complete: has_home_anchor,
            hint: "Governance should emerge from a place in the galaxy, not from nowhere."
                .to_string(),
        },
        GovernanceGate {
            key: "influence_floor".to_string(),
            title: "Reach 400 influence".to_string(),
            complete: influence_ready,
            hint: "You need enough operating weight for governance to be meaningful.".to_string(),
        },
        GovernanceGate {
            key: "civic_license".to_string(),
            title: "Secure a civic license".to_string(),
            complete: has_license,
            hint: "Licenses gate who can formally participate in sovereignty structures."
                .to_string(),
        },
        GovernanceGate {
            key: "sovereignty_bond".to_string(),
            title: "Lock a sovereignty bond".to_string(),
            complete: has_bond,
            hint: "Bonds prove the participant can back power with durable commitment.".to_string(),
        },
    ];
    let next_gate = gates
        .iter()
        .find(|gate| !gate.complete)
        .map_or_else(|| "governance_active".to_string(), |gate| gate.key.clone());
    let current_status = if eligible_now {
        "eligible".to_string()
    } else if has_license || has_bond || influence_ready {
        "approaching".to_string()
    } else {
        "starter".to_string()
    };

    GovernanceJourney {
        eligible_now,
        current_status,
        next_gate,
        gates,
    }
}

fn locate_home_anchor(
    profile: Option<&CitizenProfile>,
    maps: &GalaxyMapRegistry,
) -> Option<HomeAnchor> {
    let profile = profile?;
    maps.list().into_iter().find_map(|map| {
        map.systems.into_iter().find_map(|system| {
            system.planets.into_iter().find_map(|planet| {
                let matches_subnet = profile
                    .home_subnet_id
                    .as_deref()
                    .is_some_and(|subnet_id| planet.subnet_id.as_deref() == Some(subnet_id));
                let matches_zone = profile.home_zone_id.as_deref() == Some(planet.zone_id.as_str());
                if matches_subnet || matches_zone {
                    Some(HomeAnchor {
                        map_id: map.map_id.clone(),
                        map_name: map.name.clone(),
                        system_id: system.system_id.clone(),
                        system_name: system.name.clone(),
                        planet_id: planet.planet_id,
                        planet_name: planet.name,
                        zone_id: planet.zone_id,
                    })
                } else {
                    None
                }
            })
        })
    })
}

fn recommended_actions(
    role: Option<&RolePath>,
    stage: &GameStage,
    active_missions: usize,
    has_home_anchor: bool,
) -> Vec<String> {
    let mut actions = Vec::new();
    if !has_home_anchor {
        actions
            .push("Choose a stable home subnet or home zone inside the genesis map.".to_string());
    }
    if active_missions == 0 {
        actions
            .push("Claim one aligned open mission to keep your operating loop moving.".to_string());
    }
    match role {
        Some(RolePath::Operator) => actions.push(
            "Prioritize infrastructure and throughput missions to strengthen your operating base."
                .to_string(),
        ),
        Some(RolePath::Broker) => actions.push(
            "Prioritize trade and liquidity missions between genesis and frontier corridors."
                .to_string(),
        ),
        Some(RolePath::Enforcer) => actions.push(
            "Prioritize security and escort missions to reduce route pressure and raise authority."
                .to_string(),
        ),
        Some(RolePath::Artificer) => actions.push(
            "Prioritize culture and attraction missions that build public gravity around your node."
                .to_string(),
        ),
        None => actions.push("Set a role profile before scaling your mission strategy.".to_string()),
    }
    match stage {
        GameStage::Survival => actions.push(
            "Focus on early completion, not scale: settle your first mission and lock in a home anchor."
                .to_string(),
        ),
        GameStage::Foothold => actions.push(
            "Grow repeatable influence by stacking a few aligned mission settlements."
                .to_string(),
        ),
        GameStage::Influence => actions.push(
            "Convert accumulated trust into governance eligibility and visible civic control."
                .to_string(),
        ),
        GameStage::Expansion => actions.push(
            "Use your existing governance and influence base to prepare future map expansion."
                .to_string(),
        ),
    }
    actions
}

fn objectives_for_stage(
    stage: &GameStage,
    settled_missions: usize,
    active_missions: usize,
    has_home_anchor: bool,
    can_enter_governance: bool,
    governed_planets: usize,
    total_influence: i64,
) -> Vec<GameObjective> {
    match stage {
        GameStage::Survival => vec![
            mission_objective(
                "first_settlement",
                "Settle your first mission",
                settled_missions,
                1,
            ),
            bool_objective(
                "home_anchor",
                "Anchor to a home zone or subnet",
                has_home_anchor,
                "Bind this public identity to a home location so the rest of the galaxy can orient around its role.",
            ),
            influence_objective(
                "starter_influence",
                "Reach 150 total influence",
                total_influence,
                150,
            ),
        ],
        GameStage::Foothold => vec![
            mission_objective(
                "triple_settlement",
                "Settle 3 aligned missions",
                settled_missions,
                3,
            ),
            influence_objective(
                "foothold_influence",
                "Reach 400 total influence",
                total_influence,
                400,
            ),
            bool_objective(
                "active_loop",
                "Maintain at least one active mission",
                active_missions > 0,
                "Keep a live operating loop so your role turns into durable position instead of one-off activity.",
            ),
        ],
        GameStage::Influence => vec![
            bool_objective(
                "governance_gate",
                "Qualify for governance",
                can_enter_governance,
                "Hold a valid civic license and an active sovereignty bond.",
            ),
            influence_objective(
                "influence_gate",
                "Reach 900 total influence",
                total_influence,
                900,
            ),
            count_objective(
                "governed_planets",
                "Participate in governing 1 planet",
                governed_planets,
                1,
                "Move from personal success into visible civic power.",
            ),
        ],
        GameStage::Expansion => vec![
            count_objective(
                "planetary_control",
                "Govern at least 1 planet",
                governed_planets,
                1,
                "Expansion starts from a real power base, not isolated activity.",
            ),
            influence_objective(
                "expansion_influence",
                "Reach 1200 total influence",
                total_influence,
                1200,
            ),
            mission_objective(
                "expansion_logistics",
                "Settle 8 missions before expansion",
                settled_missions,
                8,
            ),
        ],
    }
}

fn mission_objective(
    key: &str,
    title: &str,
    settled_missions: usize,
    required: usize,
) -> GameObjective {
    count_objective(
        key,
        title,
        settled_missions,
        required,
        "Mission settlements are the first proof that your role can operate in the galaxy.",
    )
}

fn influence_objective(
    key: &str,
    title: &str,
    total_influence: i64,
    required: i64,
) -> GameObjective {
    let clamped_required = required.max(1);
    let progress_raw = ((total_influence.max(0) * 100) / clamped_required).min(100);
    let progress = u8::try_from(progress_raw).unwrap_or(100);
    GameObjective {
        key: key.to_string(),
        title: title.to_string(),
        complete: total_influence >= required,
        progress_pct: progress,
        hint: "Influence combines wealth, power, security, trade, and culture into your current strategic weight.".to_string(),
    }
}

fn count_objective(
    key: &str,
    title: &str,
    current: usize,
    required: usize,
    hint: &str,
) -> GameObjective {
    let required = required.max(1);
    let progress_raw = (current.min(required) * 100) / required;
    let progress = u8::try_from(progress_raw).unwrap_or(100);
    GameObjective {
        key: key.to_string(),
        title: title.to_string(),
        complete: current >= required,
        progress_pct: progress,
        hint: hint.to_string(),
    }
}

fn bool_objective(key: &str, title: &str, complete: bool, hint: &str) -> GameObjective {
    GameObjective {
        key: key.to_string(),
        title: title.to_string(),
        complete,
        progress_pct: if complete { 100 } else { 0 },
        hint: hint.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::civilization::metrics::CivilizationScores;
    use crate::civilization::missions::{
        MissionBoard, MissionDomain, MissionPublisherKind, MissionReward,
    };
    use crate::civilization::profiles::{CitizenRegistry, Faction, StrategyProfile};
    use crate::governance::GovernanceEngine;
    use crate::map::GalaxyMapRegistry;

    #[test]
    fn compute_status_uses_map_anchor_and_mission_progress() {
        let mut registry = CitizenRegistry::default();
        let profile = registry.set_profile(
            "agent-a",
            Faction::Freeport,
            RolePath::Broker,
            StrategyProfile::Balanced,
            Some("planet-test".to_string()),
            Some("genesis-core".to_string()),
        );

        let mut maps = GalaxyMapRegistry::default();
        maps.ensure_default_genesis_map(
            &crate::civilization::galaxy::GalaxyState::default_with_core_zones().zones(),
        )
        .unwrap();

        let mut missions = MissionBoard::default();
        let mission = missions.publish(
            "Run liquidity",
            "Keep the frontier exchange supplied.",
            "planet-test",
            MissionPublisherKind::PlanetaryGovernment,
            MissionDomain::Trade,
            Some("planet-test".to_string()),
            Some("genesis-core".to_string()),
            Some(RolePath::Broker),
            Some(Faction::Freeport),
            MissionReward {
                agent_watt: 10,
                reputation: 2,
                capacity: 1,
                treasury_share_watt: 0,
            },
            serde_json::json!({}),
        );
        missions.claim(&mission.mission_id, "agent-a").unwrap();
        missions.complete(&mission.mission_id, "agent-a").unwrap();
        missions.settle(&mission.mission_id).unwrap();

        let status = compute_status(
            "agent-a",
            Some(&profile),
            GameComputation {
                stats: &AgentStats {
                    power: 3,
                    watt: 50,
                    reputation: 6,
                    capacity: 4,
                },
                scores: &CivilizationScores {
                    wealth: 100,
                    power: 80,
                    security: 30,
                    trade: 90,
                    culture: 20,
                    total_influence: 320,
                },
                missions: &missions,
                governance: &GovernanceEngine::default(),
                maps: &maps,
                qualifications: Vec::new(),
            },
        );

        assert_eq!(status.stage, GameStage::Foothold);
        assert!(status.home_anchor.is_some());
        assert!(
            status
                .recommended_actions
                .iter()
                .any(|action| action.contains("trade"))
        );
        assert_eq!(status.governance_journey.current_status, "starter");
    }
}
