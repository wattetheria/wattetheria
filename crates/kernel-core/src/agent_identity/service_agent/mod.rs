//! Per-Service-Agent identity custody.

mod file_store;
mod layout;
mod store;

use crate::identity::{did_key_from_public_key_b64, import_ed25519_did_key};
use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signer as _, SigningKey};
use rand_core::OsRng;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use watt_did::{Did, DidKey};

pub use file_store::{
    FileServiceAgentIdentityStore, ServiceAgentIdentityProvision, ServiceAgentOperationLock,
};
pub use store::ServiceAgentIdentityStore;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ServiceAgentKeyOrigin {
    #[default]
    Generated,
    Imported,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceAgentIdentity {
    #[serde(alias = "identity_id", alias = "agent_id")]
    pub service_agent_identity_id: String,
    pub service_did: String,
    pub public_key: String,
    pub private_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bound_agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint_url: Option<String>,
    #[serde(default)]
    pub key_origin: ServiceAgentKeyOrigin,
    pub key_version: u32,
}

impl ServiceAgentIdentity {
    pub fn generate() -> Result<Self> {
        let signing_key = SigningKey::generate(&mut OsRng);
        let public_key = STANDARD.encode(signing_key.verifying_key().as_bytes());
        let service_did = did_key_from_public_key_b64(&public_key)?;
        Ok(Self {
            service_agent_identity_id: Self::service_agent_identity_id_for_did(&service_did),
            service_did,
            public_key,
            private_key: STANDARD.encode(signing_key.to_bytes()),
            bound_agent_id: None,
            endpoint_url: None,
            key_origin: ServiceAgentKeyOrigin::Generated,
            key_version: 1,
        })
    }

    pub fn import(expected_did: Option<&str>, private_key: &str) -> Result<Self> {
        let material = import_ed25519_did_key(expected_did, private_key)?;
        Ok(Self {
            service_agent_identity_id: Self::service_agent_identity_id_for_did(&material.did),
            service_did: material.did,
            public_key: material.public_key,
            private_key: material.private_key,
            bound_agent_id: None,
            endpoint_url: None,
            key_origin: ServiceAgentKeyOrigin::Imported,
            key_version: 1,
        })
    }

    pub fn bind_publication(&mut self, agent_id: &str, endpoint_url: &str) -> Result<()> {
        Self::validate_agent_id(agent_id)?;
        Self::validate_endpoint_url(endpoint_url)?;
        if let Some(bound_agent_id) = self.bound_agent_id.as_deref()
            && bound_agent_id != agent_id
        {
            bail!("Service Agent DID is already bound to Service Agent '{bound_agent_id}'");
        }
        self.bound_agent_id = Some(agent_id.to_owned());
        self.endpoint_url = Some(endpoint_url.to_owned());
        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        Self::validate_service_agent_identity_id(&self.service_agent_identity_id)?;
        if let Some(agent_id) = self.bound_agent_id.as_deref() {
            Self::validate_agent_id(agent_id)?;
        }
        if let Some(endpoint_url) = self.endpoint_url.as_deref() {
            Self::validate_endpoint_url(endpoint_url)?;
        }
        let expected_did = did_key_from_public_key_b64(&self.public_key)?;
        if self.service_did != expected_did {
            bail!("Service Agent did:key does not match its public key");
        }
        let signing_key = Self::signing_key(&self.private_key)?;
        if signing_key.verifying_key().as_bytes()
            != STANDARD
                .decode(&self.public_key)
                .context("decode Service Agent public key")?
                .as_slice()
        {
            bail!("Service Agent private key does not match its public key");
        }
        Ok(())
    }

    #[must_use]
    pub fn service_agent_identity_id_for_did(service_did: &str) -> String {
        let digest = Sha256::digest(service_did.as_bytes());
        format!("sid-{}", &hex::encode(digest)[..24])
    }

    fn validate_service_agent_identity_id(service_agent_identity_id: &str) -> Result<()> {
        let service_agent_identity_id = service_agent_identity_id.trim();
        if service_agent_identity_id.is_empty() {
            bail!("Service Agent identity ID is required");
        }
        if service_agent_identity_id.chars().any(char::is_control) {
            bail!("Service Agent identity ID must not contain control characters");
        }
        Ok(())
    }

    fn validate_agent_id(agent_id: &str) -> Result<()> {
        let agent_id = agent_id.trim();
        if agent_id.is_empty() {
            bail!("Service Agent agent_id is required");
        }
        if agent_id.chars().any(char::is_control) {
            bail!("Service Agent agent_id must not contain control characters");
        }
        Ok(())
    }

    pub(crate) fn validate_endpoint_url(endpoint_url: &str) -> Result<()> {
        let endpoint = Url::parse(endpoint_url).context("parse Service Agent endpoint URL")?;
        endpoint
            .host_str()
            .ok_or_else(|| anyhow!("Service Agent endpoint URL has no host"))?;
        Ok(())
    }

    pub fn sign(&self, payload: &[u8]) -> Result<String> {
        let signing_key = Self::signing_key(&self.private_key)?;
        Ok(STANDARD.encode(signing_key.sign(payload).to_bytes()))
    }

    fn signing_key(private_key: &str) -> Result<SigningKey> {
        let private_key = STANDARD
            .decode(private_key)
            .context("decode Service Agent private key")?;
        Ok(SigningKey::from_bytes(
            private_key
                .as_slice()
                .try_into()
                .map_err(|_| anyhow!("invalid Service Agent private key length"))?,
        ))
    }

    #[must_use]
    pub fn verification_method(&self) -> String {
        DidKey::from_did(
            Did::parse(&self.service_did).expect("stored Service Agent did:key must parse"),
        )
        .map(|did_key| format!("{}#{}", did_key.did, did_key.public_key_multibase))
        .expect("stored Service Agent identity must use did:key")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_identity_json_loads_with_generated_key_origin() {
        let identity = ServiceAgentIdentity::generate().unwrap();
        let legacy = serde_json::json!({
            "agent_id": "legacy-agent",
            "service_did": identity.service_did,
            "public_key": identity.public_key,
            "private_key": identity.private_key,
            "endpoint_url": "https://agent.example.com/a2a",
            "key_version": identity.key_version,
        });

        let loaded: ServiceAgentIdentity = serde_json::from_value(legacy).unwrap();

        assert_eq!(loaded.service_agent_identity_id, "legacy-agent");
        assert_eq!(loaded.bound_agent_id, None);
        assert_eq!(loaded.key_origin, ServiceAgentKeyOrigin::Generated);
        assert_eq!(
            loaded.endpoint_url.as_deref(),
            Some("https://agent.example.com/a2a")
        );
        loaded.validate().unwrap();
    }

    #[test]
    fn identity_id_json_field_remains_read_compatible() {
        let identity = ServiceAgentIdentity::generate().unwrap();
        let mut legacy = serde_json::to_value(&identity).unwrap();
        legacy["identity_id"] = legacy["service_agent_identity_id"].take();
        legacy
            .as_object_mut()
            .unwrap()
            .remove("service_agent_identity_id");

        let loaded: ServiceAgentIdentity = serde_json::from_value(legacy).unwrap();

        assert_eq!(
            loaded.service_agent_identity_id,
            identity.service_agent_identity_id
        );
        loaded.validate().unwrap();
    }
}
