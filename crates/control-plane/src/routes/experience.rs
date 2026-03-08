use serde::Serialize;
use serde_json::Value;
use wattetheria_kernel::civilization::emergency::EmergencyState;
use wattetheria_kernel::game::OnboardingActionKind;
use wattetheria_kernel::map::{TravelRiskLevel, TravelStateRecord};

use crate::routes::game::GameView;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum GameplayActionKind {
    ApiCall,
    OpenScreen,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct GameplayNextAction {
    pub key: String,
    pub title: String,
    pub summary: String,
    pub kind: GameplayActionKind,
    pub target: String,
    pub method: Option<String>,
    pub payload: Option<Value>,
    pub ready: bool,
    pub priority: u8,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct GameplayAlert {
    pub key: String,
    pub severity: u8,
    pub title: String,
    pub summary: String,
    pub target: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct GameplayPriorityCard {
    pub key: String,
    pub title: String,
    pub summary: String,
    pub status_label: String,
    pub progress_pct: u8,
    pub target: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct GameplayExperience {
    pub next_actions: Vec<GameplayNextAction>,
    pub alerts: Vec<GameplayAlert>,
    pub priority_cards: Vec<GameplayPriorityCard>,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct DashboardExperienceSignals<'a> {
    pub emergencies: &'a [EmergencyState],
    pub eligible_open_count: usize,
    pub local_open_count: usize,
    pub travel_required_open_count: usize,
}

pub(crate) fn build_gameplay_experience(
    game: &GameView,
    dashboard: Option<DashboardExperienceSignals<'_>>,
) -> GameplayExperience {
    GameplayExperience {
        next_actions: build_next_actions(game, dashboard),
        alerts: build_alerts(game, dashboard),
        priority_cards: build_priority_cards(game),
    }
}

fn build_next_actions(
    game: &GameView,
    dashboard: Option<DashboardExperienceSignals<'_>>,
) -> Vec<GameplayNextAction> {
    let mut actions = Vec::new();
    push_onboarding_actions(&mut actions, game);
    push_travel_actions(&mut actions, game);
    push_organization_actions(&mut actions, game);
    push_dashboard_actions(&mut actions, dashboard);
    push_recommended_actions(&mut actions, game);

    actions.truncate(6);
    actions
}

fn push_onboarding_actions(actions: &mut Vec<GameplayNextAction>, game: &GameView) {
    for action in game
        .onboarding_flow
        .action_cards
        .iter()
        .filter(|action| action.ready)
        .take(4)
    {
        push_unique_action(
            actions,
            GameplayNextAction {
                key: action.key.clone(),
                title: action.title.clone(),
                summary: action.summary.clone(),
                kind: map_action_kind(&action.kind),
                target: action.target.clone(),
                method: action.method.clone(),
                payload: action.payload.clone(),
                ready: action.ready,
                priority: 1,
            },
        );
    }
}

fn push_travel_actions(actions: &mut Vec<GameplayNextAction>, game: &GameView) {
    let Some(travel_state) = game.travel_state.as_ref() else {
        return;
    };

    if travel_state.active_session.is_some() {
        push_unique_action(
            actions,
            GameplayNextAction {
                key: "complete_active_travel".to_string(),
                title: "Resolve your active travel route".to_string(),
                summary: "Your character is currently in transit. Finish the route and review the arrival outcome.".to_string(),
                kind: GameplayActionKind::OpenScreen,
                target: "galaxy_map".to_string(),
                method: None,
                payload: None,
                ready: true,
                priority: 1,
            },
        );
        return;
    }

    if travel_state
        .last_consequence
        .as_ref()
        .is_some_and(|consequence| consequence.mission_impact.eligible_local_count > 0)
    {
        push_unique_action(
            actions,
            GameplayNextAction {
                key: "review_arrival_contracts".to_string(),
                title: "Review newly reachable local contracts".to_string(),
                summary: "Your latest arrival unlocked local work at the current landing point."
                    .to_string(),
                kind: GameplayActionKind::OpenScreen,
                target: "missions".to_string(),
                method: None,
                payload: None,
                ready: true,
                priority: 2,
            },
        );
    }
}

fn push_organization_actions(actions: &mut Vec<GameplayNextAction>, game: &GameView) {
    if game.organizations.is_empty()
        && !matches!(
            game.status.stage,
            wattetheria_kernel::game::GameStage::Survival
        )
    {
        push_unique_action(
            actions,
            GameplayNextAction {
                key: "create_or_join_organization".to_string(),
                title: "Create or join an organization".to_string(),
                summary: "Organizations unlock shared treasury, mission publishing, and longer-term civic progression.".to_string(),
                kind: GameplayActionKind::OpenScreen,
                target: "organizations".to_string(),
                method: None,
                payload: None,
                ready: true,
                priority: 2,
            },
        );
    }

    if game
        .organizations
        .iter()
        .any(|organization| organization.governance_summary.open_proposals_count > 0)
    {
        push_unique_action(
            actions,
            GameplayNextAction {
                key: "review_organization_governance".to_string(),
                title: "Review open organization proposals".to_string(),
                summary: "Your organization has governance work waiting on member action."
                    .to_string(),
                kind: GameplayActionKind::OpenScreen,
                target: "organizations".to_string(),
                method: None,
                payload: None,
                ready: true,
                priority: 2,
            },
        );
    }
}

fn push_dashboard_actions(
    actions: &mut Vec<GameplayNextAction>,
    dashboard: Option<DashboardExperienceSignals<'_>>,
) {
    if let Some(signals) = dashboard
        && signals.local_open_count == 0
        && signals.travel_required_open_count > 0
    {
        push_unique_action(
            actions,
            GameplayNextAction {
                key: "travel_for_next_contracts".to_string(),
                title: "Travel for the next available contracts".to_string(),
                summary: "Your best current contracts are off-anchor. Plan a route before the local board goes dry.".to_string(),
                kind: GameplayActionKind::OpenScreen,
                target: "galaxy_map".to_string(),
                method: None,
                payload: None,
                ready: true,
                priority: 2,
            },
        );
    }
}

fn push_recommended_actions(actions: &mut Vec<GameplayNextAction>, game: &GameView) {
    for (index, action) in game.status.recommended_actions.iter().take(2).enumerate() {
        push_unique_action(
            actions,
            GameplayNextAction {
                key: format!("recommended-action-{}", index + 1),
                title: "Follow your current role recommendation".to_string(),
                summary: action.clone(),
                kind: GameplayActionKind::OpenScreen,
                target: "dashboard_home".to_string(),
                method: None,
                payload: None,
                ready: true,
                priority: 3,
            },
        );
    }
}

fn build_alerts(
    game: &GameView,
    dashboard: Option<DashboardExperienceSignals<'_>>,
) -> Vec<GameplayAlert> {
    let mut alerts = Vec::new();

    if let Some(signals) = dashboard {
        for emergency in signals.emergencies.iter().take(3) {
            alerts.push(GameplayAlert {
                key: emergency.emergency_id.clone(),
                severity: emergency.severity,
                title: emergency.title.clone(),
                summary: emergency.description.clone(),
                target: Some("civilization_emergencies".to_string()),
            });
        }

        if signals.local_open_count == 0 && signals.travel_required_open_count > 0 {
            alerts.push(GameplayAlert {
                key: "travel_required_contracts".to_string(),
                severity: 4,
                title: "Available contracts are off your current route".to_string(),
                summary: format!(
                    "{} eligible contracts exist, but all of them require travel from your current position.",
                    signals.eligible_open_count
                ),
                target: Some("galaxy_map".to_string()),
            });
        }
    }

    if let Some(travel_state) = game.travel_state.as_ref()
        && let Some(consequence) = travel_state.last_consequence.as_ref()
    {
        let severity = travel_alert_severity(&consequence.route_risk_level);
        if severity > 0 {
            alerts.push(GameplayAlert {
                key: "travel_risk_after_arrival".to_string(),
                severity,
                title: "Your last route was risky".to_string(),
                summary: consequence.summary.clone(),
                target: Some("galaxy_map".to_string()),
            });
        }
    }

    if game
        .organizations
        .iter()
        .any(|organization| organization.governance_summary.open_proposals_count > 0)
    {
        alerts.push(GameplayAlert {
            key: "organization_governance_pending".to_string(),
            severity: 3,
            title: "Organization governance is waiting on members".to_string(),
            summary:
                "At least one organization you belong to has an open proposal awaiting action."
                    .to_string(),
            target: Some("organizations".to_string()),
        });
    }

    alerts.sort_by(|a, b| b.severity.cmp(&a.severity).then_with(|| a.key.cmp(&b.key)));
    alerts.truncate(5);
    alerts
}

fn build_priority_cards(game: &GameView) -> Vec<GameplayPriorityCard> {
    let mut cards = vec![GameplayPriorityCard {
        key: "onboarding".to_string(),
        title: "First-session progression".to_string(),
        summary: game.onboarding.current_focus.clone(),
        status_label: game.onboarding.current_phase.clone(),
        progress_pct: game.onboarding.progress_pct,
        target: "game_onboarding".to_string(),
    }];

    if let Some(starter_set) = game.starter_missions.as_ref() {
        cards.push(GameplayPriorityCard {
            key: "starter_chain".to_string(),
            title: starter_set.objective_chain.title.clone(),
            summary: starter_set.objective_chain.summary.clone(),
            status_label: starter_set
                .objective_chain
                .current_step_key
                .clone()
                .unwrap_or_else(|| "completed".to_string()),
            progress_pct: starter_set.objective_chain.progress_pct,
            target: "starter_missions".to_string(),
        });
    }

    if let Some(mission_pack) = game.mission_pack.as_ref() {
        let progress_pct = if mission_pack.summary.current_template_count == 0 {
            0
        } else {
            let progress = (mission_pack.summary.existing_count * 100)
                / mission_pack.summary.current_template_count;
            u8::try_from(progress.min(100)).unwrap_or(100)
        };
        cards.push(GameplayPriorityCard {
            key: "mission_pack".to_string(),
            title: "Current stage mission pack".to_string(),
            summary: format!(
                "{} templates are active for the {} stage.",
                mission_pack.summary.current_template_count,
                mission_pack.summary.current_stage_label
            ),
            status_label: mission_pack.summary.current_stage_label.clone(),
            progress_pct,
            target: "mission_pack".to_string(),
        });
    }

    if let Some(travel_state) = game.travel_state.as_ref() {
        cards.push(travel_priority_card(travel_state));
    }

    if let Some(organization) = game.organizations.first() {
        let complete_gates = organization
            .autonomy_track
            .gates
            .iter()
            .filter(|gate| gate.complete)
            .count();
        let total_gates = organization.autonomy_track.gates.len().max(1);
        let progress_pct = u8::try_from((complete_gates * 100) / total_gates).unwrap_or(100);
        cards.push(GameplayPriorityCard {
            key: "organization_progression".to_string(),
            title: organization.organization.name.clone(),
            summary: organization.autonomy_track.next_action.clone(),
            status_label: organization.autonomy_track.current_status.clone(),
            progress_pct,
            target: "organizations".to_string(),
        });
    }

    cards.truncate(5);
    cards
}

fn travel_priority_card(travel_state: &TravelStateRecord) -> GameplayPriorityCard {
    if let Some(session) = travel_state.active_session.as_ref() {
        return GameplayPriorityCard {
            key: "active_travel".to_string(),
            title: "Active route in progress".to_string(),
            summary: format!(
                "Transit from {} to {} is active and waiting for arrival resolution.",
                session.from_system_id, session.to_system_id
            ),
            status_label: "in_transit".to_string(),
            progress_pct: 50,
            target: "galaxy_map".to_string(),
        };
    }

    if let Some(consequence) = travel_state.last_consequence.as_ref() {
        return GameplayPriorityCard {
            key: "last_arrival".to_string(),
            title: "Latest arrival outcome".to_string(),
            summary: consequence.summary.clone(),
            status_label: format!("{:?}", consequence.route_risk_level).to_lowercase(),
            progress_pct: 100,
            target: "galaxy_map".to_string(),
        };
    }

    GameplayPriorityCard {
        key: "travel_position".to_string(),
        title: "Current route position".to_string(),
        summary: format!(
            "You are currently anchored at {} inside the active galaxy map.",
            travel_state.current_position.system_id
        ),
        status_label: "anchored".to_string(),
        progress_pct: 100,
        target: "galaxy_map".to_string(),
    }
}

fn map_action_kind(kind: &OnboardingActionKind) -> GameplayActionKind {
    match kind {
        OnboardingActionKind::ApiCall => GameplayActionKind::ApiCall,
        OnboardingActionKind::OpenScreen => GameplayActionKind::OpenScreen,
    }
}

fn push_unique_action(actions: &mut Vec<GameplayNextAction>, candidate: GameplayNextAction) {
    if actions.iter().any(|existing| existing.key == candidate.key) {
        return;
    }
    actions.push(candidate);
}

fn travel_alert_severity(risk_level: &TravelRiskLevel) -> u8 {
    match risk_level {
        TravelRiskLevel::Stable => 0,
        TravelRiskLevel::Guarded => 2,
        TravelRiskLevel::Contested => 4,
        TravelRiskLevel::Volatile => 6,
    }
}
