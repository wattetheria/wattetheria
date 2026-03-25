use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RelationshipKind {
    Follow,
    Friend,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RelationshipEdge {
    pub public_id: String,
    pub counterpart_public_id: String,
    pub kind: RelationshipKind,
    pub active: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct RelationshipRegistry {
    edges: BTreeMap<String, RelationshipEdge>,
}

impl RelationshipRegistry {
    pub fn load_or_new(path: impl AsRef<Path>) -> Result<Self> {
        if let Some(parent) = path.as_ref().parent() {
            fs::create_dir_all(parent).context("create relationship registry directory")?;
        }
        if !path.as_ref().exists() {
            return Ok(Self::default());
        }
        let raw = fs::read_to_string(path.as_ref()).context("read relationship registry")?;
        if raw.trim().is_empty() {
            return Ok(Self::default());
        }
        serde_json::from_str(&raw).context("parse relationship registry")
    }

    pub fn persist(&self, path: impl AsRef<Path>) -> Result<()> {
        if let Some(parent) = path.as_ref().parent() {
            fs::create_dir_all(parent).context("create relationship registry directory")?;
        }
        fs::write(path.as_ref(), serde_json::to_string_pretty(self)?)
            .context("write relationship registry")
    }

    pub fn upsert(
        &mut self,
        public_id: &str,
        counterpart_public_id: &str,
        kind: RelationshipKind,
        active: bool,
    ) -> RelationshipEdge {
        let key = edge_key(public_id, counterpart_public_id);
        let now = Utc::now().timestamp();
        let created_at = self.edges.get(&key).map_or(now, |edge| edge.created_at);
        let edge = RelationshipEdge {
            public_id: public_id.to_string(),
            counterpart_public_id: counterpart_public_id.to_string(),
            kind,
            active,
            created_at,
            updated_at: now,
        };
        self.edges.insert(key, edge.clone());
        edge
    }

    #[must_use]
    pub fn list_for_public(&self, public_id: &str) -> Vec<RelationshipEdge> {
        self.edges
            .values()
            .filter(|edge| edge.public_id == public_id && edge.active)
            .cloned()
            .collect()
    }

    #[must_use]
    pub fn get(&self, public_id: &str, counterpart_public_id: &str) -> Option<RelationshipEdge> {
        self.edges
            .get(&edge_key(public_id, counterpart_public_id))
            .cloned()
    }
}

#[must_use]
pub fn edge_key(public_id: &str, counterpart_public_id: &str) -> String {
    format!("{}::{}", public_id.trim(), counterpart_public_id.trim())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn relationship_registry_roundtrip_and_filtering_work() {
        let dir = tempdir().expect("tempdir");
        let path = dir.path().join("relationships.json");
        let mut registry = RelationshipRegistry::default();

        let edge = registry.upsert("self", "friend-1", RelationshipKind::Friend, true);
        registry.upsert("self", "friend-2", RelationshipKind::Follow, true);
        registry.persist(&path).expect("persist");

        let loaded = RelationshipRegistry::load_or_new(&path).expect("load");
        assert_eq!(loaded.get("self", "friend-1").expect("edge"), edge);
        assert_eq!(loaded.list_for_public("self").len(), 2);
    }
}
