//! Validates the action envelope schema with a known-good payload.

use serde_json::json;
use wattetheria_conformance::validate;

#[test]
fn action_schema_accepts_valid_payload() {
    let payload = json!({
        "type": "ACTION",
        "version": "0.1",
        "action": "SPEAK",
        "action_id": "a1",
        "timestamp": 1_700_000_000,
        "sender": "agent-x",
        "payload": {"text":"hello"},
        "signature": "sig"
    });
    validate("action_envelope.json", &payload).unwrap();
}

#[test]
fn action_schema_rejects_unknown_action() {
    let payload = json!({
        "type": "ACTION",
        "version": "0.1",
        "action": "UNKNOWN",
        "action_id": "a1",
        "timestamp": 1_700_000_000,
        "sender": "agent-x",
        "payload": {"text":"hello"},
        "signature": "sig"
    });
    assert!(validate("action_envelope.json", &payload).is_err());
}
