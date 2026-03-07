use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::{Value, json};
use wattetheria_kernel::hashcash;
use wattetheria_kernel::identity::Identity;
use wattetheria_kernel::online_proof::OnlineProofManager;
use wattetheria_kernel::signing::sign_payload;

#[derive(Debug, Clone, Serialize)]
pub struct SignedEnvelope<T: Serialize> {
    pub r#type: String,
    pub version: String,
    pub agent_id: String,
    pub payload: T,
    pub signature: String,
}

fn build_handshake_payload(identity: &Identity, enable_hashcash: bool) -> Option<Value> {
    if !enable_hashcash {
        return None;
    }
    hashcash::mint(&identity.agent_id, 12, 200_000)
        .map(|stamp| json!({"stamp": stamp, "bits": 12, "resource": identity.agent_id}))
}

pub fn build_signed_handshake(
    identity: &Identity,
    online_proof: &OnlineProofManager,
    enable_hashcash: bool,
) -> Result<SignedEnvelope<Value>> {
    let online_payload = online_proof
        .get_proof(&identity.agent_id)
        .context("online proof missing")?;
    let hashcash_value = build_handshake_payload(identity, enable_hashcash);
    let payload = json!({
        "version": "0.1",
        "agent_id": identity.agent_id,
        "nonce": uuid::Uuid::new_v4().to_string(),
        "timestamp": chrono::Utc::now().timestamp(),
        "capabilities_summary": {
            "fs": {"read": ["/data"], "write": []},
            "net": {"outbound": [], "rate_limit": 60},
            "proc": {"exec": false},
            "wallet": {"sign": false, "send": false},
            "mcp": {"call": []},
            "model": {"invoke": {"tpm": 0}},
            "p2p": {"publish": {"rate_limit": 120}}
        },
        "online_proof": online_payload,
        "hashcash": hashcash_value,
    });
    Ok(SignedEnvelope {
        r#type: "HANDSHAKE".to_string(),
        version: "0.1".to_string(),
        agent_id: identity.agent_id.clone(),
        signature: sign_payload(&payload, identity)?,
        payload,
    })
}
