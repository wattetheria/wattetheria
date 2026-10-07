use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use rand::RngCore;
use sha2::{Digest, Sha256};

const KEY_LEN: usize = 32;
const NONCE_LEN: usize = 24;
const ENVELOPE_VERSION: u8 = 1;

#[derive(Clone)]
pub struct EventCipher {
    key: Arc<[u8; KEY_LEN]>,
    key_id: String,
}

impl EventCipher {
    pub fn load_or_create(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("create MCP event key directory {}", parent.display()))?;
        }
        let mut key = [0u8; KEY_LEN];
        match open_new_key(path, &mut key) {
            Ok(()) => (),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {
                let metadata = fs::metadata(path)?;
                if !metadata.is_file() {
                    bail!("MCP event master key path is not a file");
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if metadata.permissions().mode() & 0o077 != 0 {
                        bail!("MCP event master key has permissions wider than 0600");
                    }
                }
                let mut file = File::open(path)?;
                file.read_exact(&mut key)?;
                let mut extra = [0u8; 1];
                if file.read(&mut extra)? != 0 {
                    bail!("MCP event master key must be exactly 32 bytes");
                }
            }
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("create MCP event master key {}", path.display()));
            }
        }
        let key_id = hex::encode(Sha256::digest(key));
        Ok(Self {
            key: Arc::new(key),
            key_id,
        })
    }

    pub fn key_id(&self) -> &str {
        &self.key_id
    }

    pub fn encrypt(&self, aad: &[u8], plaintext: &[u8]) -> Result<Vec<u8>> {
        let cipher = XChaCha20Poly1305::new_from_slice(self.key.as_ref())?;
        let mut nonce = [0u8; NONCE_LEN];
        rand::rng().fill_bytes(&mut nonce);
        let ciphertext = cipher
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: plaintext,
                    aad,
                },
            )
            .map_err(|_| anyhow::anyhow!("MCP event encryption failed"))?;
        let mut envelope = Vec::with_capacity(1 + NONCE_LEN + ciphertext.len());
        envelope.push(ENVELOPE_VERSION);
        envelope.extend_from_slice(&nonce);
        envelope.extend_from_slice(&ciphertext);
        Ok(envelope)
    }

    pub fn decrypt(&self, aad: &[u8], envelope: &[u8]) -> Result<Vec<u8>> {
        if envelope.len() < 1 + NONCE_LEN + 16 || envelope[0] != ENVELOPE_VERSION {
            bail!("invalid MCP event encrypted envelope");
        }
        let cipher = XChaCha20Poly1305::new_from_slice(self.key.as_ref())?;
        cipher
            .decrypt(
                XNonce::from_slice(&envelope[1..=NONCE_LEN]),
                Payload {
                    msg: &envelope[1 + NONCE_LEN..],
                    aad,
                },
            )
            .map_err(|_| {
                anyhow::anyhow!("MCP event decryption failed; restore the matching master key file")
            })
    }
}

fn open_new_key(path: &Path, key: &mut [u8; KEY_LEN]) -> std::io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    rand::rng().fill_bytes(key);
    file.write_all(key)?;
    file.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn master_key_roundtrip_and_aad_binding() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("event.key");
        let first = EventCipher::load_or_create(&path).unwrap();
        let encrypted = first.encrypt(b"owner-a", b"private").unwrap();
        assert!(!encrypted.windows(7).any(|window| window == b"private"));
        assert_eq!(first.decrypt(b"owner-a", &encrypted).unwrap(), b"private");
        assert!(first.decrypt(b"owner-b", &encrypted).is_err());
        let second = EventCipher::load_or_create(&path).unwrap();
        assert_eq!(first.key_id(), second.key_id());
        assert_eq!(second.decrypt(b"owner-a", &encrypted).unwrap(), b"private");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let metadata = std::fs::metadata(&path).unwrap();
            assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert!(EventCipher::load_or_create(&path).is_err());
        }
    }
}
