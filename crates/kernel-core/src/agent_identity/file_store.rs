use super::AgentIdentityStore;
use crate::identity::{Identity, IdentityCompatView};
use crate::identity_file::{restrict_private_identity_permissions, write_private_identity};
use anyhow::{Context, Result};
use std::fs::{self, OpenOptions};
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use uuid::Uuid;

const AGENT_IDENTITY_DIR: &str = ".agent-identity";
const PRIVATE_IDENTITY_FILE: &str = "identity.json";
const PENDING_PRIVATE_IDENTITY_FILE: &str = "pending-identity.json";
const TRANSITION_LOCK_FILE: &str = "transition.lock";
const COMPAT_IDENTITY_FILE: &str = "identity.json";

#[derive(Debug, Clone)]
pub struct FileAgentIdentityStore {
    data_dir: PathBuf,
}

#[derive(Debug)]
pub struct RuntimeIdentityTransitionLock {
    file: fs::File,
}

impl Drop for RuntimeIdentityTransitionLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.file);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeIdentityActivation {
    Active,
    RestartRequired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeIdentityTransitionPolicy {
    has_pending_import: bool,
}

impl RuntimeIdentityTransitionPolicy {
    #[must_use]
    pub fn activation(self) -> RuntimeIdentityActivation {
        if self.has_pending_import {
            RuntimeIdentityActivation::RestartRequired
        } else {
            RuntimeIdentityActivation::Active
        }
    }

    #[must_use]
    pub fn allows_service_agent_publication(self) -> bool {
        !self.has_pending_import
    }
}

impl FileAgentIdentityStore {
    pub fn new(data_dir: impl AsRef<Path>) -> Self {
        Self {
            data_dir: data_dir.as_ref().to_path_buf(),
        }
    }

    #[must_use]
    pub fn identity_path(&self) -> PathBuf {
        self.data_dir
            .join(AGENT_IDENTITY_DIR)
            .join(PRIVATE_IDENTITY_FILE)
    }

    #[must_use]
    pub fn pending_identity_path(&self) -> PathBuf {
        self.data_dir
            .join(AGENT_IDENTITY_DIR)
            .join(PENDING_PRIVATE_IDENTITY_FILE)
    }

    fn compat_identity_path(&self) -> PathBuf {
        self.data_dir.join(COMPAT_IDENTITY_FILE)
    }

    pub fn lock_transition(&self) -> Result<RuntimeIdentityTransitionLock> {
        let identity_dir = self.data_dir.join(AGENT_IDENTITY_DIR);
        fs::create_dir_all(&identity_dir).context("create agent identity lock directory")?;
        let lock_path = identity_dir.join(TRANSITION_LOCK_FILE);
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        options.mode(0o600);
        let file = options
            .open(lock_path)
            .context("open Runtime Agent identity transition lock")?;
        fs2::FileExt::lock_exclusive(&file).context("lock Runtime Agent identity transition")?;
        Ok(RuntimeIdentityTransitionLock { file })
    }

    pub fn transition_policy(&self) -> Result<RuntimeIdentityTransitionPolicy> {
        Ok(RuntimeIdentityTransitionPolicy {
            has_pending_import: self.pending_import()?.is_some(),
        })
    }

    pub fn stage_import(
        &self,
        expected_agent_did: Option<&str>,
        private_key_b64: &str,
    ) -> Result<IdentityCompatView> {
        let lock = self.lock_transition()?;
        self.stage_import_locked(expected_agent_did, private_key_b64, lock)
    }

    pub fn stage_import_locked(
        &self,
        expected_agent_did: Option<&str>,
        private_key_b64: &str,
        _lock: RuntimeIdentityTransitionLock,
    ) -> Result<IdentityCompatView> {
        let identity = Identity::import_ed25519_private_key(expected_agent_did, private_key_b64)?;
        write_private_identity(&self.pending_identity_path(), &identity)
            .context("stage imported agent identity")?;
        Ok(identity.compat_view())
    }

    pub fn pending_import(&self) -> Result<Option<IdentityCompatView>> {
        let path = self.pending_identity_path();
        if !path.exists() {
            return Ok(None);
        }
        let identity = Identity::load(&path).context("load pending agent identity")?;
        restrict_private_identity_permissions(&path)?;
        Ok(Some(identity.compat_view()))
    }

