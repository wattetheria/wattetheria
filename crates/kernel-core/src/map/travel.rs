use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

use crate::civilization::galaxy::{DynamicEvent, DynamicEventCategory, GalaxyState};

use super::model::{GalaxyMap, RouteEdge, StarSystem};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TravelRiskLevel {
    Stable,
    Guarded,
    Contested,
    Volatile,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TravelWarning {
    pub code: String,
    pub title: String,
    pub severity: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TravelOption {
    pub from_system_id: String,
    pub to_system_id: String,
    pub route_id: String,
    pub travel_cost: u32,
    pub risk: u8,
    pub risk_level: TravelRiskLevel,
    pub capacity: u32,
    pub destination_zone_id: Option<String>,
    pub warnings: Vec<TravelWarning>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TravelLeg {
    pub route_id: String,
    pub from_system_id: String,
    pub to_system_id: String,
    pub travel_cost: u32,
    pub risk: u8,
    pub capacity: u32,
    pub warnings: Vec<TravelWarning>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TravelPlan {
    pub map_id: String,
    pub from_system_id: String,
    pub to_system_id: String,
    pub total_travel_cost: u32,
    pub total_risk: u32,
    pub max_leg_risk: u8,
    pub risk_level: TravelRiskLevel,
    pub traversed_system_ids: Vec<String>,
    pub legs: Vec<TravelLeg>,
    pub warnings: Vec<TravelWarning>,
}

#[must_use]
pub fn travel_options(
    map: &GalaxyMap,
    galaxy: &GalaxyState,
    from_system_id: &str,
) -> Vec<TravelOption> {
    map.routes
        .iter()
        .filter_map(|route| {
            let to_system_id = if route.from_system_id == from_system_id {
                Some(route.to_system_id.as_str())
            } else if route.to_system_id == from_system_id {
                Some(route.from_system_id.as_str())
            } else {
                None
            }?;
            let warnings = route_warnings(map, galaxy, route, to_system_id);
            Some(TravelOption {
                from_system_id: from_system_id.to_string(),
                to_system_id: to_system_id.to_string(),
                route_id: route.route_id.clone(),
                travel_cost: route.travel_cost,
                risk: route_risk(route, &warnings),
                risk_level: risk_level(route_risk(route, &warnings)),
                capacity: route.capacity,
                destination_zone_id: system_zone_id(map, to_system_id).map(str::to_string),
                warnings,
            })
        })
        .collect()
}

pub fn travel_plan(
    map: &GalaxyMap,
    galaxy: &GalaxyState,
    from_system_id: &str,
    to_system_id: &str,
) -> Result<TravelPlan> {
    if from_system_id == to_system_id {
        return Ok(TravelPlan {
            map_id: map.map_id.clone(),
            from_system_id: from_system_id.to_string(),
            to_system_id: to_system_id.to_string(),
            total_travel_cost: 0,
            total_risk: 0,
            max_leg_risk: 0,
            risk_level: TravelRiskLevel::Stable,
            traversed_system_ids: vec![from_system_id.to_string()],
            legs: Vec::new(),
            warnings: Vec::new(),
        });
    }
    ensure_system_exists(map, from_system_id)?;
    ensure_system_exists(map, to_system_id)?;
    let previous = shortest_paths(map, from_system_id, to_system_id)?;
    let traversed_system_ids =
        reconstruct_traversed_systems(&previous, from_system_id, to_system_id)?;
    let legs = build_travel_legs(map, galaxy, &traversed_system_ids)?;

    let total_travel_cost = legs.iter().map(|leg| leg.travel_cost).sum();
    let total_risk: u32 = legs.iter().map(|leg| u32::from(leg.risk)).sum();
    let max_leg_risk = legs.iter().map(|leg| leg.risk).max().unwrap_or(0);
    let warnings = dedupe_warnings(
        legs.iter()
            .flat_map(|leg| leg.warnings.clone())
            .collect::<Vec<_>>(),
    );

    Ok(TravelPlan {
        map_id: map.map_id.clone(),
        from_system_id: from_system_id.to_string(),
        to_system_id: to_system_id.to_string(),
        total_travel_cost,
        total_risk,
        max_leg_risk,
        risk_level: risk_level(max_leg_risk),
        traversed_system_ids,
        legs,
        warnings,
    })
}

fn shortest_paths(
    map: &GalaxyMap,
    from_system_id: &str,
    to_system_id: &str,
) -> Result<BTreeMap<String, (String, String)>> {
    let mut best: BTreeMap<String, u32> = BTreeMap::new();
    let mut previous: BTreeMap<String, (String, String)> = BTreeMap::new();
    let mut frontier: BTreeSet<(u32, String)> = BTreeSet::new();
    best.insert(from_system_id.to_string(), 0);
    frontier.insert((0, from_system_id.to_string()));

    while let Some((current_cost, current_system)) = frontier.pop_first() {
        if current_system == to_system_id {
            break;
        }
        if best
            .get(&current_system)
            .is_some_and(|best_cost| current_cost > *best_cost)
        {
            continue;
        }
        for route in adjacent_routes(map, &current_system) {
            let next_system = other_end(route, &current_system).context("adjacent route end")?;
            let next_cost = current_cost.saturating_add(route.travel_cost);
            let should_update = best
                .get(next_system)
                .is_none_or(|known_cost| next_cost < *known_cost);
            if should_update {
                if let Some(old_cost) = best.insert(next_system.to_string(), next_cost) {
                    frontier.remove(&(old_cost, next_system.to_string()));
                }
                previous.insert(
                    next_system.to_string(),
                    (current_system.clone(), route.route_id.clone()),
                );
                frontier.insert((next_cost, next_system.to_string()));
            }
        }
    }

    if !previous.contains_key(to_system_id) {
        bail!("no travel path found between systems");
    }
    Ok(previous)
}

fn reconstruct_traversed_systems(
    previous: &BTreeMap<String, (String, String)>,
    from_system_id: &str,
    to_system_id: &str,
) -> Result<Vec<String>> {
    let mut traversed_system_ids = vec![to_system_id.to_string()];
    let mut cursor = to_system_id.to_string();
    while let Some((prev_system, _route_id)) = previous.get(&cursor).cloned() {
        traversed_system_ids.push(prev_system.clone());
        if prev_system == from_system_id {
            traversed_system_ids.reverse();
            return Ok(traversed_system_ids);
        }
        cursor = prev_system;
    }
    bail!("travel path reconstruction failed")
}

fn build_travel_legs(
    map: &GalaxyMap,
    galaxy: &GalaxyState,
    traversed_system_ids: &[String],
) -> Result<Vec<TravelLeg>> {
    traversed_system_ids
        .windows(2)
        .map(|systems| {
            let from = &systems[0];
            let to = &systems[1];
            let route = map
                .routes
                .iter()
                .find(|route| {
                    (route.from_system_id == *from && route.to_system_id == *to)
                        || (route.from_system_id == *to && route.to_system_id == *from)
                })
                .context("route for travel plan")?;
            let warnings = route_warnings(map, galaxy, route, to);
            Ok(TravelLeg {
                route_id: route.route_id.clone(),
                from_system_id: from.clone(),
                to_system_id: to.clone(),
                travel_cost: route.travel_cost,
                risk: route_risk(route, &warnings),
                capacity: route.capacity,
                warnings,
            })
        })
        .collect()
}

fn adjacent_routes<'a>(map: &'a GalaxyMap, system_id: &str) -> Vec<&'a RouteEdge> {
    map.routes
        .iter()
        .filter(|route| route.from_system_id == system_id || route.to_system_id == system_id)
        .collect()
}

fn other_end<'a>(route: &'a RouteEdge, system_id: &str) -> Option<&'a str> {
    if route.from_system_id == system_id {
        Some(route.to_system_id.as_str())
    } else if route.to_system_id == system_id {
        Some(route.from_system_id.as_str())
    } else {
        None
    }
}

