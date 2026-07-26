use crate::identity::Identity;
use crate::identity_file::{restrict_private_identity_permissions, write_private_identity};
use anyhow::{Context, Result};
use std::fs::{self, OpenOptions};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

const PROVIDER_IDENTITY_DIR: &str = ".provider-identity";
const PRIVATE_IDENTITY_FILE: &str = "identity.json";
const PROVIDER_LOCK_FILE: &str = "identity.lock";
const LEGACY_AGENT_IDENTITY_DIR: &str = ".agent-identity";
const LEGACY_PROVIDER_DIR: &str = "provider";

#[derive(Debug, Clone)]
pub struct FileProviderIdentityStore {
    data_dir: PathBuf,
}

#[derive(Debug)]
struct ProviderIdentityLock {
    file: fs::File,
}

impl Drop for ProviderIdentityLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.file);
    }
}

impl FileProviderIdentityStore {
    pub fn new(data_dir: impl AsRef<Path>) -> Self {
        Self {
            data_dir: data_dir.as_ref().to_path_buf(),
        }
    }

    #[must_use]
    pub fn identity_path(&self) -> PathBuf {
        self.data_dir
            .join(PROVIDER_IDENTITY_DIR)
            .join(PRIVATE_IDENTITY_FILE)
    }

    pub fn load(&self) -> Result<Identity> {
        let path = self.identity_path();
        let identity = Identity::load(&path).context("load Provider identity")?;
        restrict_private_identity_permissions(&path)?;
        Ok(identity)
    }

    pub fn load_or_create(&self) -> Result<Identity> {
        let _lock = self.lock()?;
        let provider_path = self.identity_path();
        let legacy_path = self.legacy_identity_path();
        Self::migrate_legacy_identity(&legacy_path, &provider_path)?;
        if provider_path.exists() {
            return self.load();
        }
        let provider_identity = Identity::new_random();
        write_private_identity(&provider_path, &provider_identity)
            .context("create Provider identity")?;
        Ok(provider_identity)
    }

    fn legacy_identity_path(&self) -> PathBuf {
        self.data_dir
            .join(LEGACY_AGENT_IDENTITY_DIR)
            .join(LEGACY_PROVIDER_DIR)
            .join(PRIVATE_IDENTITY_FILE)
    }

    fn lock(&self) -> Result<ProviderIdentityLock> {
        let provider_dir = self.data_dir.join(PROVIDER_IDENTITY_DIR);
        fs::create_dir_all(&provider_dir).context("create Provider identity lock directory")?;
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        options.mode(0o600);
        let file = options
            .open(provider_dir.join(PROVIDER_LOCK_FILE))
            .context("open Provider identity lock")?;
        fs2::FileExt::lock_exclusive(&file).context("lock Provider identity")?;
        Ok(ProviderIdentityLock { file })
    }

    fn migrate_legacy_identity(legacy_path: &Path, provider_path: &Path) -> Result<()> {
        if !legacy_path.exists() {
            return Ok(());
        }
        let legacy = Identity::load(legacy_path).context("load legacy Provider identity")?;
        if provider_path.exists() {
            let current =
                Identity::load(provider_path).context("load current Provider identity")?;
            if !same_identity(&legacy, &current) {
                anyhow::bail!(
                    "conflicting Provider identities exist at '{}' and '{}'",
                    legacy_path.display(),
                    provider_path.display()
                );
            }
            fs::remove_file(legacy_path).context("remove duplicated legacy Provider identity")?;
        } else {
            let provider_dir = provider_path
                .parent()
                .context("Provider identity path has no parent")?;
            fs::create_dir_all(provider_dir).context("create Provider identity directory")?;
            fs::rename(legacy_path, provider_path).context("migrate Provider identity")?;
            restrict_private_identity_permissions(provider_path)?;
        }
        if let Some(legacy_dir) = legacy_path.parent() {
            let _ = fs::remove_dir(legacy_dir);
        }
        Ok(())
    }
}

fn same_identity(left: &Identity, right: &Identity) -> bool {
    left.agent_did == right.agent_did
        && left.public_key == right.public_key
        && left.private_key == right.private_key
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_identity::{AgentIdentityStore, FileAgentIdentityStore};
    use tempfile::tempdir;

    #[test]
    fn provider_identity_is_stable_when_runtime_identity_is_replaced() {
        let dir = tempdir().unwrap();
        let runtime_store = FileAgentIdentityStore::new(dir.path());
        let provider_store = FileProviderIdentityStore::new(dir.path());
        let initial = runtime_store.load_or_create().unwrap();
        let provider = provider_store.load_or_create().unwrap();
        let imported = Identity::new_random();
        runtime_store
            .stage_import(Some(&imported.agent_did), &imported.private_key)
            .unwrap();

        let (runtime, activation) = runtime_store.load_or_create_runtime_identity().unwrap();
        let reloaded_provider = provider_store.load_or_create().unwrap();

        assert_ne!(provider.agent_did, initial.agent_did);
        assert_eq!(runtime.agent_did, imported.agent_did);
        assert_eq!(
            activation,
            crate::agent_identity::RuntimeIdentityActivation::RestartRequired
        );
        assert_eq!(reloaded_provider.agent_did, provider.agent_did);
        assert_eq!(reloaded_provider.private_key, provider.private_key);
        assert!(provider_store.identity_path().exists());
    }

    #[test]
    fn migrates_legacy_provider_identity_without_changing_its_did() {
        let dir = tempdir().unwrap();
        let store = FileProviderIdentityStore::new(dir.path());
        let legacy = Identity::new_random();
        let legacy_path = store.legacy_identity_path();
        write_private_identity(&legacy_path, &legacy).unwrap();

        let migrated = store.load_or_create().unwrap();

        assert_eq!(migrated.agent_did, legacy.agent_did);
        assert_eq!(migrated.private_key, legacy.private_key);
        assert!(store.identity_path().exists());
        assert!(!legacy_path.exists());
    }

    #[test]
    fn refuses_to_overwrite_conflicting_provider_identities() {
        let dir = tempdir().unwrap();
        let store = FileProviderIdentityStore::new(dir.path());
        let current = Identity::new_random();
        let legacy = Identity::new_random();
        let current_path = store.identity_path();
        let legacy_path = store.legacy_identity_path();
        write_private_identity(&current_path, &current).unwrap();
        write_private_identity(&legacy_path, &legacy).unwrap();

        let error = store.load_or_create().unwrap_err();

        assert!(
            error
                .to_string()
                .contains("conflicting Provider identities")
        );
        assert_eq!(
            Identity::load(current_path).unwrap().agent_did,
            current.agent_did
        );
        assert_eq!(
            Identity::load(legacy_path).unwrap().agent_did,
            legacy.agent_did
        );
    }
}
