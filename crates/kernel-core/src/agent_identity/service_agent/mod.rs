//! Per-Service-Agent identity custody.

mod file_store;
mod store;

use crate::identity::did_key_from_public_key_b64;
use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signer as _, SigningKey};
use rand_core::OsRng;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use watt_did::{Did, DidKey};

pub use file_store::{
    FileServiceAgentIdentityStore, ServiceAgentIdentityProvision, ServiceAgentOperationLock,
};
pub use store::ServiceAgentIdentityStore;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceAgentIdentity {
    pub agent_id: String,
    pub service_did: String,
    pub public_key: String,
    pub private_key: String,
    pub endpoint_url: String,
    pub key_version: u32,
}

impl ServiceAgentIdentity {
    pub fn generate(agent_id: &str, endpoint_url: &str) -> Result<Self> {
        Self::validate_endpoint_url(endpoint_url)?;
        let signing_key = SigningKey::generate(&mut OsRng);
        let public_key = STANDARD.encode(signing_key.verifying_key().as_bytes());
        Ok(Self {
            agent_id: agent_id.to_owned(),
            service_did: did_key_from_public_key_b64(&public_key)?,
            public_key,
            private_key: STANDARD.encode(signing_key.to_bytes()),
            endpoint_url: endpoint_url.to_owned(),
            key_version: 1,
        })
    }

    pub fn validate(&self) -> Result<()> {
        Self::validate_endpoint_url(&self.endpoint_url)?;
        let expected_did = did_key_from_public_key_b64(&self.public_key)?;
        if self.service_did != expected_did {
            bail!("Service Agent did:key does not match its public key");
        }
        let private_key = STANDARD
            .decode(&self.private_key)
            .context("decode Service Agent private key")?;
        let signing_key = SigningKey::from_bytes(
            private_key
                .as_slice()
                .try_into()
                .map_err(|_| anyhow!("invalid Service Agent private key length"))?,
        );
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

    pub(crate) fn validate_endpoint_url(endpoint_url: &str) -> Result<()> {
        let endpoint = Url::parse(endpoint_url).context("parse Service Agent endpoint URL")?;
        endpoint
            .host_str()
            .ok_or_else(|| anyhow!("Service Agent endpoint URL has no host"))?;
        Ok(())
    }

    pub fn sign(&self, payload: &[u8]) -> Result<String> {
        let private_key = STANDARD
            .decode(&self.private_key)
            .context("decode Service Agent private key")?;
        let signing_key = SigningKey::from_bytes(
            private_key
                .as_slice()
                .try_into()
                .map_err(|_| anyhow!("invalid Service Agent private key length"))?,
        );
        Ok(STANDARD.encode(signing_key.sign(payload).to_bytes()))
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
