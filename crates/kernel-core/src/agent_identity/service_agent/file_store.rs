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

#[derive(Debug)]
pub struct ServiceAgentIdentityProvision {
    identity: ServiceAgentIdentity,
    rollback: ProvisionRollback,
    _lock: ServiceAgentOperationLock,
}

#[derive(Debug)]
pub struct ServiceAgentOperationLock {
    file: fs::File,
}

impl Drop for ServiceAgentOperationLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.file);
    }
}

#[derive(Debug)]
enum ProvisionRollback {
    None,
    RemoveCreated,
    Restore(ServiceAgentIdentity),
}

impl ServiceAgentIdentityProvision {
    #[must_use]
    pub fn identity(&self) -> &ServiceAgentIdentity {
        &self.identity
    }
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
        let parent = path
            .parent()
            .context("Service Agent identity path has no parent")?;
        fs::create_dir_all(parent).context("create Service Agent identity directory")?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent)
            .context("create temporary Service Agent identity")?;
        temporary
            .write_all(serde_json::to_string_pretty(identity)?.as_bytes())
            .context("write temporary Service Agent identity")?;
        temporary
            .as_file()
            .sync_all()
            .context("sync temporary Service Agent identity")?;
        restrict_private_identity_permissions(temporary.path())?;
        temporary
            .persist(&path)
            .map_err(|error| error.error)
            .context("install updated Service Agent identity")?;
        Ok(())
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
            Ok(()) => Ok(true),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => Ok(false),
            Err(error) => Err(error).context("install Service Agent identity"),
        }
    }

    pub fn lock_agent_operation(&self, agent_id: &str) -> Result<ServiceAgentOperationLock> {
        let digest = Sha256::digest(agent_id.as_bytes());
        let lock_dir = self.root.join(".locks");
        fs::create_dir_all(&lock_dir).context("create Service Agent identity lock directory")?;
        let lock_path = lock_dir.join(format!("{}.lock", hex::encode(digest)));
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        options.mode(0o600);
        let file = options
            .open(lock_path)
            .context("open Service Agent identity lock")?;
        fs2::FileExt::lock_exclusive(&file).context("lock Service Agent identity")?;
        Ok(ServiceAgentOperationLock { file })
    }

    pub fn provision(
        &self,
        agent_id: &str,
        endpoint_url: &str,
    ) -> Result<ServiceAgentIdentityProvision> {
        let lock = self.lock_agent_operation(agent_id)?;
        self.provision_locked(agent_id, endpoint_url, lock)
    }

    fn provision_locked(
        &self,
        agent_id: &str,
        endpoint_url: &str,
        lock: ServiceAgentOperationLock,
    ) -> Result<ServiceAgentIdentityProvision> {
        let path = self.identity_path(agent_id);
        if path.exists() {
            let previous = self.load(agent_id)?;
            if previous.endpoint_url == endpoint_url {
                return Ok(ServiceAgentIdentityProvision {
                    identity: previous,
                    rollback: ProvisionRollback::None,
                    _lock: lock,
                });
            }
            ServiceAgentIdentity::validate_endpoint_url(endpoint_url)?;
            let mut identity = previous.clone();
            endpoint_url.clone_into(&mut identity.endpoint_url);
            self.save(&identity)?;
            return Ok(ServiceAgentIdentityProvision {
                identity,
                rollback: ProvisionRollback::Restore(previous),
                _lock: lock,
            });
        }

        let identity = ServiceAgentIdentity::generate(agent_id, endpoint_url)?;
        if self.create(&identity)? {
            return Ok(ServiceAgentIdentityProvision {
                identity,
                rollback: ProvisionRollback::RemoveCreated,
                _lock: lock,
            });
        }
        self.provision_locked(agent_id, endpoint_url, lock)
    }

    pub fn rollback_provision(&self, provision: ServiceAgentIdentityProvision) -> Result<()> {
        let ServiceAgentIdentityProvision {
            identity,
            rollback,
            _lock: lock,
        } = provision;
        let result = match rollback {
            ProvisionRollback::None => Ok(()),
            ProvisionRollback::RemoveCreated => self.remove_created_identity(&identity),
            ProvisionRollback::Restore(previous) => self.restore_identity(&identity, &previous),
        };
        drop(lock);
        result
    }

    fn remove_created_identity(&self, identity: &ServiceAgentIdentity) -> Result<()> {
        let path = self.identity_path(&identity.agent_id);
        if !path.exists() {
            return Ok(());
        }
        let current = self.load(&identity.agent_id)?;
        if current != *identity {
            bail!("refuse to remove a Service Agent identity changed after provisioning");
        }
        fs::remove_file(&path).context("remove provisioned Service Agent identity")?;
        if let Some(parent) = path.parent() {
            match fs::remove_dir(parent) {
                Ok(()) => {}
                Err(error)
                    if matches!(
                        error.kind(),
                        ErrorKind::NotFound | ErrorKind::DirectoryNotEmpty
                    ) => {}
                Err(error) => {
                    return Err(error).context("remove empty Service Agent identity directory");
                }
            }
        }
        Ok(())
    }

    fn restore_identity(
        &self,
        provisioned: &ServiceAgentIdentity,
        previous: &ServiceAgentIdentity,
    ) -> Result<()> {
        let path = self.identity_path(&provisioned.agent_id);
        if path.exists() {
            let current = self.load(&provisioned.agent_id)?;
            if current != *provisioned {
                bail!("refuse to restore a Service Agent identity changed after provisioning");
            }
        }
        self.save(previous)
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
        Ok(self.provision(agent_id, endpoint_url)?.identity)
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
    use std::sync::{Arc, Barrier, mpsc};
    use std::time::Duration;
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
    fn rolls_back_a_newly_provisioned_identity() {
        let dir = tempdir().unwrap();
        let store = FileServiceAgentIdentityStore::new(dir.path());
        let provision = store
            .provision("ride-agent", "https://agent.example.com/a2a")
            .unwrap();
        let path = store.identity_path("ride-agent");
        assert!(path.exists());

        store.rollback_provision(provision).unwrap();

        assert!(!path.exists());
        assert!(!path.parent().unwrap().exists());
    }

    #[test]
    fn rollback_restores_an_existing_identity_endpoint() {
        let dir = tempdir().unwrap();
        let store = FileServiceAgentIdentityStore::new(dir.path());
        let original = store
            .load_or_create("ride-agent", "https://agent.example.com/a2a")
            .unwrap();
        let provision = store
            .provision("ride-agent", "https://other.example.com/a2a")
            .unwrap();
        assert_eq!(
            provision.identity().endpoint_url,
            "https://other.example.com/a2a"
        );

        store.rollback_provision(provision).unwrap();

        assert_eq!(store.load("ride-agent").unwrap(), original);
    }

    #[test]
    fn provision_blocks_other_operations_for_the_same_agent() {
        let dir = tempdir().unwrap();
        let store = Arc::new(FileServiceAgentIdentityStore::new(dir.path()));
        let first = store
            .provision("ride-agent", "https://agent.example.com/a2a")
            .unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let (finished_tx, finished_rx) = mpsc::channel();
        let second_store = Arc::clone(&store);
        let thread = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            let _operation = second_store.lock_agent_operation("ride-agent").unwrap();
            finished_tx.send(()).unwrap();
        });

        started_rx.recv().unwrap();
        assert!(
            finished_rx
                .recv_timeout(Duration::from_millis(100))
                .is_err()
        );
        drop(first);

        finished_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        thread.join().unwrap();
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
