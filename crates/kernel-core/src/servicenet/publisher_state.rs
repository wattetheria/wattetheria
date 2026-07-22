use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
};

struct PublisherStateLock {
    file: fs::File,
}

impl Drop for PublisherStateLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.file);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct ServiceNetPublisherState {
    #[serde(default)]
    pub registrations: Vec<ServiceNetPublisherRegistration>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ServiceNetConnectionMode {
    #[default]
    ServicenetRelay,
    WattetheriaDirect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CustomizedAgentProtocol {
    A2aV1,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum ServiceAgentExecution {
    #[default]
    WattetheriaRuntime,
    CustomizedAgent {
        protocol: CustomizedAgentProtocol,
        customized_agent_url: String,
    },
}

impl ServiceAgentExecution {
    pub fn customized(protocol: CustomizedAgentProtocol, endpoint: &str) -> Result<Self> {
        let endpoint = endpoint.trim();
        if endpoint.is_empty() {
            anyhow::bail!("Customized Agent URL is required");
        }
        let url = reqwest::Url::parse(endpoint).context("parse Customized Agent URL")?;
        if !matches!(url.scheme(), "http" | "https") {
            anyhow::bail!("Customized Agent URL must use http:// or https://");
        }
        if url.host_str().is_none() {
            anyhow::bail!("Customized Agent URL must include a host");
        }
        if !url.username().is_empty() || url.password().is_some() {
            anyhow::bail!("Customized Agent URL must not contain credentials");
        }
        Ok(Self::CustomizedAgent {
            protocol,
            customized_agent_url: endpoint.to_owned(),
        })
    }
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
    #[serde(default)]
    pub execution: ServiceAgentExecution,
    pub agent_card: Value,
    pub deployment: Value,
    pub review: Value,
}

pub fn load_servicenet_publisher_state(data_dir: &Path) -> Result<ServiceNetPublisherState> {
    read_servicenet_publisher_state(data_dir)
}

pub fn save_servicenet_publisher_state(
    data_dir: &Path,
    state: &ServiceNetPublisherState,
) -> Result<()> {
    let _lock = lock_servicenet_publisher_state(data_dir)?;
    save_servicenet_publisher_state_locked(data_dir, state)
}

fn lock_servicenet_publisher_state(data_dir: &Path) -> Result<PublisherStateLock> {
    let directory = data_dir.join("servicenet");
    fs::create_dir_all(&directory)
        .with_context(|| format!("create ServiceNet state directory {}", directory.display()))?;
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    options.mode(0o600);
    let file = options
        .open(directory.join(".publisher-state.lock"))
        .context("open ServiceNet publisher state lock")?;
    fs2::FileExt::lock_exclusive(&file).context("lock ServiceNet publisher state")?;
    Ok(PublisherStateLock { file })
}

fn read_servicenet_publisher_state(data_dir: &Path) -> Result<ServiceNetPublisherState> {
    let path = data_dir.join("servicenet").join("publisher-state.json");
    if !path.exists() {
        return Ok(ServiceNetPublisherState::default());
    }
    let raw = fs::read_to_string(&path)
        .with_context(|| format!("read ServiceNet publisher state {}", path.display()))?;
    serde_json::from_str(&raw)
        .with_context(|| format!("parse ServiceNet publisher state {}", path.display()))
}

fn save_servicenet_publisher_state_locked(
    data_dir: &Path,
    state: &ServiceNetPublisherState,
) -> Result<()> {
    let path = data_dir.join("servicenet").join("publisher-state.json");
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create ServiceNet state directory {}", parent.display()))?;
    }
    let parent = path
        .parent()
        .context("ServiceNet publisher state path has no parent")?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .context("create temporary ServiceNet publisher state")?;
    temporary
        .write_all(&serde_json::to_vec_pretty(state)?)
        .context("write temporary ServiceNet publisher state")?;
    temporary
        .as_file()
        .sync_all()
        .context("sync temporary ServiceNet publisher state")?;
    temporary
        .persist(&path)
        .map_err(|error| error.error)
        .with_context(|| format!("install ServiceNet publisher state {}", path.display()))?;
    Ok(())
}

pub fn find_servicenet_publisher_registration(
    data_dir: &Path,
    agent_id: &str,
) -> Result<Option<ServiceNetPublisherRegistration>> {
    let state = load_servicenet_publisher_state(data_dir)?;
    Ok(state
        .registrations
        .iter()
        .find(|registration| registration.agent_id == agent_id)
        .cloned())
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
    let _lock = lock_servicenet_publisher_state(data_dir)?;
    let mut state = read_servicenet_publisher_state(data_dir)?;
    let previous = state
        .registrations
        .iter()
        .find(|item| item.agent_id == registration.agent_id)
        .cloned();
    upsert_servicenet_publisher_registration(&mut state, registration);
    save_servicenet_publisher_state_locked(data_dir, &state)?;
    Ok(previous)
}

pub fn rollback_servicenet_publisher_registration(
    data_dir: &Path,
    agent_id: &str,
    previous: Option<ServiceNetPublisherRegistration>,
) -> Result<()> {
    let _lock = lock_servicenet_publisher_state(data_dir)?;
    let mut state = read_servicenet_publisher_state(data_dir)?;
    state.registrations.retain(|item| item.agent_id != agent_id);
    if let Some(previous) = previous {
        state.registrations.push(previous);
    }
    save_servicenet_publisher_state_locked(data_dir, &state)
}

pub fn remove_servicenet_publisher_registration(
    data_dir: &Path,
    agent_id: &str,
    provider_did: &str,
) -> Result<()> {
    let _lock = lock_servicenet_publisher_state(data_dir)?;
    let mut state = read_servicenet_publisher_state(data_dir)?;
    state.registrations.retain(|registration| {
        registration.agent_id != agent_id || registration.provider_did != provider_did
    });
    save_servicenet_publisher_state_locked(data_dir, &state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::{Arc, Barrier};

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
            execution: ServiceAgentExecution::WattetheriaRuntime,
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

    #[test]
    fn concurrent_agents_do_not_overwrite_each_others_registrations() {
        let dir = tempfile::tempdir().unwrap();
        let data_dir = Arc::new(dir.path().to_path_buf());
        let barrier = Arc::new(Barrier::new(8));
        let threads = (0..8)
            .map(|index| {
                let data_dir = Arc::clone(&data_dir);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    stage_servicenet_publisher_registration(
                        &data_dir,
                        registration(&format!("agent-{index}"), "0.1.0"),
                    )
                    .unwrap();
                })
            })
            .collect::<Vec<_>>();
        for thread in threads {
            thread.join().unwrap();
        }

        let state = read_servicenet_publisher_state(&data_dir).unwrap();
        assert_eq!(state.registrations.len(), 8);
    }

    #[test]
    fn load_observes_publisher_state_written_outside_the_process_view() {
        let dir = tempfile::tempdir().unwrap();
        save_servicenet_publisher_state(
            dir.path(),
            &ServiceNetPublisherState {
                registrations: vec![registration("first", "0.1.0")],
            },
        )
        .unwrap();
        let path = dir.path().join("servicenet").join("publisher-state.json");
        fs::write(
            path,
            serde_json::to_vec_pretty(&ServiceNetPublisherState {
                registrations: vec![registration("external", "0.2.0")],
            })
            .unwrap(),
        )
        .unwrap();

        let state = load_servicenet_publisher_state(dir.path()).unwrap();
        assert_eq!(state.registrations.len(), 1);
        assert_eq!(state.registrations[0].agent_id, "external");
    }

    #[test]
    fn customized_execution_accepts_local_url_and_rejects_embedded_credentials() {
        assert!(
            ServiceAgentExecution::customized(
                CustomizedAgentProtocol::A2aV1,
                "http://127.0.0.1:9000/jsonrpc"
            )
            .is_ok()
        );
        assert!(
            ServiceAgentExecution::customized(
                CustomizedAgentProtocol::A2aV1,
                "https://user:secret@example.com/jsonrpc"
            )
            .is_err()
        );
    }
}
