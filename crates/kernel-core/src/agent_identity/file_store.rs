use super::AgentIdentityStore;
use crate::identity::Identity;
use anyhow::{Context, Result};
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

const AGENT_IDENTITY_DIR: &str = ".agent-identity";
const PRIVATE_IDENTITY_FILE: &str = "identity.json";
const COMPAT_IDENTITY_FILE: &str = "identity.json";

#[derive(Debug, Clone)]
pub struct FileAgentIdentityStore {
    data_dir: PathBuf,
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

    fn compat_identity_path(&self) -> PathBuf {
        self.data_dir.join(COMPAT_IDENTITY_FILE)
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
        fs::create_dir_all(&self.data_dir).context("create data directory for agent identity")?;
        let path = self.identity_path();
        let identity = Identity::load_or_create(&path).context("load or create agent identity")?;
        restrict_private_identity_permissions(&path)?;
        identity
            .save_compat_view(self.compat_identity_path())
            .context("write public agent identity view")?;
        Ok(identity)
    }
}

#[cfg(unix)]
fn restrict_private_identity_permissions(path: &Path) -> Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .context("restrict agent identity permissions")
}

#[cfg(not(unix))]
fn restrict_private_identity_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
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
}
