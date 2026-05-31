use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TopicProjectionKind {
    ChatRoom,
    WorkingGroup,
    Guild,
    Organization,
    MissionThread,
    DirectConversation,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HiveProfile {
    pub topic_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_id: Option<String>,
    pub feed_key: String,
    pub scope_hint: String,
    pub display_name: String,
    pub summary: Option<String>,
    pub projection_kind: TopicProjectionKind,
    pub organization_id: Option<String>,
    pub mission_id: Option<String>,
    #[serde(default)]
    pub participant_public_ids: Vec<String>,
    pub created_by_public_id: String,
    pub why_this_exists: Option<String>,
    pub active: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone)]
pub struct TopicCreateSpec {
    pub network_id: Option<String>,
    pub feed_key: String,
    pub scope_hint: String,
    pub display_name: String,
    pub summary: Option<String>,
    pub projection_kind: TopicProjectionKind,
    pub organization_id: Option<String>,
    pub mission_id: Option<String>,
    pub participant_public_ids: Vec<String>,
    pub created_by_public_id: String,
    pub why_this_exists: Option<String>,
    pub active: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct HiveRegistry {
    #[serde(default, alias = "topics")]
    hives: BTreeMap<String, HiveProfile>,
}

impl HiveRegistry {
    pub fn load_or_new(path: impl AsRef<Path>) -> Result<Self> {
        if let Some(parent) = path.as_ref().parent() {
            fs::create_dir_all(parent).context("create hive registry directory")?;
        }
        if !path.as_ref().exists() {
            return Ok(Self::default());
        }
        let raw = fs::read_to_string(path.as_ref()).context("read hive registry")?;
        if raw.trim().is_empty() {
            return Ok(Self::default());
        }
        serde_json::from_str(&raw).context("parse hive registry")
    }

    pub fn persist(&self, path: impl AsRef<Path>) -> Result<()> {
        if let Some(parent) = path.as_ref().parent() {
            fs::create_dir_all(parent).context("create hive registry directory")?;
        }
        fs::write(path.as_ref(), serde_json::to_string_pretty(self)?).context("write hive registry")
    }

    pub fn upsert_hive(&mut self, spec: TopicCreateSpec) -> HiveProfile {
        let topic_id = topic_id_for(spec.network_id.as_deref(), &spec.feed_key, &spec.scope_hint);
        let now = Utc::now().timestamp();
        let created_at = self
            .hives
            .get(&topic_id)
            .map_or(now, |topic| topic.created_at);
        let profile = HiveProfile {
            topic_id: topic_id.clone(),
            network_id: spec.network_id,
            feed_key: spec.feed_key,
            scope_hint: spec.scope_hint,
            display_name: spec.display_name,
            summary: spec.summary,
            projection_kind: spec.projection_kind,
            organization_id: spec.organization_id,
            mission_id: spec.mission_id,
            participant_public_ids: spec.participant_public_ids,
            created_by_public_id: spec.created_by_public_id,
            why_this_exists: spec.why_this_exists,
            active: spec.active,
            created_at,
            updated_at: now,
        };
        self.hives.insert(topic_id, profile.clone());
        profile
    }

    #[must_use]
    pub fn get(&self, topic_id: &str) -> Option<HiveProfile> {
        self.hives.get(topic_id).cloned()
    }

    #[must_use]
    pub fn list(&self) -> Vec<HiveProfile> {
        self.hives.values().cloned().collect()
    }

    #[must_use]
    pub fn list_filtered(
        &self,
        network_id: Option<&str>,
        projection_kind: Option<&TopicProjectionKind>,
        organization_id: Option<&str>,
        mission_id: Option<&str>,
        include_inactive: bool,
    ) -> Vec<HiveProfile> {
        let network_id = normalized_network_id(network_id);
        self.hives
            .values()
            .filter(|topic| include_inactive || topic.active)
            .filter(|topic| {
                network_id
                    .is_none_or(|id| normalized_network_id(topic.network_id.as_deref()) == Some(id))
                    && projection_kind.is_none_or(|kind| &topic.projection_kind == kind)
                    && organization_id.is_none_or(|id| topic.organization_id.as_deref() == Some(id))
                    && mission_id.is_none_or(|id| topic.mission_id.as_deref() == Some(id))
            })
            .cloned()
            .collect()
    }
}

#[must_use]
pub fn topic_id_for(network_id: Option<&str>, feed_key: &str, scope_hint: &str) -> String {
    match normalized_network_id(network_id) {
        Some(network_id) => format!("{}@{}@{}", network_id, feed_key.trim(), scope_hint.trim()),
        None => format!("{}@{}", feed_key.trim(), scope_hint.trim()),
    }
}

fn normalized_network_id(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn hive_registry_roundtrip_and_filtering_work() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("hives.json");
        let mut registry = HiveRegistry::default();

        let topic = registry.upsert_hive(TopicCreateSpec {
            network_id: Some("mainnet:test".to_string()),
            feed_key: "crew.chat".to_string(),
            scope_hint: "group:crew-7".to_string(),
            display_name: "Crew Seven".to_string(),
            summary: Some("Coordination thread".to_string()),
            projection_kind: TopicProjectionKind::WorkingGroup,
            organization_id: Some("org-7".to_string()),
            mission_id: None,
            participant_public_ids: vec!["captain-aurora".to_string()],
            created_by_public_id: "captain-aurora".to_string(),
            why_this_exists: Some("Shared mission pressure".to_string()),
            active: true,
        });
        registry.persist(&path).unwrap();

        let loaded = HiveRegistry::load_or_new(&path).unwrap();
        assert_eq!(
            loaded.get(&topic.topic_id).unwrap().display_name,
            "Crew Seven"
        );
        assert_eq!(
            loaded.get(&topic.topic_id).unwrap().network_id.as_deref(),
            Some("mainnet:test")
        );
        assert_eq!(topic.topic_id, "mainnet:test@crew.chat@group:crew-7");
        let subnet_topic = registry.upsert_hive(TopicCreateSpec {
            network_id: Some("subnet:alpha".to_string()),
            feed_key: "crew.chat".to_string(),
            scope_hint: "group:crew-7".to_string(),
            display_name: "Crew Seven Subnet".to_string(),
            summary: None,
            projection_kind: TopicProjectionKind::WorkingGroup,
            organization_id: Some("org-7".to_string()),
            mission_id: None,
            participant_public_ids: Vec::new(),
            created_by_public_id: "captain-aurora".to_string(),
            why_this_exists: None,
            active: true,
        });
        assert_ne!(topic.topic_id, subnet_topic.topic_id);
        assert_eq!(
            registry
                .list_filtered(
                    Some("mainnet:test"),
                    Some(&TopicProjectionKind::WorkingGroup),
                    Some("org-7"),
                    None,
                    false
                )
                .len(),
            1
        );
        assert_eq!(
            loaded.get(&topic.topic_id).unwrap().participant_public_ids,
            vec!["captain-aurora".to_string()]
        );
        assert_eq!(
            loaded
                .list_filtered(
                    Some("mainnet:test"),
                    Some(&TopicProjectionKind::WorkingGroup),
                    Some("org-7"),
                    None,
                    false
                )
                .len(),
            1
        );
    }
}
