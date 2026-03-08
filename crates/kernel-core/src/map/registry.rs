use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use crate::civilization::galaxy::GalaxyZone;
use crate::map::model::{GalaxyMap, default_genesis_map};
use crate::map::validator::validate_map;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct GalaxyMapRegistry {
    maps: BTreeMap<String, GalaxyMap>,
}

impl GalaxyMapRegistry {
    pub fn load_or_new(path: impl AsRef<Path>) -> Result<Self> {
        if let Some(parent) = path.as_ref().parent() {
            fs::create_dir_all(parent).context("create galaxy map registry directory")?;
        }
        if !path.as_ref().exists() {
            return Ok(Self::default());
        }
        let raw = fs::read_to_string(path.as_ref()).context("read galaxy map registry")?;
        if raw.trim().is_empty() {
            return Ok(Self::default());
        }
        serde_json::from_str(&raw).context("parse galaxy map registry")
    }

    pub fn persist(&self, path: impl AsRef<Path>) -> Result<()> {
        if let Some(parent) = path.as_ref().parent() {
            fs::create_dir_all(parent).context("create galaxy map registry directory")?;
        }
        fs::write(path.as_ref(), serde_json::to_string_pretty(self)?)
            .context("write galaxy map registry")
    }

    pub fn ensure_default_genesis_map(
        &mut self,
        allowed_zones: &[GalaxyZone],
    ) -> Result<GalaxyMap> {
        if let Some(map) = self.maps.get("genesis-base") {
            validate_map(map, allowed_zones)?;
            return Ok(map.clone());
        }
        let map = default_genesis_map();
        validate_map(&map, allowed_zones)?;
        self.maps.insert(map.map_id.clone(), map.clone());
        Ok(map)
    }

    #[must_use]
    pub fn get(&self, map_id: &str) -> Option<GalaxyMap> {
        self.maps.get(map_id).cloned()
    }

    #[must_use]
    pub fn list(&self) -> Vec<GalaxyMap> {
        self.maps.values().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::civilization::galaxy::GalaxyState;
    use tempfile::tempdir;

    #[test]
    fn registry_bootstraps_default_genesis_map() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("maps.json");
        let zones = GalaxyState::default_with_core_zones().zones();
        let mut registry = GalaxyMapRegistry::load_or_new(&path).unwrap();
        let map = registry.ensure_default_genesis_map(&zones).unwrap();
        registry.persist(&path).unwrap();

        let loaded = GalaxyMapRegistry::load_or_new(&path).unwrap();
        assert_eq!(map.map_id, "genesis-base");
        assert_eq!(loaded.list().len(), 1);
        assert_eq!(loaded.get("genesis-base").unwrap().systems.len(), 3);
    }
}