    fn activate_pending_import(&self) -> Result<Option<Identity>> {
        self.activate_pending_import_with(|identity, path| {
            write_public_identity(path, &identity.compat_view())
        })
    }

    fn activate_pending_import_with(
        &self,
        write_compat: impl FnOnce(&Identity, &Path) -> Result<()>,
    ) -> Result<Option<Identity>> {
        let pending_path = self.pending_identity_path();
        if !pending_path.exists() {
            return Ok(None);
        }
        let pending =
            Identity::load(&pending_path).context("validate pending imported agent identity")?;
        let identity_path = self.identity_path();
        let parent = identity_path
            .parent()
            .context("agent identity path has no parent")?;
        fs::create_dir_all(parent).context("create agent identity directory")?;
        let backup_path = parent.join(format!(".previous-identity-{}.json", Uuid::new_v4()));
        let previous = if identity_path.exists() {
            Some(Identity::load(&identity_path).context("load previous agent identity")?)
        } else {
            None
        };
        if previous.is_some() {
            fs::rename(&identity_path, &backup_path)
                .context("stage previous agent identity for replacement")?;
        }
        if let Err(error) = fs::rename(&pending_path, &identity_path) {
            if previous.is_some()
                && let Err(rollback_error) = fs::rename(&backup_path, &identity_path)
            {
                return Err(anyhow::anyhow!(
                    "activate imported agent identity failed: {error}; \
                     restore previous identity failed: {rollback_error}"
                ));
            }
            return Err(error).context("activate imported agent identity");
        }
        let commit = restrict_private_identity_permissions(&identity_path)
            .and_then(|()| write_compat(&pending, &self.compat_identity_path()))
            .and_then(|()| {
                if previous.is_some() {
                    fs::remove_file(&backup_path).context("remove replaced agent identity")?;
                }
                Ok(())
            });
        if let Err(error) = commit {
            if let Err(rollback_error) = self.rollback_pending_activation(
                &pending_path,
                &identity_path,
                &backup_path,
                previous.as_ref(),
            ) {
                return Err(anyhow::anyhow!(
                    "activate imported agent identity failed: {error:#}; rollback failed: {rollback_error:#}"
                ));
            }
            return Err(error).context("commit imported agent identity");
        }
        Ok(Some(pending))
    }

    fn rollback_pending_activation(
        &self,
        pending_path: &Path,
        identity_path: &Path,
        backup_path: &Path,
        previous: Option<&Identity>,
    ) -> Result<()> {
        fs::rename(identity_path, pending_path).context("restore pending imported identity")?;
        if let Some(previous) = previous {
            fs::rename(backup_path, identity_path).context("restore previous agent identity")?;
            restrict_private_identity_permissions(identity_path)?;
            write_public_identity(&self.compat_identity_path(), &previous.compat_view())
                .context("restore public agent identity view")?;
        }
        Ok(())
    }

    fn recover_interrupted_activation(&self) -> Result<()> {
        let identity_path = self.identity_path();
        let identity_dir = identity_path
            .parent()
            .context("agent identity path has no parent")?;
        if !identity_dir.exists() {
            return Ok(());
        }
        let mut backups = fs::read_dir(identity_dir)
            .context("read agent identity directory")?
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(".previous-identity-"))
                    && path
                        .extension()
                        .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
            })
            .collect::<Vec<_>>();
        backups.sort();
        if !identity_path.exists() {
            let [backup_path] = backups.as_slice() else {
                if backups.is_empty() {
                    return Ok(());
                }
                anyhow::bail!(
                    "cannot recover interrupted Runtime identity activation: \
                     multiple previous identity backups exist"
                );
            };
            fs::rename(backup_path, &identity_path)
                .context("restore previous Runtime identity after interrupted activation")?;
            restrict_private_identity_permissions(&identity_path)?;
            return Ok(());
        }
        for backup_path in backups {
            fs::remove_file(backup_path)
                .context("remove completed Runtime identity activation backup")?;
        }
        Ok(())
    }

    fn load_or_create_active(&self, activate_pending: bool) -> Result<Identity> {
        fs::create_dir_all(&self.data_dir).context("create data directory for agent identity")?;
        self.recover_interrupted_activation()?;
        let path = self.identity_path();
        let activated = if activate_pending {
            self.activate_pending_import()?
        } else {
            None
        };
        let identity = activated
            .map_or_else(|| Identity::load_or_create(&path), Ok)
            .context("load or create agent identity")?;
        restrict_private_identity_permissions(&path)?;
        write_public_identity(&self.compat_identity_path(), &identity.compat_view())
            .context("write public agent identity view")?;
        Ok(identity)
    }

    pub fn load_or_create_runtime_identity(&self) -> Result<(Identity, RuntimeIdentityActivation)> {
        let _lock = self.lock_transition()?;
        let policy = self.transition_policy()?;
        let activation = policy.activation();
        let identity = self.load_or_create_active(true)?;
        Ok((identity, activation))
    }
}

