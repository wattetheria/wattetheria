use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

const ARTIFACT_DIR: &str = ".agent-participation";
const STATUS_FILE: &str = "status.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct AgentAttachStatus {
    pub checked_at: Option<String>,
    pub brain_provider: Option<String>,
    pub control_plane_connected: bool,
    pub brain_connected: bool,
    pub network_joined: bool,
    pub status: String,
    pub last_error: Option<String>,
}

impl AgentAttachStatus {
    pub(crate) fn unknown(brain_provider: Option<String>) -> Self {
        Self {
            checked_at: None,
            brain_provider,
            control_plane_connected: false,
            brain_connected: false,
            network_joined: false,
            status: "unknown".to_string(),
            last_error: None,
        }
    }

    pub(crate) fn connected(brain_provider: String) -> Self {
        Self {
            checked_at: Some(Utc::now().to_rfc3339()),
            brain_provider: Some(brain_provider),
            control_plane_connected: true,
            brain_connected: true,
            network_joined: false,
            status: "connected".to_string(),
            last_error: None,
        }
    }

    pub(crate) fn disconnected(brain_provider: String, error: String) -> Self {
        Self {
            checked_at: Some(Utc::now().to_rfc3339()),
            brain_provider: Some(brain_provider),
            control_plane_connected: true,
            brain_connected: false,
            network_joined: false,
            status: "disconnected".to_string(),
            last_error: Some(error),
        }
    }
}

pub(crate) fn write_status(data_dir: &Path, status: &AgentAttachStatus) -> Result<()> {
    let dir = data_dir.join(ARTIFACT_DIR);
    fs::create_dir_all(&dir).context("create agent participation directory")?;
    fs::write(
        dir.join(STATUS_FILE),
        serde_json::to_vec_pretty(status).context("serialize agent attach status")?,
    )
    .context("write agent attach status")?;
    Ok(())
}

pub(crate) fn read_status(data_dir: &Path) -> Result<Option<AgentAttachStatus>> {
    let path = data_dir.join(ARTIFACT_DIR).join(STATUS_FILE);
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read(path).context("read agent attach status")?;
    let status = serde_json::from_slice(&raw).context("parse agent attach status")?;
    Ok(Some(status))
}