fn ensure_system_exists(map: &GalaxyMap, system_id: &str) -> Result<()> {
    map.systems
        .iter()
        .any(|system| system.system_id == system_id)
        .then_some(())
        .context("unknown system id")
}

fn route_warnings(
    map: &GalaxyMap,
    galaxy: &GalaxyState,
    route: &RouteEdge,
    destination_system_id: &str,
) -> Vec<TravelWarning> {
    let mut warnings = Vec::new();
    if route.risk >= 7 {
        warnings.push(TravelWarning {
            code: "route_risk_high".to_string(),
            title: "High base route risk".to_string(),
            severity: route.risk,
        });
    }
    if route.capacity <= 6 {
        warnings.push(TravelWarning {
            code: "route_capacity_tight".to_string(),
            title: "Tight route capacity".to_string(),
            severity: 4,
        });
    }
    let Some(zone_id) = system_zone_id(map, destination_system_id) else {
        return warnings;
    };
    warnings.extend(
        galaxy
            .events(Some(zone_id))
            .into_iter()
            .filter(|event| event.severity >= 6)
            .map(event_warning),
    );
    dedupe_warnings(warnings)
}

fn event_warning(event: DynamicEvent) -> TravelWarning {
    let code = match event.category {
        DynamicEventCategory::Economic => "economic_pressure",
        DynamicEventCategory::Spatial => "spatial_hazard",
        DynamicEventCategory::Political => "political_instability",
    };
    TravelWarning {
        code: code.to_string(),
        title: event.title,
        severity: event.severity,
    }
}

