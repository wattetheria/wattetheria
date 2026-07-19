use crate::signing::PayloadSigner;
use anyhow::{Context, Result};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use watt_did::{Did, DidKey, PaymentAccountBindingProof, PaymentAccountCustody};
use watt_wallet::{
    ExternalAgentPaymentAccountBindingProofOptions, FileKeyStore, FileWalletMetadataStore,
    PaymentAccount, PaymentAccountSigner, SignatureBytes, Wallet, WalletProfileMetadata,
    build_payment_account_binding_proof_with_agent_signer,
};

const DEFAULT_WALLET_PROFILE_ID: &str = "default";

pub type LocalWallet = Wallet<FileKeyStore, FileWalletMetadataStore>;

pub struct LocalWalletState {
    pub wallet: LocalWallet,
    pub profile: WalletProfileMetadata,
}

impl LocalWalletState {
    pub fn save(&self) -> Result<()> {
        Ok(self.wallet.save_profile(&self.profile)?)
    }
}

pub fn open_local_wallet(data_dir: impl AsRef<Path>) -> Result<LocalWalletState> {
    let data_dir = data_dir.as_ref();
    fs::create_dir_all(data_dir).context("create data directory for wallet")?;
    let wallet_paths = wallet_paths(data_dir);
    let metadata_store = FileWalletMetadataStore::new(&wallet_paths.metadata_path);
    let keystore =
        FileKeyStore::open(&wallet_paths.keystore_path).context("open wallet keystore")?;
    let wallet = Wallet::new(keystore, metadata_store);
    let profile = wallet
        .load_or_create_profile(DEFAULT_WALLET_PROFILE_ID, now_ms())
        .context("load or create wallet profile")?;
    Ok(LocalWalletState { wallet, profile })
}

pub fn active_payment_account(data_dir: impl AsRef<Path>) -> Result<PaymentAccount> {
    let state = open_local_wallet(data_dir)?;
    state
        .wallet
        .active_payment_account(&state.profile)
        .cloned()
        .context("resolve active payment account")
}

/// Best-effort binding proof for the active local payment account.
///
/// Missing wallets, inactive payment accounts, and watch-only accounts return
/// `Ok(None)` so callers can decide whether a signed payment binding is
/// mandatory for their flow.
pub fn active_payment_account_binding_proof(
    data_dir: impl AsRef<Path>,
    agent_signer: &(impl PayloadSigner + ?Sized),
) -> Result<Option<PaymentAccountBindingProof>> {
    let data_dir = data_dir.as_ref();
    let paths = wallet_paths(data_dir);
    if !paths.metadata_path.exists() || !paths.keystore_path.exists() {
        return Ok(None);
    }
    let Ok(wallet_state) = open_local_wallet(data_dir) else {
        return Ok(None);
    };
    let active_account = wallet_state
        .wallet
        .active_payment_account(&wallet_state.profile)
        .ok()
        .cloned();
    let Some(active_account) = active_account else {
        return Ok(None);
    };
    if active_account.key_handle.is_none() {
        return Ok(None);
    }
    let payment_key_info = wallet_state
        .wallet
        .active_payment_account_key_info(&wallet_state.profile)
        .context("load active payment account key")?
        .clone();
    let agent_did = Did::parse(agent_signer.agent_did()).context("parse agent did:key")?;
    let agent_did_key = DidKey::from_did(agent_did.clone()).context("resolve agent did:key")?;
    build_payment_account_binding_proof_with_agent_signer(
        wallet_state.wallet.keystore(),
        ExternalAgentPaymentAccountBindingProofOptions {
            agent_did,
            agent_public_key_multibase: agent_did_key.public_key_multibase,
            rail: active_account.rail.clone(),
            network: active_account.network.clone(),
            custody: PaymentAccountCustody::LocalGenerated,
            receive_only: false,
            can_sign: true,
            capabilities: active_account.capabilities.clone(),
            issued_at_ms: now_ms(),
            expires_at_ms: None,
            nonce: None,
            payment_signer: Some(PaymentAccountSigner {
                key_handle: &payment_key_info.key_handle,
                public_key_multibase: payment_key_info.public_key_multibase.clone(),
            }),
            watch_only_payment_address: None,
        },
        |payload| {
            let signature = agent_signer
                .sign_bytes(payload)
                .context("sign payment account binding with agent identity")?;
            let bytes = STANDARD
                .decode(signature)
                .context("decode agent binding signature")?;
            Ok::<SignatureBytes, anyhow::Error>(SignatureBytes(bytes))
        },
    )
    .context("build active payment account binding proof")
    .map(Some)
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
    use crate::agent_identity::load_or_create_agent_identity;
    use tempfile::tempdir;

    #[test]
    fn local_wallet_state_supports_payment_accounts() {
        let dir = tempdir().unwrap();
        let mut state = open_local_wallet(dir.path()).unwrap();
        let account = state
            .wallet
            .create_payment_account_web3_evm(
                &mut state.profile,
                Some("settlement".into()),
                Some("base-sepolia".into()),
                Some("x402".into()),
                now_ms(),
            )
            .unwrap();
        state
            .wallet
            .set_active_payment_account(&mut state.profile, &account.account_id, now_ms())
            .unwrap();
        let active = active_payment_account(dir.path()).unwrap();
        assert_eq!(active.account_id, account.account_id);
        assert!(active.address.as_deref().is_some());
    }

    #[test]
    fn payment_binding_uses_agent_identity_outside_the_wallet() {
        let dir = tempdir().unwrap();
        let identity = load_or_create_agent_identity(dir.path()).unwrap();
        let mut state = open_local_wallet(dir.path()).unwrap();
        let account = state
            .wallet
            .create_payment_account_web3_evm(
                &mut state.profile,
                Some("settlement".into()),
                Some("base-sepolia".into()),
                Some("x402".into()),
                now_ms(),
            )
            .unwrap();
        state
            .wallet
            .set_active_payment_account(&mut state.profile, &account.account_id, now_ms())
            .unwrap();

        let proof = active_payment_account_binding_proof(dir.path(), &identity)
            .unwrap()
            .unwrap();

        assert_eq!(proof.agent_did.to_string(), identity.agent_did);
        watt_wallet::verify_payment_account_binding_proof(&proof).unwrap();
    }

    #[test]
    fn payment_binding_probe_does_not_create_an_optional_wallet() {
        let dir = tempdir().unwrap();
        let identity = load_or_create_agent_identity(dir.path()).unwrap();

        let proof = active_payment_account_binding_proof(dir.path(), &identity).unwrap();

        assert!(proof.is_none());
        assert!(!dir.path().join(".watt-wallet").exists());
    }
}
