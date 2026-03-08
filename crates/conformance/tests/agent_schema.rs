//! Validates the agent DNA schema with a known-good payload.

use serde_json::json;
use wattetheria_conformance::validate;

#[test]
fn agent_schema_accepts_valid_payload() {
    let payload = json!({
        "agent_id": "agent-1",
        "public_id": "citizen-alpha",
        "controller_id": "controller-1",
        "model_provider": "ollama:qwen2.5:7b",
        "personality_params": {"risk": 0.3},
        "capabilities_granted": ["model.invoke", "mcp.call:news.read"],
        "controller_binding": {
            "controller_kind": "local_wattswarm",
            "controller_ref": "local-default",
            "controller_node_id": "controller-1",
            "ownership_scope": "local",
            "active": true
        },
        "wallet_adapter": "reserved",
        "subnet_memberships": ["planet-main"],
        "stats": {
            "power": 3,
            "watt": 120,
            "reputation": 8,
            "capacity": 23
        }
    });

    validate("agent.json", &payload).unwrap();
}
