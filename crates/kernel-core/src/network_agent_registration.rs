//! Wire types, signature validation, and local persistence for registry Agent membership.
//!
//! The registry stores the canonical record in `PostgreSQL`. Wattetheria stores the
//! public credential it receives in its existing `wattetheria.db` `SQLite` database
//! so subsequent cards and envelopes can carry the same signed artifact.

use anyhow::{Context, Result, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::local_db::{LocalDb, NetworkAgentCredentialRecord, NetworkPermissionCheckpoint};

pub const REGISTRATION_PROTOCOL_VERSION: u32 = 1;
pub const REGISTRATION_REQUEST_DOMAIN: &str = "wattetheria:network-registration-request:v1";
pub const PERMISSION_STATUS_WAITING: &str = "waiting";
pub const PERMISSION_STATUS_PENDING: &str = "pending";
pub const PERMISSION_STATUS_ACTIVE: &str = "active";
pub const PERMISSION_STATUS_REJECTED: &str = "rejected";
pub const PERMISSION_STATUS_SUSPENDED: &str = "suspended";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RegistrationRequest {
    pub version: u32,
    pub request_id: String,
    pub network_id: String,
    pub agent_did: String,
    pub nickname: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_card: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_card_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tenant_instance_id: Option<String>,
    pub nonce: String,
    pub signature_b64: String,
}

impl RegistrationRequest {
    pub fn signing_bytes(&self) -> Result<Vec<u8>> {
        self.signing_bytes_with_nickname(false)
    }

    /// The v1 request format included nickname in the signed payload.
    /// Keep this form for verifying requests created before nickname became
    /// mutable metadata.
    pub fn legacy_signing_bytes(&self) -> Result<Vec<u8>> {
        self.signing_bytes_with_nickname(true)
    }

    fn signing_bytes_with_nickname(&self, include_nickname: bool) -> Result<Vec<u8>> {
        let mut payload = serde_json::Map::new();
        payload.insert("domain".to_owned(), json!(REGISTRATION_REQUEST_DOMAIN));
        payload.insert("version".to_owned(), json!(self.version));
        payload.insert("request_id".to_owned(), json!(self.request_id));
        payload.insert("network_id".to_owned(), json!(self.network_id));
        payload.insert("agent_did".to_owned(), json!(self.agent_did));
        if include_nickname {
            payload.insert("nickname".to_owned(), json!(self.nickname));
        }
        if let Some(agent_card_hash) = self.agent_card_hash.as_ref() {
            payload.insert("agent_card_hash".to_owned(), json!(agent_card_hash));
        }
        payload.insert(
            "tenant_instance_id".to_owned(),
            json!(self.tenant_instance_id),
        );
        payload.insert("nonce".to_owned(), json!(self.nonce));
        Ok(serde_jcs::to_vec(&Value::Object(payload))?)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UnsignedMembershipCredential {
    pub version: u32,
    pub credential_id: String,
    pub request_id: String,
    pub network_id: String,
    pub agent_did: String,
    #[serde(rename = "issuer_genesis_id", alias = "issuer_authority_id")]
    pub issuer_authority_id: String,
    #[serde(rename = "issued_at", alias = "issued_at_ms")]
    pub issued_at_ms: u64,
    #[serde(
        rename = "expires_at",
        alias = "expires_at_ms",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub expires_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signing_key_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature_algorithm: Option<String>,
    #[serde(flatten)]
    pub extensions: BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MembershipCredential {
    #[serde(flatten)]
    pub unsigned: UnsignedMembershipCredential,
    pub signature_hex: String,
}

impl MembershipCredential {
    pub fn verify_for(&self, request: &RegistrationRequest, now_ms: u64) -> Result<()> {
        if self.unsigned.request_id != request.request_id
            || self.unsigned.network_id != request.network_id
            || self.unsigned.agent_did != request.agent_did
        {
            bail!("membership credential subject does not match registration request");
        }
        if self
            .unsigned
            .expires_at_ms
            .is_some_and(|expires_at| expires_at <= now_ms)
        {
            bail!("membership credential has expired");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NetworkPermissionUpdate {
    #[serde(flatten)]
    pub checkpoint: NetworkPermissionCheckpoint,
    pub credential_id: Option<String>,
    pub credential_hash: Option<String>,
    pub credential_expires_at_ms: Option<u64>,
}

pub fn store_membership_credential(
    db: &LocalDb,
    request: &RegistrationRequest,
    credential: &MembershipCredential,
    now_ms: u64,
) -> Result<bool> {
    credential.verify_for(request, now_ms)?;
    let credential_json = serde_json::to_string(&credential)
        .context("serialize network Agent membership Credential")?;
    let record = NetworkAgentCredentialRecord {
        network_id: request.network_id.clone(),
        agent_did: request.agent_did.clone(),
        request_id: request.request_id.clone(),
        credential_id: credential.unsigned.credential_id.clone(),
        credential_hash: membership_credential_hash(credential),
        credential_json,
        status: "active".to_owned(),
        issued_at_ms: credential.unsigned.issued_at_ms,
        credential_expires_at_ms: credential.unsigned.expires_at_ms,
        stored_at_ms: now_ms,
        updated_at_ms: now_ms,
    };
    db.upsert_network_agent_credential(&record)
}

#[allow(clippy::too_many_arguments)]
pub fn update_network_permission_checkpoint(
    db: &LocalDb,
    network_id: &str,
    node_id: &str,
    agent_did: &str,
    permission_status: &str,
    last_error: Option<String>,
    now_ms: u64,
) -> Result<NetworkPermissionCheckpoint> {
    let previous = db
        .load_network_permission_checkpoint(agent_did, Some(network_id), Some(node_id))?
        .or_else(|| {
            db.load_network_permission_checkpoint(agent_did, Some(network_id), None)
                .ok()
                .flatten()
        });
    let checkpoint = NetworkPermissionCheckpoint {
        network_id: network_id.to_owned(),
        node_id: node_id.to_owned(),
        agent_did: agent_did.to_owned(),
        permission_status: permission_status.to_owned(),
        network_status: if permission_status == PERMISSION_STATUS_ACTIVE {
            "running".to_owned()
        } else {
            "stopped".to_owned()
        },
        revision: previous.map_or(1, |checkpoint| checkpoint.revision.saturating_add(1)),
        last_error,
        updated_at_ms: now_ms,
    };
    db.upsert_network_permission_checkpoint(&checkpoint)?;
    Ok(checkpoint)
}

#[must_use]
pub fn membership_credential_hash(credential: &MembershipCredential) -> String {
    let bytes = serde_jcs::to_vec(credential).unwrap_or_default();
    let digest = Sha256::digest(bytes);
    format!("sha256:{}", hex::encode(digest))
}

pub fn load_network_permission_checkpoint(
    db: &LocalDb,
    agent_did: &str,
    network_id: Option<&str>,
) -> Result<Option<NetworkPermissionCheckpoint>> {
    db.load_network_permission_checkpoint(agent_did, network_id, None)
}

pub fn network_permission_is_active(db: &LocalDb, agent_did: &str) -> Result<bool> {
    let Some(checkpoint) = load_network_permission_checkpoint(db, agent_did, None)? else {
        return Ok(false);
    };
    if checkpoint.permission_status != PERMISSION_STATUS_ACTIVE {
        return Ok(false);
    }
    Ok(load_valid_membership_credential(db, agent_did, Some(&checkpoint.network_id))?.is_some())
}

pub fn network_permission_update(
    db: &LocalDb,
    checkpoint: &NetworkPermissionCheckpoint,
) -> Result<NetworkPermissionUpdate> {
    let credential = if checkpoint.permission_status == PERMISSION_STATUS_ACTIVE {
        load_valid_membership_credential(db, &checkpoint.agent_did, Some(&checkpoint.network_id))?
            .map(|(record, _)| record)
    } else {
        None
    };
    Ok(NetworkPermissionUpdate {
        checkpoint: checkpoint.clone(),
        credential_id: credential
            .as_ref()
            .map(|record| record.credential_id.clone()),
        credential_hash: credential
            .as_ref()
            .map(|record| record.credential_hash.clone()),
        credential_expires_at_ms: credential.and_then(|record| record.credential_expires_at_ms),
    })
}

pub fn set_membership_credential_status(
    db: &LocalDb,
    request: &RegistrationRequest,
    status: &str,
    now_ms: u64,
) -> Result<bool> {
    db.update_network_agent_credential_status(
        &request.network_id,
        &request.agent_did,
        &request.request_id,
        status,
        now_ms,
    )
}

pub fn load_active_membership_credential(
    db: &LocalDb,
    agent_did: &str,
    network_id: Option<&str>,
) -> Result<Option<MembershipCredential>> {
    Ok(load_valid_membership_credential(db, agent_did, network_id)?
        .map(|(_, credential)| credential))
}

fn load_valid_membership_credential(
    db: &LocalDb,
    agent_did: &str,
    network_id: Option<&str>,
) -> Result<Option<(NetworkAgentCredentialRecord, MembershipCredential)>> {
    let Some(record) = db.load_network_agent_credential(agent_did, network_id)? else {
        return Ok(None);
    };
    if record.status != "active"
        || record
            .credential_expires_at_ms
            .is_some_and(|expires_at_ms| expires_at_ms <= now_ms())
    {
        return Ok(None);
    }
    let credential: MembershipCredential = serde_json::from_str(&record.credential_json)
        .context("decode stored network Agent membership Credential")?;
    let valid = credential.unsigned.credential_id == record.credential_id
        && credential.unsigned.request_id == record.request_id
        && credential.unsigned.network_id == record.network_id
        && credential.unsigned.agent_did == record.agent_did
        && credential.unsigned.issued_at_ms == record.issued_at_ms
        && credential.unsigned.expires_at_ms == record.credential_expires_at_ms
        && membership_credential_hash(&credential) == record.credential_hash;
    Ok(valid.then_some((record, credential)))
}

#[must_use]
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

pub fn parse_registration_response(
    value: &Value,
) -> Result<(RegistrationRequest, Option<MembershipCredential>)> {
    let request: RegistrationRequest = serde_json::from_value(
        value
            .get("request")
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("registry response missing request"))?,
    )
    .context("decode registry registration request")?;
    let credential = value
        .get("credential")
        .filter(|candidate| !candidate.is_null())
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .context("decode registry membership credential")?;
    Ok((request, credential))
}

pub fn decode_request(value: &Value) -> Result<RegistrationRequest> {
    serde_json::from_value(
        value
            .get("request")
            .cloned()
            .unwrap_or_else(|| value.clone()),
    )
    .context("decode Agent registration request")
}

pub fn decode_credential(value: &Value) -> Result<MembershipCredential> {
    serde_json::from_value(
        value
            .get("credential")
            .cloned()
            .unwrap_or_else(|| value.clone()),
    )
    .context("decode membership credential")
}

pub fn request_signature_is_valid(request: &RegistrationRequest) -> Result<()> {
    let public_key = watt_did::Did::parse(&request.agent_did)
        .context("parse Agent DID in registration request")?;
    let did_key = watt_did::DidKey::from_did(public_key).context("decode Agent DID key")?;
    let watt_did::DidKeyPublicKey::Ed25519(public_key) = did_key
        .decode_public_key()
        .context("decode Agent DID public key")?
    else {
        bail!("registration request DID is not an Ed25519 key");
    };
    let signature = BASE64_STANDARD
        .decode(request.signature_b64.trim())
        .context("decode registration request signature")?;
    let signature: [u8; 64] = signature
        .try_into()
        .map_err(|_| anyhow::anyhow!("registration request signature must be 64 bytes"))?;
    let verifying_key =
        VerifyingKey::from_bytes(&public_key).context("parse Agent registration public key")?;
    let signature = Signature::from_bytes(&signature);
    if verifying_key
        .verify(&request.signing_bytes()?, &signature)
        .is_ok()
    {
        return Ok(());
    }
    verifying_key
        .verify(&request.legacy_signing_bytes()?, &signature)
        .context("verify Agent registration request signature")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn signed_registration_request(signing_key: &SigningKey) -> RegistrationRequest {
        let did_key =
            watt_did::DidKey::from_ed25519_public_key(signing_key.verifying_key().to_bytes())
                .expect("build Agent DID");
        let mut request = RegistrationRequest {
            version: REGISTRATION_PROTOCOL_VERSION,
            request_id: "request-signature".to_owned(),
            network_id: "network-1".to_owned(),
            agent_did: format!("did:key:{}", did_key.public_key_multibase),
            nickname: "Agent One".to_owned(),
            agent_card: None,
            agent_card_hash: None,
            tenant_instance_id: None,
            nonce: "nonce-signature".to_owned(),
            signature_b64: String::new(),
        };
        request.signature_b64 = BASE64_STANDARD.encode(
            signing_key
                .sign(&request.signing_bytes().expect("signing bytes"))
                .to_bytes(),
        );
        request
    }

    #[test]
    fn changing_nickname_does_not_invalidate_current_request_signature() {
        let signing_key = SigningKey::from_bytes(&[9_u8; 32]);
        let mut request = signed_registration_request(&signing_key);
        request.nickname = "Agent Renamed".to_owned();
        request_signature_is_valid(&request).expect("renamed request verifies");
    }

    #[test]
    fn changing_agent_card_hash_invalidates_request_signature() {
        let signing_key = SigningKey::from_bytes(&[8_u8; 32]);
        let mut request = signed_registration_request(&signing_key);
        request.agent_card_hash = Some("sha256:original".to_owned());
        request.signature_b64 = BASE64_STANDARD.encode(
            signing_key
                .sign(&request.signing_bytes().expect("Agent Card signing bytes"))
                .to_bytes(),
        );
        request.agent_card_hash = Some("sha256:tampered".to_owned());
        request_signature_is_valid(&request).expect_err("tampered Agent Card hash is rejected");
    }

    #[test]
    fn legacy_request_signature_remains_accepted_during_transition() {
        let signing_key = SigningKey::from_bytes(&[10_u8; 32]);
        let mut request = signed_registration_request(&signing_key);
        request.signature_b64 = BASE64_STANDARD.encode(
            signing_key
                .sign(
                    &request
                        .legacy_signing_bytes()
                        .expect("legacy signing bytes"),
                )
                .to_bytes(),
        );
        request_signature_is_valid(&request).expect("legacy request verifies");
    }

    fn signed_credential(request: &RegistrationRequest) -> MembershipCredential {
        let unsigned = UnsignedMembershipCredential {
            version: 1,
            credential_id: "cred-1".to_owned(),
            request_id: request.request_id.clone(),
            network_id: request.network_id.clone(),
            agent_did: request.agent_did.clone(),
            issuer_authority_id: "test-registry-authority".to_owned(),
            issued_at_ms: 1,
            expires_at_ms: None,
            signing_key_id: None,
            signature_algorithm: None,
            extensions: BTreeMap::new(),
        };
        MembershipCredential {
            unsigned,
            signature_hex: "registry-owned-opaque-proof".to_owned(),
        }
    }

    #[test]
    fn stores_membership_credentials_in_the_normalized_local_table() {
        let db = LocalDb::open_in_memory().unwrap();
        let request = RegistrationRequest {
            version: 1,
            request_id: "request-1".to_owned(),
            network_id: "network-1".to_owned(),
            agent_did: "did:key:z6MkhExample".to_owned(),
            nickname: "Agent".to_owned(),
            agent_card: None,
            agent_card_hash: None,
            tenant_instance_id: None,
            nonce: "nonce-1".to_owned(),
            signature_b64: "unused-in-this-test".to_owned(),
        };
        let credential = signed_credential(&request);
        store_membership_credential(&db, &request, &credential, 100).unwrap();
        assert_eq!(
            load_active_membership_credential(&db, &request.agent_did, Some(&request.network_id))
                .unwrap(),
            Some(credential)
        );
        assert!(set_membership_credential_status(&db, &request, "disabled", 200).unwrap());
        assert_eq!(
            load_active_membership_credential(&db, &request.agent_did, Some(&request.network_id))
                .unwrap(),
            None
        );
    }

    #[test]
    fn permission_checkpoint_requires_active_credential() {
        let db = LocalDb::open_in_memory().unwrap();
        let request = RegistrationRequest {
            version: 1,
            request_id: "request-1".to_owned(),
            network_id: "network-1".to_owned(),
            agent_did: "did:key:z6MkhExample".to_owned(),
            nickname: "Agent".to_owned(),
            agent_card: None,
            agent_card_hash: None,
            tenant_instance_id: None,
            nonce: "nonce-1".to_owned(),
            signature_b64: "unused-in-this-test".to_owned(),
        };
        assert!(!network_permission_is_active(&db, &request.agent_did).unwrap());

        let credential = signed_credential(&request);
        update_network_permission_checkpoint(
            &db,
            &request.network_id,
            "node-1",
            &request.agent_did,
            PERMISSION_STATUS_ACTIVE,
            None,
            100,
        )
        .unwrap();
        assert!(!network_permission_is_active(&db, &request.agent_did).unwrap());

        store_membership_credential(&db, &request, &credential, 100).unwrap();
        assert!(network_permission_is_active(&db, &request.agent_did).unwrap());

        let mut tampered = db
            .load_network_agent_credential(&request.agent_did, Some(&request.network_id))
            .unwrap()
            .unwrap();
        let valid_hash = tampered.credential_hash.clone();
        tampered.credential_hash = "sha256:tampered".to_owned();
        db.upsert_network_agent_credential(&tampered).unwrap();
        assert!(!network_permission_is_active(&db, &request.agent_did).unwrap());
        tampered.credential_hash = valid_hash;
        db.upsert_network_agent_credential(&tampered).unwrap();
        assert!(network_permission_is_active(&db, &request.agent_did).unwrap());

        tampered.credential_expires_at_ms = Some(99);
        db.upsert_network_agent_credential(&tampered).unwrap();
        assert!(!network_permission_is_active(&db, &request.agent_did).unwrap());

        update_network_permission_checkpoint(
            &db,
            &request.network_id,
            "node-1",
            &request.agent_did,
            PERMISSION_STATUS_PENDING,
            Some("awaiting approval".to_owned()),
            200,
        )
        .unwrap();
        assert!(!network_permission_is_active(&db, &request.agent_did).unwrap());
    }

    #[test]
    fn credential_without_expiry_remains_valid() {
        let request = RegistrationRequest {
            version: 1,
            request_id: "request-1".to_owned(),
            network_id: "network-1".to_owned(),
            agent_did: "did:key:z6MkhExample".to_owned(),
            nickname: "Agent".to_owned(),
            agent_card: None,
            agent_card_hash: None,
            tenant_instance_id: None,
            nonce: "nonce-1".to_owned(),
            signature_b64: "unused-in-this-test".to_owned(),
        };
        let credential = signed_credential(&request);
        credential.verify_for(&request, u64::MAX).unwrap();
    }

    #[test]
    fn local_credential_validation_is_signature_algorithm_agnostic() {
        let request = RegistrationRequest {
            version: 1,
            request_id: "request-1".to_owned(),
            network_id: "network-1".to_owned(),
            agent_did: "did:key:z6MkhExample".to_owned(),
            nickname: "Agent".to_owned(),
            agent_card: None,
            agent_card_hash: None,
            tenant_instance_id: None,
            nonce: "nonce-1".to_owned(),
            signature_b64: "unused-in-this-test".to_owned(),
        };
        let credential = MembershipCredential {
            unsigned: UnsignedMembershipCredential {
                version: 1,
                credential_id: "credential-1".to_owned(),
                request_id: request.request_id.clone(),
                network_id: request.network_id.clone(),
                agent_did: request.agent_did.clone(),
                issuer_authority_id: "opaque-authority".to_owned(),
                issued_at_ms: 1,
                expires_at_ms: None,
                signing_key_id: Some("key-1".to_owned()),
                signature_algorithm: Some("future-algorithm".to_owned()),
                extensions: BTreeMap::from([(
                    "algorithm_parameters".to_owned(),
                    json!({"curve": "future-curve"}),
                )]),
            },
            signature_hex: "opaque-registry-proof".to_owned(),
        };

        credential.verify_for(&request, 2).unwrap();
        let encoded = serde_json::to_string(&credential).unwrap();
        let decoded: MembershipCredential = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, credential);
        assert_eq!(
            decoded.unsigned.signature_algorithm.as_deref(),
            Some("future-algorithm")
        );
        assert_eq!(
            decoded.unsigned.extensions["algorithm_parameters"]["curve"],
            "future-curve"
        );
    }
}
