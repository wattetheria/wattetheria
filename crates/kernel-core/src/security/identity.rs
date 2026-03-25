//! Agent identity storage and Ed25519 signing primitives.

use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use rand_core::OsRng;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;
use std::path::Path;
use watt_did::{Did, DidKey, DidKeyPublicKey};

const DID_KEY_PREFIX: &str = "did:key:";
const DID_KEY_BASE58BTC_PREFIX: char = 'z';
const ED25519_MULTICODEC_PREFIX: [u8; 2] = [0xed, 0x01];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Identity {
    pub agent_did: String,
    pub public_key: String,
    pub private_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdentityCompatView {
    pub agent_did: String,
    pub public_key: String,
}

impl Identity {
    #[must_use]
    pub fn new_random() -> Self {
        let mut csprng = OsRng;
        let signing = SigningKey::generate(&mut csprng);
        let verifying = signing.verifying_key();
        let public_key = STANDARD.encode(verifying.as_bytes());
        let private_key = STANDARD.encode(signing.to_bytes());
        let agent_did =
            did_key_from_public_key_b64(&public_key).expect("new_random public key must be valid");
        Self {
            agent_did,
            public_key,
            private_key,
        }
    }

    pub fn load_or_create(path: impl AsRef<Path>) -> Result<Self> {
        if path.as_ref().exists() {
            let identity = Self::load(&path)?;
            identity.save(path)?;
            return Ok(identity);
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

    pub fn save_compat_view(&self, path: impl AsRef<Path>) -> Result<()> {
        if let Some(parent) = path.as_ref().parent() {
            fs::create_dir_all(parent).context("create identity directory")?;
        }
        let content = serde_json::to_string_pretty(&self.compat_view())?;
        fs::write(path, content).context("write compatibility identity view")?;
        Ok(())
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let raw = fs::read_to_string(path).context("read identity")?;
        let raw_value: Value = serde_json::from_str(&raw).context("parse identity value")?;
        let public_key = raw_value
            .get("public_key")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("identity missing public_key"))?
            .to_string();
        let private_key = raw_value
            .get("private_key")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("identity missing private_key"))?
            .to_string();
        let agent_did = resolve_agent_did(
            raw_value.get("agent_did").and_then(Value::as_str),
            &public_key,
        )?;
        Ok(Self {
            agent_did,
            public_key,
            private_key,
        })
    }

    pub fn sign(&self, message: &[u8]) -> Result<String> {
        let bytes = STANDARD
            .decode(&self.private_key)
            .context("decode private key base64")?;
        let signing = SigningKey::from_bytes(
            bytes
                .as_slice()
                .try_into()
                .map_err(|_| anyhow!("invalid private key length"))?,
        );
        let signature = signing.sign(message);
        Ok(STANDARD.encode(signature.to_bytes()))
    }

    pub fn verify(&self, message: &[u8], signature_b64: &str) -> Result<bool> {
        verify_with_public_key(message, signature_b64, &self.public_key)
    }

    pub fn from_ed25519_seed(agent_did: impl Into<String>, seed: [u8; 32]) -> Result<Self> {
        let signing = SigningKey::from_bytes(&seed);
        let public_key = STANDARD.encode(signing.verifying_key().as_bytes());
        let private_key = STANDARD.encode(seed);
        let agent_did = agent_did.into();
        let agent_did = resolve_agent_did(Some(agent_did.as_str()), &public_key)?;
        Ok(Self {
            agent_did,
            public_key,
            private_key,
        })
    }

    #[must_use]
    pub fn compat_view(&self) -> IdentityCompatView {
        IdentityCompatView {
            agent_did: self.agent_did.clone(),
            public_key: self.public_key.clone(),
        }
    }
}

impl IdentityCompatView {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let raw = fs::read_to_string(path).context("read identity compatibility view")?;
        let raw_value: Value =
            serde_json::from_str(&raw).context("parse identity compatibility value")?;
        let public_key = raw_value
            .get("public_key")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("identity compatibility view missing public_key"))?
            .to_string();
        let agent_did = resolve_agent_did(
            raw_value.get("agent_did").and_then(Value::as_str),
            &public_key,
        )?;
        Ok(Self {
            agent_did,
            public_key,
        })
    }
}

