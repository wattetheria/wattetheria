use chrono::Utc;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PlanetKind {
    Hub,
    Industrial,
    Research,
    Relay,
    Fortress,
    Frontier,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MapCoordinate {
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PlanetNode {
    pub planet_id: String,
    pub name: String,
    pub kind: PlanetKind,
    pub zone_id: String,
    pub subnet_id: Option<String>,
    pub resource_multiplier: f64,
    pub governance_template: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StarSystem {
    pub system_id: String,
    pub name: String,
    pub position: MapCoordinate,
    pub description: String,
    pub planets: Vec<PlanetNode>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RouteEdge {
    pub route_id: String,
    pub from_system_id: String,
    pub to_system_id: String,
    pub travel_cost: u32,
    pub risk: u8,
    pub capacity: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GalaxyMap {
    pub map_id: String,
    pub name: String,
    pub description: String,
    pub official: bool,
    pub systems: Vec<StarSystem>,
    pub routes: Vec<RouteEdge>,
    pub tags: Vec<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GalaxyMapSummary {
    pub map_id: String,
    pub name: String,
    pub official: bool,
    pub system_count: usize,
    pub route_count: usize,
    pub tags: Vec<String>,
}

impl GalaxyMap {
    #[must_use]
    pub fn summary(&self) -> GalaxyMapSummary {
        GalaxyMapSummary {
            map_id: self.map_id.clone(),
            name: self.name.clone(),
            official: self.official,
            system_count: self.systems.len(),
            route_count: self.routes.len(),
            tags: self.tags.clone(),
        }
    }
}

#[must_use]
pub fn default_genesis_map() -> GalaxyMap {
    let now = Utc::now().timestamp();
    GalaxyMap {
        map_id: "genesis-base".to_string(),
        name: "Genesis Base Map".to_string(),
        description: "Official bootstrap star chart for the initial Wattetheria galaxy network."
            .to_string(),
        official: true,
        systems: vec![
            StarSystem {
                system_id: "genesis-prime".to_string(),
                name: "Genesis Prime".to_string(),
                position: MapCoordinate { x: 0, y: 0 },
                description: "Starter system anchored to the protected genesis core.".to_string(),
                planets: vec![
                    PlanetNode {
                        planet_id: "planet-main".to_string(),
                        name: "Planet Main".to_string(),
                        kind: PlanetKind::Hub,
                        zone_id: "genesis-core".to_string(),
                        subnet_id: Some("planet-main".to_string()),
                        resource_multiplier: 1.0,
                        governance_template: Some("migrant_council".to_string()),
                    },
                    PlanetNode {
                        planet_id: "relay-one".to_string(),
                        name: "Relay One".to_string(),
                        kind: PlanetKind::Relay,
                        zone_id: "genesis-core".to_string(),
                        subnet_id: None,
                        resource_multiplier: 0.8,
                        governance_template: None,
                    },
                ],
            },
            StarSystem {
                system_id: "frontier-gate".to_string(),
                name: "Frontier Gate".to_string(),
                position: MapCoordinate { x: 140, y: 40 },
                description: "Trade and sovereignty corridor leading into the frontier belt."
                    .to_string(),
                planets: vec![PlanetNode {
                    planet_id: "planet-test".to_string(),
                    name: "Planet Test".to_string(),
                    kind: PlanetKind::Frontier,
                    zone_id: "frontier-belt".to_string(),
                    subnet_id: Some("planet-test".to_string()),
                    resource_multiplier: 1.4,
                    governance_template: Some("freeport_exchange".to_string()),
                }],
            },
            StarSystem {
                system_id: "abyss-watch".to_string(),
                name: "Abyss Watch".to_string(),
                position: MapCoordinate { x: 260, y: -20 },
                description: "Deep-space observation and defense station.".to_string(),
                planets: vec![PlanetNode {
                    planet_id: "deep-watch".to_string(),
                    name: "Deep Watch".to_string(),
                    kind: PlanetKind::Fortress,
                    zone_id: "deep-space".to_string(),
                    subnet_id: None,
                    resource_multiplier: 2.0,
                    governance_template: Some("fortress_command".to_string()),
                }],
            },
        ],
        routes: vec![
            RouteEdge {
                route_id: "route-genesis-frontier".to_string(),
                from_system_id: "genesis-prime".to_string(),
                to_system_id: "frontier-gate".to_string(),
                travel_cost: 3,
                risk: 2,
                capacity: 10,
            },
            RouteEdge {
                route_id: "route-frontier-abyss".to_string(),
                from_system_id: "frontier-gate".to_string(),
                to_system_id: "abyss-watch".to_string(),
                travel_cost: 5,
                risk: 7,
                capacity: 6,
            },
        ],
        tags: vec!["official".to_string(), "genesis".to_string()],
        created_at: now,
        updated_at: now,
    }
}
