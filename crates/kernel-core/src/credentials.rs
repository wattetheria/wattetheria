//! Credential custody for Runtime Agent, Service Agent, and Provider identities.

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use watt_did::Did;

const CREDENTIAL_ROOT: &str = ".agent-identity/credentials";
const PROVIDER_CREDENTIAL_ROOT: &str = ".provider-identity/credentials";
const TRUST_ANCHORS_FILE: &str = "trust-anchors.json";
const STORE_LOCK_FILE: &str = ".store.lock";
const MAX_CREDENTIAL_BYTES: usize = 512 * 1024;

pub use watt_credential::{
    CanonicalCredential, CredentialEnvelope, CredentialFormat, CredentialState, ProofOutcome,
    TrustAnchor, TrustDecision, TrustOutcome, VerificationEvidence, VerifiedCredentialContext,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentCredentialOwnerKind {
    Runtime,
    Provider,
    ServiceAgent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentCredentialBinding {
    pub owner_kind: AgentCredentialOwnerKind,
    #[serde(default, alias = "agent_id", skip_serializing_if = "Option::is_none")]
    pub service_agent_identity_id: Option<String>,
    #[serde(alias = "agent_did")]
    pub owner_did: String,
}

impl AgentCredentialBinding {
    pub fn runtime(agent_did: impl Into<String>) -> Result<Self> {
        let binding = Self {
            owner_kind: AgentCredentialOwnerKind::Runtime,
            service_agent_identity_id: None,
            owner_did: agent_did.into(),
        };
        binding.validate()?;
        Ok(binding)
    }

    pub fn provider(provider_did: impl Into<String>) -> Result<Self> {
        let binding = Self {
            owner_kind: AgentCredentialOwnerKind::Provider,
            service_agent_identity_id: None,
            owner_did: provider_did.into(),
        };
        binding.validate()?;
        Ok(binding)
    }

    pub fn service_agent(
        service_agent_identity_id: impl Into<String>,
        owner_did: impl Into<String>,
    ) -> Result<Self> {
        let binding = Self {
            owner_kind: AgentCredentialOwnerKind::ServiceAgent,
            service_agent_identity_id: Some(service_agent_identity_id.into()),
            owner_did: owner_did.into(),
        };
        binding.validate()?;
        Ok(binding)
    }

    pub fn validate(&self) -> Result<()> {
        Did::parse(&self.owner_did).context("parse credential owner DID")?;
        match (self.owner_kind, self.service_agent_identity_id.as_deref()) {
            (AgentCredentialOwnerKind::Runtime | AgentCredentialOwnerKind::Provider, None) => {
                Ok(())
            }
            (AgentCredentialOwnerKind::ServiceAgent, Some(service_agent_identity_id))
                if !service_agent_identity_id.trim().is_empty()
                    && !service_agent_identity_id.chars().any(char::is_control) =>
            {
                Ok(())
            }
            (AgentCredentialOwnerKind::Runtime | AgentCredentialOwnerKind::Provider, Some(_)) => {
                bail!(
                    "Runtime and Provider credential bindings cannot include a Service Agent identity ID"
                )
            }
            (AgentCredentialOwnerKind::ServiceAgent, _) => {
                bail!("Service Agent credential binding requires a Service Agent identity ID")
            }
        }
    }

    #[must_use]
    pub fn owner_did(&self) -> &str {
        &self.owner_did
    }

    fn storage_key(&self) -> String {
        let owner = match self.owner_kind {
            AgentCredentialOwnerKind::Runtime => "runtime",
            AgentCredentialOwnerKind::Provider => "provider",
            AgentCredentialOwnerKind::ServiceAgent => "service_agent",
        };
        let value = format!(
            "{owner}|{}|{}",
            self.service_agent_identity_id
                .as_deref()
                .unwrap_or_default(),
            self.owner_did
        );
        hex::encode(Sha256::digest(value.as_bytes()))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum AgentCredentialVerification {
    Pending {
        reason: String,
    },
    Verified {
        context: Box<VerifiedCredentialContext>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentCredentialRecord {
    pub credential_id: String,
    pub binding: AgentCredentialBinding,
    pub envelope: CredentialEnvelope,
    pub imported_at: DateTime<Utc>,
    pub verification: AgentCredentialVerification,
}

impl AgentCredentialRecord {
    #[must_use]
    pub fn sha256(&self) -> String {
        self.envelope.sha256()
    }

    #[must_use]
    pub fn verification_status(&self) -> &'static str {
        match &self.verification {
            AgentCredentialVerification::Pending { .. } => "pending",
            AgentCredentialVerification::Verified { context }
                if context.status.state == CredentialState::Active
                    && context.trust.outcome == TrustOutcome::Trusted =>
            {
                "trusted"
            }
            AgentCredentialVerification::Verified { context }
                if context.status.state == CredentialState::Active =>
            {
                "untrusted"
            }
            AgentCredentialVerification::Verified { .. } => "inactive",
        }
    }
}

#[derive(Debug, Clone)]
pub struct FileAgentCredentialStore {
    root: PathBuf,
    provider_root: PathBuf,
}

pub use AgentCredentialBinding as CredentialBinding;
pub use AgentCredentialOwnerKind as CredentialOwnerKind;
pub use AgentCredentialRecord as CredentialRecord;
pub use AgentCredentialVerification as CredentialVerification;
pub use FileAgentCredentialStore as FileCredentialStore;

#[derive(Debug)]
struct CredentialStoreLock {
    file: fs::File,
}

impl Drop for CredentialStoreLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.file);
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct TrustAnchorFile {
    version: u32,
    anchors: Vec<TrustAnchor>,
}

impl FileAgentCredentialStore {
    pub fn new(data_dir: impl AsRef<Path>) -> Self {
        Self {
            root: data_dir.as_ref().join(CREDENTIAL_ROOT),
            provider_root: data_dir.as_ref().join(PROVIDER_CREDENTIAL_ROOT),
        }
    }

    pub fn import_pending(
        &self,
        binding: AgentCredentialBinding,
        envelope: CredentialEnvelope,
    ) -> Result<AgentCredentialRecord> {
        binding.validate()?;
        validate_envelope(&envelope)?;
        let _lock = self.lock()?;
        let credential_id = credential_id(&envelope);
        let path = self.record_path(&binding, &credential_id)?;
        if path.exists() {
            let existing = read_record(&path)?;
            if existing.binding != binding || existing.envelope != envelope {
                bail!("credential digest collides with a different stored credential");
            }
            return Ok(existing);
        }
        let record = AgentCredentialRecord {
            credential_id,
            binding,
            envelope,
            imported_at: Utc::now(),
            verification: AgentCredentialVerification::Pending {
                reason: "no credential adapter has verified this artifact yet".to_owned(),
            },
        };
        write_private_json(&path, &record)?;
        Ok(record)
    }

    pub fn store_verified(
        &self,
        binding: AgentCredentialBinding,
        envelope: CredentialEnvelope,
        context: VerifiedCredentialContext,
    ) -> Result<AgentCredentialRecord> {
        binding.validate()?;
        validate_envelope(&envelope)?;
        validate_verified_context(&binding, &envelope, &context)?;
        let _lock = self.lock()?;
        let credential_id = credential_id(&envelope);
        let path = self.record_path(&binding, &credential_id)?;
        let imported_at = if path.exists() {
            read_record(&path)?.imported_at
        } else {
            Utc::now()
        };
        let record = AgentCredentialRecord {
            credential_id,
            binding,
            envelope,
            imported_at,
            verification: AgentCredentialVerification::Verified {
                context: Box::new(context),
            },
        };
        write_private_json(&path, &record)?;
        Ok(record)
    }

    pub fn list(&self, binding: &AgentCredentialBinding) -> Result<Vec<AgentCredentialRecord>> {
        binding.validate()?;
        let _lock = self.lock()?;
        let directory = self.binding_directory(binding);
        if !directory.exists() {
            return Ok(vec![]);
        }
        let mut records = vec![];
        for entry in fs::read_dir(&directory).context("list Agent credential directory")? {
            let entry = entry.context("read Agent credential directory entry")?;
            if !entry
                .file_type()
                .context("read Agent credential entry type")?
                .is_file()
                || entry.path().extension().and_then(|value| value.to_str()) != Some("json")
            {
                continue;
            }
            let record = read_record(&entry.path())?;
            if record.binding != *binding {
                bail!("stored credential binding does not match its directory");
            }
            records.push(record);
        }
        records.sort_by(|left, right| {
            right
                .imported_at
                .cmp(&left.imported_at)
                .then_with(|| left.credential_id.cmp(&right.credential_id))
        });
        Ok(records)
    }

    pub fn delete(&self, binding: &AgentCredentialBinding, credential_id: &str) -> Result<bool> {
        binding.validate()?;
        let _lock = self.lock()?;
        let path = self.record_path(binding, credential_id)?;
        if !path.exists() {
            return Ok(false);
        }
        let record = read_record(&path)?;
        if record.binding != *binding || record.credential_id != credential_id {
            bail!("stored credential does not match delete request");
        }
        fs::remove_file(path).context("delete Agent credential")?;
        Ok(true)
    }

    pub fn load_trust_anchors(&self) -> Result<Vec<TrustAnchor>> {
        let _lock = self.lock()?;
        let path = self.root.join(TRUST_ANCHORS_FILE);
        if !path.exists() {
            return Ok(vec![]);
        }
        let file: TrustAnchorFile = serde_json::from_str(
            &fs::read_to_string(&path).context("read credential trust anchors")?,
        )
        .context("parse credential trust anchors")?;
        validate_trust_anchors(&file.anchors)?;
        Ok(file.anchors)
    }

    pub fn replace_trust_anchors(&self, anchors: Vec<TrustAnchor>) -> Result<()> {
        validate_trust_anchors(&anchors)?;
        let _lock = self.lock()?;
        write_private_json(
            &self.root.join(TRUST_ANCHORS_FILE),
            &TrustAnchorFile {
                version: 1,
                anchors,
            },
        )
    }

    fn binding_directory(&self, binding: &AgentCredentialBinding) -> PathBuf {
        let root = match binding.owner_kind {
            AgentCredentialOwnerKind::Provider => &self.provider_root,
            AgentCredentialOwnerKind::Runtime | AgentCredentialOwnerKind::ServiceAgent => {
                &self.root
            }
        };
        root.join("records").join(binding.storage_key())
    }

    fn record_path(
        &self,
        binding: &AgentCredentialBinding,
        credential_id: &str,
    ) -> Result<PathBuf> {
        let digest = credential_id
            .strip_prefix("sha256:")
            .filter(|value| value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .context("credential_id must be sha256:<64 lowercase hex characters>")?;
        Ok(self
            .binding_directory(binding)
            .join(format!("{}.json", digest.to_ascii_lowercase())))
    }

    fn lock(&self) -> Result<CredentialStoreLock> {
        fs::create_dir_all(&self.root).context("create Agent credential store")?;
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        options.mode(0o600);
        let file = options
            .open(self.root.join(STORE_LOCK_FILE))
            .context("open Agent credential store lock")?;
        fs2::FileExt::lock_exclusive(&file).context("lock Agent credential store")?;
        Ok(CredentialStoreLock { file })
    }
}

fn credential_id(envelope: &CredentialEnvelope) -> String {
    format!("sha256:{}", envelope.sha256())
}

fn validate_envelope(envelope: &CredentialEnvelope) -> Result<()> {
    if envelope.format.0.trim().is_empty() {
        bail!("credential format is required");
    }
    if envelope.payload.is_empty() {
        bail!("credential payload is required");
    }
    if envelope.payload.len() > MAX_CREDENTIAL_BYTES {
        bail!("credential payload exceeds {MAX_CREDENTIAL_BYTES} bytes");
    }
    Ok(())
}

fn validate_verified_context(
    binding: &AgentCredentialBinding,
    envelope: &CredentialEnvelope,
    context: &VerifiedCredentialContext,
) -> Result<()> {
    if context.evidence.proof.outcome != ProofOutcome::Valid {
        bail!("verified credential context must contain a valid proof outcome");
    }
    if context.evidence.original_sha256 != envelope.sha256()
        || context.credential.original.sha256 != envelope.sha256()
        || context.credential.original.format != envelope.format
    {
        bail!("verified credential context does not match the original credential artifact");
    }
    context
        .credential
        .validate()
        .context("validate canonical credential")?;
    if !context
        .credential
        .credential_subject
        .iter()
        .filter_map(|subject| subject.id.as_deref())
        .any(|subject| subject == binding.owner_did)
    {
        bail!(
            "verified credential subject does not contain owner DID '{}'",
            binding.owner_did
        );
    }
    Ok(())
}

fn validate_trust_anchors(anchors: &[TrustAnchor]) -> Result<()> {
    let mut ids = BTreeSet::new();
    for anchor in anchors {
        anchor
            .validate()
            .context("validate credential trust anchor")?;
        if !ids.insert(&anchor.id) {
            bail!("duplicate credential trust anchor id '{}'", anchor.id);
        }
    }
    Ok(())
}

fn read_record(path: &Path) -> Result<AgentCredentialRecord> {
    let record: AgentCredentialRecord =
        serde_json::from_str(&fs::read_to_string(path).context("read Agent credential")?)
            .context("parse Agent credential")?;
    restrict_private_permissions(path)?;
    Ok(record)
}

fn write_private_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let parent = path.parent().context("private JSON path has no parent")?;
    fs::create_dir_all(parent).context("create private JSON directory")?;
    let mut temporary =
        tempfile::NamedTempFile::new_in(parent).context("create temporary private JSON")?;
    temporary
        .write_all(serde_json::to_vec_pretty(value)?.as_slice())
        .context("write temporary private JSON")?;
    temporary
        .as_file()
        .sync_all()
        .context("sync temporary private JSON")?;
    restrict_private_permissions(temporary.path())?;
    temporary
        .persist(path)
        .map_err(|error| error.error)
        .context("install private JSON")?;
    restrict_private_permissions(path)
}

#[cfg(unix)]
fn restrict_private_permissions(path: &Path) -> Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .context("restrict private credential permissions")
}

#[cfg(not(unix))]
fn restrict_private_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn legacy_agent_id_credential_binding_remains_read_compatible() {
        let binding: AgentCredentialBinding = serde_json::from_value(serde_json::json!({
            "owner_kind": "service_agent",
            "agent_id": "sid-legacy",
            "agent_did": "did:key:z6MkvQ4QZz7T1cA7GJYk7oPK5vVsQt1zAr72Xd23LgzX776S",
        }))
        .unwrap();

        assert_eq!(
            binding.service_agent_identity_id.as_deref(),
            Some("sid-legacy")
        );
        binding.validate().unwrap();
        let current = AgentCredentialBinding::service_agent(
            "sid-legacy",
            "did:key:z6MkvQ4QZz7T1cA7GJYk7oPK5vVsQt1zAr72Xd23LgzX776S",
        )
        .unwrap();
        assert_eq!(binding.storage_key(), current.storage_key());
        let serialized = serde_json::to_value(binding).unwrap();
        assert_eq!(
            serialized["service_agent_identity_id"].as_str(),
            Some("sid-legacy")
        );
        assert!(serialized.get("agent_id").is_none());
        assert!(serialized.get("agent_did").is_none());
        assert_eq!(
            serialized["owner_did"].as_str(),
            Some("did:key:z6MkvQ4QZz7T1cA7GJYk7oPK5vVsQt1zAr72Xd23LgzX776S")
        );
    }

    fn verified_context(
        envelope: &CredentialEnvelope,
        subject: &str,
        state: CredentialState,
        trust: TrustOutcome,
    ) -> VerifiedCredentialContext {
        serde_json::from_value(serde_json::json!({
            "credential": {
                "@context": ["https://www.w3.org/ns/credentials/v2"],
                "type": ["VerifiableCredential", "RegionalIdentityCredential"],
                "issuer": { "id": "did:web:issuer.example" },
                "credentialSubject": [{ "id": subject }],
                "original": {
                    "format": envelope.format.clone(),
                    "sha256": envelope.sha256(),
                },
            },
            "evidence": {
                "adapter": {
                    "scheme": "test",
                    "version": "1",
                },
                "original_sha256": envelope.sha256(),
                "proof": {
                    "outcome": "valid",
                    "verification_method": "did:web:issuer.example#key-1",
                },
                "verified_at": Utc::now(),
            },
            "status": {
                "state": state,
                "checked_at": Utc::now(),
            },
            "trust": {
                "outcome": trust,
                "framework_id": "test",
            },
        }))
        .unwrap()
    }

    #[test]
    fn pending_credentials_are_private_idempotent_and_bound_to_one_agent_did() {
        let dir = tempdir().unwrap();
        let store = FileAgentCredentialStore::new(dir.path());
        let binding = AgentCredentialBinding::runtime(
            "did:key:z6MkvQ4QZz7T1cA7GJYk7oPK5vVsQt1zAr72Xd23LgzX776S",
        )
        .unwrap();
        let envelope =
            CredentialEnvelope::new("w3c_vc_json", br#"{"type":["VerifiableCredential"]}"#);

        let first = store
            .import_pending(binding.clone(), envelope.clone())
            .unwrap();
        let second = store.import_pending(binding.clone(), envelope).unwrap();
        let listed = store.list(&binding).unwrap();

        assert_eq!(first, second);
        assert_eq!(listed, vec![first]);
        assert_eq!(listed[0].verification_status(), "pending");
        #[cfg(unix)]
        {
            let path = store
                .record_path(&binding, &listed[0].credential_id)
                .unwrap();
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn provider_credentials_are_stored_below_the_provider_identity_directory() {
        let dir = tempdir().unwrap();
        let store = FileAgentCredentialStore::new(dir.path());
        let provider = AgentCredentialBinding::provider(
            "did:key:z6MkvQ4QZz7T1cA7GJYk7oPK5vVsQt1zAr72Xd23LgzX776S",
        )
        .unwrap();
        let runtime = AgentCredentialBinding::runtime(
            "did:key:z6MkpTHR8VNsBxYAAWHut2Geadd9jSwuBV8xRoAnwWsdvktH",
        )
        .unwrap();

        store
            .import_pending(
                provider.clone(),
                CredentialEnvelope::new("w3c_vc_json", b"provider-credential".to_vec()),
            )
            .unwrap();

        assert!(
            store
                .binding_directory(&provider)
                .starts_with(dir.path().join(PROVIDER_CREDENTIAL_ROOT))
        );
        assert!(
            store
                .binding_directory(&runtime)
                .starts_with(dir.path().join(CREDENTIAL_ROOT))
        );
        assert_eq!(store.list(&provider).unwrap().len(), 1);
        assert!(store.list(&runtime).unwrap().is_empty());
    }

    #[test]
    fn trust_anchor_replacement_validates_and_round_trips() {
        let dir = tempdir().unwrap();
        let store = FileAgentCredentialStore::new(dir.path());
        let anchor = TrustAnchor {
            id: "eu-anchor".to_owned(),
            framework_id: "eu-eidas".to_owned(),
            issuer: "did:web:issuer.example".to_owned(),
            jurisdiction: Some("EU".to_owned()),
            credential_types: vec!["VerifiableCredential".to_owned()],
            valid_from: None,
            valid_until: None,
        };

        store.replace_trust_anchors(vec![anchor.clone()]).unwrap();

        assert_eq!(store.load_trust_anchors().unwrap(), vec![anchor]);
    }

    #[test]
    fn credential_ids_cannot_escape_the_binding_directory() {
        let dir = tempdir().unwrap();
        let store = FileAgentCredentialStore::new(dir.path());
        let binding = AgentCredentialBinding::runtime(
            "did:key:z6MkvQ4QZz7T1cA7GJYk7oPK5vVsQt1zAr72Xd23LgzX776S",
        )
        .unwrap();

        assert!(store.delete(&binding, "../../identity.json").is_err());
    }

    #[test]
    fn verified_credential_must_name_the_bound_agent_did_as_a_subject() {
        let dir = tempdir().unwrap();
        let store = FileAgentCredentialStore::new(dir.path());
        let binding = AgentCredentialBinding::runtime(
            "did:key:z6MkpTHR8VNsBxYAAWHut2Geadd9jSwuBV8xRoAnwWsdvktH",
        )
        .unwrap();
        let envelope = CredentialEnvelope::new("w3c_vc_json", b"credential-a".to_vec());
        let context = verified_context(
            &envelope,
            "did:key:z6MkmismatchedSubject",
            CredentialState::Active,
            TrustOutcome::Trusted,
        );

        let error = store
            .store_verified(binding, envelope, context)
            .unwrap_err();

        assert!(error.to_string().contains("does not contain owner DID"));
    }

    #[test]
    fn verification_status_keeps_proof_status_and_policy_decisions_distinct() {
        let dir = tempdir().unwrap();
        let store = FileAgentCredentialStore::new(dir.path());
        let owner_did = "did:key:z6MkpTHR8VNsBxYAAWHut2Geadd9jSwuBV8xRoAnwWsdvktH";
        let binding = AgentCredentialBinding::runtime(owner_did).unwrap();
        let inactive_envelope =
            CredentialEnvelope::new("w3c_vc_json", b"inactive-credential".to_vec());
        let untrusted_envelope =
            CredentialEnvelope::new("w3c_vc_json", b"untrusted-credential".to_vec());
        let trusted_envelope =
            CredentialEnvelope::new("w3c_vc_json", b"trusted-credential".to_vec());

        let inactive = store
            .store_verified(
                binding.clone(),
                inactive_envelope.clone(),
                verified_context(
                    &inactive_envelope,
                    owner_did,
                    CredentialState::Revoked,
                    TrustOutcome::Trusted,
                ),
            )
            .unwrap();
        let untrusted = store
            .store_verified(
                binding.clone(),
                untrusted_envelope.clone(),
                verified_context(
                    &untrusted_envelope,
                    owner_did,
                    CredentialState::Active,
                    TrustOutcome::Untrusted,
                ),
            )
            .unwrap();
        let trusted = store
            .store_verified(
                binding,
                trusted_envelope.clone(),
                verified_context(
                    &trusted_envelope,
                    owner_did,
                    CredentialState::Active,
                    TrustOutcome::Trusted,
                ),
            )
            .unwrap();

        assert_eq!(inactive.verification_status(), "inactive");
        assert_eq!(untrusted.verification_status(), "untrusted");
        assert_eq!(trusted.verification_status(), "trusted");
    }
}
