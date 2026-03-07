//! Validates the cross-subnet mailbox message schema.

use serde_json::json;
use wattetheria_conformance::validate;

#[test]
fn mailbox_schema_accepts_valid_payload() {
    let payload = json!({
        "message_id": "m1",
        "from_agent": "a",
        "to_agent": "b",
        "from_subnet": "planet-a",
        "to_subnet": "planet-b",
        "timestamp": 1_700_000_000,
        "payload": {"text":"hi"},
        "signature": "sig"
    });
    validate("mailbox_message.json", &payload).unwrap();
}
