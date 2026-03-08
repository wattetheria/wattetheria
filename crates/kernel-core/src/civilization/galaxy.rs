use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ZoneKind {
    Genesis,
    Frontier,
    DeepSpace,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ZoneSecurityMode {
    Peace,
    LimitedPvp,
    OpenPvp,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DynamicEventCategory {
    Economic,
    Spatial,
    Political,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GalaxyZone {
    pub zone_id: String,
    pub name: String,
    pub kind: ZoneKind,
    pub security_mode: ZoneSecurityMode,
    pub resource_multiplier: f64,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DynamicEvent {
    pub event_id: String,
    pub category: DynamicEventCategory,
    pub zone_id: String,
    pub title: String,
    pub description: String,
    pub severity: u8,
    pub created_at: i64,
    pub expires_at: Option<i64>,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GalaxyState {
    zones: Vec<GalaxyZone>,
    events: Vec<DynamicEvent>,
}

impl GalaxyState {
    pub fn load_or_new(path: impl AsRef<Path>) -> Result<Self> {
        if let Some(parent) = path.as_ref().parent() {
            fs::create_dir_all(parent).context("create galaxy state directory")?;
        }
        if !path.as_ref().exists() {
            return Ok(Self::default_with_core_zones());
        }
        let raw = fs::read_to_string(path.as_ref()).context("read galaxy state")?;
        if raw.trim().is_empty() {
            return Ok(Self::default_with_core_zones());
        }
        let mut state: Self = serde_json::from_str(&raw).context("parse galaxy state")?;
        if state.zones.is_empty() {
            state.zones = Self::default_with_core_zones().zones;
        }
        Ok(state)
    }

    pub fn persist(&self, path: impl AsRef<Path>) -> Result<()> {
        if let Some(parent) = path.as_ref().parent() {
            fs::create_dir_all(parent).context("create galaxy state directory")?;
        }
        fs::write(path.as_ref(), serde_json::to_string_pretty(self)?).context("write galaxy state")
    }

    #[must_use]
    pub fn default_with_core_zones() -> Self {
        Self {
            zones: vec![
                GalaxyZone {
                    zone_id: "genesis-core".to_string(),
                    name: "Genesis Core".to_string(),
                    kind: ZoneKind::Genesis,
                    security_mode: ZoneSecurityMode::Peace,
                    resource_multiplier: 1.0,
                    description: "Stable starter core with strong institutional guardrails."
                        .to_string(),
                },
                GalaxyZone {
                    zone_id: "frontier-belt".to_string(),
                    name: "Frontier Belt".to_string(),
                    kind: ZoneKind::Frontier,
                    security_mode: ZoneSecurityMode::LimitedPvp,
                    resource_multiplier: 1.4,
                    description: "Half-governed expansion belt with sovereignty pressure."
                        .to_string(),
                },
                GalaxyZone {
                    zone_id: "deep-space".to_string(),
                    name: "Deep Space".to_string(),
                    kind: ZoneKind::DeepSpace,
                    security_mode: ZoneSecurityMode::OpenPvp,
                    resource_multiplier: 2.0,
                    description: "High-risk void where logistics, war, and salvage dominate."
                        .to_string(),
                },
            ],
            events: Vec::new(),
        }
    }

    #[must_use]
    pub fn zones(&self) -> Vec<GalaxyZone> {
        self.zones.clone()
    }

    #[must_use]
    pub fn events(&self, zone_id: Option<&str>) -> Vec<DynamicEvent> {
        self.events
            .iter()
            .filter(|event| zone_id.is_none_or(|zone| event.zone_id == zone))
            .cloned()
            .collect()
    }

    #[allow(clippy::too_many_arguments)]
    pub fn publish_event(
        &mut self,
        category: DynamicEventCategory,
        zone_id: &str,
        title: &str,
        description: &str,
        severity: u8,
        expires_at: Option<i64>,
        tags: Vec<String>,
    ) -> Result<DynamicEvent> {
        if !self.zones.iter().any(|zone| zone.zone_id == zone_id) {
            anyhow::bail!("unknown galaxy zone");
        }

        let event = DynamicEvent {
            event_id: uuid::Uuid::new_v4().to_string(),
            category,
            zone_id: zone_id.to_string(),
            title: title.to_string(),
            description: description.to_string(),
            severity: severity.min(10),
            created_at: Utc::now().timestamp(),
            expires_at,
            tags,
        };
        self.events.push(event.clone());
        Ok(event)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn galaxy_state_bootstraps_and_persists_events() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("galaxy.json");
        let mut galaxy = GalaxyState::load_or_new(&path).unwrap();
        assert_eq!(galaxy.zones().len(), 3);

        let event = galaxy
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
        galaxy.persist(&path).unwrap();

        let loaded = GalaxyState::load_or_new(&path).unwrap();
        assert_eq!(loaded.events(Some("genesis-core")).len(), 1);
        assert_eq!(loaded.events(None)[0], event);
    }
}
