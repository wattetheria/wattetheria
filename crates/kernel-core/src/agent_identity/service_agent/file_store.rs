use super::{ServiceAgentIdentity, ServiceAgentIdentityStore};
use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::{ErrorKind, Write};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use uuid::Uuid;

const SERVICE_AGENT_IDENTITY_DIR: &str = "service-agents";
const PRIVATE_IDENTITY_FILE: &str = "identity.json";

#[derive(Debug, Clone)]
pub struct FileServiceAgentIdentityStore {
    root: PathBuf,
}

impl FileServiceAgentIdentityStore {
    pub fn new(data_dir: impl AsRef<Path>) -> Self {
        Self {
            root: data_dir
                .as_ref()
                .join(".agent-identity")
                .join(SERVICE_AGENT_IDENTITY_DIR),
        }
    }

    #[must_use]
    pub fn identity_path(&self, agent_id: &str) -> PathBuf {
        let digest = Sha256::digest(agent_id.as_bytes());
        self.root
            .join(hex::encode(digest))
            .join(PRIVATE_IDENTITY_FILE)
    }

    fn save(&self, identity: &ServiceAgentIdentity) -> Result<()> {
        let path = self.identity_path(&identity.agent_id);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).context("create Service Agent identity directory")?;
        }
        fs::write(&path, serde_json::to_string_pretty(identity)?)
            .context("write Service Agent identity")?;
        restrict_private_identity_permissions(&path)
    }

    fn create(&self, identity: &ServiceAgentIdentity) -> Result<bool> {
        let path = self.identity_path(&identity.agent_id);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).context("create Service Agent identity directory")?;
        }
        let temporary_path = path.with_file_name(format!(".identity-{}.tmp", Uuid::new_v4()));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options
            .open(&temporary_path)
            .context("create temporary Service Agent identity")?;
        file.write_all(serde_json::to_string_pretty(identity)?.as_bytes())
            .context("write Service Agent identity")?;
        file.sync_all().context("sync Service Agent identity")?;
        drop(file);
        let linked = fs::hard_link(&temporary_path, &path);
        let _ = fs::remove_file(&temporary_path);
        match linked {
            Ok(()) => {
                restrict_private_identity_permissions(&path)?;
                Ok(true)
            }
            Err(error) if error.kind() == ErrorKind::AlreadyExists => Ok(false),
            Err(error) => Err(error).context("install Service Agent identity"),
        }
    }
}

impl ServiceAgentIdentityStore for FileServiceAgentIdentityStore {
    fn load(&self, agent_id: &str) -> Result<ServiceAgentIdentity> {
        let path = self.identity_path(agent_id);
        let identity: ServiceAgentIdentity = serde_json::from_str(
            &fs::read_to_string(&path).context("read Service Agent identity")?,
        )
        .context("parse Service Agent identity")?;
        if identity.agent_id != agent_id {
            bail!("stored Service Agent identity does not match requested agent_id");
        }
        identity.validate()?;
        restrict_private_identity_permissions(&path)?;
        Ok(identity)
    }

    fn load_or_create(&self, agent_id: &str, endpoint_url: &str) -> Result<ServiceAgentIdentity> {
        let path = self.identity_path(agent_id);
        if path.exists() {
            let mut identity = self.load(agent_id)?;
            if identity.endpoint_url != endpoint_url {
                ServiceAgentIdentity::validate_endpoint_url(endpoint_url)?;
                endpoint_url.clone_into(&mut identity.endpoint_url);
                self.save(&identity)?;
            }
            return Ok(identity);
        }
        let identity = ServiceAgentIdentity::generate(agent_id, endpoint_url)?;
        if self.create(&identity)? {
            return Ok(identity);
        }
        self.load_or_create(agent_id, endpoint_url)
    }
}

#[cfg(unix)]
fn restrict_private_identity_permissions(path: &Path) -> Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .context("restrict Service Agent identity permissions")
}

#[cfg(not(unix))]
fn restrict_private_identity_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};
    use tempfile::tempdir;

    #[test]
    fn creates_isolated_stable_service_agent_identities() {
        let dir = tempdir().unwrap();
        let store = FileServiceAgentIdentityStore::new(dir.path());

        let first = store
            .load_or_create("ride-agent", "https://agent.example.com/a2a")
            .unwrap();
        let reloaded = store
            .load_or_create("ride-agent", "https://agent.example.com/a2a")
            .unwrap();
        let second = store
            .load_or_create("food-agent", "https://agent.example.com/a2a")
            .unwrap();

        assert_eq!(first, reloaded);
        assert_ne!(first.service_did, second.service_did);
        assert_ne!(first.private_key, second.private_key);
        assert!(first.service_did.starts_with("did:key:z"));
        #[cfg(unix)]
        assert_eq!(
            fs::metadata(store.identity_path("ride-agent"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn preserves_identity_when_endpoint_authority_changes() {
        let dir = tempdir().unwrap();
        let store = FileServiceAgentIdentityStore::new(dir.path());
        let original = store
            .load_or_create("ride-agent", "https://agent.example.com/a2a")
            .unwrap();

        let updated = store
            .load_or_create("ride-agent", "https://other.example.com/a2a")
            .unwrap();

        assert_eq!(updated.service_did, original.service_did);
        assert_eq!(updated.private_key, original.private_key);
        assert_eq!(updated.endpoint_url, "https://other.example.com/a2a");
    }

    #[test]
    fn preserves_identity_when_endpoint_path_changes_on_same_authority() {
        let dir = tempdir().unwrap();
        let store = FileServiceAgentIdentityStore::new(dir.path());
        let original = store
            .load_or_create("ride-agent", "https://agent.example.com/a2a")
            .unwrap();

        let updated = store
            .load_or_create("ride-agent", "https://agent.example.com/v2/a2a")
            .unwrap();

        assert_eq!(updated.service_did, original.service_did);
        assert_eq!(updated.private_key, original.private_key);
        assert_eq!(updated.endpoint_url, "https://agent.example.com/v2/a2a");
    }

    #[test]
    fn concurrent_creation_converges_on_one_identity() {
        let dir = tempdir().unwrap();
        let store = Arc::new(FileServiceAgentIdentityStore::new(dir.path()));
        let barrier = Arc::new(Barrier::new(8));
        let threads = (0..8)
            .map(|_| {
                let store = Arc::clone(&store);
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    store
                        .load_or_create("ride-agent", "https://agent.example.com/a2a")
                        .unwrap()
                })
            })
            .collect::<Vec<_>>();
        let identities = threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect::<Vec<_>>();

        assert!(identities.iter().all(|identity| identity == &identities[0]));
    }
}
