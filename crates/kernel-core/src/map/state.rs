use anyhow::{Context, Result, bail};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use super::consequence::TravelConsequence;
use super::model::GalaxyMap;
use super::travel::TravelPlan;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TravelPosition {
    pub map_id: String,
    pub system_id: String,
    pub planet_id: Option<String>,
    pub zone_id: Option<String>,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TravelSessionStatus {
    InTransit,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TravelSession {
    pub session_id: String,
    pub map_id: String,
    pub from_system_id: String,
    pub to_system_id: String,
    pub plan: TravelPlan,
    pub status: TravelSessionStatus,
    pub departed_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TravelStateRecord {
    pub public_id: String,
    pub controller_id: String,
    pub current_position: TravelPosition,
    pub active_session: Option<TravelSession>,
    pub last_consequence: Option<TravelConsequence>,
    pub recent_consequences: Vec<TravelConsequence>,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TravelStateRegistry {
    records: BTreeMap<String, TravelStateRecord>,
}

impl TravelStateRegistry {
    pub fn load_or_new(path: impl AsRef<Path>) -> Result<Self> {
        if let Some(parent) = path.as_ref().parent() {
            fs::create_dir_all(parent).context("create travel state directory")?;
        }
        if !path.as_ref().exists() {
            return Ok(Self::default());
        }
        let raw = fs::read_to_string(path.as_ref()).context("read travel state registry")?;
        if raw.trim().is_empty() {
            return Ok(Self::default());
        }
        serde_json::from_str(&raw).context("parse travel state registry")
    }

    pub fn persist(&self, path: impl AsRef<Path>) -> Result<()> {
        if let Some(parent) = path.as_ref().parent() {
            fs::create_dir_all(parent).context("create travel state directory")?;
        }
        fs::write(path.as_ref(), serde_json::to_string_pretty(self)?)
            .context("write travel state registry")
    }

    #[must_use]
    pub fn get(&self, public_id: &str) -> Option<TravelStateRecord> {
        self.records.get(public_id).cloned()
    }

    #[must_use]
    pub fn ensure_position(
        &mut self,
        public_id: &str,
        controller_id: &str,
        position: TravelPosition,
    ) -> TravelStateRecord {
        if let Some(existing) = self.records.get(public_id) {
            return existing.clone();
        }
        let now = Utc::now().timestamp();
        let record = TravelStateRecord {
            public_id: public_id.to_string(),
            controller_id: controller_id.to_string(),
            current_position: position,
            active_session: None,
            last_consequence: None,
            recent_consequences: Vec::new(),
            updated_at: now,
        };
        self.records.insert(public_id.to_string(), record.clone());
        record
    }

    pub fn set_position(
        &mut self,
        public_id: &str,
        controller_id: &str,
        position: TravelPosition,
    ) -> TravelStateRecord {
        let now = Utc::now().timestamp();
        let record = TravelStateRecord {
            public_id: public_id.to_string(),
            controller_id: controller_id.to_string(),
            current_position: position,
            active_session: None,
            last_consequence: None,
            recent_consequences: Vec::new(),
            updated_at: now,
        };
        self.records.insert(public_id.to_string(), record.clone());
        record
    }

    pub fn depart(
        &mut self,
        public_id: &str,
        controller_id: &str,
        plan: TravelPlan,
    ) -> Result<TravelStateRecord> {
        let record = self
            .records
            .get_mut(public_id)
            .context("travel state missing for public identity")?;
        if record.controller_id != controller_id {
            bail!("travel state controller mismatch");
        }
        if record.active_session.is_some() {
            bail!("travel session already active");
        }
        if record.current_position.map_id != plan.map_id {
            bail!("travel origin map mismatch");
        }
        if record.current_position.system_id != plan.from_system_id {
            bail!("travel origin does not match current position");
        }
        let now = Utc::now().timestamp();
        record.active_session = Some(TravelSession {
            session_id: format!("travel-{public_id}-{now}"),
            map_id: plan.map_id.clone(),
            from_system_id: plan.from_system_id.clone(),
            to_system_id: plan.to_system_id.clone(),
            plan,
            status: TravelSessionStatus::InTransit,
            departed_at: now,
            updated_at: now,
        });
        record.updated_at = now;
        Ok(record.clone())
    }

    pub fn arrive(&mut self, public_id: &str, controller_id: &str) -> Result<TravelStateRecord> {
        let record = self
            .records
            .get(public_id)
            .context("travel state missing for public identity")?;
        let session = record
            .active_session
            .as_ref()
            .context("no active travel session")?;
        let position = TravelPosition {
            map_id: session.map_id.clone(),
            system_id: session.to_system_id.clone(),
            planet_id: None,
            zone_id: None,
            updated_at: Utc::now().timestamp(),
        };
        self.arrive_with(public_id, controller_id, position, None)
    }

    pub fn arrive_with(
        &mut self,
        public_id: &str,
        controller_id: &str,
        position: TravelPosition,
        consequence: Option<TravelConsequence>,
    ) -> Result<TravelStateRecord> {
        let record = self
            .records
            .get_mut(public_id)
            .context("travel state missing for public identity")?;
        if record.controller_id != controller_id {
            bail!("travel state controller mismatch");
        }
        let session = record
            .active_session
            .take()
            .context("no active travel session")?;
        let now = Utc::now().timestamp();
        record.current_position = TravelPosition {
            updated_at: now,
            ..position
        };
        if let Some(consequence) = consequence {
            record.last_consequence = Some(consequence.clone());
            record.recent_consequences.push(consequence);
            if record.recent_consequences.len() > 8 {
                let overflow = record.recent_consequences.len() - 8;
                record.recent_consequences.drain(..overflow);
            }
        }
        record.updated_at = now;
        let _ = session;
        Ok(record.clone())
    }
}

#[must_use]
pub fn resolve_anchor_position(
    map: &GalaxyMap,
    home_subnet_id: Option<&str>,
    home_zone_id: Option<&str>,
) -> Option<TravelPosition> {
    let now = Utc::now().timestamp();
    if let Some(subnet_id) = home_subnet_id {
        for system in &map.systems {
            for planet in &system.planets {
                if planet.subnet_id.as_deref() == Some(subnet_id) {
                    return Some(TravelPosition {
                        map_id: map.map_id.clone(),
                        system_id: system.system_id.clone(),
                        planet_id: Some(planet.planet_id.clone()),
                        zone_id: Some(planet.zone_id.clone()),
                        updated_at: now,
                    });
                }
            }
        }
    }
    for system in &map.systems {
        for planet in &system.planets {
            if home_zone_id.is_some_and(|zone_id| planet.zone_id == zone_id) {
                return Some(TravelPosition {
                    map_id: map.map_id.clone(),
                    system_id: system.system_id.clone(),
                    planet_id: Some(planet.planet_id.clone()),
                    zone_id: Some(planet.zone_id.clone()),
                    updated_at: now,
                });
            }
        }
    }
    map.systems.first().map(|system| TravelPosition {
        map_id: map.map_id.clone(),
        system_id: system.system_id.clone(),
        planet_id: system
            .planets
            .first()
            .map(|planet| planet.planet_id.clone()),
        zone_id: system.planets.first().map(|planet| planet.zone_id.clone()),
        updated_at: now,
    })
}

#[must_use]
pub fn resolve_system_position(map: &GalaxyMap, system_id: &str) -> Option<TravelPosition> {
    let now = Utc::now().timestamp();
    map.systems
        .iter()
        .find(|system| system.system_id == system_id)
        .map(|system| TravelPosition {
            map_id: map.map_id.clone(),
            system_id: system.system_id.clone(),
            planet_id: system
                .planets
                .first()
                .map(|planet| planet.planet_id.clone()),
            zone_id: system.planets.first().map(|planet| planet.zone_id.clone()),
            updated_at: now,
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::civilization::galaxy::GalaxyState;
    use crate::map::model::default_genesis_map;
    use crate::map::travel::travel_plan;
    use tempfile::tempdir;

    #[test]
    fn registry_roundtrip_and_depart_arrive() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("travel.json");
        let mut registry = TravelStateRegistry::load_or_new(&path).unwrap();
        let map = default_genesis_map();
        let position =
            resolve_anchor_position(&map, Some("planet-main"), Some("genesis-core")).unwrap();
        let _ = registry.ensure_position("captain-aurora", "agent-a", position);

        let plan = travel_plan(
            &map,
            &GalaxyState::default_with_core_zones(),
            "genesis-prime",
            "frontier-gate",
        )
        .unwrap();
        let departed = registry.depart("captain-aurora", "agent-a", plan).unwrap();
        assert!(departed.active_session.is_some());
        let arrived = registry.arrive("captain-aurora", "agent-a").unwrap();
        assert_eq!(arrived.current_position.system_id, "frontier-gate");
        assert!(arrived.active_session.is_none());

        registry.persist(&path).unwrap();
        let loaded = TravelStateRegistry::load_or_new(&path).unwrap();
        assert_eq!(
            loaded
                .get("captain-aurora")
                .unwrap()
                .current_position
                .system_id,
            "frontier-gate"
        );
    }
}
