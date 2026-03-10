use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::civilization::identities::PublicIdentity;
use crate::civilization::missions::{MissionBoard, MissionStatus};

use super::progression::{GameStage, GameStatus};
use super::{GameMissionPack, StarterMissionSet};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BootstrapStep {
    pub key: String,
    pub title: String,
    pub complete: bool,
    pub progress_pct: u8,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BootstrapState {
    pub current_phase: String,
    pub progress_pct: u8,
    pub current_focus: String,
    pub steps: Vec<BootstrapStep>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BootstrapActionKind {
    ApiCall,
    OpenScreen,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BootstrapActionCard {
    pub key: String,
    pub title: String,
    pub summary: String,
    pub ready: bool,
    pub progress_pct: u8,
    pub kind: BootstrapActionKind,
    pub target: String,
    pub method: Option<String>,
    pub payload: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BootstrapFlow {
    pub state: BootstrapState,
    pub first_hour_focus: String,
    pub first_hour_plan: Vec<String>,
    pub first_cycle_focus: String,
    pub first_cycle_plan: Vec<String>,
    pub action_cards: Vec<BootstrapActionCard>,
}

struct ActionTargetSpec<'a> {
    target: &'a str,
    method: Option<&'a str>,
    payload: Option<Value>,
}

#[must_use]
pub fn compute_bootstrap_state(
    controller_id: &str,
    public_identity: Option<&PublicIdentity>,
    status: &GameStatus,
    missions: &MissionBoard,
) -> BootstrapState {
    let open_starters = starter_count(missions, controller_id, Some(&MissionStatus::Open));
    let claimed_starters = starter_count(missions, controller_id, Some(&MissionStatus::Claimed));
    let settled_starters = starter_count(missions, controller_id, Some(&MissionStatus::Settled));

    let steps = vec![
        step(
            "create_identity",
            "Create a public identity",
            public_identity.is_some(),
            "This public identity needs to be bound and initialized before it can act meaningfully inside the galaxy network.",
        ),
        step(
            "set_home_anchor",
            "Bind to a home anchor",
            status.home_anchor.is_some(),
            "Choose a home subnet or home zone so the rest of your progression can attach to a real place.",
        ),
        count_step(
            "bootstrap_starters",
            "Generate starter missions",
            open_starters + claimed_starters + settled_starters,
            2,
            "Starter missions are the first role-specific contracts that teach your operating loop.",
        ),
        count_step(
            "claim_starter",
            "Claim your first starter mission",
            claimed_starters + settled_starters,
            1,
            "Claiming a mission starts the real loop: identity, work, and public progress.",
        ),
        count_step(
            "settle_starter",
            "Settle your first starter mission",
            settled_starters,
            1,
            "Settlement is the first proof that your role can convert work into durable influence.",
        ),
        step(
            "reach_foothold",
            "Reach foothold stage",
            status.stage != GameStage::Survival,
            "Move beyond basic survival and into repeatable operation inside the galaxy.",
        ),
    ];
    let completed = steps.iter().filter(|step| step.complete).count();
    let progress_pct = u8::try_from((completed * 100) / steps.len()).unwrap_or(100);
    let current_focus = steps.iter().find(|step| !step.complete).map_or_else(
        || "Expand from foothold into broader influence.".to_string(),
        |step| step.title.clone(),
    );
    let current_phase = if progress_pct < 50 {
        "introduction".to_string()
    } else if progress_pct < 100 {
        "activation".to_string()
    } else {
        "operational".to_string()
    };

    BootstrapState {
        current_phase,
        progress_pct,
        current_focus,
        steps,
    }
}

#[must_use]
pub fn compute_bootstrap_flow(
    public_identity: Option<&PublicIdentity>,
    status: &GameStatus,
    bootstrap: BootstrapState,
    starter_missions: Option<&StarterMissionSet>,
    mission_pack: Option<&GameMissionPack>,
) -> BootstrapFlow {
    let action_cards = bootstrap_action_cards(
        public_identity,
        status,
        &bootstrap,
        starter_missions,
        mission_pack,
    );
    let first_hour_plan = starter_missions
        .map(|starter_set| {
            starter_set
                .objective_chain
                .steps
                .iter()
                .filter(|step| step.progress_pct < 100)
                .take(2)
                .map(|step| step.title.clone())
                .collect::<Vec<_>>()
        })
        .filter(|steps| !steps.is_empty())
        .unwrap_or_else(|| {
            action_cards
                .iter()
                .filter(|card| card.ready)
                .take(4)
                .map(|card| card.title.clone())
                .collect()
        });

    BootstrapFlow {
        first_hour_focus: bootstrap.current_focus.clone(),
        state: bootstrap,
        first_cycle_focus: first_hour_plan
            .first()
            .cloned()
            .unwrap_or_else(|| "Review the current bootstrap plan.".to_string()),
        first_cycle_plan: first_hour_plan.clone(),
        first_hour_plan,
        action_cards,
    }
}

fn bootstrap_action_cards(
    public_identity: Option<&PublicIdentity>,
    status: &GameStatus,
    bootstrap: &BootstrapState,
    starter_missions: Option<&StarterMissionSet>,
    mission_pack: Option<&GameMissionPack>,
) -> Vec<BootstrapActionCard> {
    let first_open_starter = starter_missions.and_then(|starter_set| {
        starter_set
            .existing
            .iter()
            .find(|mission| mission.status == MissionStatus::Open)
            .map(|mission| mission.mission_id.clone())
    });
    let starter_missing =
        starter_missions.map_or(0, |starter_set| starter_set.missing_template_ids.len());
    let pack_missing = mission_pack.map_or(0, |pack| pack.missing_template_ids.len());
    let has_settled_starter = bootstrap
        .steps
        .iter()
        .find(|step| step.key == "settle_starter")
        .is_some_and(|step| step.complete);
    vec![
        action_card(
            "open_briefing",
            "Read your operator briefing",
            "Start every supervision cycle by reading the current briefing and emergency profile for your home anchor.",
            true,
            100,
            BootstrapActionKind::OpenScreen,
            ActionTargetSpec {
                target: "supervision_home",
                method: None,
                payload: None,
            },
        ),
        action_card(
            "open_galaxy_map",
            "Inspect your home anchor on the galaxy map",
            "Use the genesis map to understand which system, planet, and route your first work attaches to.",
            status.home_anchor.is_some(),
            if status.home_anchor.is_some() { 100 } else { 0 },
            BootstrapActionKind::OpenScreen,
            ActionTargetSpec {
                target: "galaxy_map",
                method: None,
                payload: None,
            },
        ),
        action_card(
            "bootstrap_starter_missions",
            "Bootstrap starter mission chain",
            starter_chain_summary(starter_missions),
            public_identity.is_some() && status.home_anchor.is_some(),
            completion_progress(starter_missing == 0),
            BootstrapActionKind::ApiCall,
            ActionTargetSpec {
                target: "/v1/game/starter-missions/bootstrap",
                method: Some("POST"),
                payload: Some(json!({
                    "public_id": public_identity.map(|identity| identity.public_id.clone()),
                })),
            },
        ),
        action_card(
            "claim_first_starter",
            "Claim your first starter contract",
            "Move from setup into action by claiming one starter mission tied to your home anchor.",
            first_open_starter.is_some(),
            completion_progress(has_settled_starter || status.active_missions > 0),
            BootstrapActionKind::OpenScreen,
            ActionTargetSpec {
                target: "missions",
                method: None,
                payload: first_open_starter.map(|mission_id| json!({ "mission_id": mission_id })),
            },
        ),
        action_card(
            "bootstrap_stage_pack",
            "Bootstrap your current stage mission pack",
            "Once starter work is flowing, generate the next set of role and civic missions for your current stage.",
            public_identity.is_some() && has_settled_starter,
            completion_progress(pack_missing == 0),
            BootstrapActionKind::ApiCall,
            ActionTargetSpec {
                target: "/v1/game/mission-pack/bootstrap",
                method: Some("POST"),
                payload: Some(json!({
                    "public_id": public_identity.map(|identity| identity.public_id.clone()),
                })),
            },
        ),
        action_card(
            "review_governance_journey",
            "Review your governance journey",
            "See which civic gates still separate your current role from formal governance participation.",
            true,
            completion_progress(status.can_enter_governance),
            BootstrapActionKind::OpenScreen,
            ActionTargetSpec {
                target: "governance",
                method: None,
                payload: None,
            },
        ),
    ]
}

fn starter_chain_summary(starter_missions: Option<&StarterMissionSet>) -> &str {
    starter_missions.map_or(
        "Generate the role-specific contracts that teach the first operating loop.",
        |starter_set| starter_set.objective_chain.summary.as_str(),
    )
}

fn starter_count(
    missions: &MissionBoard,
    controller_id: &str,
    status: Option<&MissionStatus>,
) -> usize {
    missions
        .list(status)
        .into_iter()
        .filter(|mission| mission.payload["starter_owner_agent_id"].as_str() == Some(controller_id))
        .count()
}

fn step(key: &str, title: &str, complete: bool, summary: &str) -> BootstrapStep {
    BootstrapStep {
        key: key.to_string(),
        title: title.to_string(),
        complete,
        progress_pct: if complete { 100 } else { 0 },
        summary: summary.to_string(),
    }
}

fn count_step(
    key: &str,
    title: &str,
    current: usize,
    required: usize,
    summary: &str,
) -> BootstrapStep {
    let required = required.max(1);
    let progress_pct = u8::try_from((current.min(required) * 100) / required).unwrap_or(100);
    BootstrapStep {
        key: key.to_string(),
        title: title.to_string(),
        complete: current >= required,
        progress_pct,
        summary: summary.to_string(),
    }
}

fn completion_progress(complete: bool) -> u8 {
    if complete { 100 } else { 0 }
}

fn action_card(
    key: &str,
    title: &str,
    summary: &str,
    ready: bool,
    progress_pct: u8,
    kind: BootstrapActionKind,
    target: ActionTargetSpec<'_>,
) -> BootstrapActionCard {
    BootstrapActionCard {
        key: key.to_string(),
        title: title.to_string(),
        summary: summary.to_string(),
        ready,
        progress_pct,
        kind,
        target: target.target.to_string(),
        method: target.method.map(str::to_string),
        payload: target.payload,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::civilization::galaxy::GalaxyState;
    use crate::civilization::profiles::{CitizenProfile, Faction, RolePath, StrategyProfile};
    use crate::game::mission_pack::mission_pack_set;
    use crate::game::progression::GameStatus;
    use crate::game::starter::bootstrap_starter_missions;
    use crate::map::registry::GalaxyMapRegistry;

    #[test]
    fn bootstrap_progress_moves_with_identity_and_starter_missions() {
        let mut board = MissionBoard::default();
        let profile = CitizenProfile {
            agent_id: "agent-a".to_string(),
            faction: Faction::Freeport,
            role: RolePath::Broker,
            strategy: StrategyProfile::Balanced,
            home_subnet_id: Some("planet-test".to_string()),
            home_zone_id: Some("genesis-core".to_string()),
            updated_at: 0,
        };
        let mut maps = GalaxyMapRegistry::default();
        maps.ensure_default_genesis_map(&GalaxyState::default_with_core_zones().zones())
            .unwrap();
        let created = bootstrap_starter_missions("agent-a", &profile, &maps, &mut board);
        let state = compute_bootstrap_state(
            "agent-a",
            Some(&PublicIdentity {
                public_id: "captain-aurora".to_string(),
                display_name: "Captain Aurora".to_string(),
                legacy_agent_id: Some("agent-a".to_string()),
                active: true,
                created_at: 0,
                updated_at: 0,
            }),
            &GameStatus {
                stage: GameStage::Survival,
                tier: super::super::progression::ProgressionTier::Initiate,
                headline: String::new(),
                summary: String::new(),
                total_influence: 10,
                settled_missions: 0,
                active_missions: 0,
                governed_planets: 0,
                can_enter_governance: false,
                home_anchor: None,
                recommended_actions: Vec::new(),
                objectives: Vec::new(),
                qualifications: Vec::new(),
                governance_journey: super::super::progression::GovernanceJourney {
                    eligible_now: false,
                    current_status: "starter".to_string(),
                    next_gate: "license".to_string(),
                    gates: Vec::new(),
                },
            },
            &board,
        );
        assert_eq!(created.len(), 2);
        assert!(state.progress_pct > 0);
        assert_eq!(state.current_phase, "introduction");
    }

    #[test]
    fn bootstrap_flow_surfaces_first_hour_actions() {
        let mut board = MissionBoard::default();
        let profile = CitizenProfile {
            agent_id: "agent-a".to_string(),
            faction: Faction::Freeport,
            role: RolePath::Broker,
            strategy: StrategyProfile::Balanced,
            home_subnet_id: Some("planet-test".to_string()),
            home_zone_id: Some("genesis-core".to_string()),
            updated_at: 0,
        };
        let mut maps = GalaxyMapRegistry::default();
        maps.ensure_default_genesis_map(&GalaxyState::default_with_core_zones().zones())
            .unwrap();
        let created = bootstrap_starter_missions("agent-a", &profile, &maps, &mut board);
        assert_eq!(created.len(), 2);
        let starter_set =
            super::super::starter::starter_mission_set("agent-a", &profile, &maps, &board);
        let galaxy = GalaxyState::default_with_core_zones();
        let mission_pack = mission_pack_set(
            "agent-a",
            &profile,
            GameStage::Foothold,
            &maps,
            &galaxy,
            &board,
        );
        let public_identity = test_public_identity();
        let status = test_foothold_status();
        let bootstrap = compute_bootstrap_state("agent-a", Some(&public_identity), &status, &board);
        let flow = compute_bootstrap_flow(
            Some(&public_identity),
            &status,
            bootstrap,
            Some(&starter_set),
            Some(&mission_pack),
        );
        assert!(
            flow.action_cards
                .iter()
                .any(|card| card.key == "bootstrap_stage_pack")
        );
        assert!(!flow.first_hour_plan.is_empty());
        assert_eq!(flow.first_cycle_plan, flow.first_hour_plan);
        assert_eq!(flow.first_hour_plan[0], "Seed your first trade corridor");
    }

    fn test_public_identity() -> PublicIdentity {
        PublicIdentity {
            public_id: "captain-aurora".to_string(),
            display_name: "Captain Aurora".to_string(),
            legacy_agent_id: Some("agent-a".to_string()),
            active: true,
            created_at: 0,
            updated_at: 0,
        }
    }

    fn test_foothold_status() -> GameStatus {
        GameStatus {
            stage: GameStage::Foothold,
            tier: super::super::progression::ProgressionTier::Specialist,
            headline: String::new(),
            summary: String::new(),
            total_influence: 240,
            settled_missions: 1,
            active_missions: 0,
            governed_planets: 0,
            can_enter_governance: false,
            home_anchor: Some(super::super::progression::HomeAnchor {
                map_id: "genesis-base".to_string(),
                map_name: "Genesis Base".to_string(),
                system_id: "system-prime".to_string(),
                system_name: "Genesis Prime".to_string(),
                planet_id: "planet-test".to_string(),
                planet_name: "Planet Test".to_string(),
                zone_id: "genesis-core".to_string(),
            }),
            recommended_actions: Vec::new(),
            objectives: Vec::new(),
            qualifications: Vec::new(),
            governance_journey: super::super::progression::GovernanceJourney {
                eligible_now: false,
                current_status: "approaching".to_string(),
                next_gate: "civic_license".to_string(),
                gates: Vec::new(),
            },
        }
    }
}
