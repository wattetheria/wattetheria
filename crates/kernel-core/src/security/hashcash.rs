//! Optional Hashcash minting and verification for anti-spam admission cost.

use chrono::Utc;
use sha2::{Digest, Sha256};

#[derive(Debug, Clone)]
pub struct HashcashStamp {
    pub version: u8,
    pub bits: u8,
    pub date: String,
    pub resource: String,
    pub nonce: String,
    pub counter: u64,
}

impl HashcashStamp {
    #[must_use]
    pub fn as_string(&self) -> String {
        format!(
            "{}:{}:{}:{}:{}:{}",
            self.version, self.bits, self.date, self.resource, self.nonce, self.counter
        )
    }

    #[must_use]
    pub fn parse(stamp: &str) -> Option<Self> {
        let mut head = stamp.splitn(4, ':');
        let version = head.next()?.parse().ok()?;
        let bits = head.next()?.parse().ok()?;
        let date = head.next()?.to_string();
        let tail = head.next()?;

        let mut tail_parts = tail.rsplitn(3, ':');
        let counter = tail_parts.next()?.parse().ok()?;
        let nonce = tail_parts.next()?.to_string();
        let resource = tail_parts.next()?.to_string();

        Some(Self {
            version,
            bits,
            date,
            resource,
            nonce,
            counter,
        })
    }
}

#[must_use]
pub fn mint(resource: &str, bits: u8, max_iterations: u64) -> Option<String> {
    let nonce = uuid::Uuid::new_v4().to_string();
    let date = Utc::now().format("%Y%m%d").to_string();

    for counter in 0..max_iterations {
        let stamp = HashcashStamp {
            version: 1,
            bits,
            date: date.clone(),
            resource: resource.to_string(),
            nonce: nonce.clone(),
            counter,
        };
        if meets_difficulty(&stamp.as_string(), bits) {
            return Some(stamp.as_string());
        }
    }
    None
}

#[must_use]
pub fn verify(stamp: &str, resource: &str, min_bits: u8) -> bool {
    let Some(parsed) = HashcashStamp::parse(stamp) else {
        return false;
    };
    if parsed.version != 1 {
        return false;
    }
    if parsed.resource != resource {
        return false;
    }
    if parsed.bits < min_bits {
        return false;
    }
    meets_difficulty(stamp, parsed.bits)
}

fn meets_difficulty(input: &str, bits: u8) -> bool {
    let hash = Sha256::digest(input.as_bytes());
    leading_zero_bits(&hash) >= bits
}

fn leading_zero_bits(bytes: &[u8]) -> u8 {
    let mut count: u8 = 0;
    for byte in bytes {
        if *byte == 0 {
            count = count.saturating_add(8);
            continue;
        }
        let leading = u8::try_from(byte.leading_zeros()).expect("u8::leading_zeros <= 8");
        count = count.saturating_add(leading);
        break;
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashcash_roundtrip() {
        let stamp = mint("agent-a", 12, 200_000).expect("mint hashcash");
        assert!(verify(&stamp, "agent-a", 12));
        assert!(!verify(&stamp, "agent-b", 12));
    }

    #[test]
    fn hashcash_roundtrip_with_did_resource() {
        let resource = "did:key:z6MkhaXg8SC7t8v4AbCdEfGh1234567890";
        let stamp = mint(resource, 12, 200_000).expect("mint hashcash");
        assert!(verify(&stamp, resource, 12));
        assert!(!verify(&stamp, "did:key:z6Mkother", 12));
    }
}
