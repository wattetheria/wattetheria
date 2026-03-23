use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::civilization::galaxy::{DynamicEvent, DynamicEventCategory, GalaxyState};
use crate::civilization::missions::{
    CivilMission, MissionBoard, MissionDomain, MissionPublisherKind, MissionReward,
};
use crate::civilization::profiles::{CitizenProfile, Faction, RolePath};
use crate::map::registry::GalaxyMapRegistry;

use super::anchor::{MissionAnchor, locate_anchor};
use super::progression::GameStage;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GameMissionTemplate {
    pub template_id: String,
    pub stage: GameStage,
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
    pub tags: Vec<String>,
    pub payload_schema: GameMissionPayloadSchema,
    pub suggested_payload: serde_json::Value,
    pub phase: MissionPackPhase,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MissionPackPhase {
    Role,
    Civic,
    Event,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MissionPayloadField {
    pub key: String,
    pub title: String,
    pub field_type: String,
    pub required: bool,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GameMissionPayloadSchema {
    pub objective_type: String,
    pub fields: Vec<MissionPayloadField>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GameMissionPackSummary {
    pub current_stage_label: String,
    pub current_template_count: usize,
    pub existing_count: usize,
    pub missing_count: usize,
    pub role_template_count: usize,
    pub civic_template_count: usize,
    pub event_template_count: usize,
    pub upcoming_stage_label: Option<String>,
    pub upcoming_template_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GameMissionPack {
    pub stage: GameStage,
    pub templates: Vec<GameMissionTemplate>,
    pub existing: Vec<CivilMission>,
    pub missing_template_ids: Vec<String>,
    pub upcoming_stage: Option<GameStage>,
    pub upcoming_templates: Vec<GameMissionTemplate>,
    pub summary: GameMissionPackSummary,
}

#[must_use]
pub fn mission_pack_set(
    controller_id: &str,
    profile: &CitizenProfile,
    stage: GameStage,
    maps: &GalaxyMapRegistry,
    galaxy: &GalaxyState,
    board: &MissionBoard,
) -> GameMissionPack {
    let anchor = locate_anchor(profile, maps);
    let templates = templates_for_stage(profile, stage.clone(), &anchor, galaxy);
    let upcoming_stage = next_stage(&stage);
    let upcoming_templates = upcoming_stage.as_ref().map_or_else(Vec::new, |next_stage| {
        templates_for_stage(profile, next_stage.clone(), &anchor, galaxy)
    });
    let existing: Vec<_> = board
        .list(None)
        .into_iter()
        .filter(|mission| {
            mission.payload["game_pack_owner_agent_did"].as_str() == Some(controller_id)
                && mission.payload["game_pack_stage"].as_str() == Some(stage_label(&stage))
                && mission.payload["game_pack_template_id"].is_string()
        })
        .collect();
    let missing_template_ids: Vec<String> = templates
        .iter()
        .filter(|template| {
            !existing.iter().any(|mission| {
                mission.payload["game_pack_template_id"].as_str()
                    == Some(template.template_id.as_str())
            })
        })
        .map(|template| template.template_id.clone())
        .collect();
    let summary = build_pack_summary(
        &stage,
        &templates,
        &existing,
        &missing_template_ids,
        upcoming_stage.as_ref(),
        &upcoming_templates,
    );

    GameMissionPack {
        stage,
        templates,
        existing,
        missing_template_ids,
        upcoming_stage,
        upcoming_templates,
        summary,
    }
}

pub fn bootstrap_mission_pack(
    controller_id: &str,
    profile: &CitizenProfile,
    stage: &GameStage,
    maps: &GalaxyMapRegistry,
    galaxy: &GalaxyState,
    board: &mut MissionBoard,
) -> Vec<CivilMission> {
    let pack = mission_pack_set(controller_id, profile, stage.clone(), maps, galaxy, board);
    let mut created = Vec::new();
    for template in pack.templates {
        if !pack
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
                "game_pack_template_id": template.template_id,
                "game_pack_owner_agent_did": controller_id,
                "game_pack_role": profile.role,
                "game_pack_stage": stage_label(stage),
                "game_pack_phase": template.phase,
                "game_pack_payload_schema": template.payload_schema,
                "game_pack_suggested_payload": template.suggested_payload,
                "map_anchor": template.anchor,
                "tags": template.tags,
            }),
        );
        created.push(mission);
    }
    created
}

fn templates_for_stage(
    profile: &CitizenProfile,
    stage: GameStage,
    anchor: &MissionAnchor,
    galaxy: &GalaxyState,
) -> Vec<GameMissionTemplate> {
    let mut templates = vec![
        role_template(profile, stage.clone(), anchor.clone()),
        civic_template(profile, stage, anchor.clone()),
    ];
    templates.extend(event_templates(
        profile,
        &templates[0].stage,
        anchor,
        galaxy,
    ));
    templates
}

fn event_templates(
    profile: &CitizenProfile,
    stage: &GameStage,
    anchor: &MissionAnchor,
    galaxy: &GalaxyState,
) -> Vec<GameMissionTemplate> {
    let Some(home_zone_id) = profile.home_zone_id.as_deref() else {
        return Vec::new();
    };
    galaxy
        .events(Some(home_zone_id))
        .into_iter()
        .filter(|event| event.severity >= 6)
        .take(2)
        .map(|event| event_template(profile, stage, anchor, &event))
        .collect()
}

fn event_template(
    profile: &CitizenProfile,
    stage: &GameStage,
    anchor: &MissionAnchor,
    event: &DynamicEvent,
) -> GameMissionTemplate {
    let (domain, title_prefix, description_focus) = match event.category {
        DynamicEventCategory::Economic => (
            MissionDomain::Trade,
            "Stabilize",
            "convert economic pressure into corridor liquidity and local resilience",
        ),
        DynamicEventCategory::Spatial => (
            MissionDomain::Security,
            "Respond to",
            "reduce route pressure and restore safe movement through the affected corridor",
        ),
        DynamicEventCategory::Political => (
            MissionDomain::Power,
            "Resolve",
            "turn political instability into civic legibility and governed order",
        ),
    };
    let severity_bonus = i64::from(event.severity.saturating_sub(5));
    mission_template(
        &format!("event-{}-{}", stage_label(stage), event.event_id),
        stage.clone(),
        &format!("{title_prefix} {}", event.title),
        &format!(
            "{}. Use your {} role around {} to {}.",
            event.description,
            role_slug(&profile.role),
            anchor.system_name,
            description_focus
        ),
        profile,
        domain,
        starter_reward(
            22 + severity_bonus * 6,
            2 + severity_bonus,
            4 + severity_bonus,
        ),
        anchor.clone(),
        vec![
            role_slug(&profile.role).to_string(),
            stage_label(stage).to_string(),
            "event".to_string(),
            event.zone_id.clone(),
            event_category_slug(&event.category).to_string(),
        ],
        MissionPackPhase::Event,
    )
}

fn role_template(
    profile: &CitizenProfile,
    stage: GameStage,
    anchor: MissionAnchor,
) -> GameMissionTemplate {
    let stage_key = stage_label(&stage).to_string();
    let spec = role_template_spec(&profile.role, &stage);

    mission_template(
        &format!("{}-{}-core", role_slug(&profile.role), stage_label(&stage)),
        stage,
        &format!(
            "{} {} in {}",
            capitalize(spec.verb),
            spec.target,
            anchor.system_name
        ),
        &format!(
            "Use your {} role to {} around {} and convert local work into durable {} influence.",
            role_slug(&profile.role),
            spec.verb,
            anchor.system_name,
            domain_slug(&spec.domain)
        ),
        profile,
        spec.domain,
        starter_reward(spec.agent_watt, spec.reputation, spec.treasury_share_watt),
        anchor,
        vec![
            role_slug(&profile.role).to_string(),
            stage_key,
            "core".to_string(),
        ],
        MissionPackPhase::Role,
    )
}

struct RoleTemplateSpec {
    domain: MissionDomain,
    verb: &'static str,
    target: &'static str,
    agent_watt: i64,
    reputation: i64,
    treasury_share_watt: i64,
}

fn role_template_spec(role: &RolePath, stage: &GameStage) -> RoleTemplateSpec {
    match role {
        RolePath::Operator => operator_spec(stage),
        RolePath::Broker => broker_spec(stage),
        RolePath::Enforcer => enforcer_spec(stage),
        RolePath::Artificer => artificer_spec(stage),
    }
}

fn operator_spec(stage: &GameStage) -> RoleTemplateSpec {
    match stage {
        GameStage::Survival => spec(
            MissionDomain::Wealth,
            "stabilize",
            "relay throughput",
            24,
            2,
            4,
        ),
        GameStage::Foothold => spec(
            MissionDomain::Power,
            "scale",
            "infrastructure reliability",
            34,
            3,
            6,
        ),
        GameStage::Influence => spec(
            MissionDomain::Power,
            "coordinate",
            "public infrastructure flow",
            48,
            5,
            8,
        ),
        GameStage::Expansion => spec(
            MissionDomain::Wealth,
            "prepare",
            "expansion logistics",
            60,
            6,
            10,
        ),
    }
}

fn broker_spec(stage: &GameStage) -> RoleTemplateSpec {
    match stage {
        GameStage::Survival => spec(MissionDomain::Trade, "seed", "route liquidity", 26, 3, 5),
        GameStage::Foothold => spec(
            MissionDomain::Wealth,
            "rebalance",
            "frontier demand",
            38,
            4,
            7,
        ),
        GameStage::Influence => spec(MissionDomain::Trade, "shape", "corridor pricing", 52, 5, 9),
        GameStage::Expansion => spec(
            MissionDomain::Trade,
            "open",
            "new market corridors",
            66,
            7,
            12,
        ),
    }
}

fn enforcer_spec(stage: &GameStage) -> RoleTemplateSpec {
    match stage {
        GameStage::Survival => spec(
            MissionDomain::Security,
            "patrol",
            "home-route risk",
            26,
            3,
            5,
        ),
        GameStage::Foothold => spec(MissionDomain::Power, "escort", "civic convoys", 38, 4, 7),
        GameStage::Influence => spec(
            MissionDomain::Security,
            "suppress",
            "frontier instability",
            52,
            6,
            9,
        ),
        GameStage::Expansion => spec(
            MissionDomain::Power,
            "secure",
            "new expansion lanes",
            66,
            7,
            12,
        ),
    }
}

fn artificer_spec(stage: &GameStage) -> RoleTemplateSpec {
    match stage {
        GameStage::Survival => spec(
            MissionDomain::Culture,
            "signal",
            "home-zone identity",
            22,
            3,
            4,
        ),
        GameStage::Foothold => spec(
            MissionDomain::Trade,
            "attract",
            "corridor attention",
            34,
            4,
            6,
        ),
        GameStage::Influence => spec(MissionDomain::Culture, "shape", "public gravity", 48, 6, 8),
        GameStage::Expansion => spec(
            MissionDomain::Culture,
            "design",
            "expansion landmarks",
            62,
            7,
            10,
        ),
    }
}

fn spec(
    domain: MissionDomain,
    verb: &'static str,
    target: &'static str,
    agent_watt: i64,
    reputation: i64,
    treasury_share_watt: i64,
) -> RoleTemplateSpec {
    RoleTemplateSpec {
        domain,
        verb,
        target,
        agent_watt,
        reputation,
        treasury_share_watt,
    }
}

fn civic_template(
    profile: &CitizenProfile,
    stage: GameStage,
    anchor: MissionAnchor,
) -> GameMissionTemplate {
    let stage_key = stage_label(&stage).to_string();
    let (domain, title, description, watt, reputation, treasury) = match stage {
        GameStage::Survival => (
            MissionDomain::Power,
            "Document local civic conditions",
            "Build the first public record that turns private work into civic legibility.",
            20,
            2,
            4,
        ),
        GameStage::Foothold => (
            MissionDomain::Power,
            "Support local treasury and stability",
            "Use your home anchor to prove that repeatable work can support public order.",
            32,
            3,
            6,
        ),
        GameStage::Influence => (
            MissionDomain::Power,
            "Prepare governance readiness",
            "Cross the final civic gates between private success and formal sovereignty participation.",
            46,
            5,
            8,
        ),
        GameStage::Expansion => (
            MissionDomain::Power,
            "Stage frontier expansion support",
            "Demonstrate that your current base can underwrite the next layer of map growth.",
            58,
            6,
            10,
        ),
    };

    mission_template(
        &format!("{}-{}-civic", role_slug(&profile.role), stage_label(&stage)),
        stage,
        title,
        description,
        profile,
        domain,
        starter_reward(watt, reputation, treasury),
        anchor,
        vec![
            role_slug(&profile.role).to_string(),
            stage_key,
            "civic".to_string(),
        ],
        MissionPackPhase::Civic,
    )
}

#[allow(clippy::too_many_arguments)]
fn mission_template(
    template_id: &str,
    stage: GameStage,
    title: &str,
    description: &str,
    profile: &CitizenProfile,
    domain: MissionDomain,
    reward: MissionReward,
    anchor: MissionAnchor,
    tags: Vec<String>,
    phase: MissionPackPhase,
) -> GameMissionTemplate {
    GameMissionTemplate {
        template_id: template_id.to_string(),
        stage,
        title: title.to_string(),
        description: description.to_string(),
        publisher: profile
            .home_subnet_id
            .clone()
            .unwrap_or_else(|| anchor.system_id.clone()),
        publisher_kind: MissionPublisherKind::System,
        domain,
        subnet_id: profile.home_subnet_id.clone(),
        zone_id: profile.home_zone_id.clone(),
        required_role: Some(profile.role.clone()),
        required_faction: Some(profile.faction.clone()),
        reward,
        anchor,
        tags,
        payload_schema: default_payload_schema(&phase),
        suggested_payload: default_suggested_payload(template_id, &phase),
        phase,
    }
}

fn default_payload_schema(phase: &MissionPackPhase) -> GameMissionPayloadSchema {
    let mut fields = vec![
        MissionPayloadField {
            key: "objective".to_string(),
            title: "Objective".to_string(),
            field_type: "string".to_string(),
            required: true,
            description: "Short machine-readable objective key for the mission flow.".to_string(),
        },
        MissionPayloadField {
            key: "map_anchor".to_string(),
            title: "Map Anchor".to_string(),
            field_type: "object".to_string(),
            required: true,
            description: "Canonical system, planet, and route anchor for this mission.".to_string(),
        },
    ];
    match phase {
        MissionPackPhase::Role => fields.push(MissionPayloadField {
            key: "role_track".to_string(),
            title: "Role Track".to_string(),
            field_type: "string".to_string(),
            required: true,
            description: "Role-specialized execution track for the mission.".to_string(),
        }),
        MissionPackPhase::Civic => fields.push(MissionPayloadField {
            key: "civic_goal".to_string(),
            title: "Civic Goal".to_string(),
            field_type: "string".to_string(),
            required: true,
            description: "Public-order or governance outcome the mission should support."
                .to_string(),
        }),
        MissionPackPhase::Event => fields.push(MissionPayloadField {
            key: "event_id".to_string(),
            title: "Event Id".to_string(),
            field_type: "string".to_string(),
            required: true,
            description: "Source galaxy event that produced this response mission.".to_string(),
        }),
    }
    GameMissionPayloadSchema {
        objective_type: match phase {
            MissionPackPhase::Role => "role_track".to_string(),
            MissionPackPhase::Civic => "civic_support".to_string(),
            MissionPackPhase::Event => "event_response".to_string(),
        },
        fields,
    }
}

fn default_suggested_payload(template_id: &str, phase: &MissionPackPhase) -> serde_json::Value {
    match phase {
        MissionPackPhase::Role => json!({
            "objective": template_id,
            "role_track": "core-specialization",
        }),
        MissionPackPhase::Civic => json!({
            "objective": template_id,
            "civic_goal": "stability_support",
        }),
        MissionPackPhase::Event => json!({
            "objective": template_id,
            "event_id": template_id,
        }),
    }
}

fn build_pack_summary(
    stage: &GameStage,
    templates: &[GameMissionTemplate],
    existing: &[CivilMission],
    missing_template_ids: &[String],
    upcoming_stage: Option<&GameStage>,
    upcoming_templates: &[GameMissionTemplate],
) -> GameMissionPackSummary {
    GameMissionPackSummary {
        current_stage_label: stage_label(stage).to_string(),
        current_template_count: templates.len(),
        existing_count: existing.len(),
        missing_count: missing_template_ids.len(),
        role_template_count: templates
            .iter()
            .filter(|template| template.phase == MissionPackPhase::Role)
            .count(),
        civic_template_count: templates
            .iter()
            .filter(|template| template.phase == MissionPackPhase::Civic)
            .count(),
        event_template_count: templates
            .iter()
            .filter(|template| template.phase == MissionPackPhase::Event)
            .count(),
        upcoming_stage_label: upcoming_stage.map(stage_label).map(str::to_string),
        upcoming_template_count: upcoming_templates.len(),
    }
}

fn next_stage(stage: &GameStage) -> Option<GameStage> {
    match stage {
        GameStage::Survival => Some(GameStage::Foothold),
        GameStage::Foothold => Some(GameStage::Influence),
        GameStage::Influence => Some(GameStage::Expansion),
        GameStage::Expansion => None,
    }
}

fn starter_reward(agent_watt: i64, reputation: i64, treasury_share_watt: i64) -> MissionReward {
    MissionReward {
        agent_watt,
        reputation,
        capacity: 1,
        treasury_share_watt,
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

fn domain_slug(domain: &MissionDomain) -> &'static str {
    match domain {
        MissionDomain::Wealth => "wealth",
        MissionDomain::Power => "power",
        MissionDomain::Security => "security",
        MissionDomain::Trade => "trade",
        MissionDomain::Culture => "culture",
    }
}

fn event_category_slug(category: &DynamicEventCategory) -> &'static str {
    match category {
        DynamicEventCategory::Economic => "economic",
        DynamicEventCategory::Spatial => "spatial",
        DynamicEventCategory::Political => "political",
    }
}

fn capitalize(value: &str) -> String {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) => format!("{}{}", first.to_uppercase(), chars.as_str()),
        None => String::new(),
    }
}

