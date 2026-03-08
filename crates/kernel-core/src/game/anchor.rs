use serde::{Deserialize, Serialize};

use crate::civilization::profiles::{CitizenProfile, RolePath};
use crate::map::model::{GalaxyMap, PlanetNode, RouteEdge, StarSystem};
use crate::map::registry::GalaxyMapRegistry;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MissionAnchor {
    pub map_id: String,
    pub map_name: String,
    pub system_id: String,
    pub system_name: String,
    pub planet_id: Option<String>,
    pub planet_name: Option<String>,
    pub route_id: Option<String>,
}

#[must_use]
pub fn locate_anchor(profile: &CitizenProfile, maps: &GalaxyMapRegistry) -> MissionAnchor {
    maps.list()
        .into_iter()
        .find_map(|map| map_anchor_for_profile(&map, profile))
        .unwrap_or_else(default_anchor)
}

fn map_anchor_for_profile(map: &GalaxyMap, profile: &CitizenProfile) -> Option<MissionAnchor> {
    let (system, planet) = locate_home_system(map, profile)?;
    let route = preferred_route(map, &system.system_id, &profile.role);
    Some(MissionAnchor {
        map_id: map.map_id.clone(),
        map_name: map.name.clone(),
        system_id: system.system_id.clone(),
        system_name: system.name.clone(),
        planet_id: planet.map(|planet| planet.planet_id.clone()),
        planet_name: planet.map(|planet| planet.name.clone()),
        route_id: route.map(|route| route.route_id.clone()),
    })
}

fn locate_home_system<'a>(
    map: &'a GalaxyMap,
    profile: &CitizenProfile,
) -> Option<(&'a StarSystem, Option<&'a PlanetNode>)> {
    map.systems.iter().find_map(|system| {
        let planet = system.planets.iter().find(|planet| {
            profile
                .home_subnet_id
                .as_deref()
                .is_some_and(|subnet_id| planet.subnet_id.as_deref() == Some(subnet_id))
                || profile.home_zone_id.as_deref() == Some(planet.zone_id.as_str())
        });
        planet.map(|planet| (system, Some(planet)))
    })
}

fn preferred_route<'a>(
    map: &'a GalaxyMap,
    system_id: &str,
    role: &RolePath,
) -> Option<&'a RouteEdge> {
    let mut routes: Vec<_> = map
        .routes
        .iter()
        .filter(|route| route.from_system_id == system_id || route.to_system_id == system_id)
        .collect();
    if routes.is_empty() {
        return None;
    }
    match role {
        RolePath::Broker => routes.sort_by_key(|route| route.travel_cost),
        RolePath::Enforcer => routes.sort_by(|left, right| right.risk.cmp(&left.risk)),
        RolePath::Operator | RolePath::Artificer => routes.sort_by_key(|route| route.risk),
    }
    routes.into_iter().next()
}

fn default_anchor() -> MissionAnchor {
    MissionAnchor {
        map_id: "genesis-base".to_string(),
        map_name: "Genesis Base Map".to_string(),
        system_id: "genesis-prime".to_string(),
        system_name: "Genesis Prime".to_string(),
        planet_id: Some("planet-main".to_string()),
        planet_name: Some("Planet Main".to_string()),
        route_id: Some("route-genesis-frontier".to_string()),
    }
}
