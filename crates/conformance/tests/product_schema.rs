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
fn agent_participation_manifest_schema_accepts_valid_payload() {
    let payload = json!({
        "version": "v1",
        "generated_at": "2026-04-01T12:00:00Z",
        "node": {
            "agent_did": "did:key:zTest",
            "data_dir": "/tmp/.wattetheria"
        },
        "network": {
            "control_plane_bind": "0.0.0.0:7777",
            "control_plane_endpoint": "http://127.0.0.1:7777",
            "wattswarm_ui_base_url": "http://127.0.0.1:7788",
            "wattswarm_sync_grpc_endpoint": "http://127.0.0.1:7791",
            "topic_bridge_enabled": true
        },
        "auth": {
            "kind": "bearer_token",
            "header_name": "authorization",
            "header_format": "Bearer <token>",
            "token_file": "/tmp/.wattetheria/control.token"
        },
        "brain_provider": {
            "kind": "openai-compatible",
            "base_url": "http://127.0.0.1:4000/v1",
            "model": "openclaw-agent",
            "api_key_env": "OPENCLAW_API_KEY"
        },
        "endpoints": {
            "list_topics": {
                "method": "GET",
                "path": "/v1/civilization/topics",
                "url": "http://127.0.0.1:7777/v1/civilization/topics",
                "available": true
            },
            "create_topic": {
                "method": "POST",
                "path": "/v1/civilization/topics",
                "url": "http://127.0.0.1:7777/v1/civilization/topics",
                "available": true
            },
            "list_topic_messages": {
                "method": "GET",
                "path": "/v1/civilization/topics/messages",
                "url": "http://127.0.0.1:7777/v1/civilization/topics/messages",
                "available": true
            },
            "post_topic_message": {
                "method": "POST",
                "path": "/v1/civilization/topics/messages",
                "url": "http://127.0.0.1:7777/v1/civilization/topics/messages",
                "available": true
            },
            "subscribe_topic": {
                "method": "POST",
                "path": "/v1/civilization/topics/subscribe",
                "url": "http://127.0.0.1:7777/v1/civilization/topics/subscribe",
                "available": true
            }
        }
    });

    validate("agent_participation_manifest.json", &payload).unwrap();
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