#[must_use]
pub fn stage_label(stage: &GameStage) -> &'static str {
    match stage {
        GameStage::Survival => "survival",
        GameStage::Foothold => "foothold",
        GameStage::Influence => "influence",
        GameStage::Expansion => "expansion",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::civilization::galaxy::GalaxyState;
    use crate::civilization::profiles::{Faction, StrategyProfile};

    #[test]
    fn mission_pack_bootstrap_creates_stage_aligned_missions() {
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
        let galaxy = GalaxyState::default_with_core_zones();

        let created = bootstrap_mission_pack(
            "agent-a",
            &profile,
            &GameStage::Foothold,
            &maps,
            &galaxy,
            &mut board,
        );
        assert_eq!(created.len(), 2);
        let duplicate = bootstrap_mission_pack(
            "agent-a",
            &profile,
            &GameStage::Foothold,
            &maps,
            &galaxy,
            &mut board,
        );
        assert!(duplicate.is_empty());

        let pack = mission_pack_set(
            "agent-a",
            &profile,
            GameStage::Foothold,
            &maps,
            &galaxy,
            &board,
        );
        assert_eq!(pack.templates.len(), 2);
        assert_eq!(pack.existing.len(), 2);
        assert_eq!(pack.summary.current_template_count, 2);
        assert_eq!(pack.summary.role_template_count, 1);
        assert_eq!(pack.summary.civic_template_count, 1);
        assert_eq!(pack.summary.event_template_count, 0);
        assert_eq!(pack.upcoming_stage, Some(GameStage::Influence));
        assert_eq!(pack.upcoming_templates.len(), 2);
        assert!(
            pack.templates
                .iter()
                .all(|template| template.anchor.map_id == "genesis-base")
        );
        assert!(pack.templates.iter().all(|template| {
            template
                .payload_schema
                .fields
                .iter()
                .any(|field| field.key == "map_anchor")
        }));
    }

    #[test]
    fn mission_pack_includes_high_severity_home_zone_events() {
        let profile = CitizenProfile {
            agent_did: "agent-a".to_string(),
            faction: Faction::Freeport,
            role: RolePath::Broker,
            strategy: StrategyProfile::Balanced,
            home_subnet_id: Some("planet-test".to_string()),
            home_zone_id: Some("genesis-core".to_string()),
            updated_at: 0,
        };
        let board = MissionBoard::default();
        let mut maps = GalaxyMapRegistry::default();
        let mut galaxy = GalaxyState::default_with_core_zones();
        maps.ensure_default_genesis_map(&galaxy.zones()).unwrap();
        galaxy
            .publish_event(
                DynamicEventCategory::Economic,
                "genesis-core",
                "Power shortage",
                "Industrial demand outpaced supply.",
                7,
                None,
                vec!["supply".to_string()],
            )
            .unwrap();

        let pack = mission_pack_set(
            "agent-a",
            &profile,
            GameStage::Foothold,
            &maps,
            &galaxy,
            &board,
        );
        assert!(
            pack.templates
                .iter()
                .any(|template| template.tags.iter().any(|tag| tag == "event"))
        );
        assert_eq!(pack.summary.event_template_count, 1);
        assert!(
            pack.templates
                .iter()
                .filter(|template| template.phase == MissionPackPhase::Event)
                .all(|template| template.suggested_payload["event_id"].is_string())
        );
    }
}
