//! Offline registration of an autonomous Wattetheria network with a mainnet authority.

use crate::identity::Identity;
use crate::identity_file::{restrict_private_identity_permissions, write_private_identity};
use crate::signing::{canonical_bytes, sign_payload, verify_payload};
use anyhow::{Context, Result, bail};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

pub const NETWORK_REGISTRATION_PROTOCOL_VERSION: &str = "wattetheria.network-registration.v1";
pub const MEMBERSHIP_STATUS_ACTIVE: &str = "active";
pub const MEMBERSHIP_STATUS_REVOKED: &str = "revoked";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FederationEndpoint {
    pub transport: String,
    pub endpoint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedArtifact<T> {
    pub payload: T,
    pub signed_by: String,
    pub signature: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkManifestPayload {
    pub protocol_version: String,
    pub network_did: String,
    pub network_id: String,
    pub name: String,
    pub authority_did: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub federation_endpoints: Vec<FederationEndpoint>,
    pub issued_at_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at_ms: Option<u64>,
}

pub type NetworkManifest = SignedArtifact<NetworkManifestPayload>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkRegistrationRequestPayload {
    pub protocol_version: String,
    pub request_id: String,
    pub target_network_did: String,
    pub manifest: NetworkManifest,
    pub issued_at_ms: u64,
    pub expires_at_ms: u64,
}

pub type NetworkRegistrationRequest = SignedArtifact<NetworkRegistrationRequestPayload>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MainnetTrustBundlePayload {
    pub protocol_version: String,
    pub network_did: String,
    pub network_id: String,
    pub authority_did: String,
    pub issued_at_ms: u64,
}

pub type MainnetTrustBundle = SignedArtifact<MainnetTrustBundlePayload>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkMembershipCredentialPayload {
    pub protocol_version: String,
    pub credential_id: String,
    pub request_id: String,
    pub issuer_network_did: String,
    pub subject_network_did: String,
    pub subject_network_id: String,
    pub subject_authority_did: String,
    pub manifest_hash: String,
    pub issued_at_ms: u64,
    pub expires_at_ms: u64,
}

pub type NetworkMembershipCredential = SignedArtifact<NetworkMembershipCredentialPayload>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkMembershipRevocationPayload {
    pub protocol_version: String,
    pub revocation_id: String,
    pub credential_id: String,
    pub issuer_network_did: String,
    pub revoked_at_ms: u64,
    pub reason: String,
}

pub type NetworkMembershipRevocation = SignedArtifact<NetworkMembershipRevocationPayload>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredNetworkMembershipCredential {
    pub credential: NetworkMembershipCredential,
    pub status: String,
    pub registration_request: NetworkRegistrationRequest,
    pub trust_bundle: MainnetTrustBundle,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revocation: Option<NetworkMembershipRevocation>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct NetworkRegistrationState {
    #[serde(default)]
    requests: Vec<NetworkRegistrationRequest>,
    #[serde(default)]
    credentials: Vec<StoredNetworkMembershipCredential>,
}

struct FileLock(fs::File);

impl Drop for FileLock {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

#[derive(Debug, Clone)]
pub struct NetworkRegistrationStore {
    data_dir: PathBuf,
}

impl NetworkRegistrationStore {
    #[must_use]
    pub fn new(data_dir: impl AsRef<Path>) -> Self {
        Self {
            data_dir: data_dir.as_ref().to_path_buf(),
        }
    }

    pub fn save_request(&self, request: &NetworkRegistrationRequest) -> Result<()> {
        verify_registration_request(request, request.payload.issued_at_ms)?;
        let _lock = self.lock()?;
        let mut state = self.read_state()?;
        if let Some(existing) = state
            .requests
            .iter()
            .find(|item| item.payload.request_id == request.payload.request_id)
        {
            if existing != request {
                bail!("registration request id already stores a different request");
            }
            return Ok(());
        }
        state.requests.push(request.clone());
        self.write_state(&state)
    }

    pub fn store_credential(
        &self,
        credential: &NetworkMembershipCredential,
        request: &NetworkRegistrationRequest,
        trust_bundle: &MainnetTrustBundle,
    ) -> Result<()> {
        verify_membership_credential(
            credential,
            request,
            trust_bundle,
            credential.payload.issued_at_ms,
        )?;
        let _lock = self.lock()?;
        let mut state = self.read_state()?;
        if let Some(existing) = state.credentials.iter().find(|item| {
            item.credential.payload.credential_id == credential.payload.credential_id
                || item.credential.payload.request_id == credential.payload.request_id
        }) {
            if existing.credential != *credential
                || existing.registration_request != *request
                || existing.trust_bundle != *trust_bundle
            {
                bail!("credential or request id already stores different registration evidence");
            }
            return Ok(());
        }
        state.credentials.push(StoredNetworkMembershipCredential {
            credential: credential.clone(),
            status: MEMBERSHIP_STATUS_ACTIVE.to_owned(),
            registration_request: request.clone(),
            trust_bundle: trust_bundle.clone(),
            revocation: None,
        });
        self.write_state(&state)
    }

    pub fn list_credentials(
        &self,
        subject_network_did: Option<&str>,
    ) -> Result<Vec<StoredNetworkMembershipCredential>> {
        let mut records = self.read_state()?.credentials;
        if let Some(subject_network_did) = subject_network_did {
            records.retain(|record| {
                record.credential.payload.subject_network_did == subject_network_did
            });
        }
        records.sort_by(|left, right| {
            right
                .credential
                .payload
                .issued_at_ms
                .cmp(&left.credential.payload.issued_at_ms)
        });
        Ok(records)
    }

    pub fn credential(
        &self,
        credential_id: &str,
    ) -> Result<Option<StoredNetworkMembershipCredential>> {
        Ok(self
            .read_state()?
            .credentials
            .into_iter()
            .find(|record| record.credential.payload.credential_id == credential_id))
    }

    pub fn apply_revocation(&self, revocation: &NetworkMembershipRevocation) -> Result<()> {
        let _lock = self.lock()?;
        let mut state = self.read_state()?;
        let Some(record) = state.credentials.iter_mut().find(|record| {
            record.credential.payload.credential_id == revocation.payload.credential_id
        }) else {
            bail!("network membership credential not found for revocation");
        };
        verify_membership_revocation(revocation, &record.trust_bundle)?;
        if revocation.payload.issuer_network_did != record.credential.payload.issuer_network_did {
            bail!("revocation issuer does not match credential issuer");
        }
        if revocation.payload.revoked_at_ms < record.credential.payload.issued_at_ms {
            bail!("revocation predates credential issuance");
        }
        if let Some(existing) = &record.revocation {
            if existing == revocation {
                return Ok(());
            }
            bail!("network membership credential is already revoked");
        }
        MEMBERSHIP_STATUS_REVOKED.clone_into(&mut record.status);
        record.revocation = Some(revocation.clone());
        self.write_state(&state)
    }

    fn registration_dir(&self) -> PathBuf {
        self.data_dir.join(".network-registration")
    }

    fn state_path(&self) -> PathBuf {
        self.registration_dir().join("state.json")
    }

    fn lock(&self) -> Result<FileLock> {
        let directory = self.registration_dir();
        fs::create_dir_all(&directory).context("create network registration directory")?;
        acquire_file_lock(&directory.join(".state.lock"), "network registration state")
    }

    fn read_state(&self) -> Result<NetworkRegistrationState> {
        let path = self.state_path();
        if !path.exists() {
            return Ok(NetworkRegistrationState::default());
        }
        let raw = fs::read(&path)
            .with_context(|| format!("read network registration state {}", path.display()))?;
        serde_json::from_slice(&raw)
            .with_context(|| format!("parse network registration state {}", path.display()))
    }

    fn write_state(&self, state: &NetworkRegistrationState) -> Result<()> {
        let directory = self.registration_dir();
        fs::create_dir_all(&directory).context("create network registration directory")?;
        let path = self.state_path();
        let mut temporary = tempfile::NamedTempFile::new_in(&directory)
            .context("create registration state temp")?;
        temporary
            .write_all(&serde_json::to_vec_pretty(state)?)
            .context("write registration state temp")?;
        temporary
            .as_file()
            .sync_all()
            .context("sync registration state temp")?;
        temporary
            .persist(&path)
            .map_err(|error| error.error)
            .context("install network registration state")?;
        #[cfg(unix)]
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .context("restrict network registration state permissions")?;
        Ok(())
    }
}

#[must_use]
pub fn network_authority_path(data_dir: impl AsRef<Path>) -> PathBuf {
    data_dir
        .as_ref()
        .join(".network-authority")
        .join("identity.json")
}

pub fn load_or_create_network_authority(data_dir: impl AsRef<Path>) -> Result<Identity> {
    let data_dir = data_dir.as_ref();
    let path = network_authority_path(data_dir);
    let directory = path
        .parent()
        .context("network authority path has no parent")?;
    fs::create_dir_all(directory).context("create network authority directory")?;
    let _lock = acquire_file_lock(
        &directory.join(".identity.lock"),
        "network authority identity",
    )?;
    if path.exists() {
        let identity = Identity::load(&path)?;
        restrict_private_identity_permissions(&path)?;
        return Ok(identity);
    }
    let identity = Identity::new_random();
    write_private_identity(&path, &identity)?;
    Ok(identity)
}

fn acquire_file_lock(path: &Path, label: &str) -> Result<FileLock> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    options.mode(0o600);
    let file = options
        .open(path)
        .with_context(|| format!("open {label} lock"))?;
    file.lock_exclusive()
        .with_context(|| format!("lock {label}"))?;
    Ok(FileLock(file))
}

pub fn load_network_authority(data_dir: impl AsRef<Path>) -> Result<Identity> {
    let path = network_authority_path(data_dir);
    if !path.exists() {
        bail!("network authority is not initialized; run wattetheria network authority-init");
    }
    let identity = Identity::load(&path)?;
    restrict_private_identity_permissions(&path)?;
    Ok(identity)
}

fn sign_artifact<T>(payload: T, identity: &Identity) -> Result<SignedArtifact<T>>
where
    T: Serialize,
{
    let signature = sign_payload(&payload, identity)?;
    Ok(SignedArtifact {
        payload,
        signed_by: identity.agent_did.clone(),
        signature,
    })
}

fn verify_artifact<T>(artifact: &SignedArtifact<T>) -> Result<()>
where
    T: Serialize,
{
    if !verify_payload(&artifact.payload, &artifact.signature, &artifact.signed_by)? {
        bail!("signed network registration artifact signature is invalid");
    }
    Ok(())
}

fn require_version(version: &str) -> Result<()> {
    if version != NETWORK_REGISTRATION_PROTOCOL_VERSION {
        bail!("unsupported network registration protocol version '{version}'");
    }
    Ok(())
}

fn require_did(value: &str, name: &str) -> Result<()> {
    if !value.starts_with("did:") || value.len() <= 4 || value.chars().any(char::is_whitespace) {
        bail!("{name} must be a non-empty DID");
    }
    Ok(())
}

pub fn manifest_hash(manifest: &NetworkManifest) -> Result<String> {
    Ok(format!(
        "sha256:{:x}",
        Sha256::digest(canonical_bytes(manifest)?)
    ))
}

pub fn sign_network_manifest(
    payload: NetworkManifestPayload,
    authority: &Identity,
) -> Result<NetworkManifest> {
    if payload.authority_did != authority.agent_did {
        bail!("network manifest authority DID does not match signer");
    }
    sign_artifact(payload, authority)
}

pub fn verify_network_manifest(manifest: &NetworkManifest) -> Result<()> {
    require_version(&manifest.payload.protocol_version)?;
    require_did(&manifest.payload.network_did, "network_did")?;
    require_did(&manifest.payload.authority_did, "authority_did")?;
    if manifest.payload.network_id.trim().is_empty() || manifest.payload.name.trim().is_empty() {
        bail!("network manifest id and name are required");
    }
    if manifest.signed_by != manifest.payload.authority_did {
        bail!("network manifest signer does not match authority DID");
    }
    if manifest
        .payload
        .expires_at_ms
        .is_some_and(|expires| expires <= manifest.payload.issued_at_ms)
    {
        bail!("network manifest expiry must follow issuance");
    }
    let mut unique_endpoints = HashSet::new();
    for endpoint in &manifest.payload.federation_endpoints {
        if endpoint.transport.trim().is_empty() || endpoint.endpoint.trim().is_empty() {
            bail!("federation endpoint transport and endpoint are required");
        }
        if endpoint.transport.trim() != endpoint.transport
            || endpoint.endpoint.trim() != endpoint.endpoint
        {
            bail!("federation endpoint values must not have surrounding whitespace");
        }
        if !unique_endpoints.insert((endpoint.transport.as_str(), endpoint.endpoint.as_str())) {
            bail!("federation endpoints must be unique");
        }
    }
    verify_artifact(manifest)
}

pub fn sign_registration_request(
    payload: NetworkRegistrationRequestPayload,
    authority: &Identity,
) -> Result<NetworkRegistrationRequest> {
    verify_network_manifest(&payload.manifest)?;
    if payload.manifest.payload.authority_did != authority.agent_did {
        bail!("registration request authority does not match signer");
    }
    sign_artifact(payload, authority)
}

pub fn verify_registration_request(
    request: &NetworkRegistrationRequest,
    now_ms: u64,
) -> Result<()> {
    require_version(&request.payload.protocol_version)?;
    require_did(&request.payload.target_network_did, "target_network_did")?;
    if request.payload.request_id.trim().is_empty()
        || request.payload.expires_at_ms <= request.payload.issued_at_ms
    {
        bail!("registration request id and validity window are required");
    }
    verify_network_manifest(&request.payload.manifest)?;
    if request.signed_by != request.payload.manifest.payload.authority_did {
        bail!("registration request signer does not match network authority");
    }
    verify_artifact(request)?;
    if now_ms < request.payload.issued_at_ms || now_ms > request.payload.expires_at_ms {
        bail!("network registration request is not currently valid");
    }
    Ok(())
}

pub fn sign_trust_bundle(
    payload: MainnetTrustBundlePayload,
    authority: &Identity,
) -> Result<MainnetTrustBundle> {
    if payload.authority_did != authority.agent_did {
        bail!("mainnet trust bundle authority DID does not match signer");
    }
    sign_artifact(payload, authority)
}

pub fn verify_trust_bundle(bundle: &MainnetTrustBundle) -> Result<()> {
    require_version(&bundle.payload.protocol_version)?;
    require_did(&bundle.payload.network_did, "mainnet network_did")?;
    require_did(&bundle.payload.authority_did, "mainnet authority_did")?;
    if bundle.payload.network_id.trim().is_empty()
        || bundle.signed_by != bundle.payload.authority_did
    {
        bail!("mainnet trust bundle identity is invalid");
    }
    verify_artifact(bundle)
}

pub fn sign_membership_credential(
    payload: NetworkMembershipCredentialPayload,
    authority: &Identity,
) -> Result<NetworkMembershipCredential> {
    sign_artifact(payload, authority)
}

pub fn verify_membership_credential(
    credential: &NetworkMembershipCredential,
    request: &NetworkRegistrationRequest,
    trust_bundle: &MainnetTrustBundle,
    now_ms: u64,
) -> Result<()> {
    verify_trust_bundle(trust_bundle)?;
    verify_registration_request(request, credential.payload.issued_at_ms)?;
    let payload = &credential.payload;
    require_version(&payload.protocol_version)?;
    if payload.credential_id.trim().is_empty()
        || payload.expires_at_ms <= payload.issued_at_ms
        || now_ms < payload.issued_at_ms
        || now_ms > payload.expires_at_ms
    {
        bail!("network membership credential is not currently valid");
    }
    if credential.signed_by != trust_bundle.payload.authority_did
        || payload.issuer_network_did != trust_bundle.payload.network_did
        || request.payload.target_network_did != trust_bundle.payload.network_did
    {
        bail!("membership credential issuer does not match pinned mainnet authority");
    }
    let manifest = &request.payload.manifest;
    if payload.request_id != request.payload.request_id
        || payload.subject_network_did != manifest.payload.network_did
        || payload.subject_network_id != manifest.payload.network_id
        || payload.subject_authority_did != manifest.payload.authority_did
        || payload.manifest_hash != manifest_hash(manifest)?
    {
        bail!("membership credential does not match registration request");
    }
    if manifest
        .payload
        .expires_at_ms
        .is_some_and(|expires| payload.expires_at_ms > expires)
    {
        bail!("membership credential outlives the signed network manifest");
    }
    verify_artifact(credential)
}

pub fn sign_membership_revocation(
    payload: NetworkMembershipRevocationPayload,
    authority: &Identity,
) -> Result<NetworkMembershipRevocation> {
    if payload.reason.trim().is_empty() {
        bail!("membership revocation reason is required");
    }
    sign_artifact(payload, authority)
}

pub fn verify_membership_revocation(
    revocation: &NetworkMembershipRevocation,
    trust_bundle: &MainnetTrustBundle,
) -> Result<()> {
    verify_trust_bundle(trust_bundle)?;
    require_version(&revocation.payload.protocol_version)?;
    if revocation.payload.revocation_id.trim().is_empty()
        || revocation.payload.credential_id.trim().is_empty()
        || revocation.payload.reason.trim().is_empty()
    {
        bail!("membership revocation id, credential id, and reason are required");
    }
    if revocation.payload.issuer_network_did != trust_bundle.payload.network_did
        || revocation.signed_by != trust_bundle.payload.authority_did
    {
        bail!("membership revocation issuer does not match pinned mainnet authority");
    }
    verify_artifact(revocation)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flow() -> (
        NetworkRegistrationRequest,
        MainnetTrustBundle,
        NetworkMembershipCredential,
        Identity,
    ) {
        let autonomous = Identity::new_random();
        let mainnet = Identity::new_random();
        let manifest = sign_network_manifest(
            NetworkManifestPayload {
                protocol_version: NETWORK_REGISTRATION_PROTOCOL_VERSION.to_owned(),
                network_did: "did:watt:network:campus-a".to_owned(),
                network_id: "campus-a".to_owned(),
                name: "Campus A".to_owned(),
                authority_did: autonomous.agent_did.clone(),
                federation_endpoints: vec![],
                issued_at_ms: 1_000,
                expires_at_ms: None,
            },
            &autonomous,
        )
        .unwrap();
        let request = sign_registration_request(
            NetworkRegistrationRequestPayload {
                protocol_version: NETWORK_REGISTRATION_PROTOCOL_VERSION.to_owned(),
                request_id: "request-1".to_owned(),
                target_network_did: "did:watt:network:mainnet".to_owned(),
                manifest,
                issued_at_ms: 1_000,
                expires_at_ms: 10_000,
            },
            &autonomous,
        )
        .unwrap();
        let trust = sign_trust_bundle(
            MainnetTrustBundlePayload {
                protocol_version: NETWORK_REGISTRATION_PROTOCOL_VERSION.to_owned(),
                network_did: "did:watt:network:mainnet".to_owned(),
                network_id: "mainnet".to_owned(),
                authority_did: mainnet.agent_did.clone(),
                issued_at_ms: 1_000,
            },
            &mainnet,
        )
        .unwrap();
        let credential = sign_membership_credential(
            NetworkMembershipCredentialPayload {
                protocol_version: NETWORK_REGISTRATION_PROTOCOL_VERSION.to_owned(),
                credential_id: "credential-1".to_owned(),
                request_id: request.payload.request_id.clone(),
                issuer_network_did: trust.payload.network_did.clone(),
                subject_network_did: request.payload.manifest.payload.network_did.clone(),
                subject_network_id: request.payload.manifest.payload.network_id.clone(),
                subject_authority_did: request.payload.manifest.payload.authority_did.clone(),
                manifest_hash: manifest_hash(&request.payload.manifest).unwrap(),
                issued_at_ms: 2_000,
                expires_at_ms: 20_000,
            },
            &mainnet,
        )
        .unwrap();
        (request, trust, credential, mainnet)
    }

    #[test]
    fn signed_flow_verifies_and_rejects_tampering() {
        let (request, trust, credential, _) = flow();
        verify_membership_credential(&credential, &request, &trust, 3_000).unwrap();

        let mut tampered = request.clone();
        tampered.payload.manifest.payload.name = "Changed".to_owned();
        assert!(verify_registration_request(&tampered, 2_000).is_err());
    }

    #[test]
    fn file_store_keeps_evidence_and_revocation() {
        let dir = tempfile::tempdir().unwrap();
        let (request, trust, credential, authority) = flow();
        let store = NetworkRegistrationStore::new(dir.path());
        store.save_request(&request).unwrap();
        store
            .store_credential(&credential, &request, &trust)
            .unwrap();
        let revocation = sign_membership_revocation(
            NetworkMembershipRevocationPayload {
                protocol_version: NETWORK_REGISTRATION_PROTOCOL_VERSION.to_owned(),
                revocation_id: "revocation-1".to_owned(),
                credential_id: credential.payload.credential_id.clone(),
                issuer_network_did: credential.payload.issuer_network_did.clone(),
                revoked_at_ms: 4_000,
                reason: "operator decision".to_owned(),
            },
            &authority,
        )
        .unwrap();
        store.apply_revocation(&revocation).unwrap();
        let stored = store
            .credential(&credential.payload.credential_id)
            .unwrap()
            .unwrap();
        assert_eq!(stored.status, MEMBERSHIP_STATUS_REVOKED);
        assert_eq!(stored.registration_request, request);
        assert_eq!(stored.trust_bundle, trust);
    }

    #[test]
    fn concurrent_authority_initialization_keeps_one_identity() {
        let dir = tempfile::tempdir().unwrap();
        let identities = std::thread::scope(|scope| {
            (0..8)
                .map(|_| {
                    scope.spawn(|| {
                        load_or_create_network_authority(dir.path())
                            .unwrap()
                            .agent_did
                    })
                })
                .map(|handle| handle.join().unwrap())
                .collect::<Vec<_>>()
        });

        assert!(identities.iter().all(|did| did == &identities[0]));
        assert_eq!(
            load_network_authority(dir.path()).unwrap().agent_did,
            identities[0]
        );
    }
}
