use super::layout::{ensure_layout, read_identity};
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
const PROVIDER_IDENTITY_DIR: &str = ".provider-identity";
const LEGACY_AGENT_IDENTITY_DIR: &str = ".agent-identity";
const PRIVATE_IDENTITY_FILE: &str = "identity.json";

#[derive(Debug, Clone)]
pub struct FileServiceAgentIdentityStore {
    root: PathBuf,
    legacy_root: PathBuf,
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

#[derive(Debug)]
pub struct ServiceAgentIdentityList {
    pub identities: Vec<ServiceAgentIdentity>,
    pub warnings: Vec<ServiceAgentIdentityListWarning>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ServiceAgentIdentityListWarning {
    pub identity_ref: String,
    pub error: String,
}

impl Drop for ServiceAgentOperationLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.file);
    }
}

#[derive(Debug)]
enum ProvisionRollback {
    None,
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
        let data_dir = data_dir.as_ref();
        Self {
            root: data_dir
                .join(PROVIDER_IDENTITY_DIR)
                .join(SERVICE_AGENT_IDENTITY_DIR),
            legacy_root: data_dir
                .join(LEGACY_AGENT_IDENTITY_DIR)
                .join(SERVICE_AGENT_IDENTITY_DIR),
        }
    }

    #[must_use]
    pub fn service_agent_identity_path(&self, service_agent_identity_id: &str) -> PathBuf {
        let digest = Sha256::digest(service_agent_identity_id.as_bytes());
        self.root
            .join(hex::encode(digest))
            .join(PRIVATE_IDENTITY_FILE)
    }

    fn legacy_service_agent_identity_path(&self, service_agent_identity_id: &str) -> PathBuf {
        let digest = Sha256::digest(service_agent_identity_id.as_bytes());
        self.legacy_root
            .join(hex::encode(digest))
            .join(PRIVATE_IDENTITY_FILE)
    }

    fn save(&self, identity: &ServiceAgentIdentity) -> Result<()> {
        identity.validate()?;
        let path = self.service_agent_identity_path(&identity.service_agent_identity_id);
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
        let path = self.service_agent_identity_path(&identity.service_agent_identity_id);
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

    fn lock_operation(&self, scope: &str, id: &str) -> Result<ServiceAgentOperationLock> {
        ensure_layout(&self.root, &self.legacy_root)?;
        let digest = Sha256::digest(format!("{scope}\0{id}").as_bytes());
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

    pub fn lock_service_agent_identity_operation(
        &self,
        service_agent_identity_id: &str,
    ) -> Result<ServiceAgentOperationLock> {
        self.lock_operation("identity", service_agent_identity_id)
    }

    pub fn lock_published_agent_operation(
        &self,
        agent_id: &str,
    ) -> Result<ServiceAgentOperationLock> {
        self.lock_operation("published-agent", agent_id)
    }

    fn lock_did_operation(&self, service_did: &str) -> Result<ServiceAgentOperationLock> {
        self.lock_operation("service-did", service_did)
    }

    #[must_use]
    pub fn publication_service_agent_identity_id(
        &self,
        service_did: &str,
        legacy_agent_id: &str,
    ) -> String {
        let service_agent_identity_id =
            ServiceAgentIdentity::service_agent_identity_id_for_did(service_did);
        if self
            .service_agent_identity_path(&service_agent_identity_id)
            .exists()
            || self
                .legacy_service_agent_identity_path(&service_agent_identity_id)
                .exists()
        {
            service_agent_identity_id
        } else {
            legacy_agent_id.to_owned()
        }
    }

    pub fn generate(&self) -> Result<ServiceAgentIdentity> {
        let identity = ServiceAgentIdentity::generate()?;
        let _lock =
            self.lock_service_agent_identity_operation(&identity.service_agent_identity_id)?;
        if !self.create(&identity)? {
            bail!(
                "Service Agent DID '{}' already exists",
                identity.service_did
            );
        }
        Ok(identity)
    }

    pub fn delete_locked(
        &self,
        service_agent_identity_id: &str,
        _lock: ServiceAgentOperationLock,
    ) -> Result<Option<ServiceAgentIdentity>> {
        let path = self.service_agent_identity_path(service_agent_identity_id);
        if !path.exists() {
            return Ok(None);
        }
        let identity = self.load(service_agent_identity_id)?;
        fs::remove_file(&path).context("delete Service Agent identity")?;
        if let Some(parent) = path.parent() {
            // The identity file is the deletion boundary; empty-directory cleanup is best-effort.
            let _ = fs::remove_dir(parent);
        }
        Ok(Some(identity))
    }

    pub fn import(
        &self,
        expected_did: Option<&str>,
        private_key: &str,
    ) -> Result<ServiceAgentIdentity> {
        let identity = ServiceAgentIdentity::import(expected_did, private_key)?;
        let _did_lock = self.lock_did_operation(&identity.service_did)?;
        if self
            .list()?
            .iter()
            .any(|stored| stored.service_did == identity.service_did)
        {
            bail!(
                "Service Agent DID '{}' already exists",
                identity.service_did
            );
        }
        let _lock =
            self.lock_service_agent_identity_operation(&identity.service_agent_identity_id)?;
        if !self.create(&identity)? {
            bail!(
                "Service Agent DID '{}' already exists",
                identity.service_did
            );
        }
        Ok(identity)
    }

    pub fn list(&self) -> Result<Vec<ServiceAgentIdentity>> {
        Ok(self.list_with_warnings()?.identities)
    }

    pub fn list_with_warnings(&self) -> Result<ServiceAgentIdentityList> {
        ensure_layout(&self.root, &self.legacy_root)?;
        if !self.root.exists() {
            return Ok(ServiceAgentIdentityList {
                identities: vec![],
                warnings: vec![],
            });
        }
        let mut identities = vec![];
        let mut warnings = vec![];
        for entry in fs::read_dir(&self.root).context("list Service Agent identity directory")? {
            let entry = entry.context("read Service Agent identity directory entry")?;
            if !entry
                .file_type()
                .context("read Service Agent identity entry type")?
                .is_dir()
                || entry.file_name() == ".locks"
            {
                continue;
            }
            let path = entry.path().join(PRIVATE_IDENTITY_FILE);
            if !path.exists() {
                continue;
            }
            let loaded = read_identity(&path).and_then(|identity| {
                identity.validate()?;
                restrict_private_identity_permissions(&path)?;
                Ok(identity)
            });
            match loaded {
                Ok(identity) => identities.push(identity),
                Err(error) => warnings.push(ServiceAgentIdentityListWarning {
                    identity_ref: entry.file_name().to_string_lossy().into_owned(),
                    error: error.to_string(),
                }),
            }
        }
        identities.sort_by(|left, right| {
            left.service_agent_identity_id
                .cmp(&right.service_agent_identity_id)
        });
        Ok(ServiceAgentIdentityList {
            identities,
            warnings,
        })
    }

    pub fn provision(
        &self,
        service_agent_identity_id: &str,
        agent_id: &str,
        endpoint_url: &str,
    ) -> Result<ServiceAgentIdentityProvision> {
        let lock = self.lock_service_agent_identity_operation(service_agent_identity_id)?;
        self.provision_locked(service_agent_identity_id, agent_id, endpoint_url, lock)
    }

    fn provision_locked(
        &self,
        service_agent_identity_id: &str,
        agent_id: &str,
        endpoint_url: &str,
        lock: ServiceAgentOperationLock,
    ) -> Result<ServiceAgentIdentityProvision> {
        let path = self.service_agent_identity_path(service_agent_identity_id);
        if !path.exists() {
            bail!(
                "Service Agent identity '{service_agent_identity_id}' must be generated or imported before publication"
            );
        }
        let previous = self.load(service_agent_identity_id)?;
        if previous.bound_agent_id.as_deref() == Some(agent_id)
            && previous.endpoint_url.as_deref() == Some(endpoint_url)
        {
            return Ok(ServiceAgentIdentityProvision {
                identity: previous,
                rollback: ProvisionRollback::None,
                _lock: lock,
            });
        }
        let mut identity = previous.clone();
        identity.bind_publication(agent_id, endpoint_url)?;
        self.save(&identity)?;
        Ok(ServiceAgentIdentityProvision {
            identity,
            rollback: ProvisionRollback::Restore(previous),
            _lock: lock,
        })
    }

    pub fn rollback_provision(&self, provision: ServiceAgentIdentityProvision) -> Result<()> {
        let ServiceAgentIdentityProvision {
            identity,
            rollback,
            _lock: lock,
        } = provision;
        let result = match rollback {
            ProvisionRollback::None => Ok(()),
            ProvisionRollback::Restore(previous) => self.restore_identity(&identity, &previous),
        };
        drop(lock);
        result
    }

    fn restore_identity(
        &self,
        provisioned: &ServiceAgentIdentity,
        previous: &ServiceAgentIdentity,
    ) -> Result<()> {
        let path = self.service_agent_identity_path(&provisioned.service_agent_identity_id);
        if path.exists() {
            let current = self.load(&provisioned.service_agent_identity_id)?;
            if current != *provisioned {
                bail!("refuse to restore a Service Agent identity changed after provisioning");
            }
        }
        self.save(previous)
    }
}

impl ServiceAgentIdentityStore for FileServiceAgentIdentityStore {
    fn load(&self, service_agent_identity_id: &str) -> Result<ServiceAgentIdentity> {
        ensure_layout(&self.root, &self.legacy_root)?;
        let path = self.service_agent_identity_path(service_agent_identity_id);
        let identity = read_identity(&path)?;
        if identity.service_agent_identity_id != service_agent_identity_id {
            bail!(
                "stored Service Agent identity does not match requested service_agent_identity_id"
            );
        }
        identity.validate()?;
        restrict_private_identity_permissions(&path)?;
        Ok(identity)
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
    use std::sync::{Arc, mpsc};
    use std::time::Duration;
    use tempfile::tempdir;

    #[test]
    fn creates_isolated_unbound_service_agent_identities() {
        let dir = tempdir().unwrap();
        let store = FileServiceAgentIdentityStore::new(dir.path());

        let first = store.generate().unwrap();
        let reloaded = store.load(&first.service_agent_identity_id).unwrap();
        let second = store.generate().unwrap();

        assert_eq!(first, reloaded);
        assert_eq!(
            store.publication_service_agent_identity_id(&first.service_did, "legacy-agent"),
            first.service_agent_identity_id
        );
        assert_ne!(first.service_did, second.service_did);
        assert_ne!(first.private_key, second.private_key);
        assert_eq!(first.bound_agent_id, None);
        assert!(first.service_did.starts_with("did:key:z"));
        #[cfg(unix)]
        assert_eq!(
            fs::metadata(store.service_agent_identity_path(&first.service_agent_identity_id))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn publication_binds_an_identity_and_preserves_its_key() {
        let dir = tempdir().unwrap();
        let store = FileServiceAgentIdentityStore::new(dir.path());
        let original = store.generate().unwrap();
        let provision = store
            .provision(
                &original.service_agent_identity_id,
                "ride-agent",
                "https://agent.example.com/a2a",
            )
            .unwrap();

        let published = provision.identity();

        assert_eq!(published.service_did, original.service_did);
        assert_eq!(published.private_key, original.private_key);
        assert_eq!(published.bound_agent_id.as_deref(), Some("ride-agent"));
        assert_eq!(
            published.endpoint_url.as_deref(),
            Some("https://agent.example.com/a2a")
        );
    }

    #[test]
    fn bound_identity_cannot_be_rebound_to_another_service_agent() {
        let dir = tempdir().unwrap();
        let store = FileServiceAgentIdentityStore::new(dir.path());
        let identity = store.generate().unwrap();
        drop(
            store
                .provision(
                    &identity.service_agent_identity_id,
                    "ride-agent",
                    "https://agent.example.com/a2a",
                )
                .unwrap(),
        );

        let error = store
            .provision(
                &identity.service_agent_identity_id,
                "food-agent",
                "https://agent.example.com/a2a",
            )
            .unwrap_err();

        assert!(error.to_string().contains("already bound"));
    }

    #[test]
    fn preserves_identity_when_endpoint_changes() {
        let dir = tempdir().unwrap();
        let store = FileServiceAgentIdentityStore::new(dir.path());
        let original = store.generate().unwrap();
        drop(
            store
                .provision(
                    &original.service_agent_identity_id,
                    "ride-agent",
                    "https://agent.example.com/a2a",
                )
                .unwrap(),
        );

        let updated = store
            .provision(
                &original.service_agent_identity_id,
                "ride-agent",
                "https://other.example.com/v2/a2a",
            )
            .unwrap();

        assert_eq!(updated.identity().service_did, original.service_did);
        assert_eq!(updated.identity().private_key, original.private_key);
        assert_eq!(
            updated.identity().endpoint_url.as_deref(),
            Some("https://other.example.com/v2/a2a")
        );
    }

    #[test]
    fn publication_requires_a_preexisting_identity() {
        let dir = tempdir().unwrap();
        let store = FileServiceAgentIdentityStore::new(dir.path());
        let error = store
            .provision(
                "missing-identity",
                "ride-agent",
                "https://agent.example.com/a2a",
            )
            .unwrap_err();

        assert!(
            error
                .to_string()
                .contains("must be generated or imported before publication")
        );
    }

    #[test]
    fn rollback_restores_an_unbound_identity() {
        let dir = tempdir().unwrap();
        let store = FileServiceAgentIdentityStore::new(dir.path());
        let original = store.generate().unwrap();
        let provision = store
            .provision(
                &original.service_agent_identity_id,
                "ride-agent",
                "https://other.example.com/a2a",
            )
            .unwrap();
        assert_eq!(
            provision.identity().endpoint_url.as_deref(),
            Some("https://other.example.com/a2a")
        );

        store.rollback_provision(provision).unwrap();

        assert_eq!(
            store.load(&original.service_agent_identity_id).unwrap(),
            original
        );
    }

    #[test]
    fn provision_blocks_other_operations_for_the_same_agent() {
        let dir = tempdir().unwrap();
        let store = Arc::new(FileServiceAgentIdentityStore::new(dir.path()));
        let identity = store.generate().unwrap();
        let first = store
            .provision(
                &identity.service_agent_identity_id,
                "ride-agent",
                "https://agent.example.com/a2a",
            )
            .unwrap();
        let (started_tx, started_rx) = mpsc::channel();
        let (finished_tx, finished_rx) = mpsc::channel();
        let second_store = Arc::clone(&store);
        let service_agent_identity_id = identity.service_agent_identity_id;
        let thread = std::thread::spawn(move || {
            started_tx.send(()).unwrap();
            let _operation = second_store
                .lock_service_agent_identity_operation(&service_agent_identity_id)
                .unwrap();
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
    fn imports_and_lists_service_agent_identity_without_an_endpoint() {
        let dir = tempdir().unwrap();
        let store = FileServiceAgentIdentityStore::new(dir.path());
        let generated = ServiceAgentIdentity::generate().unwrap();

        let imported = store
            .import(Some(&generated.service_did), &generated.private_key)
            .unwrap();
        let listed = store.list().unwrap();

        assert_eq!(imported.service_did, generated.service_did);
        assert_eq!(
            imported.key_origin,
            super::super::ServiceAgentKeyOrigin::Imported
        );
        assert_eq!(imported.bound_agent_id, None);
        assert_eq!(imported.endpoint_url, None);
        assert_eq!(listed, vec![imported]);
    }

    #[test]
    fn deletes_a_service_agent_identity_under_its_operation_lock() {
        let dir = tempdir().unwrap();
        let store = FileServiceAgentIdentityStore::new(dir.path());
        let identity = store.generate().unwrap();
        let path = store.service_agent_identity_path(&identity.service_agent_identity_id);

        let lock = store
            .lock_service_agent_identity_operation(&identity.service_agent_identity_id)
            .unwrap();
        let deleted = store
            .delete_locked(&identity.service_agent_identity_id, lock)
            .unwrap()
            .unwrap();

        assert_eq!(deleted, identity);
        assert!(!path.exists());
        assert!(!path.parent().unwrap().exists());

        let lock = store
            .lock_service_agent_identity_operation(&identity.service_agent_identity_id)
            .unwrap();
        assert!(
            store
                .delete_locked(&identity.service_agent_identity_id, lock)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn legacy_agent_id_is_migrated_to_identity_and_binding_ids() {
        let dir = tempdir().unwrap();
        let store = FileServiceAgentIdentityStore::new(dir.path());
        let generated = ServiceAgentIdentity::generate().unwrap();
        let path = store.service_agent_identity_path("legacy-agent");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            serde_json::to_vec_pretty(&serde_json::json!({
                "agent_id": "legacy-agent",
                "service_did": generated.service_did,
                "public_key": generated.public_key,
                "private_key": generated.private_key,
                "endpoint_url": "https://agent.example.com/a2a",
                "key_version": 1,
            }))
            .unwrap(),
        )
        .unwrap();

        let migrated = store.load("legacy-agent").unwrap();

        assert_eq!(migrated.service_agent_identity_id, "legacy-agent");
        assert_eq!(migrated.bound_agent_id.as_deref(), Some("legacy-agent"));
        assert_eq!(
            store.publication_service_agent_identity_id(&migrated.service_did, "legacy-agent"),
            "legacy-agent"
        );
        let duplicate = store
            .import(Some(&migrated.service_did), &migrated.private_key)
            .unwrap_err();
        assert!(duplicate.to_string().contains("already exists"));
        assert_eq!(store.list().unwrap(), vec![migrated]);
    }

    #[test]
    fn migrates_service_agent_identities_under_the_provider_directory() {
        let dir = tempdir().unwrap();
        let store = FileServiceAgentIdentityStore::new(dir.path());
        let identity = ServiceAgentIdentity::generate().unwrap();
        let legacy_path =
            store.legacy_service_agent_identity_path(&identity.service_agent_identity_id);
        fs::create_dir_all(legacy_path.parent().unwrap()).unwrap();
        fs::write(&legacy_path, serde_json::to_vec_pretty(&identity).unwrap()).unwrap();

        let listed = store.list().unwrap();

        assert_eq!(listed, vec![identity.clone()]);
        assert!(
            store
                .service_agent_identity_path(&identity.service_agent_identity_id)
                .exists()
        );
        assert!(!legacy_path.exists());
        assert!(!store.legacy_root.exists());
    }

    #[test]
    fn refuses_to_merge_conflicting_service_agent_identity_layouts() {
        let dir = tempdir().unwrap();
        let store = FileServiceAgentIdentityStore::new(dir.path());
        let current = store.generate().unwrap();
        let conflicting = ServiceAgentIdentity::generate().unwrap();
        let legacy_path =
            store.legacy_service_agent_identity_path(&current.service_agent_identity_id);
        fs::create_dir_all(legacy_path.parent().unwrap()).unwrap();
        fs::write(
            &legacy_path,
            serde_json::to_vec_pretty(&conflicting).unwrap(),
        )
        .unwrap();

        let error = store.list().unwrap_err();

        assert!(
            error
                .to_string()
                .contains("conflicting Service Agent identities")
        );
        assert!(
            store
                .service_agent_identity_path(&current.service_agent_identity_id)
                .exists()
        );
        assert!(legacy_path.exists());
    }
}
