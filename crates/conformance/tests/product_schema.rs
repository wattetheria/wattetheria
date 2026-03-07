// Validates MCP, brain provider, and oracle schemas.

use serde_json::json;
use wattetheria_conformance::validate;

#[test]
fn mcp_server_config_schema_accepts_valid_payload() {
    let payload = json!({
        "name": "news",
        "url": "http://127.0.0.1:9999",
        "enabled": true,
        "tools_allowlist": ["news.read"],
        "timeout_sec": 5,
        "budget_per_minute": 30
    });

    validate("mcp_server_config.json", &payload).unwrap();
}

#[test]
fn brain_provider_config_schema_accepts_valid_payload() {
    let payload = json!({
        "kind": "ollama",
        "base_url": "http://127.0.0.1:11434",
        "model": "qwen2.5:7b"
    });

    validate("brain_provider_config.json", &payload).unwrap();
}

#[test]
fn oracle_feed_schema_accepts_valid_payload() {
    let payload = json!({
        "feed_id": "btc-price",
        "publisher": "agent-1",
        "timestamp": 1_700_000_000,
        "payload": {"price": 100_000},
        "price_watt": 2,
        "signature": "base64sig"
    });

    validate("oracle_feed.json", &payload).unwrap();
}