fn dedupe_warnings(warnings: Vec<TravelWarning>) -> Vec<TravelWarning> {
    let mut deduped = BTreeMap::new();
    for warning in warnings {
        deduped
            .entry(warning.code.clone())
            .and_modify(|existing: &mut TravelWarning| {
                if warning.severity > existing.severity {
                    *existing = warning.clone();
                }
            })
            .or_insert(warning);
    }
    deduped.into_values().collect()
}

fn route_risk(route: &RouteEdge, warnings: &[TravelWarning]) -> u8 {
    let bonus: u8 = warnings
        .iter()
        .filter(|warning| warning.code != "route_capacity_tight")
        .map(|warning| warning.severity.saturating_sub(5))
        .sum();
    route.risk.saturating_add(bonus).min(10)
}

fn system_zone_id<'a>(map: &'a GalaxyMap, system_id: &str) -> Option<&'a str> {
    map.systems
        .iter()
        .find(|system| system.system_id == system_id)
        .and_then(primary_zone_id)
}

fn primary_zone_id(system: &StarSystem) -> Option<&str> {
    system.planets.first().map(|planet| planet.zone_id.as_str())
}

fn risk_level(risk: u8) -> TravelRiskLevel {
    match risk {
        0..=2 => TravelRiskLevel::Stable,
        3..=5 => TravelRiskLevel::Guarded,
        6..=7 => TravelRiskLevel::Contested,
        _ => TravelRiskLevel::Volatile,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::civilization::galaxy::{DynamicEventCategory, GalaxyState};
    use crate::map::model::default_genesis_map;

    #[test]
    fn travel_options_surface_home_route_warnings() {
        let map = default_genesis_map();
        let mut galaxy = GalaxyState::default_with_core_zones();
        galaxy
            .publish_event(
                DynamicEventCategory::Spatial,
                "frontier-belt",
                "Route turbulence",
                "Instability across the frontier belt.",
                8,
                None,
                vec!["hazard".to_string()],
            )
            .unwrap();

        let options = travel_options(&map, &galaxy, "genesis-prime");
        assert_eq!(options.len(), 1);
        assert_eq!(options[0].to_system_id, "frontier-gate");
        assert!(
            options[0]
                .warnings
                .iter()
                .any(|warning| warning.code == "spatial_hazard")
        );
        assert_eq!(options[0].risk_level, TravelRiskLevel::Guarded);
    }

    #[test]
    fn travel_plan_prefers_lower_total_cost() {
        let map = default_genesis_map();
        let galaxy = GalaxyState::default_with_core_zones();
        let plan = travel_plan(&map, &galaxy, "genesis-prime", "abyss-watch").unwrap();
        assert_eq!(
            plan.traversed_system_ids,
            vec!["genesis-prime", "frontier-gate", "abyss-watch"]
        );
        assert_eq!(plan.total_travel_cost, 8);
        assert_eq!(plan.legs.len(), 2);
    }
}
