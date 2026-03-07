//! Validates the governance planet schema with a known-good payload.

use serde_json::json;
use wattetheria_conformance::validate;

#[test]
fn governance_planet_schema_accepts_valid_payload() {
    let payload = json!({
        "subnet_id": "planet-a",
        "name": "Planet A",
        "creator": "agent",
        "tax_rate": 0.05,
        "created_at": 1_700_000_000,
        "validators": ["a", "b"],
        "relays": ["a"],
        "archivists": ["a", "b"]
    });
    validate("governance_planet.json", &payload).unwrap();
}
