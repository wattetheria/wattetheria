//! Admission checks for incoming handshake packets (signature, clock drift, hashcash).

use anyhow::Result;
use chrono::Utc;
use serde_json::Value;
use std::collections::BTreeMap;

use crate::hashcash;
use crate::signing::verify_payload;

#[derive(Debug, Clone)]
pub struct AdmissionConfig {
    pub max_time_drift_sec: i64,
    pub min_hashcash_bits: u8,
    pub require_hashcash_for_handshake: bool,
    pub require_hashcash_for_broadcast: bool,
}

impl Default for AdmissionConfig {
    fn default() -> Self {
        Self {
            max_time_drift_sec: 180,
            min_hashcash_bits: 12,
            require_hashcash_for_handshake: false,
            require_hashcash_for_broadcast: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdmissionVerdict {
    Accept,
    Reject(String),
}

/// Tracks seen handshake nonces to reject replay attacks.
#[derive(Debug)]
pub struct NonceTracker {
    seen: BTreeMap<String, i64>,
    ttl_sec: i64,
}

impl NonceTracker {
    #[must_use]
    pub fn new(ttl_sec: i64) -> Self {
        Self {
            seen: BTreeMap::new(),
            ttl_sec,
        }
    }

    /// Returns `true` if the nonce is fresh (not seen before within TTL).
    pub fn check_and_record(&mut self, nonce: &str) -> bool {
        let now = Utc::now().timestamp();
        self.gc(now);
        if self.seen.contains_key(nonce) {
            return false;
        }
        self.seen.insert(nonce.to_string(), now);
        true
    }

    fn gc(&mut self, now: i64) {
        let cutoff = now - self.ttl_sec;
        self.seen.retain(|_, ts| *ts >= cutoff);
    }
}

#[must_use]
pub fn validate_gossip_packet(bytes: &[u8], config: &AdmissionConfig) -> AdmissionVerdict {
    let Ok(value) = serde_json::from_slice::<Value>(bytes) else {
        return AdmissionVerdict::Reject("invalid_json".to_string());
    };
    validate_envelope(&value, config, None).unwrap_or_else(AdmissionVerdict::Reject)
}

/// Stateful variant that also checks nonce replay for handshakes.
pub fn validate_gossip_packet_with_nonce(
    bytes: &[u8],
    config: &AdmissionConfig,
    nonce_tracker: &mut NonceTracker,
) -> AdmissionVerdict {
    let Ok(value) = serde_json::from_slice::<Value>(bytes) else {
        return AdmissionVerdict::Reject("invalid_json".to_string());
    };
    validate_envelope(&value, config, Some(nonce_tracker)).unwrap_or_else(AdmissionVerdict::Reject)
}

fn validate_envelope(
    value: &Value,
    config: &AdmissionConfig,
    nonce_tracker: Option<&mut NonceTracker>,
) -> Result<AdmissionVerdict, String> {
    let envelope_type = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();

    if envelope_type == "HANDSHAKE" {
        let agent_did = value
            .get("agent_did")
            .and_then(Value::as_str)
            .ok_or_else(|| "missing_agent_did".to_string())?;
        let payload = value
            .get("payload")
            .ok_or_else(|| "missing_payload".to_string())?;
        let signature = value
            .get("signature")
            .and_then(Value::as_str)
            .ok_or_else(|| "missing_signature".to_string())?;
        let controller_id = payload
            .get("controller_id")
            .and_then(Value::as_str)
            .unwrap_or(agent_did);

        if !verify_payload(payload, signature, controller_id)
            .map_err(|error| format!("verify_error:{error}"))?
        {
            return Ok(AdmissionVerdict::Reject("invalid_signature".to_string()));
        }

        let timestamp = payload
            .get("timestamp")
            .and_then(Value::as_i64)
            .ok_or_else(|| "missing_timestamp".to_string())?;
        let drift = (Utc::now().timestamp() - timestamp).abs();
        if drift > config.max_time_drift_sec {
            return Ok(AdmissionVerdict::Reject("clock_drift_exceeded".to_string()));
        }

        let nonce = payload
            .get("nonce")
            .and_then(Value::as_str)
            .ok_or_else(|| "missing_nonce".to_string())?;
        if nonce.trim().is_empty() {
            return Ok(AdmissionVerdict::Reject("missing_nonce".to_string()));
        }

        // Nonce replay protection: reject handshakes with previously seen nonces.
        if let Some(tracker) = nonce_tracker
            && !tracker.check_and_record(nonce)
        {
            return Ok(AdmissionVerdict::Reject("nonce_replay".to_string()));
        }

        let hashcash_payload = payload.get("hashcash").filter(|value| !value.is_null());
        if config.require_hashcash_for_handshake && hashcash_payload.is_none() {
            return Ok(AdmissionVerdict::Reject("hashcash_required".to_string()));
        }

        if let Some(hashcash_payload) = hashcash_payload {
            let stamp = hashcash_payload
                .get("stamp")
                .and_then(Value::as_str)
                .ok_or_else(|| "hashcash_missing_stamp".to_string())?;
            let resource = hashcash_payload
                .get("resource")
                .and_then(Value::as_str)
                .unwrap_or(agent_did);

            if !hashcash::verify(stamp, resource, config.min_hashcash_bits) {
                return Ok(AdmissionVerdict::Reject("invalid_hashcash".to_string()));
            }
        }

        return Ok(AdmissionVerdict::Accept);
    }

    if config.require_hashcash_for_broadcast && matches!(envelope_type, "ACTION" | "ORACLE_FEED") {
        let hashcash_payload = value
            .get("hashcash")
            .filter(|payload| !payload.is_null())
            .ok_or_else(|| "hashcash_required_for_broadcast".to_string())?;
        let stamp = hashcash_payload
            .get("stamp")
            .and_then(Value::as_str)
            .ok_or_else(|| "hashcash_missing_stamp".to_string())?;
        let resource = hashcash_payload
            .get("resource")
            .and_then(Value::as_str)
            .unwrap_or(envelope_type);

        if !hashcash::verify(stamp, resource, config.min_hashcash_bits) {
            return Ok(AdmissionVerdict::Reject("invalid_hashcash".to_string()));
        }
    }

    Ok(AdmissionVerdict::Accept)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hashcash;
    use crate::identity::Identity;
    use crate::signing::sign_payload;
    use serde_json::json;

    fn make_handshake(identity: &Identity, hashcash_enabled: bool, timestamp: i64) -> Value {
        let hashcash_value = if hashcash_enabled {
            let stamp = hashcash::mint(&identity.agent_did, 12, 300_000).unwrap();
            Some(json!({
                "stamp": stamp,
                "bits": 12,
                "resource": identity.agent_did
            }))
        } else {
            None
        };
        let payload = json!({
            "version": "0.1",
            "agent_did": identity.agent_did,
            "controller_id": identity.agent_did,
            "public_id": identity.agent_did,
            "nonce": "n-1",
            "timestamp": timestamp,
            "capabilities_summary": {},
            "online_proof": {"lease_id":"a"},
            "hashcash": hashcash_value,
        });
        let signature = sign_payload(&payload, identity).unwrap();
        json!({
            "type": "HANDSHAKE",
            "version": "0.1",
            "agent_did": identity.agent_did,
            "payload": payload,
            "signature": signature,
        })
    }

    #[test]
    fn handshake_requires_hashcash_when_enabled() {
        let identity = Identity::new_random();
        let now = Utc::now().timestamp();
        let packet = make_handshake(&identity, false, now);
        let bytes = serde_json::to_vec(&packet).unwrap();
        let verdict = validate_gossip_packet(
            &bytes,
            &AdmissionConfig {
                require_hashcash_for_handshake: true,
                ..AdmissionConfig::default()
            },
        );
        assert_eq!(
            verdict,
            AdmissionVerdict::Reject("hashcash_required".to_string())
        );
    }

    #[test]
    fn handshake_accepts_valid_signature_and_hashcash() {
        let identity = Identity::new_random();
        let now = Utc::now().timestamp();
        let packet = make_handshake(&identity, true, now);
        let bytes = serde_json::to_vec(&packet).unwrap();
        let verdict = validate_gossip_packet(
            &bytes,
            &AdmissionConfig {
                require_hashcash_for_handshake: true,
                ..AdmissionConfig::default()
            },
        );
        assert_eq!(verdict, AdmissionVerdict::Accept);
    }

    #[test]
    fn handshake_rejects_bad_clock_drift() {
        let identity = Identity::new_random();
        let now = Utc::now().timestamp();
        let packet = make_handshake(&identity, true, now - 1_000);
        let bytes = serde_json::to_vec(&packet).unwrap();
        let verdict = validate_gossip_packet(
            &bytes,
            &AdmissionConfig {
                max_time_drift_sec: 120,
                require_hashcash_for_handshake: true,
                ..AdmissionConfig::default()
            },
        );
        assert_eq!(
            verdict,
            AdmissionVerdict::Reject("clock_drift_exceeded".to_string())
        );
    }

    #[test]
    fn handshake_rejects_nonce_replay() {
        let identity = Identity::new_random();
        let now = Utc::now().timestamp();
        let packet = make_handshake(&identity, false, now);
        let bytes = serde_json::to_vec(&packet).unwrap();
        let config = AdmissionConfig::default();
        let mut tracker = NonceTracker::new(300);

        let first = validate_gossip_packet_with_nonce(&bytes, &config, &mut tracker);
        assert_eq!(first, AdmissionVerdict::Accept);

        let second = validate_gossip_packet_with_nonce(&bytes, &config, &mut tracker);
        assert_eq!(second, AdmissionVerdict::Reject("nonce_replay".to_string()));
    }

    #[test]
    fn handshake_rejects_missing_nonce() {
        let identity = Identity::new_random();
        let payload = json!({
            "version": "0.1",
            "agent_did": identity.agent_did,
            "timestamp": Utc::now().timestamp(),
            "capabilities_summary": {},
            "online_proof": {"lease_id":"a"},
            "hashcash": null
        });
        let signature = sign_payload(&payload, &identity).unwrap();
        let packet = json!({
            "type": "HANDSHAKE",
            "version": "0.1",
            "agent_did": identity.agent_did,
            "payload": payload,
            "signature": signature,
        });
        let bytes = serde_json::to_vec(&packet).unwrap();
        let mut tracker = NonceTracker::new(300);
        let verdict =
            validate_gossip_packet_with_nonce(&bytes, &AdmissionConfig::default(), &mut tracker);
        assert_eq!(
            verdict,
            AdmissionVerdict::Reject("missing_nonce".to_string())
        );
    }

    #[test]
    fn broadcast_requires_hashcash_when_enabled() {
        let packet = json!({
            "type": "ORACLE_FEED",
            "feed": {"feed_id":"btc-price"}
        });
        let bytes = serde_json::to_vec(&packet).unwrap();
        let verdict = validate_gossip_packet(
            &bytes,
            &AdmissionConfig {
                require_hashcash_for_broadcast: true,
                ..AdmissionConfig::default()
            },
        );
        assert_eq!(
            verdict,
            AdmissionVerdict::Reject("hashcash_required_for_broadcast".to_string())
        );
    }
}
