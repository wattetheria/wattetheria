use super::*;

use std::collections::BTreeSet;

use watt_did::PaymentAccountCustody;

use watt_wallet::{
    InMemoryKeyStore, KeyStore, PaymentAccountBindingProofOptions, PaymentAccountSigner,
    build_payment_account_binding_proof,
};

use wattetheria_social::domain::friend_requests::{
    FriendRequest, FriendRequestDirection, FriendRequestState,
};

fn alpha_x402_settlement_receipt() -> Value {
    json!({
        "success": true,
        "payer": "0x1111111111111111111111111111111111111111",
        "transaction": "0x89c91c789e57059b17285e7ba1716a1f5ff4c5dace0ea5a5135f26158d0421b9",
        "network": "base",
        "amount": "180000",
        "payTo": "0x742d35Cc6634C0532925a3b844Bc454e4438f44e"
    })
}

fn discovered_source_agent_card(
    public_id: &str,
    display_name: &str,
    agent_did: &str,
    remote_node_id: &str,
    card_hash_suffix: &str,
) -> SwarmSourceAgentCard {
    SwarmSourceAgentCard {
        agent_id: agent_did.to_owned(),
        node_id: Some(remote_node_id.to_owned()),
        card_hash: format!("sha256:{card_hash_suffix}"),
        issued_at: 1_716_120_000_000,
        card: json!({
            "name": display_name,
            "description": "Discovered network agent.",
            "metadata": {
                "agent_id": agent_did,
                "node_id": remote_node_id,
                "public_id": public_id,
                "display_name": display_name,
            },
            "skills": [
                {"id": "social", "name": "Social direct message"}
            ]
        }),
        signature: Some(format!("sig-{card_hash_suffix}")),
    }
}

async fn mcp_request(app: Router, token: &str, body: Value) -> Value {
    request_json(
        app,
        axum::http::Request::builder()
            .method("POST")
            .uri("/mcp")
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(axum::body::Body::from(body.to_string()))
            .unwrap(),
    )
    .await
}

fn find_tool<'a>(tools: &'a [Value], name: &str) -> &'a Value {
    tools
        .iter()
        .find(|tool| tool["name"].as_str() == Some(name))
        .unwrap()
}

fn assert_schema_requires(tool: &Value, expected: &[&str]) {
    let required = tool["inputSchema"]["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(Value::as_str)
        .collect::<Vec<_>>();
    for field in expected {
        assert!(
            required.contains(&Some(*field)),
            "expected {} schema to require {field}, got {required:?}",
            tool["name"].as_str().unwrap()
        );
    }
}

fn assert_schema_optional(tool: &Value, field: &str) {
    let properties = tool["inputSchema"]["properties"].as_object().unwrap();
    assert!(
        properties.contains_key(field),
        "expected {} schema to include optional field {field}",
        tool["name"].as_str().unwrap()
    );
    let required = tool["inputSchema"]["required"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>();
    assert!(
        !required.contains(&field),
        "expected {} schema field {field} to be optional, got required {required:?}",
        tool["name"].as_str().unwrap()
    );
}

fn assert_public_geo_projection(value: &Value) {
    assert_eq!(value["lat"].as_f64(), Some(0.0));
    assert_eq!(value["lng"].as_f64(), Some(0.0));
    assert_eq!(value["coordinate_source"].as_str(), Some("derived"));
}

mod collective;
mod direct_messages;
mod discovery;
mod friend_requests;
mod friendships;
mod hives;
mod identity;
mod missions;
mod payments;
mod protocol;
mod receipts;
mod schemas;
mod servicenet;
