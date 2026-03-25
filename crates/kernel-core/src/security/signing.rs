//! Canonical JSON signing helpers shared by protocol payloads.

use anyhow::{Context, Result};
use serde::Serialize;

use crate::identity::{Identity, verify_with_public_key};

pub trait PayloadSigner: Send + Sync {
    fn agent_did(&self) -> &str;
    fn public_key(&self) -> &str;
    fn sign_bytes(&self, payload: &[u8]) -> Result<String>;
}

impl PayloadSigner for Identity {
    fn agent_did(&self) -> &str {
        &self.agent_did
    }

    fn public_key(&self) -> &str {
        &self.public_key
    }

    fn sign_bytes(&self, payload: &[u8]) -> Result<String> {
        self.sign(payload)
    }
}

pub fn canonical_bytes(payload: &impl Serialize) -> Result<Vec<u8>> {
    let json = serde_jcs::to_string(payload).context("canonicalize payload")?;
    Ok(json.into_bytes())
}

pub fn sign_payload(payload: &impl Serialize, identity: &Identity) -> Result<String> {
    sign_payload_with(payload, identity)
}

pub fn sign_payload_with(
    payload: &impl Serialize,
    signer: &(impl PayloadSigner + ?Sized),
) -> Result<String> {
    signer.sign_bytes(&canonical_bytes(payload)?)
}

pub fn verify_payload(
    payload: &impl Serialize,
    signature_b64: &str,
    public_key_b64: &str,
) -> Result<bool> {
    verify_with_public_key(&canonical_bytes(payload)?, signature_b64, public_key_b64)
}

pub fn canonical_equal(a: &impl Serialize, b: &impl Serialize) -> Result<bool> {
    Ok(canonical_bytes(a)? == canonical_bytes(b)?)
}
