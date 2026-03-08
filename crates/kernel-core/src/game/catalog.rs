use serde::{Deserialize, Serialize};

use crate::civilization::missions::MissionDomain;
use crate::civilization::profiles::{Faction, RolePath};

use super::progression::GameStage;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GameStageDefinition {
    pub stage: GameStage,
    pub title: String,
    pub summary: String,
    pub north_star: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RolePlaybook {
    pub role: RolePath,
    pub title: String,
    pub summary: String,
    pub focus_domains: Vec<MissionDomain>,
    pub starter_objectives: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FactionPlaybook {
    pub faction: Faction,
    pub title: String,
    pub summary: String,
    pub political_style: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GameCatalog {
    pub stages: Vec<GameStageDefinition>,
    pub roles: Vec<RolePlaybook>,
    pub factions: Vec<FactionPlaybook>,
}

#[must_use]
pub fn catalog() -> GameCatalog {
    GameCatalog {
        stages: vec![
            GameStageDefinition {
                stage: GameStage::Survival,
                title: "Survival".to_string(),
                summary: "Establish a working foothold inside the genesis network."
                    .to_string(),
                north_star: "Create a stable identity, settle early missions, and anchor yourself to a home zone.".to_string(),
            },
            GameStageDefinition {
                stage: GameStage::Foothold,
                title: "Foothold".to_string(),
                summary: "Turn ad-hoc work into reliable influence and operating capacity."
                    .to_string(),
                north_star: "Build repeatable income, finish aligned missions, and strengthen your home position.".to_string(),
            },
            GameStageDefinition {
                stage: GameStage::Influence,
                title: "Influence".to_string(),
                summary: "Shape market, security, and governance outcomes beyond your own node."
                    .to_string(),
                north_star: "Earn enough trust and power to participate in sovereignty and major civic decisions.".to_string(),
            },
            GameStageDefinition {
                stage: GameStage::Expansion,
                title: "Expansion".to_string(),
                summary: "Push the galaxy outward through governance, route control, and map growth."
                    .to_string(),
                north_star: "Turn local success into lasting control over new systems, routes, and future map deployments.".to_string(),
            },
        ],
        roles: vec![
            RolePlaybook {
                role: RolePath::Operator,
                title: "Operator".to_string(),
                summary: "Infrastructure, maintenance, and dependable throughput.".to_string(),
                focus_domains: vec![MissionDomain::Wealth, MissionDomain::Power],
                starter_objectives: vec![
                    "Stabilize your home zone".to_string(),
                    "Complete logistics and infrastructure contracts".to_string(),
                ],
            },
            RolePlaybook {
                role: RolePath::Broker,
                title: "Broker".to_string(),
                summary: "Trade, liquidity, information flow, and market leverage."
                    .to_string(),
                focus_domains: vec![MissionDomain::Trade, MissionDomain::Wealth],
                starter_objectives: vec![
                    "Connect frontier and genesis markets".to_string(),
                    "Build repeatable trade influence".to_string(),
                ],
            },
            RolePlaybook {
                role: RolePath::Enforcer,
                title: "Enforcer".to_string(),
                summary: "Security, escort, patrol, and sovereignty pressure.".to_string(),
                focus_domains: vec![MissionDomain::Security, MissionDomain::Power],
                starter_objectives: vec![
                    "Reduce route risk".to_string(),
                    "Protect high-value planetary or convoy missions".to_string(),
                ],
            },
            RolePlaybook {
                role: RolePath::Artificer,
                title: "Artificer".to_string(),
                summary: "Culture, construction, signaling, and social gravity.".to_string(),
                focus_domains: vec![MissionDomain::Culture, MissionDomain::Trade],
                starter_objectives: vec![
                    "Improve local attraction and identity".to_string(),
                    "Turn creativity into durable cultural influence".to_string(),
                ],
            },
        ],
        factions: vec![
            FactionPlaybook {
                faction: Faction::Order,
                title: "Order".to_string(),
                summary: "Prefers stability, infrastructure, and enforceable legitimacy."
                    .to_string(),
                political_style: "Infrastructure-first governance".to_string(),
            },
            FactionPlaybook {
                faction: Faction::Freeport,
                title: "Freeport".to_string(),
                summary: "Prefers liquidity, open exchange, and neutral corridors.".to_string(),
                political_style: "Market-first coordination".to_string(),
            },
            FactionPlaybook {
                faction: Faction::Raider,
                title: "Raider".to_string(),
                summary: "Prefers risk, pressure, and opportunistic route domination."
                    .to_string(),
                political_style: "Pressure-first frontier politics".to_string(),
            },
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_contains_all_core_roles_and_stages() {
        let catalog = catalog();
        assert_eq!(catalog.stages.len(), 4);
        assert_eq!(catalog.roles.len(), 4);
        assert_eq!(catalog.factions.len(), 3);
        assert!(
            catalog
                .roles
                .iter()
                .any(|role| role.role == RolePath::Broker)
        );
        assert!(
            catalog
                .stages
                .iter()
                .any(|stage| stage.stage == GameStage::Expansion)
        );
    }
}
