use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{fs, path::Path};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ServiceNetPublisherState {
    #[serde(default)]
    pub registrations: Vec<ServiceNetPublisherRegistration>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceNetPublisherRegistration {
    pub provider_id: String,
    pub provider_did: String,
    pub agent_id: String,
    pub service_did: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_address: Option<String>,
    pub card_hash: String,
    pub version: String,
    pub updated_at: String,
    pub agent_card: Value,
    pub deployment: Value,
    pub review: Value,
}

pub fn load_servicenet_publisher_state(data_dir: &Path) -> Result<ServiceNetPublisherState> {
    let path = data_dir.join("servicenet").join("publisher-state.json");
    if !path.exists() {
        return Ok(ServiceNetPublisherState::default());
    }
    let raw = fs::read_to_string(&path)
        .with_context(|| format!("read ServiceNet publisher state {}", path.display()))?;
    serde_json::from_str(&raw)
        .with_context(|| format!("parse ServiceNet publisher state {}", path.display()))
}

pub fn save_servicenet_publisher_state(
    data_dir: &Path,
    state: &ServiceNetPublisherState,
) -> Result<()> {
    let path = data_dir.join("servicenet").join("publisher-state.json");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create ServiceNet state directory {}", parent.display()))?;
    }
    let temporary_path = path.with_file_name(format!(".publisher-state-{}.tmp", Uuid::new_v4()));
    fs::write(&temporary_path, serde_json::to_vec_pretty(state)?).with_context(|| {
        format!(
            "write temporary ServiceNet publisher state {}",
            temporary_path.display()
        )
    })?;
    if let Err(error) = fs::rename(&temporary_path, &path) {
        let _ = fs::remove_file(&temporary_path);
        return Err(error)
            .with_context(|| format!("install ServiceNet publisher state {}", path.display()));
    }
    Ok(())
}

pub fn upsert_servicenet_publisher_registration(
    state: &mut ServiceNetPublisherState,
    registration: ServiceNetPublisherRegistration,
) {
    state
        .registrations
        .retain(|item| item.agent_id != registration.agent_id);
    state.registrations.push(registration);
}

pub fn stage_servicenet_publisher_registration(
    data_dir: &Path,
    registration: ServiceNetPublisherRegistration,
) -> Result<Option<ServiceNetPublisherRegistration>> {
    let mut state = load_servicenet_publisher_state(data_dir)?;
    let previous = state
        .registrations
        .iter()
        .find(|item| item.agent_id == registration.agent_id)
        .cloned();
    upsert_servicenet_publisher_registration(&mut state, registration);
    save_servicenet_publisher_state(data_dir, &state)?;
    Ok(previous)
}

pub fn rollback_servicenet_publisher_registration(
    data_dir: &Path,
    agent_id: &str,
    previous: Option<ServiceNetPublisherRegistration>,
) -> Result<()> {
    let mut state = load_servicenet_publisher_state(data_dir)?;
    state.registrations.retain(|item| item.agent_id != agent_id);
    if let Some(previous) = previous {
        state.registrations.push(previous);
    }
    save_servicenet_publisher_state(data_dir, &state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn registration(agent_id: &str, version: &str) -> ServiceNetPublisherRegistration {
        ServiceNetPublisherRegistration {
            provider_id: "provider-1".to_owned(),
            provider_did: "did:key:provider".to_owned(),
            agent_id: agent_id.to_owned(),
            service_did: "did:key:z6Mkg5K92URgXhcuTfqt9jntq75JgPKgaQj36ougEQ3PrDXM".to_owned(),
            service_address: None,
            card_hash: "sha256:card".to_owned(),
            version: version.to_owned(),
            updated_at: "2026-07-19T00:00:00Z".to_owned(),
            agent_card: json!({}),
            deployment: json!({}),
            review: json!({}),
        }
    }

    #[test]
    fn staged_registration_can_roll_back_to_previous_record() {
        let dir = tempfile::tempdir().unwrap();
        stage_servicenet_publisher_registration(dir.path(), registration("ride", "0.1.0")).unwrap();
        let previous =
            stage_servicenet_publisher_registration(dir.path(), registration("ride", "0.2.0"))
                .unwrap();
        rollback_servicenet_publisher_registration(dir.path(), "ride", previous).unwrap();

        let state = load_servicenet_publisher_state(dir.path()).unwrap();
        assert_eq!(state.registrations.len(), 1);
        assert_eq!(state.registrations[0].version, "0.1.0");
    }
}
