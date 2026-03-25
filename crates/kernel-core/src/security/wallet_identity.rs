use crate::identity::Identity;
use crate::identity::IdentityCompatView;
use crate::signing::PayloadSigner;
use anyhow::{Context, Result, anyhow};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use watt_wallet::WalletMetadataStore;
use watt_wallet::{FileKeyStore, FileWalletMetadataStore, SignerPurpose, Wallet};

const DEFAULT_WALLET_PROFILE_ID: &str = "default";

pub fn load_or_create_wallet_backed_identity(data_dir: impl AsRef<Path>) -> Result<Identity> {
    let data_dir = data_dir.as_ref();
    fs::create_dir_all(data_dir).context("create data directory for wallet identity")?;

    let wallet_paths = wallet_paths(data_dir);
    let metadata_store = FileWalletMetadataStore::new(&wallet_paths.metadata_path);
    let keystore = FileKeyStore::open(&wallet_paths.keystore_path)
        .context("open wallet keystore for runtime identity")?;
    let mut wallet = Wallet::new(keystore, metadata_store);
    let now_ms = now_ms();
    let mut profile = wallet
        .load_or_create_profile(DEFAULT_WALLET_PROFILE_ID, now_ms)
        .context("load or create default wallet profile")?;

    if profile.active_identity().is_none() {
        if let Some(first_identity_id) = profile
            .identities
            .iter()
            .find(|identity| matches!(identity.status, watt_wallet::IdentityStatus::Active))
            .map(|identity| identity.identity_id.clone())
        {
            wallet
                .set_active_identity(&mut profile, &first_identity_id, now_ms)
                .context("set existing wallet identity active")?;
        } else {
            wallet
                .create_identity_ed25519(
                    &mut profile,
                    Some("wattetheria-node".to_string()),
                    vec![
                        SignerPurpose::General,
                        SignerPurpose::Authentication,
                        SignerPurpose::AssertionMethod,
                        SignerPurpose::CapabilityInvocation,
                    ],
                    now_ms,
                )
                .context("create wallet-backed runtime identity")?;
        }
    }

    let active_identity = wallet
        .active_identity(&profile)
        .context("resolve active wallet identity")?;
    let seed = wallet
        .export_active_identity_ed25519_seed(&profile)
        .context("export active wallet seed for runtime identity")?;
    let runtime_identity = Identity::from_ed25519_seed(active_identity.did.to_string(), seed)
        .context("build runtime identity from wallet")?;

    runtime_identity
        .save_compat_view(data_dir.join("identity.json"))
        .context("write compatibility identity view")?;

    Ok(runtime_identity)
}

pub fn load_wallet_backed_identity(data_dir: impl AsRef<Path>) -> Result<Identity> {
    let data_dir = data_dir.as_ref();
    let wallet_paths = wallet_paths(data_dir);
    let metadata_store = FileWalletMetadataStore::new(&wallet_paths.metadata_path);
    let profile = metadata_store
        .load()
        .context("load wallet metadata for runtime identity")?
        .ok_or_else(|| anyhow!("wallet metadata missing for runtime identity"))?;
    let keystore = FileKeyStore::open(&wallet_paths.keystore_path)
        .context("open wallet keystore for runtime identity")?;
    let wallet = Wallet::new(keystore, metadata_store);

    let active_identity = wallet
        .active_identity(&profile)
        .context("resolve active wallet identity")?;
    let seed = wallet
        .export_active_identity_ed25519_seed(&profile)
        .context("export active wallet seed for runtime identity")?;
    Identity::from_ed25519_seed(active_identity.did.to_string(), seed)
        .context("build runtime identity from wallet")
}

#[derive(Debug, Clone)]
pub struct WalletSigner {
    data_dir: PathBuf,
    identity: IdentityCompatView,
}

impl WalletSigner {
    pub fn new(data_dir: impl AsRef<Path>, identity: IdentityCompatView) -> Self {
        Self {
            data_dir: data_dir.as_ref().to_path_buf(),
            identity,
        }
    }

    pub fn from_data_dir(data_dir: impl AsRef<Path>) -> Result<Self> {
        let data_dir = data_dir.as_ref();
        let identity = IdentityCompatView::load(data_dir.join("identity.json"))
            .context("load compatibility identity view for wallet signer")?;
        Ok(Self::new(data_dir, identity))
    }

    fn sign_with_wallet(&self, payload: &[u8]) -> Result<String> {
        let wallet_paths = wallet_paths(&self.data_dir);
        let metadata_store = FileWalletMetadataStore::new(&wallet_paths.metadata_path);
        let profile = metadata_store
            .load()
            .context("load wallet metadata for signer")?
            .ok_or_else(|| anyhow!("wallet metadata missing for signer"))?;
        let keystore = FileKeyStore::open(&wallet_paths.keystore_path)
            .context("open wallet keystore for signer")?;
        let wallet = Wallet::new(keystore, metadata_store);
        let signature = wallet
            .sign_with_active_identity(&profile, payload)
            .context("sign payload with active wallet identity")?;
        Ok(STANDARD.encode(signature.0))
    }
}

impl PayloadSigner for WalletSigner {
    fn agent_did(&self) -> &str {
        &self.identity.agent_did
    }

    fn public_key(&self) -> &str {
        &self.identity.public_key
    }

    fn sign_bytes(&self, payload: &[u8]) -> Result<String> {
        self.sign_with_wallet(payload)
    }
}

#[derive(Debug)]
struct WalletPaths {
    metadata_path: PathBuf,
    keystore_path: PathBuf,
}

fn wallet_paths(data_dir: &Path) -> WalletPaths {
    let wallet_dir = data_dir.join(".watt-wallet");
    WalletPaths {
        metadata_path: wallet_dir.join("metadata.json"),
        keystore_path: wallet_dir.join("keystore.json"),
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn wallet_backed_identity_creates_wallet_artifacts_and_compat_identity() {
        let dir = tempdir().unwrap();
        let identity = load_or_create_wallet_backed_identity(dir.path()).unwrap();

        assert!(identity.agent_did.starts_with("did:key:z"));
        assert!(dir.path().join("identity.json").exists());
        assert!(dir.path().join(".watt-wallet/metadata.json").exists());
        assert!(dir.path().join(".watt-wallet/keystore.json").exists());

        let reloaded = load_or_create_wallet_backed_identity(dir.path()).unwrap();
        assert_eq!(reloaded.agent_did, identity.agent_did);
        assert_eq!(reloaded.public_key, identity.public_key);
    }

    #[test]
    fn load_wallet_backed_identity_requires_existing_wallet_state() {
        let dir = tempdir().unwrap();
        let error = load_wallet_backed_identity(dir.path()).unwrap_err();
        assert!(error.to_string().contains("wallet metadata missing"));
    }

    #[test]
    fn wallet_signer_matches_runtime_identity_signature() {
        let dir = tempdir().unwrap();
        let identity = load_or_create_wallet_backed_identity(dir.path()).unwrap();
        let signer = WalletSigner::from_data_dir(dir.path()).unwrap();
        let payload = br#"{"probe":"wallet-signer"}"#;

        let wallet_signature = signer.sign_bytes(payload).unwrap();
        assert!(
            crate::identity::verify_with_public_key(
                payload,
                &wallet_signature,
                signer.public_key()
            )
            .unwrap()
        );
        assert_eq!(signer.agent_did(), identity.agent_did);
    }
}
