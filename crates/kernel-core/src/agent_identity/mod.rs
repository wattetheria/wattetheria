//! Local Agent identity custody and storage backends.

mod file_store;
pub mod service_agent;
mod store;

use crate::identity::Identity;
use anyhow::Result;
use std::path::{Path, PathBuf};

pub use file_store::FileAgentIdentityStore;
pub use store::AgentIdentityStore;

#[must_use]
pub fn agent_identity_path(data_dir: impl AsRef<Path>) -> PathBuf {
    FileAgentIdentityStore::new(data_dir).identity_path()
}

pub fn load_or_create_agent_identity(data_dir: impl AsRef<Path>) -> Result<Identity> {
    FileAgentIdentityStore::new(data_dir).load_or_create()
}

pub fn load_agent_identity(data_dir: impl AsRef<Path>) -> Result<Identity> {
    FileAgentIdentityStore::new(data_dir).load()
}
