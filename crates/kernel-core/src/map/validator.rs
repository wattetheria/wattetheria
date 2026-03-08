use anyhow::{Result, bail};
use std::collections::BTreeSet;

use crate::civilization::galaxy::GalaxyZone;
use crate::map::model::GalaxyMap;

pub fn validate_map(map: &GalaxyMap, allowed_zones: &[GalaxyZone]) -> Result<()> {
    if map.systems.is_empty() {
        bail!("galaxy map must contain at least one star system");
    }

    let allowed_zone_ids: BTreeSet<_> = allowed_zones
        .iter()
        .map(|zone| zone.zone_id.as_str())
        .collect();
    let mut system_ids = BTreeSet::new();
    let mut planet_ids = BTreeSet::new();

    for system in &map.systems {
        if system.system_id.trim().is_empty() {
            bail!("star system id cannot be empty");
        }
        if !system_ids.insert(system.system_id.clone()) {
            bail!("duplicate star system id {}", system.system_id);
        }
        if system.planets.is_empty() {
            bail!(
                "star system {} must contain at least one planet",
                system.system_id
            );
        }
        for planet in &system.planets {
            if planet.planet_id.trim().is_empty() {
                bail!("planet id cannot be empty");
            }
            if !planet_ids.insert(planet.planet_id.clone()) {
                bail!("duplicate planet id {}", planet.planet_id);
            }
            if !allowed_zone_ids.contains(planet.zone_id.as_str()) {
                bail!(
                    "planet {} references unknown galaxy zone {}",
                    planet.planet_id,
                    planet.zone_id
                );
            }
            if !(0.1..=10.0).contains(&planet.resource_multiplier) {
                bail!(
                    "planet {} resource multiplier out of bounds",
                    planet.planet_id
                );
            }
        }
    }

    let mut route_ids = BTreeSet::new();
    for route in &map.routes {
        if !route_ids.insert(route.route_id.clone()) {
            bail!("duplicate route id {}", route.route_id);
        }
        if !system_ids.contains(&route.from_system_id) || !system_ids.contains(&route.to_system_id)
        {
            bail!("route {} references unknown star system", route.route_id);
        }
        if route.from_system_id == route.to_system_id {
            bail!(
                "route {} cannot loop to the same star system",
                route.route_id
            );
        }
        if route.travel_cost == 0 {
            bail!(
                "route {} travel_cost must be greater than zero",
                route.route_id
            );
        }
        if route.capacity == 0 {
            bail!(
                "route {} capacity must be greater than zero",
                route.route_id
            );
        }
    }

    Ok(())
}