impl AgentIdentityStore for FileAgentIdentityStore {
    type Signer = Identity;

    fn load(&self) -> Result<Identity> {
        let path = self.identity_path();
        let identity = Identity::load(&path).context("load agent identity")?;
        restrict_private_identity_permissions(&path)?;
        Ok(identity)
    }

    fn load_or_create(&self) -> Result<Identity> {
        let _lock = self.lock_transition()?;
        self.load_or_create_active(false)
    }
}

fn write_public_identity(path: &Path, identity: &IdentityCompatView) -> Result<()> {
    let parent = path
        .parent()
        .context("public identity path has no parent")?;
    fs::create_dir_all(parent).context("create public identity directory")?;
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).context("create temporary public identity")?;
    temporary
        .write_all(serde_json::to_string_pretty(identity)?.as_bytes())
        .context("write temporary public identity")?;
    temporary
        .as_file()
        .sync_all()
        .context("sync temporary public identity")?;
    temporary
        .persist(path)
        .map_err(|error| error.error)
        .context("install public identity")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    #[test]
    fn creates_private_agent_identity_without_creating_a_wallet() {
        let dir = tempdir().unwrap();
        let store = FileAgentIdentityStore::new(dir.path());
        let identity = store.load_or_create().unwrap();

        assert!(identity.agent_did.starts_with("did:key:z"));
        assert!(store.identity_path().exists());
        assert!(dir.path().join("identity.json").exists());
        assert!(!dir.path().join(".watt-wallet").exists());

        let private: Value =
            serde_json::from_str(&fs::read_to_string(store.identity_path()).unwrap()).unwrap();
        let public: Value =
            serde_json::from_str(&fs::read_to_string(dir.path().join("identity.json")).unwrap())
                .unwrap();
        assert!(private.get("private_key").is_some());
        assert!(public.get("private_key").is_none());
        #[cfg(unix)]
        assert_eq!(
            fs::metadata(store.identity_path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn reloads_the_same_agent_identity() {
        let dir = tempdir().unwrap();
        let store = FileAgentIdentityStore::new(dir.path());
        let created = store.load_or_create().unwrap();
        let reloaded = store.load_or_create().unwrap();

        assert_eq!(reloaded.agent_did, created.agent_did);
        assert_eq!(reloaded.public_key, created.public_key);
        assert_eq!(reloaded.private_key, created.private_key);
    }

    #[test]
    fn load_requires_an_existing_private_agent_identity() {
        let dir = tempdir().unwrap();
        let store = FileAgentIdentityStore::new(dir.path());
        let error = store.load().unwrap_err();

        assert!(error.to_string().contains("load agent identity"));
    }

    #[test]
    fn store_is_usable_through_the_backend_trait() {
        fn load_twice<S: AgentIdentityStore>(store: &S) -> (S::Signer, S::Signer) {
            (store.load_or_create().unwrap(), store.load().unwrap())
        }

        let dir = tempdir().unwrap();
        let store = FileAgentIdentityStore::new(dir.path());

        let (created, loaded) = load_twice(&store);

        assert_eq!(loaded.agent_did, created.agent_did);
    }

    #[test]
    fn staged_import_activates_only_through_the_runtime_startup_entry() {
        let dir = tempdir().unwrap();
        let store = FileAgentIdentityStore::new(dir.path());
        let initial = store.load_or_create().unwrap();
        let imported = Identity::new_random();

        let pending = store
            .stage_import(Some(&imported.agent_did), &imported.private_key)
            .unwrap();

        assert_eq!(pending.agent_did, imported.agent_did);
        assert_eq!(store.load().unwrap().agent_did, initial.agent_did);
        assert!(store.pending_identity_path().exists());

        let unchanged = store.load_or_create().unwrap();
        assert_eq!(unchanged.agent_did, initial.agent_did);
        assert!(store.pending_identity_path().exists());

        let (activated, activation) = store.load_or_create_runtime_identity().unwrap();
        let public = IdentityCompatView::load(dir.path().join("identity.json")).unwrap();

        assert_eq!(activation, RuntimeIdentityActivation::RestartRequired);
        assert_eq!(activated.agent_did, imported.agent_did);
        assert_eq!(activated.private_key, imported.private_key);
        assert_eq!(public.agent_did, imported.agent_did);
        assert!(!store.pending_identity_path().exists());
        assert!(
            fs::read_dir(store.identity_path().parent().unwrap())
                .unwrap()
                .filter_map(std::result::Result::ok)
                .all(|entry| !entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".previous-identity-"))
        );
    }

    #[test]
    fn invalid_import_does_not_create_pending_identity() {
        let dir = tempdir().unwrap();
        let store = FileAgentIdentityStore::new(dir.path());
        let imported = Identity::new_random();

        let error = store
            .stage_import(Some("did:key:zWrong"), &imported.private_key)
            .unwrap_err();

        assert!(error.to_string().contains("does not match expected DID"));
        assert!(!store.pending_identity_path().exists());
    }

    #[test]
    fn pending_import_can_remain_staged_while_the_existing_identity_stays_active() {
        let dir = tempdir().unwrap();
        let store = FileAgentIdentityStore::new(dir.path());
        let initial = store.load_or_create().unwrap();
        let imported = Identity::new_random();
        store
            .stage_import(Some(&imported.agent_did), &imported.private_key)
            .unwrap();

        let still_active = store.load_or_create().unwrap();

        assert_eq!(still_active.agent_did, initial.agent_did);
        assert!(store.pending_identity_path().exists());
    }

    #[test]
    fn failed_activation_restores_the_previous_identity_and_pending_import() {
        let dir = tempdir().unwrap();
        let store = FileAgentIdentityStore::new(dir.path());
        let initial = store.load_or_create().unwrap();
        let imported = Identity::new_random();
        store
            .stage_import(Some(&imported.agent_did), &imported.private_key)
            .unwrap();

        let error = store
            .activate_pending_import_with(|_, _| anyhow::bail!("injected compat write failure"))
            .unwrap_err();
        let active = store.load().unwrap();
        let pending = store.pending_import().unwrap().unwrap();
        let public = IdentityCompatView::load(dir.path().join("identity.json")).unwrap();

        assert!(error.to_string().contains("commit imported agent identity"));
        assert_eq!(active.agent_did, initial.agent_did);
        assert_eq!(pending.agent_did, imported.agent_did);
        assert_eq!(public.agent_did, initial.agent_did);
    }

    #[test]
    fn transition_policy_only_blocks_service_publication_while_import_is_pending() {
        let dir = tempdir().unwrap();
        let store = FileAgentIdentityStore::new(dir.path());

        let active = store.transition_policy().unwrap();
        assert_eq!(active.activation(), RuntimeIdentityActivation::Active);
        assert!(active.allows_service_agent_publication());

        let imported = Identity::new_random();
        store
            .stage_import(Some(&imported.agent_did), &imported.private_key)
            .unwrap();
        let pending = store.transition_policy().unwrap();
        assert_eq!(
            pending.activation(),
            RuntimeIdentityActivation::RestartRequired
        );
        assert!(!pending.allows_service_agent_publication());
    }

    #[test]
    fn interrupted_activation_restores_the_previous_identity() {
        let dir = tempdir().unwrap();
        let store = FileAgentIdentityStore::new(dir.path());
        let initial = store.load_or_create().unwrap();
        let identity_path = store.identity_path();
        let identity_dir = identity_path.parent().unwrap();
        let backup_path = identity_dir.join(".previous-identity-interrupted.json");
        fs::rename(&identity_path, &backup_path).unwrap();

        let recovered = store.load_or_create().unwrap();

        assert_eq!(recovered.agent_did, initial.agent_did);
        assert!(identity_path.exists());
        assert!(!backup_path.exists());
    }
}
