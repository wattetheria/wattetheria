use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::civilization::missions::{
    CivilMission, MissionBoard, MissionDomain, MissionPublisherKind, MissionReward,
};
use crate::civilization::profiles::{CitizenProfile, Faction, RolePath};
use crate::map::registry::GalaxyMapRegistry;

use super::anchor::{MissionAnchor, locate_anchor};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StarterObjectiveState {
    Missing,
    Open,
    Claimed,
    Completed,
    Settled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StarterObjectiveStep {
    pub step_key: String,
    pub title: String,
    pub template_id: String,
    pub order: u8,
    pub state: StarterObjectiveState,
    pub progress_pct: u8,
    pub mission_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StarterObjectiveChain {
    pub chain_id: String,
    pub title: String,
    pub summary: String,
    pub role: RolePath,
    pub progress_pct: u8,
    pub current_step_key: Option<String>,
    pub steps: Vec<StarterObjectiveStep>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StarterMissionTemplate {
    pub template_id: String,
    pub step_key: String,
    pub step_title: String,
    pub step_order: u8,
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
    pub anchor: MissionAnchor,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StarterMissionSet {
    pub objective_chain: StarterObjectiveChain,
    pub templates: Vec<StarterMissionTemplate>,
    pub existing: Vec<CivilMission>,
    pub missing_template_ids: Vec<String>,
}

#[must_use]
pub fn starter_mission_set(
    controller_id: &str,
    profile: &CitizenProfile,
    maps: &GalaxyMapRegistry,
    board: &MissionBoard,
) -> StarterMissionSet {
    let templates = starter_templates(profile, maps);
    let existing: Vec<_> = board
        .list(None)
        .into_iter()
        .filter(|mission| {
            mission.payload["starter_owner_agent_did"].as_str() == Some(controller_id)
                && mission.payload["starter_template_id"].is_string()
        })
        .collect();
    let missing_template_ids = templates
        .iter()
        .filter(|template| {
            !existing.iter().any(|mission| {
                mission.payload["starter_template_id"].as_str()
                    == Some(template.template_id.as_str())
            })
        })
        .map(|template| template.template_id.clone())
        .collect();
    let objective_chain = objective_chain(profile, &templates, &existing);

    StarterMissionSet {
        objective_chain,
        templates,
        existing,
        missing_template_ids,
    }
}

pub fn bootstrap_starter_missions(
    controller_id: &str,
    profile: &CitizenProfile,
    maps: &GalaxyMapRegistry,
    board: &mut MissionBoard,
) -> Vec<CivilMission> {
    let set = starter_mission_set(controller_id, profile, maps, board);
    let mut created = Vec::new();
    for template in set.templates {
        if !set
            .missing_template_ids
            .iter()
            .any(|missing| missing == &template.template_id)
        {
            continue;
        }
        let mission = board.publish(
            &template.title,
            &template.description,
            &template.publisher,
            template.publisher_kind.clone(),
            template.domain.clone(),
            template.subnet_id.clone(),
            template.zone_id.clone(),
            template.required_role.clone(),
            template.required_faction.clone(),
            template.reward.clone(),
            json!({
                "starter_template_id": template.template_id,
                "starter_owner_agent_did": controller_id,
                "starter_step_key": template.step_key,
                "starter_step_title": template.step_title,
                "starter_step_order": template.step_order,
                "starter_role": profile.role,
                "starter_faction": profile.faction,
                "home_zone_id": profile.home_zone_id,
                "home_subnet_id": profile.home_subnet_id,
                "map_anchor": template.anchor,
            }),
        );
        created.push(mission);
    }
    created
}

fn starter_templates(
    profile: &CitizenProfile,
    maps: &GalaxyMapRegistry,
) -> Vec<StarterMissionTemplate> {
    let anchor = locate_anchor(profile, maps);
    match profile.role {
        RolePath::Operator => operator_templates(profile, &anchor),
        RolePath::Broker => broker_templates(profile, &anchor),
        RolePath::Enforcer => enforcer_templates(profile, &anchor),
        RolePath::Artificer => artificer_templates(profile, &anchor),
    }
}

fn operator_templates(
    profile: &CitizenProfile,
    anchor: &MissionAnchor,
) -> Vec<StarterMissionTemplate> {
    vec![
        template(
            StarterStepSpec {
                template_id: "operator-grid-check",
                step_key: "stabilize_home_grid",
                step_title: "Stabilize your home grid",
                step_order: 1,
            },
            "Stabilize the local relay grid",
            "Inspect and stabilize the relay path that anchors your home zone.",
            MissionDomain::Wealth,
            profile,
            starter_reward(20, 2, 4),
            anchor.clone(),
        ),
        template(
            StarterStepSpec {
                template_id: "operator-throughput-audit",
                step_key: "audit_infrastructure_flow",
                step_title: "Audit infrastructure flow",
                step_order: 2,
            },
            "Audit local throughput",
            "Bring one home-zone infrastructure path back into dependable operating condition.",
            MissionDomain::Power,
            profile,
            starter_reward(30, 3, 6),
            anchor.clone(),
        ),
    ]
}

fn broker_templates(
    profile: &CitizenProfile,
    anchor: &MissionAnchor,
) -> Vec<StarterMissionTemplate> {
    vec![
        template(
            StarterStepSpec {
                template_id: "broker-genesis-frontier-liquidity",
                step_key: "seed_trade_corridor",
                step_title: "Seed your first trade corridor",
                step_order: 1,
            },
            "Seed the genesis-frontier corridor",
            "Move liquidity across the first safe trade corridor and restore flow between genesis and frontier systems.",
            MissionDomain::Trade,
            profile,
            starter_reward(25, 3, 5),
            anchor.clone(),
        ),
        template(
            StarterStepSpec {
                template_id: "broker-market-balance",
                step_key: "rebalance_frontier_exchange",
                step_title: "Rebalance the frontier exchange",
                step_order: 2,
            },
            "Rebalance the frontier exchange",
            "Resolve a supply imbalance for your home subnet and prove that you can keep a route alive.",
            MissionDomain::Wealth,
            profile,
            starter_reward(35, 4, 7),
            anchor.clone(),
        ),
    ]
}

fn enforcer_templates(
    profile: &CitizenProfile,
    anchor: &MissionAnchor,
) -> Vec<StarterMissionTemplate> {
    vec![
        template(
            StarterStepSpec {
                template_id: "enforcer-corridor-patrol",
                step_key: "patrol_home_corridor",
                step_title: "Patrol your home corridor",
                step_order: 1,
            },
            "Patrol the frontier corridor",
            "Reduce route risk around your home zone and establish a basic security presence.",
            MissionDomain::Security,
            profile,
            starter_reward(25, 3, 5),
            anchor.clone(),
        ),
        template(
            StarterStepSpec {
                template_id: "enforcer-convoy-escort",
                step_key: "escort_priority_convoy",
                step_title: "Escort a priority convoy",
                step_order: 2,
            },
            "Escort a priority convoy",
            "Protect a convoy moving into the frontier belt and demonstrate local authority.",
            MissionDomain::Power,
            profile,
            starter_reward(35, 4, 7),
            anchor.clone(),
        ),
    ]
}

fn artificer_templates(
    profile: &CitizenProfile,
    anchor: &MissionAnchor,
) -> Vec<StarterMissionTemplate> {
    vec![
        template(
            StarterStepSpec {
                template_id: "artificer-home-signal",
                step_key: "signal_home_zone",
                step_title: "Signal your home zone",
                step_order: 1,
            },
            "Signal your home zone",
            "Create a visible civic marker that makes your home zone legible to other participants.",
            MissionDomain::Culture,
            profile,
            starter_reward(20, 3, 4),
            anchor.clone(),
        ),
        template(
            StarterStepSpec {
                template_id: "artificer-frontier-attraction",
                step_key: "increase_frontier_attraction",
                step_title: "Increase frontier attraction",
                step_order: 2,
            },
            "Increase frontier attraction",
            "Improve local social gravity so your home subnet becomes easier to notice and harder to ignore.",
            MissionDomain::Trade,
            profile,
            starter_reward(30, 4, 6),
            anchor.clone(),
        ),
    ]
}

fn starter_reward(agent_watt: i64, reputation: i64, treasury_share_watt: i64) -> MissionReward {
    MissionReward {
        agent_watt,
        reputation,
        capacity: 1,
        treasury_share_watt,
    }
}

#[derive(Clone, Copy)]
struct StarterStepSpec<'a> {
    template_id: &'a str,
    step_key: &'a str,
    step_title: &'a str,
    step_order: u8,
}

fn template(
    step: StarterStepSpec<'_>,
    title: &str,
    description: &str,
    domain: MissionDomain,
    profile: &CitizenProfile,
    reward: MissionReward,
    anchor: MissionAnchor,
) -> StarterMissionTemplate {
    StarterMissionTemplate {
        template_id: step.template_id.to_string(),
        step_key: step.step_key.to_string(),
        step_title: step.step_title.to_string(),
        step_order: step.step_order,
        title: title.to_string(),
        description: description.to_string(),
        publisher: profile
            .home_subnet_id
            .clone()
            .unwrap_or_else(|| "genesis-prime".to_string()),
        publisher_kind: MissionPublisherKind::System,
        domain,
        subnet_id: profile.home_subnet_id.clone(),
        zone_id: profile.home_zone_id.clone(),
        required_role: Some(profile.role.clone()),
        required_faction: Some(profile.faction.clone()),
        reward,
        anchor,
    }
}

fn objective_chain(
    profile: &CitizenProfile,
    templates: &[StarterMissionTemplate],
    existing: &[CivilMission],
) -> StarterObjectiveChain {
    let steps: Vec<_> = templates
        .iter()
        .map(|template| {
            let mission = existing.iter().find(|mission| {
                mission.payload["starter_template_id"].as_str()
                    == Some(template.template_id.as_str())
            });
            let (state, progress_pct, mission_id) =
                mission.map_or((StarterObjectiveState::Missing, 0, None), |mission| {
                    let state = match mission.status {
                        crate::civilization::missions::MissionStatus::Open => {
                            StarterObjectiveState::Open
                        }
                        crate::civilization::missions::MissionStatus::Claimed => {
                            StarterObjectiveState::Claimed
                        }
                        crate::civilization::missions::MissionStatus::Completed => {
                            StarterObjectiveState::Completed
                        }
                        crate::civilization::missions::MissionStatus::Settled => {
                            StarterObjectiveState::Settled
                        }
                        crate::civilization::missions::MissionStatus::Cancelled => {
                            StarterObjectiveState::Missing
                        }
                    };
                    (
                        state,
                        starter_objective_progress(&mission.status),
                        Some(mission.mission_id.clone()),
                    )
                });
            StarterObjectiveStep {
                step_key: template.step_key.clone(),
                title: template.step_title.clone(),
                template_id: template.template_id.clone(),
                order: template.step_order,
                state,
                progress_pct,
                mission_id,
            }
        })
        .collect();
    let progress_pct = if steps.is_empty() {
        0
    } else {
        let total: u16 = steps.iter().map(|step| u16::from(step.progress_pct)).sum();
        u8::try_from(total / u16::try_from(steps.len()).unwrap_or(1)).unwrap_or(100)
    };
    let current_step_key = steps
        .iter()
        .find(|step| step.progress_pct < 100)
        .map(|step| step.step_key.clone());

    StarterObjectiveChain {
        chain_id: format!("starter-{}", role_slug(&profile.role)),
        title: format!("{} starter chain", role_title(&profile.role)),
        summary: role_chain_summary(&profile.role).to_string(),
        role: profile.role.clone(),
        progress_pct,
        current_step_key,
        steps,
    }
}

fn starter_objective_progress(status: &crate::civilization::missions::MissionStatus) -> u8 {
    match status {
        crate::civilization::missions::MissionStatus::Open => 25,
        crate::civilization::missions::MissionStatus::Claimed => 60,
        crate::civilization::missions::MissionStatus::Completed => 85,
        crate::civilization::missions::MissionStatus::Settled => 100,
        crate::civilization::missions::MissionStatus::Cancelled => 0,
    }
}

fn role_slug(role: &RolePath) -> &'static str {
    match role {
        RolePath::Operator => "operator",
        RolePath::Broker => "broker",
        RolePath::Enforcer => "enforcer",
        RolePath::Artificer => "artificer",
    }
}

fn role_title(role: &RolePath) -> &'static str {
    match role {
        RolePath::Operator => "Operator",
        RolePath::Broker => "Broker",
        RolePath::Enforcer => "Enforcer",
        RolePath::Artificer => "Artificer",
    }
}

fn role_chain_summary(role: &RolePath) -> &'static str {
    match role {
        RolePath::Operator => {
            "Stabilize local infrastructure first, then prove you can turn throughput into durable civic reliability."
        }
        RolePath::Broker => {
            "Open your first liquidity corridor, then show you can rebalance a live frontier market."
        }
        RolePath::Enforcer => {
            "Reduce route risk first, then establish authority by protecting a higher-value convoy."
        }
        RolePath::Artificer => {
            "Make your home zone visible first, then turn that signal into broader frontier attraction."
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::civilization::galaxy::GalaxyState;
    use crate::civilization::missions::MissionStatus;
    use crate::civilization::profiles::{Faction, StrategyProfile};
    use crate::map::registry::GalaxyMapRegistry;

    #[test]
    fn starter_bootstrap_creates_only_missing_templates() {
        let profile = CitizenProfile {
            agent_did: "agent-a".to_string(),
            faction: Faction::Freeport,
            role: RolePath::Broker,
            strategy: StrategyProfile::Balanced,
            home_subnet_id: Some("planet-test".to_string()),
            home_zone_id: Some("genesis-core".to_string()),
            updated_at: 0,
        };
        let mut board = MissionBoard::default();
        let mut maps = GalaxyMapRegistry::default();
        maps.ensure_default_genesis_map(&GalaxyState::default_with_core_zones().zones())
            .unwrap();
        let created = bootstrap_starter_missions("agent-a", &profile, &maps, &mut board);
        assert_eq!(created.len(), 2);
        let duplicate = bootstrap_starter_missions("agent-a", &profile, &maps, &mut board);
        assert!(duplicate.is_empty());
        let set = starter_mission_set("agent-a", &profile, &maps, &board);
        assert_eq!(set.templates.len(), 2);
        assert_eq!(set.existing.len(), 2);
        assert_eq!(set.objective_chain.steps.len(), 2);
        assert_eq!(set.objective_chain.progress_pct, 25);
        assert_eq!(
            set.objective_chain.current_step_key.as_deref(),
            Some("seed_trade_corridor")
        );
        assert!(
            set.existing
                .iter()
                .all(|mission| mission.status == MissionStatus::Open)
        );
        assert!(
            set.templates
                .iter()
                .all(|template| template.anchor.map_id == "genesis-base")
        );
        assert!(
            set.existing
                .iter()
                .all(|mission| mission.payload["map_anchor"].is_object())
        );
        assert!(
            set.existing
                .iter()
                .all(|mission| mission.payload["starter_step_key"].is_string())
        );
    }
}
