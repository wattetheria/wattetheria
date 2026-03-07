use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Faction {
    Order,
    Freeport,
    Raider,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RolePath {
    Operator,
    Broker,
    Enforcer,
    Artificer,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StrategyProfile {
    Conservative,
    Balanced,
    Aggressive,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CitizenProfile {
    pub agent_id: String,
    pub faction: Faction,
    pub role: RolePath,
    pub strategy: StrategyProfile,
    pub home_subnet_id: Option<String>,
    pub home_zone_id: Option<String>,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StrategyDirective {
    pub max_auto_actions: u8,
    pub allow_high_risk: bool,
    pub emergency_recall_threshold: u8,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct CitizenRegistry {
    profiles: BTreeMap<String, CitizenProfile>,
}

impl CitizenRegistry {
    pub fn load_or_new(path: impl AsRef<Path>) -> Result<Self> {
        if let Some(parent) = path.as_ref().parent() {
            fs::create_dir_all(parent).context("create citizen registry directory")?;
        }
        if !path.as_ref().exists() {
            return Ok(Self::default());
        }
        let raw = fs::read_to_string(path.as_ref()).context("read citizen registry")?;
        if raw.trim().is_empty() {
            return Ok(Self::default());
        }
        serde_json::from_str(&raw).context("parse citizen registry")
    }

    pub fn persist(&self, path: impl AsRef<Path>) -> Result<()> {
        if let Some(parent) = path.as_ref().parent() {
            fs::create_dir_all(parent).context("create citizen registry directory")?;
        }
        fs::write(path.as_ref(), serde_json::to_string_pretty(self)?)
            .context("write citizen registry")
    }

    pub fn set_profile(
        &mut self,
        agent_id: &str,
        faction: Faction,
        role: RolePath,
        strategy: StrategyProfile,
        home_subnet_id: Option<String>,
        home_zone_id: Option<String>,
    ) -> CitizenProfile {
        let profile = CitizenProfile {
            agent_id: agent_id.to_string(),
            faction,
            role,
            strategy,
            home_subnet_id,
            home_zone_id,
            updated_at: Utc::now().timestamp(),
        };
        self.profiles.insert(agent_id.to_string(), profile.clone());
        profile
    }

    #[must_use]
    pub fn profile(&self, agent_id: &str) -> Option<CitizenProfile> {
        self.profiles.get(agent_id).cloned()
    }

    #[must_use]
    pub fn list_profiles(&self) -> Vec<CitizenProfile> {
        self.profiles.values().cloned().collect()
    }
}

#[must_use]
pub fn strategy_directive(strategy: &StrategyProfile) -> StrategyDirective {
    match strategy {
        StrategyProfile::Conservative => StrategyDirective {
            max_auto_actions: 1,
            allow_high_risk: false,
            emergency_recall_threshold: 3,
        },
        StrategyProfile::Balanced => StrategyDirective {
            max_auto_actions: 2,
            allow_high_risk: false,
            emergency_recall_threshold: 4,
        },
        StrategyProfile::Aggressive => StrategyDirective {
            max_auto_actions: 3,
            allow_high_risk: true,
            emergency_recall_threshold: 5,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn registry_roundtrip_and_update() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("profiles.json");
        let mut registry = CitizenRegistry::default();

        let profile = registry.set_profile(
            "agent-a",
            Faction::Freeport,
            RolePath::Broker,
            StrategyProfile::Balanced,
            Some("planet-a".to_string()),
            Some("genesis-core".to_string()),
        );
        assert_eq!(profile.agent_id, "agent-a");
        registry.persist(&path).unwrap();

        let loaded = CitizenRegistry::load_or_new(&path).unwrap();
        let loaded_profile = loaded.profile("agent-a").unwrap();
        assert_eq!(loaded_profile.faction, Faction::Freeport);
        assert_eq!(loaded_profile.role, RolePath::Broker);
        assert_eq!(loaded_profile.strategy, StrategyProfile::Balanced);
        assert_eq!(loaded_profile.home_subnet_id.as_deref(), Some("planet-a"));
    }

    #[test]
    fn strategy_directive_matches_profiles() {
        let conservative = strategy_directive(&StrategyProfile::Conservative);
        let aggressive = strategy_directive(&StrategyProfile::Aggressive);
        assert!(!conservative.allow_high_risk);
        assert!(aggressive.allow_high_risk);
        assert!(aggressive.max_auto_actions > conservative.max_auto_actions);
    }
}
