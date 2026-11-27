use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::civilization::identities::PublicIdentity;

use super::progression::{GameStage, GameStatus};

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
    public_identity: Option<&PublicIdentity>,
    status: &GameStatus,
) -> BootstrapState {
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
pub fn compute_bootstrap_flow(status: &GameStatus, bootstrap: BootstrapState) -> BootstrapFlow {
    let action_cards = bootstrap_action_cards(status);
    let first_hour_plan: Vec<String> = action_cards
        .iter()
        .filter(|card| card.ready)
        .take(4)
        .map(|card| card.title.clone())
        .collect();

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

fn bootstrap_action_cards(status: &GameStatus) -> Vec<BootstrapActionCard> {
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

fn step(key: &str, title: &str, complete: bool, summary: &str) -> BootstrapStep {
    BootstrapStep {
        key: key.to_string(),
        title: title.to_string(),
        complete,
        progress_pct: if complete { 100 } else { 0 },
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
    use crate::game::progression::{GovernanceJourney, HomeAnchor};

    fn test_public_identity() -> PublicIdentity {
        PublicIdentity {
            public_id: "captain-aurora".to_string(),
            display_name: "Captain Aurora".to_string(),
            agent_did: Some("agent-a".to_string()),
            active: true,
            created_at: 0,
            updated_at: 0,
        }
    }

    fn test_status(stage: GameStage, home_anchor: Option<HomeAnchor>) -> GameStatus {
        GameStatus {
            stage,
            headline: String::new(),
            summary: String::new(),
            total_influence: 240,
            settled_missions: 1,
            active_missions: 0,
            governed_planets: 0,
            can_enter_governance: false,
            home_anchor,
            recommended_actions: Vec::new(),
            objectives: Vec::new(),
            qualifications: Vec::new(),
            governance_journey: GovernanceJourney {
                eligible_now: false,
                current_status: "approaching".to_string(),
                next_gate: "civic_license".to_string(),
                gates: Vec::new(),
            },
        }
    }

    fn home_anchor() -> HomeAnchor {
        HomeAnchor {
            map_id: "genesis-base".to_string(),
            map_name: "Genesis Base".to_string(),
            system_id: "system-prime".to_string(),
            system_name: "Genesis Prime".to_string(),
            planet_id: "planet-test".to_string(),
            planet_name: "Planet Test".to_string(),
            zone_id: "genesis-core".to_string(),
        }
    }

    #[test]
    fn bootstrap_progress_moves_with_identity_anchor_and_stage() {
        let fresh = compute_bootstrap_state(None, &test_status(GameStage::Survival, None));
        assert_eq!(fresh.progress_pct, 0);
        assert_eq!(fresh.current_phase, "introduction");
        let identity = test_public_identity();
        let done = compute_bootstrap_state(
            Some(&identity),
            &test_status(GameStage::Foothold, Some(home_anchor())),
        );
        assert_eq!(done.progress_pct, 100);
        assert_eq!(done.current_phase, "operational");
    }

    #[test]
    fn bootstrap_flow_surfaces_first_hour_actions() {
        let identity = test_public_identity();
        let status = test_status(GameStage::Foothold, Some(home_anchor()));
        let bootstrap = compute_bootstrap_state(Some(&identity), &status);
        let flow = compute_bootstrap_flow(&status, bootstrap);
        assert!(
            flow.action_cards
                .iter()
                .all(|card| !card.key.contains("starter") && card.key != "bootstrap_stage_pack")
        );
        assert_eq!(flow.first_cycle_plan, flow.first_hour_plan);
        assert_eq!(flow.first_hour_plan[0], "Read your operator briefing");
    }
}
