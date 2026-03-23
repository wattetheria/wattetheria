//! Handshake, task, and signed-summary schema conformance tests.

use serde_json::json;
use wattetheria_conformance::validate;

#[test]
fn handshake_schema_accepts_valid_payload() {
    let payload = json!({
        "version": "0.1",
        "agent_did": "abc",
        "controller_id": "controller-abc",
        "public_id": "citizen-abc",
        "nonce": "n1",
        "timestamp": 1_700_000_000,
        "capabilities_summary": {"p2p":{"publish":{"rate_limit":120}}},
        "online_proof": {
            "lease_id": "lease-1",
            "lease_expiry": 1_700_003_600,
            "heartbeat_interval_sec": 20,
            "last_heartbeat": 1_700_000_001
        }
    });
    validate("handshake.json", &payload).unwrap();
}

#[test]
fn task_schema_rejects_invalid_mode() {
    let payload = json!({
        "task_id": "t1",
        "task_family": "market.match",
        "tier": "T0",
        "input_spec": {},
        "verification": {"mode": "bad"},
        "reward": {"watt":1,"reputation":1,"capacity":1},
        "sla": {"timeout_sec": 10},
        "signature": "sig"
    });
    assert!(validate("task.json", &payload).is_err());
}

#[test]
fn signed_summary_schema_accepts_valid_payload() {
    let payload = json!({
        "agent_did": "abc",
        "controller_id": "controller-abc",
        "public_id": "citizen-abc",
        "timestamp": 1_700_000_000,
        "power": 10,
        "watt": 20,
        "reputation": 4,
        "capacity": 12,
        "task_stats": {"completed":3,"success_rate":1.0,"contribution":11},
        "events_digest": "abcd",
        "signature": "sig"
    });
    validate("signed_summary.json", &payload).unwrap();
}