pub fn verify_with_public_key(
    message: &[u8],
    signature_b64: &str,
    public_key_ref: &str,
) -> Result<bool> {
    let public = decode_public_key_from_ref(public_key_ref)?;
    let signature = STANDARD
        .decode(signature_b64)
        .context("decode signature base64")?;

    let verifying = VerifyingKey::from_bytes(
        public
            .as_slice()
            .try_into()
            .map_err(|_| anyhow!("invalid public key length"))?,
    )?;
    let sig = Signature::from_bytes(
        signature
            .as_slice()
            .try_into()
            .map_err(|_| anyhow!("invalid signature length"))?,
    );

    Ok(verifying.verify(message, &sig).is_ok())
}

fn resolve_agent_did(agent_did: Option<&str>, public_key_b64: &str) -> Result<String> {
    if let Some(agent_did) = agent_did.filter(|value| !value.trim().is_empty()) {
        let parsed = Did::parse(agent_did).context("parse agent_did")?;
        if parsed.method() != "key" {
            bail!("agent_did must use did:key");
        }
        let derived_public_key = public_key_b64_from_did_key(agent_did)?;
        if derived_public_key != public_key_b64 {
            bail!("agent_did does not match identity public_key");
        }
        return Ok(parsed.to_string());
    }
    bail!("identity missing valid agent_did for public_key {public_key_b64}")
}

fn decode_public_key_from_ref(public_key_ref: &str) -> Result<Vec<u8>> {
    if public_key_ref.starts_with(DID_KEY_PREFIX) {
        let public_key_b64 = public_key_b64_from_did_key(public_key_ref)?;
        return STANDARD
            .decode(&public_key_b64)
            .context("decode did:key public key base64");
    }

    STANDARD
        .decode(public_key_ref)
        .context("decode public key base64")
}

fn public_key_b64_from_did_key(agent_did: &str) -> Result<String> {
    let did = Did::parse(agent_did).context("parse did:key")?;
    let did_key = DidKey::from_did(did).context("build did:key helper")?;
    let DidKeyPublicKey::Ed25519(public_key) = did_key
        .decode_public_key()
        .context("decode did:key public key")?
    else {
        bail!("did:key is not an Ed25519 verification key");
    };
    Ok(STANDARD.encode(public_key))
}

fn did_key_from_public_key_b64(public_key_b64: &str) -> Result<String> {
    let public_key = STANDARD
        .decode(public_key_b64)
        .context("decode public key base64")?;
    if public_key.len() != 32 {
        bail!("invalid public key length");
    }
    let mut multicodec = Vec::with_capacity(ED25519_MULTICODEC_PREFIX.len() + public_key.len());
    multicodec.extend_from_slice(&ED25519_MULTICODEC_PREFIX);
    multicodec.extend_from_slice(&public_key);
    Ok(format!(
        "{DID_KEY_PREFIX}{DID_KEY_BASE58BTC_PREFIX}{}",
        bs58::encode(multicodec).into_string()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn sign_and_verify_roundtrip() {
        let identity = Identity::new_random();
        let msg = b"wattetheria";
        let sig = identity.sign(msg).unwrap();
        assert!(identity.verify(msg, &sig).unwrap());
        assert!(!identity.verify(b"other", &sig).unwrap());
        assert!(identity.agent_did.starts_with("did:key:z"));
        assert!(verify_with_public_key(msg, &sig, &identity.agent_did).unwrap());
    }

    #[test]
    fn load_current_identity_json_preserves_did() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("identity.json");
        let identity = Identity::new_random();
        identity.save(&path).unwrap();

        let loaded = Identity::load(&path).unwrap();
        assert_eq!(loaded.agent_did, identity.agent_did);
    }

    #[test]
    fn compat_view_excludes_private_key() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("identity.json");
        let identity = Identity::new_random();
        identity.save_compat_view(&path).unwrap();

        let raw: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(raw["agent_did"], identity.agent_did);
        assert_eq!(raw["public_key"], identity.public_key);
        assert!(raw.get("private_key").is_none());

        let loaded = IdentityCompatView::load(&path).unwrap();
        assert_eq!(loaded.agent_did, identity.agent_did);
    }
}
