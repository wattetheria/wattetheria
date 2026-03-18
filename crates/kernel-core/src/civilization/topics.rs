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
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TopicProfile {
    pub topic_id: String,
    pub feed_key: String,
    pub scope_hint: String,
    pub display_name: String,
    pub summary: Option<String>,
    pub projection_kind: TopicProjectionKind,
    pub organization_id: Option<String>,
    pub mission_id: Option<String>,
    pub created_by_public_id: String,
    pub why_this_exists: Option<String>,
    pub active: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone)]
pub struct TopicCreateSpec {
    pub feed_key: String,
    pub scope_hint: String,
    pub display_name: String,
    pub summary: Option<String>,
    pub projection_kind: TopicProjectionKind,
    pub organization_id: Option<String>,
    pub mission_id: Option<String>,
    pub created_by_public_id: String,
    pub why_this_exists: Option<String>,
    pub active: bool,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct TopicRegistry {
    topics: BTreeMap<String, TopicProfile>,
}

impl TopicRegistry {
    pub fn load_or_new(path: impl AsRef<Path>) -> Result<Self> {
        if let Some(parent) = path.as_ref().parent() {
            fs::create_dir_all(parent).context("create topic registry directory")?;
        }
        if !path.as_ref().exists() {
            return Ok(Self::default());
        }
        let raw = fs::read_to_string(path.as_ref()).context("read topic registry")?;
        if raw.trim().is_empty() {
            return Ok(Self::default());
        }
        serde_json::from_str(&raw).context("parse topic registry")
    }

    pub fn persist(&self, path: impl AsRef<Path>) -> Result<()> {
        if let Some(parent) = path.as_ref().parent() {
            fs::create_dir_all(parent).context("create topic registry directory")?;
        }
        fs::write(path.as_ref(), serde_json::to_string_pretty(self)?)
            .context("write topic registry")
    }

    pub fn upsert_topic(&mut self, spec: TopicCreateSpec) -> TopicProfile {
        let topic_id = topic_id_for(&spec.feed_key, &spec.scope_hint);
        let now = Utc::now().timestamp();
        let created_at = self
            .topics
            .get(&topic_id)
            .map_or(now, |topic| topic.created_at);
        let profile = TopicProfile {
            topic_id: topic_id.clone(),
            feed_key: spec.feed_key,
            scope_hint: spec.scope_hint,
            display_name: spec.display_name,
            summary: spec.summary,
            projection_kind: spec.projection_kind,
            organization_id: spec.organization_id,
            mission_id: spec.mission_id,
            created_by_public_id: spec.created_by_public_id,
            why_this_exists: spec.why_this_exists,
            active: spec.active,
            created_at,
            updated_at: now,
        };
        self.topics.insert(topic_id, profile.clone());
        profile
    }

    #[must_use]
    pub fn get(&self, topic_id: &str) -> Option<TopicProfile> {
        self.topics.get(topic_id).cloned()
    }

    #[must_use]
    pub fn list(&self) -> Vec<TopicProfile> {
        self.topics.values().cloned().collect()
    }

    #[must_use]
    pub fn list_filtered(
        &self,
        projection_kind: Option<&TopicProjectionKind>,
        organization_id: Option<&str>,
        mission_id: Option<&str>,
        include_inactive: bool,
    ) -> Vec<TopicProfile> {
        self.topics
            .values()
            .filter(|topic| include_inactive || topic.active)
            .filter(|topic| {
                projection_kind.is_none_or(|kind| &topic.projection_kind == kind)
                    && organization_id.is_none_or(|id| topic.organization_id.as_deref() == Some(id))
                    && mission_id.is_none_or(|id| topic.mission_id.as_deref() == Some(id))
            })
            .cloned()
            .collect()
    }
}

#[must_use]
pub fn topic_id_for(feed_key: &str, scope_hint: &str) -> String {
    format!("{}@{}", feed_key.trim(), scope_hint.trim())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn topic_registry_roundtrip_and_filtering_work() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("topics.json");
        let mut registry = TopicRegistry::default();

        let topic = registry.upsert_topic(TopicCreateSpec {
            feed_key: "crew.chat".to_string(),
            scope_hint: "group:crew-7".to_string(),
            display_name: "Crew Seven".to_string(),
            summary: Some("Coordination thread".to_string()),
            projection_kind: TopicProjectionKind::WorkingGroup,
            organization_id: Some("org-7".to_string()),
            mission_id: None,
            created_by_public_id: "captain-aurora".to_string(),
            why_this_exists: Some("Shared mission pressure".to_string()),
            active: true,
        });
        registry.persist(&path).unwrap();

        let loaded = TopicRegistry::load_or_new(&path).unwrap();
        assert_eq!(
            loaded.get(&topic.topic_id).unwrap().display_name,
            "Crew Seven"
        );
        assert_eq!(
            loaded
                .list_filtered(
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
