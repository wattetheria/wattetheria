//! Agent identity storage and Ed25519 signing primitives.

use anyhow::{Context, Result};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use rand_core::OsRng;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Identity {
    pub agent_id: String,
    pub public_key: String,
    pub private_key: String,
}

impl Identity {
    #[must_use]
    pub fn new_random() -> Self {
        let mut csprng = OsRng;
        let signing = SigningKey::generate(&mut csprng);
        let verifying = signing.verifying_key();
        let public_key = STANDARD.encode(verifying.as_bytes());
        let private_key = STANDARD.encode(signing.to_bytes());
        Self {
            agent_id: public_key.clone(),
            public_key,
            private_key,
        }
    }

    pub fn load_or_create(path: impl AsRef<Path>) -> Result<Self> {
        if path.as_ref().exists() {
            return Self::load(path);
        }
        let identity = Self::new_random();
        identity.save(path)?;
        Ok(identity)
    }

    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        if let Some(parent) = path.as_ref().parent() {
            fs::create_dir_all(parent).context("create identity directory")?;
        }
        let content = serde_json::to_string_pretty(self)?;
        fs::write(path, content).context("write identity")?;
        Ok(())
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let raw = fs::read_to_string(path).context("read identity")?;
        let identity: Self = serde_json::from_str(&raw).context("parse identity")?;
        Ok(identity)
    }

    pub fn sign(&self, message: &[u8]) -> Result<String> {
        let bytes = STANDARD
            .decode(&self.private_key)
            .context("decode private key base64")?;
        let signing = SigningKey::from_bytes(
            bytes
                .as_slice()
                .try_into()
                .map_err(|_| anyhow::anyhow!("invalid private key length"))?,
        );
        let signature = signing.sign(message);
        Ok(STANDARD.encode(signature.to_bytes()))
    }

    pub fn verify(&self, message: &[u8], signature_b64: &str) -> Result<bool> {
        verify_with_public_key(message, signature_b64, &self.public_key)
    }
}

pub fn verify_with_public_key(
    message: &[u8],
    signature_b64: &str,
    public_key_b64: &str,
) -> Result<bool> {
    let public = STANDARD
        .decode(public_key_b64)
        .context("decode public key base64")?;
    let signature = STANDARD
        .decode(signature_b64)
        .context("decode signature base64")?;

    let verifying = VerifyingKey::from_bytes(
        public
            .as_slice()
            .try_into()
            .map_err(|_| anyhow::anyhow!("invalid public key length"))?,
    )?;
    let sig = Signature::from_bytes(
        signature
            .as_slice()
            .try_into()
            .map_err(|_| anyhow::anyhow!("invalid signature length"))?,
    );

    Ok(verifying.verify(message, &sig).is_ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sign_and_verify_roundtrip() {
        let identity = Identity::new_random();
        let msg = b"wattetheria";
        let sig = identity.sign(msg).unwrap();
        assert!(identity.verify(msg, &sig).unwrap());
        assert!(!identity.verify(b"other", &sig).unwrap());
    }
}
