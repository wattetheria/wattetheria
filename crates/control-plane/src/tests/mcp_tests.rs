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

#[derive(Clone)]
struct PaymentBindingFixture {
    agent_did: String,
    payment_address: String,
    binding: Value,
}

fn alpha_servicenet_settlement() -> Value {
    json!({
        "layer": "web3",
        "rail": "x402",
        "request": {
            "settlement_receipt": alpha_x402_settlement_receipt()
        }
    })
}

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

fn build_payment_binding_fixture(network: &str) -> PaymentBindingFixture {
    let mut keystore = InMemoryKeyStore::new();
    let agent_info = keystore.generate_ed25519().expect("ed25519 key");
    let payment_info = keystore.generate_secp256k1().expect("secp256k1 key");
    let proof = build_payment_account_binding_proof(
        &keystore,
        PaymentAccountBindingProofOptions {
            agent_did: agent_info.did.clone(),
            agent_key_handle: &agent_info.key_handle,
            agent_public_key_multibase: agent_info.public_key_multibase.clone(),
            rail: "x402".to_string(),
            network: Some(network.to_string()),
            custody: PaymentAccountCustody::LocalGenerated,
            receive_only: false,
            can_sign: true,
            capabilities: vec!["payment.receive".to_string()],
            issued_at_ms: 1_716_120_000_000,
            expires_at_ms: None,
            nonce: None,
            payment_signer: Some(PaymentAccountSigner {
                key_handle: &payment_info.key_handle,
                public_key_multibase: payment_info.public_key_multibase.clone(),
            }),
            watch_only_payment_address: None,
        },
    )
    .expect("build payment account binding");
    PaymentBindingFixture {
        agent_did: proof.agent_did.to_string(),
        payment_address: proof.payment_address.clone(),
        binding: serde_json::to_value(proof).expect("serialize payment account binding"),
    }
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

async fn spawn_servicenet_with_binding_payment(
    payment_binding: PaymentBindingFixture,
    static_pay_to: &'static str,
) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    let agent = json!({
        "agent_id": "agent-binding",
        "service_address": "binding@wattetheria",
        "provider_id": "provider-binding",
        "version": "0.1.0",
        "status": "approved",
        "agent_card": {
            "name": "Binding Agent",
            "description": "Agent with static x402 and wallet binding",
            "cost": 1,
            "currency": "USDC",
            "supportsTask": false,
            "didDocument": {
                "id": payment_binding.agent_did,
                "payment_account_binding": payment_binding.binding,
            },
            "capabilities": {
                "extensions": [{
                    "uri": "https://github.com/google-a2a/a2a-x402/v0.1",
                    "required": false,
                    "description": "Supports x402 payments for ServiceNet invocation.",
                    "params": {
                        "accepts": [{
                            "scheme": "exact",
                            "network": "base",
                            "payTo": static_pay_to,
                            "maxAmountRequired": "1000000",
                            "resource": "servicenet:agent:binding-agent",
                            "description": "ServiceNet agent invocation",
                            "maxTimeoutSeconds": 600
                        }]
                    }
                }]
            },
            "skills": [{"name": "Charge", "description": "Charges the caller"}],
            "securitySchemes": {"none": {"type": "none"}},
            "security": [{"none": []}]
        },
        "deployment": {
            "runtime": "agent",
            "endpoint": {
                "url": "https://binding.example.com/a2a",
                "interaction_protocol": "google_a2a",
                "protocol_binding": "JSONRPC"
            }
        },
        "review": {"risk_level": "low"}
    });
    let app = axum::Router::new()
        .route(
            "/v1/agents",
            axum::routing::get({
                let agent = agent.clone();
                move || {
                    let agent = agent.clone();
                    async move {
                        Json(json!({
                            "items": [agent],
                            "count": 1,
                            "limit": 50,
                            "offset": 0,
                            "has_more": false,
                            "known_count": 1
                        }))
                    }
                }
            }),
        )
        .route(
            "/v1/agents/{agent_id}",
            axum::routing::get(move |Path(agent_id): Path<String>| {
                let agent = agent.clone();
                async move {
                    assert_eq!(agent_id, "agent-binding");
                    Json(agent)
                }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (addr, server)
}

const MCP_AGENT_TOOL_NAMES: &[&str] = &[
    "list_agent_payments",
    "get_agent_payment",
    "propose_agent_payment",
    "authorize_agent_payment",
    "submit_agent_payment",
    "settle_agent_payment",
    "reject_agent_payment",
    "cancel_agent_payment",
    "list_hives",
    "list_private_hives",
    "create_hive",
    "create_private_hive",
    "list_hive_messages",
    "post_hive_message",
    "subscribe_hive",
    "unsubscribe_hive",
    "invite_private_hive_participant",
    "list_missions",
    "publish_mission",
    "publish_delegated_mission",
    "publish_collective_mission",
    "start_collective_mission",
    "get_collective_mission_result",
    "claim_mission",
    "complete_mission",
    "settle_mission",
    "list_friends",
    "list_nearby",
    "search_agents",
    "get_agent_card",
    "list_friend_requests",
    "list_sent_friend_requests",
    "get_friend_request",
    "accept_friend_request",
    "reject_friend_request",
    "request_agent_friend",
    "remove_agent_friend",
    "list_agent_dm_threads",
    "list_agent_dm_messages",
    "send_agent_dm_message",
    "list_servicenet_agents",
    "get_servicenet_agent",
    "delete_servicenet_agent",
    "invoke_servicenet_agent_sync",
    "invoke_servicenet_agent_async",
    "get_servicenet_agent_task",
    "get_servicenet_receipt",
];

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

#[tokio::test]
async fn mcp_tools_list_matches_expected_agent_tool_surface() {
    let (_dir, app, token, _policy, _state) = build_test_app(100);

    let response = mcp_request(
        app.clone(),
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/list"
        }),
    )
    .await;

    let mut actual = response["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    actual.sort();
    let mut expected = MCP_AGENT_TOOL_NAMES
        .iter()
        .map(|name| (*name).to_string())
        .collect::<Vec<_>>();
    expected.sort();

    assert_eq!(actual, expected);
    assert!(!actual.iter().any(|name| name == "client_export"));
    assert!(!actual.iter().any(|name| name == "client_task_activity"));
}

#[tokio::test]
async fn mcp_success_records_contribution_reward_event() {
    let (_dir, app, token, _policy, state) = build_test_app(100);

    let response = mcp_request(
        app.clone(),
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "client_export",
                "arguments": {}
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let log: wattetheria_kernel::economy::ContributionEventLog = state
        .local_db
        .load_domain_or_default(wattetheria_kernel::local_db::domain::CONTRIBUTION_EVENT_LOG)
        .unwrap();
    let event = log
        .events
        .values()
        .find(|event| event.action_type == "mcp.tool.success")
        .unwrap();
    assert_eq!(event.receipt["tool_name"].as_str(), Some("client_export"));

    let balances: wattetheria_kernel::economy::WalletBalanceState = state
        .local_db
        .load_domain_or_default(wattetheria_kernel::local_db::domain::WATT_BALANCE_STATE)
        .unwrap();
    let balance = balances
        .get(&event.controller_id, event.public_id.as_deref())
        .unwrap();
    assert_eq!(balance.watt_balance, 1);
}

#[tokio::test]
async fn mcp_success_receipt_redacts_sensitive_arguments_and_results() {
    let (servicenet_addr, servicenet_server) = spawn_mock_servicenet().await;
    let (_dir, _app, token, _policy, state) = build_test_app(100);
    let state = ControlPlaneState {
        servicenet_client: Some(Arc::new(
            ServiceNetClient::new(format!("http://{servicenet_addr}")).unwrap(),
        )),
        ..state
    };
    let app = app(state.clone());

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "invoke_servicenet_agent_sync",
                "arguments": {
                    "service_address": "alpha@wattetheria",
                    "message": "hello servicenet",
                    "auth_token": "servicenet-secret-token",
                    "auth_context_id": "00000000-0000-0000-0000-00000000abcd",
                    "input": {
                        "api_key": "input-api-secret",
                        "safe_value": "kept"
                    },
                    "settlement": {
                        "layer": "web3",
                        "rail": "x402",
                        "request": {
                            "settlement_receipt": alpha_x402_settlement_receipt(),
                            "client_secret": "settlement-client-secret",
                            "payment_account_ref": "payment-account-123"
                        }
                    }
                }
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let log: wattetheria_kernel::economy::ContributionEventLog = state
        .local_db
        .load_domain_or_default(wattetheria_kernel::local_db::domain::CONTRIBUTION_EVENT_LOG)
        .unwrap();
    let event = log
        .events
        .values()
        .find(|event| event.action_type == "servicenet.agent.invoke.success")
        .unwrap();
    let receipt = &event.receipt;
    assert_eq!(
        receipt["arguments"]["auth_token"].as_str(),
        Some("[REDACTED]")
    );
    assert_eq!(
        receipt["arguments"]["auth_context_id"].as_str(),
        Some("[REDACTED]")
    );
    assert_eq!(
        receipt["arguments"]["input"]["api_key"].as_str(),
        Some("[REDACTED]")
    );
    assert_eq!(
        receipt["arguments"]["input"]["safe_value"].as_str(),
        Some("kept")
    );
    assert_eq!(
        receipt["arguments"]["settlement"]["request"]["client_secret"].as_str(),
        Some("[REDACTED]")
    );
    assert_eq!(
        receipt["result"]["structuredContent"]["settlement"]["request"]["client_secret"].as_str(),
        Some("[REDACTED]")
    );
    assert_eq!(
        receipt["result"]["structuredContent"]["settlement"]["request"]["payment_account_ref"]
            .as_str(),
        Some("payment-account-123")
    );
    let receipt_json = serde_json::to_string(receipt).unwrap();
    assert!(!receipt_json.contains("servicenet-secret-token"));
    assert!(!receipt_json.contains("00000000-0000-0000-0000-00000000abcd"));
    assert!(!receipt_json.contains("input-api-secret"));
    assert!(!receipt_json.contains("settlement-client-secret"));

    servicenet_server.abort();
}

#[tokio::test]
async fn mcp_tools_list_surfaces_tool_availability_metadata() {
    let (_dir, _app, token, _policy, state) = build_test_app(100);
    let state = ControlPlaneState {
        agent_topic_bridge_enabled: false,
        ..state
    };
    let app = app(state.clone());

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/list"
        }),
    )
    .await;
    let tools = response["result"]["tools"].as_array().unwrap();
    let create_hive = tools
        .iter()
        .find(|tool| tool["name"].as_str() == Some("create_hive"))
        .unwrap();
    let list_hives = tools
        .iter()
        .find(|tool| tool["name"].as_str() == Some("list_hives"))
        .unwrap();
    let servicenet = tools
        .iter()
        .find(|tool| tool["name"].as_str() == Some("list_servicenet_agents"))
        .unwrap();

    assert_eq!(
        create_hive["_meta"]["wattetheria"]["available"].as_bool(),
        Some(false)
    );
    assert_eq!(
        list_hives["_meta"]["wattetheria"]["available"].as_bool(),
        Some(true)
    );
    assert_eq!(
        servicenet["_meta"]["wattetheria"]["available"].as_bool(),
        Some(false)
    );
    assert_eq!(
        servicenet["_meta"]["wattetheria"]["path"].as_str(),
        Some("/v1/wattetheria/servicenet/agents")
    );
    assert_eq!(
        list_hives["_meta"]["wattetheria"]["readOnly"].as_bool(),
        Some(true)
    );
    assert_eq!(
        create_hive["_meta"]["wattetheria"]["readOnly"].as_bool(),
        Some(false)
    );
}

#[tokio::test]
async fn mcp_list_servicenet_agents_reads_configured_servicenet() {
    let (servicenet_addr, servicenet_server) = spawn_mock_servicenet().await;
    let (_dir, _app, token, _policy, state) = build_test_app(100);
    let state = ControlPlaneState {
        servicenet_client: Some(Arc::new(
            ServiceNetClient::new(format!("http://{servicenet_addr}")).unwrap(),
        )),
        ..state
    };
    let app = app(state.clone());

    let response = mcp_request(
        app.clone(),
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "list_servicenet_agents",
                "arguments": {
                    "limit": 1,
                    "offset": 1
                }
            }
        }),
    )
    .await;

    assert_eq!(response["jsonrpc"].as_str(), Some("2.0"));
    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let content = &response["result"]["structuredContent"];
    assert_eq!(content["count"].as_u64(), Some(1));
    assert_eq!(content["limit"].as_u64(), Some(1));
    assert_eq!(content["offset"].as_u64(), Some(1));
    assert_eq!(content["next_offset"].as_u64(), Some(2));
    assert_eq!(content["has_more"].as_bool(), Some(true));
    assert_eq!(content["known_count"].as_u64(), Some(3));
    let agents = content["items"].as_array().unwrap();
    assert_eq!(agents.len(), 1);
    let beta = &agents[0];
    assert_eq!(beta["service_address"].as_str(), Some("beta@wattetheria"));
    assert_eq!(beta["name"].as_str(), Some("Agent Beta"));
    assert_eq!(beta["description"].as_str(), Some("Beta test agent"));
    assert_eq!(beta["status"].as_str(), Some("online"));
    assert_eq!(beta["version"].as_str(), Some("0.2.0"));
    assert_eq!(beta["provider_id"].as_str(), Some("provider-two"));
    assert_eq!(beta["runtime"].as_str(), Some("agent"));
    assert_eq!(beta["protocol"].as_str(), Some("google_a2a / JSONRPC"));
    assert!(beta.get("url").is_none());
    assert_eq!(beta["risk_level"].as_str(), Some("medium"));
    assert_eq!(beta["reputation_score"].as_f64(), Some(500.0));
    assert_eq!(beta["cost"].as_u64(), Some(7));
    assert_eq!(beta["currency"].as_str(), Some("USDT"));
    assert!(beta.get("skills").is_none());

    servicenet_server.abort();
}

#[tokio::test]
async fn mcp_get_servicenet_agent_returns_enriched_summary() {
    let (servicenet_addr, servicenet_server) = spawn_mock_servicenet().await;
    let (_dir, _app, token, _policy, state) = build_test_app(100);
    let state = ControlPlaneState {
        servicenet_client: Some(Arc::new(
            ServiceNetClient::new(format!("http://{servicenet_addr}")).unwrap(),
        )),
        ..state
    };
    let app = app(state);

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "get_servicenet_agent",
                "arguments": {
                    "service_address": "alpha@wattetheria"
                }
            }
        }),
    )
    .await;

    assert_eq!(response["jsonrpc"].as_str(), Some("2.0"));
    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let agent = &response["result"]["structuredContent"];
    assert_eq!(agent["service_address"].as_str(), Some("alpha@wattetheria"));
    assert_eq!(agent["name"].as_str(), Some("Agent Alpha"));
    assert_eq!(agent["description"].as_str(), Some("Alpha test agent"));
    assert_eq!(agent["status"].as_str(), Some("published"));
    assert_eq!(agent["version"].as_str(), Some("0.1.0"));
    assert_eq!(agent["provider_id"].as_str(), Some("provider-one"));
    assert_eq!(agent["runtime"].as_str(), Some("agent"));
    assert_eq!(agent["protocol"].as_str(), Some("google_a2a / JSONRPC"));
    assert!(agent.get("url").is_none());
    assert_eq!(agent["risk_level"].as_str(), Some("low"));
    assert_eq!(agent["reputation_score"].as_f64(), Some(750.0));
    assert_eq!(agent["cost"].as_u64(), Some(18));
    assert_eq!(agent["currency"].as_str(), Some("USDC"));
    assert_eq!(agent["supportsTask"].as_bool(), Some(true));
    assert_eq!(
        agent["payment"]["params"]["accepts"][0]["payTo"].as_str(),
        Some("0x742d35Cc6634C0532925a3b844Bc454e4438f44e")
    );
    assert_eq!(
        agent["skills"],
        json!([
            {
                "name": "Get weather",
                "description": "Returns current weather"
            }
        ])
    );

    servicenet_server.abort();
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn mcp_propose_agent_payment_accepts_servicenet_service_address() {
    let (servicenet_addr, servicenet_server) = spawn_mock_servicenet().await;
    let (_dir, _app, token, _policy, state) = build_test_app(100);
    let sender_address = seed_active_payment_account(&state);
    let state = ControlPlaneState {
        servicenet_client: Some(Arc::new(
            ServiceNetClient::new(format!("http://{servicenet_addr}")).unwrap(),
        )),
        ..state
    };
    let app = app(state);

    let response = mcp_request(
        app.clone(),
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "propose_agent_payment",
                "arguments": {
                    "target_kind": "service_agent",
                    "target_address": "alpha@wattetheria",
                    "amount": "0.18",
                    "currency": "USDC",
                    "rail": "x402",
                    "layer": "web3"
                }
            }
        }),
    )
    .await;

    assert_eq!(response["jsonrpc"].as_str(), Some("2.0"));
    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let content = &response["result"]["structuredContent"];
    assert_eq!(content["ok"].as_bool(), Some(true));
    assert_eq!(
        content["payment"]["recipient_public_id"].as_str(),
        Some("agent-alpha")
    );
    assert_eq!(
        content["payment"]["recipient_address"].as_str(),
        Some("0x742d35Cc6634C0532925a3b844Bc454e4438f44e")
    );
    assert_eq!(content["payment"]["amount"].as_str(), Some("0.18"));
    assert_eq!(content["payment"]["network"].as_str(), Some("base"));
    assert_eq!(content["transport"]["mode"].as_str(), Some("servicenet"));
    assert_eq!(
        content["transport"]["agent_id"].as_str(),
        Some("agent-alpha")
    );
    let payment_id = content["payment"]["payment_id"].as_str().unwrap();

    let authorized = mcp_request(
        app.clone(),
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "authorize_agent_payment",
                "arguments": {
                    "payment_id": payment_id
                }
            }
        }),
    )
    .await;
    assert_eq!(authorized["result"]["isError"].as_bool(), Some(false));

    let submitted = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "submit_agent_payment",
                "arguments": {
                    "payment_id": payment_id,
                    "settlement_receipt": {
                        "success": true,
                        "payer": sender_address,
                        "transaction": "0x89c91c789e57059b17285e7ba1716a1f5ff4c5dace0ea5a5135f26158d0421b9",
                        "network": "base",
                        "amount": "180000",
                        "payTo": "0x742d35Cc6634C0532925a3b844Bc454e4438f44e"
                    }
                }
            }
        }),
    )
    .await;
    assert_eq!(submitted["result"]["isError"].as_bool(), Some(false));
    assert_eq!(
        submitted["result"]["structuredContent"]["status"].as_str(),
        Some("submitted")
    );
    assert_eq!(
        submitted["result"]["structuredContent"]["amount"].as_str(),
        Some("0.18")
    );
    assert_eq!(
        submitted["result"]["structuredContent"]["settlement_receipt"]["amount"].as_str(),
        Some("0.18")
    );

    servicenet_server.abort();
}

#[tokio::test]
async fn mcp_propose_agent_payment_prefers_servicenet_binding_over_static_pay_to() {
    let payment_binding = build_payment_binding_fixture("base");
    let static_pay_to = "0x742d35Cc6634C0532925a3b844Bc454e4438f44e";
    let (servicenet_addr, servicenet_server) =
        spawn_servicenet_with_binding_payment(payment_binding.clone(), static_pay_to).await;
    let (_dir, _app, token, _policy, state) = build_test_app(100);
    let state = ControlPlaneState {
        servicenet_client: Some(Arc::new(
            ServiceNetClient::new(format!("http://{servicenet_addr}")).unwrap(),
        )),
        ..state
    };
    let app = app(state);

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "propose_agent_payment",
                "arguments": {
                    "target_kind": "service_agent",
                    "target_address": "binding@wattetheria",
                    "amount": "1",
                    "currency": "USDC",
                    "rail": "x402",
                    "layer": "web3",
                    "network": "base"
                }
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let payment = &response["result"]["structuredContent"]["payment"];
    assert_eq!(
        payment["recipient_address"].as_str(),
        Some(payment_binding.payment_address.as_str())
    );
    assert_ne!(payment["recipient_address"].as_str(), Some(static_pay_to));
    assert_eq!(payment["network"].as_str(), Some("base"));

    servicenet_server.abort();
}

#[tokio::test]
async fn mcp_propose_agent_payment_rejects_payment_address_target_kind() {
    let (_dir, app, token, _policy, _state) = build_test_app(100);
    let recipient_address = "0x742d35Cc6634C0532925a3b844Bc454e4438f44e";

    let response = mcp_request(
        app.clone(),
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "propose_agent_payment",
                "arguments": {
                    "target_kind": "payment_address",
                    "target_address": recipient_address,
                    "amount": "2",
                    "currency": "USDC",
                    "rail": "x402",
                    "layer": "web3",
                    "network": "base"
                }
            }
        }),
    )
    .await;

    assert_eq!(response["jsonrpc"].as_str(), Some("2.0"));
    assert_eq!(response["result"]["isError"].as_bool(), Some(true));
    assert_eq!(
        response["result"]["structuredContent"]["error"].as_str(),
        Some("target_kind must be network_agent or service_agent")
    );
}

#[tokio::test]
async fn mcp_list_agent_payments_rejects_payment_address_target_kind() {
    let (_dir, app, token, _policy, _state) = build_test_app(100);

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "list_agent_payments",
                "arguments": {
                    "target_kind": "payment_address",
                    "target_address": "0x742d35Cc6634C0532925a3b844Bc454e4438f44e"
                }
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"].as_bool(), Some(true));
    assert_eq!(
        response["result"]["structuredContent"]["error"].as_str(),
        Some("target_kind must be network_agent or service_agent")
    );
}

#[tokio::test]
async fn mcp_propose_agent_payment_normalizes_stablecoin_amount_for_counterpart() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let payment_binding = build_payment_binding_fixture("base");
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge::default_for(identity.agent_did.clone()));
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, app, token, _policy, state) =
        build_test_app_with_bridge(100, dir, identity.clone(), event_log, bridge_handle);
    let _local_public_id =
        bootstrap_broker_identity(app.clone(), &token, &identity.agent_did).await;
    let remote_public_id = scoped_id("broker-stable", &payment_binding.agent_did);
    {
        let mut identities = state.public_identity_registry.lock().await;
        identities
            .upsert(
                &remote_public_id,
                "Broker Stable".to_string(),
                Some(payment_binding.agent_did.clone()),
                true,
            )
            .unwrap();
    }
    {
        let mut bindings = state.controller_binding_registry.lock().await;
        bindings.upsert(
            &remote_public_id,
            wattetheria_kernel::civilization::identities::ControllerKind::ExternalRuntime,
            "remote-runtime".to_string(),
            Some("12D3KooStablePeer".to_string()),
            wattetheria_kernel::civilization::identities::OwnershipScope::External,
            true,
        );
    }
    wattetheria_social::application::remote_identity_service::upsert_remote_identity(
        &*state.social_store,
        &wattetheria_social::domain::identities::RemoteIdentityProfile {
            public_id: remote_public_id.clone(),
            agent_did: payment_binding.agent_did.clone(),
            display_name: "Broker Stable".to_string(),
            description: None,
            capabilities: Vec::new(),
            skills: Vec::new(),
            did_document_json: Some(json!({
                "id": payment_binding.agent_did,
                "payment_account_binding": payment_binding.binding,
            })),
            active: true,
            last_profile_fetched_at: Some(1),
            created_at: 1,
            updated_at: 1,
        },
    )
    .expect("seed remote identity");

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "propose_agent_payment",
                "arguments": {
                    "target_kind": "network_agent",
                    "target_address": remote_public_id,
                    "amount": "1",
                    "currency": "USDT",
                    "rail": "x402",
                    "layer": "web3",
                    "network": "base"
                }
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let content = &response["result"]["structuredContent"];
    assert_eq!(content["payment"]["amount"].as_str(), Some("1"));
    let payment_commands = bridge.payment_commands.lock().await;
    assert_eq!(payment_commands.len(), 1);
    assert_eq!(
        payment_commands[0].payment["amount"].as_str(),
        Some("1000000")
    );
}

#[tokio::test]
async fn mcp_propose_agent_payment_rejects_network_agent_without_verified_payment_address() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let remote_identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge::default_for(identity.agent_did.clone()));
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, app, token, _policy, state) =
        build_test_app_with_bridge(100, dir, identity.clone(), event_log, bridge_handle);
    let _local_public_id =
        bootstrap_broker_identity(app.clone(), &token, &identity.agent_did).await;
    let remote_public_id = scoped_id("broker-no-pay", &remote_identity.agent_did);
    {
        let mut identities = state.public_identity_registry.lock().await;
        identities
            .upsert(
                &remote_public_id,
                "Broker No Pay".to_string(),
                Some(remote_identity.agent_did.clone()),
                true,
            )
            .unwrap();
    }
    {
        let mut bindings = state.controller_binding_registry.lock().await;
        bindings.upsert(
            &remote_public_id,
            wattetheria_kernel::civilization::identities::ControllerKind::ExternalRuntime,
            "remote-runtime".to_string(),
            Some("12D3KooNoPayPeer".to_string()),
            wattetheria_kernel::civilization::identities::OwnershipScope::External,
            true,
        );
    }

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "propose_agent_payment",
                "arguments": {
                    "target_kind": "network_agent",
                    "target_address": remote_public_id,
                    "amount": "1",
                    "currency": "USDC",
                    "rail": "x402",
                    "layer": "web3",
                    "network": "base"
                }
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"].as_bool(), Some(true));
    assert!(
        response["result"]["structuredContent"]["error"]
            .as_str()
            .is_some_and(|error| error.contains("has no verified payment address"))
    );
    let payment_commands = bridge.payment_commands.lock().await;
    assert!(payment_commands.is_empty());
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn mcp_agent_payments_support_network_agent_target_address() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let payment_binding = build_payment_binding_fixture("base");
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge::default_for(identity.agent_did.clone()));
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, app, token, _policy, state) =
        build_test_app_with_bridge(100, dir, identity.clone(), event_log, bridge_handle);
    bootstrap_broker_identity(app.clone(), &token, &identity.agent_did).await;
    let context = crate::routes::identity::resolve_identity_context(&state, None, None).await;
    let local_public_id = context
        .public_memory_owner
        .public
        .unwrap_or(context.public_memory_owner.controller);
    let remote_public_id = scoped_id("broker-payments", &payment_binding.agent_did);
    {
        let mut identities = state.public_identity_registry.lock().await;
        identities
            .upsert(
                &remote_public_id,
                "Broker Payments".to_string(),
                Some(payment_binding.agent_did.clone()),
                true,
            )
            .unwrap();
    }
    {
        let mut bindings = state.controller_binding_registry.lock().await;
        bindings.upsert(
            &remote_public_id,
            wattetheria_kernel::civilization::identities::ControllerKind::ExternalRuntime,
            "remote-runtime".to_string(),
            Some("12D3KooPaymentPeer".to_string()),
            wattetheria_kernel::civilization::identities::OwnershipScope::External,
            true,
        );
    }
    wattetheria_social::application::remote_identity_service::upsert_remote_identity(
        &*state.social_store,
        &wattetheria_social::domain::identities::RemoteIdentityProfile {
            public_id: remote_public_id.clone(),
            agent_did: payment_binding.agent_did.clone(),
            display_name: "Broker Payments".to_string(),
            description: None,
            capabilities: Vec::new(),
            skills: Vec::new(),
            did_document_json: Some(json!({
                "id": payment_binding.agent_did,
                "payment_account_binding": payment_binding.binding,
            })),
            active: true,
            last_profile_fetched_at: Some(1),
            created_at: 1,
            updated_at: 1,
        },
    )
    .expect("seed remote identity");
    wattetheria_social::application::transport_binding_service::upsert_transport_binding(
        &*state.social_store,
        &wattetheria_social::domain::transport_bindings::RemoteTransportBinding {
            public_id: remote_public_id.clone(),
            agent_did: Some(payment_binding.agent_did.clone()),
            transport_kind:
                wattetheria_social::domain::transport_bindings::TransportKind::Wattswarm,
            transport_node_id: "12D3KooPaymentPeer".to_string(),
            binding_source: "friendship".to_string(),
            binding_confidence: 90,
            binding_proof_json: None,
            binding_verified: true,
            binding_verified_at: Some(1),
            updated_at: 1,
        },
    )
    .expect("seed remote transport binding");
    friendship_service::upsert_friendship(
        &*state.social_store,
        &wattetheria_social::domain::friendships::Friendship {
            friendship_id: format!("friendship:{local_public_id}:{remote_public_id}"),
            local_public_id: local_public_id.clone(),
            remote_public_id: remote_public_id.clone(),
            display_name: Some("Broker Payments".to_string()),
            state: wattetheria_social::domain::friendships::FriendshipState::Active,
            established_from_request_id: None,
            thread_id: None,
            created_at: 1,
            updated_at: 1,
        },
    )
    .expect("seed active friendship");

    let proposed = mcp_request(
        app.clone(),
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "propose_agent_payment",
                "arguments": {
                    "target_kind": "network_agent",
                    "target_address": remote_public_id,
                    "amount": "2.50",
                    "currency": "USDC",
                    "rail": "x402",
                    "layer": "web3",
                    "network": "base"
                }
            }
        }),
    )
    .await;

    assert_eq!(proposed["result"]["isError"].as_bool(), Some(false));
    let payment = &proposed["result"]["structuredContent"]["payment"];
    assert_eq!(
        payment["recipient_public_id"].as_str(),
        Some(remote_public_id.as_str())
    );
    assert_eq!(
        payment["recipient_display_name"].as_str(),
        Some("Broker Payments")
    );
    assert_eq!(
        payment["counterpart_display_name"].as_str(),
        Some("Broker Payments")
    );
    assert_eq!(
        payment["recipient_address"].as_str(),
        Some(payment_binding.payment_address.as_str())
    );
    let payment_id = payment["payment_id"].as_str().unwrap();
    let payment_commands = bridge.payment_commands.lock().await;
    assert_eq!(payment_commands.len(), 1);
    assert_eq!(payment_commands[0].remote_node_id, "12D3KooPaymentPeer");
    assert_eq!(
        payment_commands[0].payment["recipient_address"].as_str(),
        Some(payment_binding.payment_address.as_str())
    );
    drop(payment_commands);

    let listed = mcp_request(
        app.clone(),
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "list_agent_payments",
                "arguments": {
                    "target_kind": "network_agent",
                    "target_address": remote_public_id
                }
            }
        }),
    )
    .await;
    assert_eq!(listed["result"]["isError"].as_bool(), Some(false));
    assert_eq!(
        listed["result"]["structuredContent"]["count"].as_u64(),
        Some(1)
    );
    assert_eq!(
        listed["result"]["structuredContent"]["items"][0]["counterpart_display_name"].as_str(),
        Some("Broker Payments")
    );

    let fetched = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "get_agent_payment",
                "arguments": {
                    "payment_id": payment_id
                }
            }
        }),
    )
    .await;
    assert_eq!(fetched["result"]["isError"].as_bool(), Some(false));
    assert_eq!(
        fetched["result"]["structuredContent"]["counterpart_display_name"].as_str(),
        Some("Broker Payments")
    );
}

#[tokio::test]
async fn mcp_invoke_servicenet_agent_sync_rejects_paid_agent_without_settlement_receipt() {
    let (servicenet_addr, servicenet_server) = spawn_mock_servicenet().await;
    let (_dir, _app, token, _policy, state) = build_test_app(100);
    let state = ControlPlaneState {
        servicenet_client: Some(Arc::new(
            ServiceNetClient::new(format!("http://{servicenet_addr}")).unwrap(),
        )),
        ..state
    };
    let app = app(state);

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "invoke_servicenet_agent_sync",
                "arguments": {
                    "service_address": "alpha@wattetheria",
                    "message": "hello without payment proof"
                }
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"].as_bool(), Some(true));
    assert!(
        response["result"]["structuredContent"]["error"]
            .as_str()
            .unwrap()
            .contains("requires x402 settlement_receipt")
    );

    servicenet_server.abort();
}

#[tokio::test]
async fn mcp_invoke_servicenet_agent_sync_attaches_agent_envelope_for_public_agent() {
    let (servicenet_addr, servicenet_server) = spawn_mock_servicenet().await;
    let callback_events = Arc::new(Mutex::new(Vec::<Value>::new()));
    let callback_app = Router::new().route(
        "/agent-events",
        post({
            let callback_events = Arc::clone(&callback_events);
            move |Json(payload): Json<Value>| {
                let callback_events = Arc::clone(&callback_events);
                async move {
                    callback_events.lock().await.push(payload);
                    Json(json!({"ok": true, "acked_at": 1}))
                }
            }
        }),
    );
    let callback_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let callback_addr = callback_listener.local_addr().unwrap();
    let callback_server = tokio::spawn(async move {
        axum::serve(callback_listener, callback_app).await.unwrap();
    });
    let (_dir, _app, token, _policy, state) = build_test_app(100);
    let agent_did = state.agent_did.clone();
    let expected_public_id = state
        .public_identity_registry
        .lock()
        .await
        .active_for_agent_did(&agent_did)
        .expect("default public identity should exist")
        .public_id;
    let state = ControlPlaneState {
        servicenet_client: Some(Arc::new(
            ServiceNetClient::new(format!("http://{servicenet_addr}")).unwrap(),
        )),
        agent_event_callback_base_url: Some(format!("http://{callback_addr}")),
        ..state
    };
    let app = app(state);

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "invoke_servicenet_agent_sync",
                "arguments": {
                    "service_address": "alpha@wattetheria",
                    "message": "hello servicenet",
                    "settlement": alpha_servicenet_settlement()
                }
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let content = &response["result"]["structuredContent"];
    assert_eq!(content["status"].as_str(), Some("completed"));
    assert_eq!(
        content["output"]["agent_envelope_source"].as_str(),
        Some(agent_did.as_str())
    );
    assert_eq!(
        content["output"]["caller_public_id"].as_str(),
        Some(expected_public_id.as_str())
    );
    let callback_events = callback_events.lock().await;
    assert_eq!(callback_events.len(), 1);
    assert_eq!(
        callback_events[0]["event"]["payload"]["operation"].as_str(),
        Some("invoke")
    );
    assert_eq!(
        callback_events[0]["event"]["agent_envelope"]["source_agent_id"].as_str(),
        Some(agent_did.as_str())
    );
    assert_eq!(
        callback_events[0]["event"]["agent_envelope"]["target_agent_id"].as_str(),
        Some("agent-alpha")
    );

    callback_server.abort();
    servicenet_server.abort();
}

#[tokio::test]
async fn mcp_invoke_servicenet_agent_sync_resolves_service_address() {
    let (servicenet_addr, servicenet_server) = spawn_mock_servicenet().await;
    let (_dir, _app, token, _policy, state) = build_test_app(100);
    let state = ControlPlaneState {
        servicenet_client: Some(Arc::new(
            ServiceNetClient::new(format!("http://{servicenet_addr}")).unwrap(),
        )),
        ..state
    };
    let app = app(state);

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "invoke_servicenet_agent_sync",
                "arguments": {
                    "service_address": "alpha@wattetheria",
                    "message": "hello by service address",
                    "settlement": alpha_servicenet_settlement()
                }
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let content = &response["result"]["structuredContent"];
    assert_eq!(
        content["service_address"].as_str(),
        Some("alpha@wattetheria")
    );
    assert_eq!(
        content["output"]["echo"].as_str(),
        Some("hello by service address")
    );

    servicenet_server.abort();
}

#[tokio::test]
async fn mcp_invoke_servicenet_agent_async_returns_receipt_id() {
    let (servicenet_addr, servicenet_server) = spawn_mock_servicenet().await;
    let (_dir, _app, token, _policy, state) = build_test_app(100);
    let state = ControlPlaneState {
        servicenet_client: Some(Arc::new(
            ServiceNetClient::new(format!("http://{servicenet_addr}")).unwrap(),
        )),
        ..state
    };
    let app = app(state.clone());

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "invoke_servicenet_agent_async",
                "arguments": {
                    "service_address": "alpha@wattetheria",
                    "message": "hello servicenet",
                    "settlement": alpha_servicenet_settlement()
                }
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let content = &response["result"]["structuredContent"];
    assert_eq!(content["status"].as_str(), Some("running"));
    assert_eq!(
        content["receipt_id"].as_str(),
        Some("00000000-0000-0000-0000-000000000099")
    );
    let pending: crate::routes::servicenet::async_jobs::ServiceNetAsyncInvocationStore = state
        .local_db
        .load_domain(wattetheria_kernel::local_db::domain::SERVICENET_ASYNC_INVOCATIONS)
        .expect("load async invocation store")
        .expect("async invocation store exists");
    assert!(
        pending
            .invocations
            .contains_key("00000000-0000-0000-0000-000000000099")
    );

    servicenet_server.abort();
}

#[tokio::test]
async fn mcp_get_servicenet_receipt_returns_receipt_status() {
    let (servicenet_addr, servicenet_server) = spawn_mock_servicenet().await;
    let (_dir, _app, token, _policy, state) = build_test_app(100);
    let state = ControlPlaneState {
        servicenet_client: Some(Arc::new(
            ServiceNetClient::new(format!("http://{servicenet_addr}")).unwrap(),
        )),
        ..state
    };
    let app = app(state);

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "get_servicenet_receipt",
                "arguments": {
                    "receipt_id": "00000000-0000-0000-0000-000000000099"
                }
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let content = &response["result"]["structuredContent"];
    assert_eq!(content["receipt"]["status"].as_str(), Some("running"));
    assert_eq!(
        content["receipt"]["receipt_id"].as_str(),
        Some("00000000-0000-0000-0000-000000000099")
    );
    assert_eq!(
        content["receipt"]["service_address"].as_str(),
        Some("alpha@wattetheria")
    );
    assert!(content["receipt"].get("agent_id").is_none());

    servicenet_server.abort();
}

#[tokio::test]
async fn mcp_delete_servicenet_agent_resolves_service_address() {
    let (servicenet_addr, servicenet_server) = spawn_mock_servicenet().await;
    let (_dir, _app, token, _policy, state) = build_test_app(100);
    let state = ControlPlaneState {
        servicenet_client: Some(Arc::new(
            ServiceNetClient::new(format!("http://{servicenet_addr}")).unwrap(),
        )),
        ..state
    };
    crate::routes::servicenet::publish::save_publisher_state(
        &state.data_dir,
        &crate::routes::servicenet::publish::ServiceNetPublisherState {
            registrations: vec![
                crate::routes::servicenet::publish::ServiceNetPublisherRegistration {
                    provider_id: "provider-one".to_string(),
                    provider_did: state.agent_did.clone(),
                    agent_id: "agent-alpha".to_string(),
                    service_address: Some("alpha@wattetheria".to_string()),
                    card_hash: "sha256:agent-alpha".to_string(),
                    version: "0.1.0".to_string(),
                    updated_at: "2026-06-04T00:00:00Z".to_string(),
                    agent_card: json!({}),
                    deployment: json!({}),
                    review: json!({}),
                },
            ],
        },
    )
    .expect("save publisher state");
    let app = app(state);

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "delete_servicenet_agent",
                "arguments": {
                    "service_address": "alpha@wattetheria",
                    "reason": "retired"
                }
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let content = &response["result"]["structuredContent"];
    assert_eq!(content["status"].as_str(), Some("ok"));
    assert_eq!(
        content["service_address"].as_str(),
        Some("alpha@wattetheria")
    );
    assert!(content.get("agent_id").is_none());
    assert_eq!(content["unpublished"]["status"].as_str(), Some("revoked"));
    assert_eq!(
        content["unpublished"]["service_address"].as_str(),
        Some("alpha@wattetheria")
    );
    assert!(content["unpublished"].get("agent_id").is_none());

    servicenet_server.abort();
}

#[tokio::test]
async fn mcp_get_servicenet_agent_task_resolves_service_address() {
    let (servicenet_addr, servicenet_server) = spawn_mock_servicenet().await;
    let (_dir, _app, token, _policy, state) = build_test_app(100);
    let state = ControlPlaneState {
        servicenet_client: Some(Arc::new(
            ServiceNetClient::new(format!("http://{servicenet_addr}")).unwrap(),
        )),
        ..state
    };
    let app = app(state);

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "get_servicenet_agent_task",
                "arguments": {
                    "service_address": "alpha@wattetheria",
                    "task_id": "task-42",
                    "history_length": 3
                }
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let content = &response["result"]["structuredContent"];
    assert_eq!(content["status"].as_str(), Some("completed"));
    assert_eq!(content["task_id"].as_str(), Some("task-42"));
    assert_eq!(content["output"]["history_length"].as_u64(), Some(3));
    assert_eq!(
        content["service_address"].as_str(),
        Some("alpha@wattetheria")
    );
    assert!(content.get("agent_id").is_none());

    servicenet_server.abort();
}

#[tokio::test]
async fn mcp_invoke_servicenet_agent_sync_returns_authorization_url_when_oauth_is_required() {
    let (servicenet_addr, servicenet_server) = spawn_mock_servicenet().await;
    let (_dir, _app, token, _policy, state) = build_test_app(100);
    let state = ControlPlaneState {
        servicenet_client: Some(Arc::new(
            ServiceNetClient::new(format!("http://{servicenet_addr}")).unwrap(),
        )),
        ..state
    };
    let app = app(state);

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "invoke_servicenet_agent_sync",
                "arguments": {
                    "service_address": "oauth@wattetheria",
                    "message": "request ride"
                }
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let content = &response["result"]["structuredContent"];
    assert_eq!(content["status"].as_str(), Some("auth_required"));
    assert_eq!(
        content["service_address"].as_str(),
        Some("oauth@wattetheria")
    );
    assert_eq!(
        content["authorizationUrl"].as_str(),
        Some("https://auth.example.com/oauth/authorize")
    );
    assert_eq!(
        content["security"][0]["oauth2"][0].as_str(),
        Some("rides:request")
    );

    servicenet_server.abort();
}

#[tokio::test]
async fn mcp_tools_call_writes_product_diagnostics() {
    let (_dir, app, token, _policy, state) = build_test_app(100);

    let response = mcp_request(
        app.clone(),
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "client_export",
                "arguments": {}
            }
        }),
    )
    .await;
    assert_eq!(response["result"]["isError"].as_bool(), Some(false));

    let entries = crate::diagnostics::list_diagnostics(
        &state.data_dir,
        &crate::diagnostics::DiagnosticFilter {
            component: Some("wattetheria.mcp".to_owned()),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(
        entries
            .iter()
            .any(|entry| entry.phase == "tool.call.received"
                && entry.details["tool_name"].as_str() == Some("client_export"))
    );
    assert!(
        entries
            .iter()
            .any(|entry| entry.phase == "tool.call.succeeded"
                && entry.details["tool_name"].as_str() == Some("client_export"))
    );
}

#[tokio::test]
async fn mcp_array_payload_tools_return_object_structured_content() {
    let (_dir, app, token, _policy, _state) = build_test_app(100);

    for (id, tool_name) in [
        (1, "list_friends"),
        (2, "list_agent_dm_threads"),
        (3, "list_agent_dm_messages"),
    ] {
        let response = mcp_request(
            app.clone(),
            &token,
            json!({
                "jsonrpc": "2.0",
                "id": id,
                "method": "tools/call",
                "params": {
                    "name": tool_name,
                    "arguments": {}
                }
            }),
        )
        .await;

        assert_eq!(response["result"]["isError"].as_bool(), Some(false));
        let structured_content = &response["result"]["structuredContent"];
        assert!(structured_content.is_object(), "{tool_name}");
        assert!(structured_content["items"].is_array(), "{tool_name}");
        let text_payload: Value =
            serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap())
                .unwrap();
        assert!(text_payload.is_object(), "{tool_name}");
        assert!(text_payload["items"].is_array(), "{tool_name}");
    }
}

#[tokio::test]
async fn mcp_request_agent_friend_sends_relationship_action_to_remote_node() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge::default_for(identity.agent_did.clone()));
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, app, token, _policy, _state) =
        build_test_app_with_bridge(100, dir, identity.clone(), event_log, bridge_handle);
    let _local_public_id =
        bootstrap_broker_identity(app.clone(), &token, &identity.agent_did).await;

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "request_agent_friend",
                "arguments": {
                    "remote_node_id": "nearby-node-1",
                    "message": {
                        "kind": "friend_request",
                        "text": "hello nearby node"
                    }
                }
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let commands = bridge.relationship_commands.lock().await;
    assert_eq!(commands.len(), 1);
    let command = &commands[0];
    assert_eq!(command.remote_node_id, "nearby-node-1");
    assert_eq!(
        serde_json::to_value(&command.action).unwrap().as_str(),
        Some("request")
    );
    assert_eq!(
        command.agent_envelope.capability.as_deref(),
        Some("social.friend.request")
    );
    assert!(
        command
            .agent_envelope
            .message
            .get("source_public_id")
            .and_then(Value::as_str)
            .is_some_and(|value| !value.is_empty())
    );
    assert_eq!(
        command
            .agent_envelope
            .message
            .get("target_public_id")
            .and_then(Value::as_str),
        Some("nearby-node-1")
    );
}

#[tokio::test]
async fn mcp_request_agent_friend_rejects_overlong_message() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge::default_for(identity.agent_did.clone()));
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, app, token, _policy, _state) =
        build_test_app_with_bridge(100, dir, identity.clone(), event_log, bridge_handle);
    let _local_public_id =
        bootstrap_broker_identity(app.clone(), &token, &identity.agent_did).await;

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "request_agent_friend",
                "arguments": {
                    "remote_node_id": "nearby-node-1",
                    "message": "x".repeat(121)
                }
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"].as_bool(), Some(true));
    assert_eq!(
        response["result"]["structuredContent"]["error"].as_str(),
        Some("friend request message must be at most 120 characters")
    );
    assert_eq!(
        response["result"]["structuredContent"]["max_chars"].as_u64(),
        Some(120)
    );
    assert_eq!(
        response["result"]["structuredContent"]["actual_chars"].as_u64(),
        Some(121)
    );
    let commands = bridge.relationship_commands.lock().await;
    assert!(commands.is_empty());
}

#[tokio::test]
async fn mcp_request_agent_friend_resolves_target_agent_did_to_remote_node() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let remote_identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge::default_for(identity.agent_did.clone()));
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, app, token, _policy, state) =
        build_test_app_with_bridge(100, dir, identity.clone(), event_log, bridge_handle);
    let _local_public_id =
        bootstrap_broker_identity(app.clone(), &token, &identity.agent_did).await;
    let remote_public_id = scoped_id("broker-delta", &remote_identity.agent_did);
    {
        let mut identities = state.public_identity_registry.lock().await;
        identities
            .upsert(
                &remote_public_id,
                "Broker Delta".to_string(),
                Some(remote_identity.agent_did.clone()),
                true,
            )
            .unwrap();
    }
    {
        let mut bindings = state.controller_binding_registry.lock().await;
        bindings.upsert(
            &remote_public_id,
            wattetheria_kernel::civilization::identities::ControllerKind::ExternalRuntime,
            "remote-runtime".to_string(),
            Some("12D3KooTargetPeer".to_string()),
            wattetheria_kernel::civilization::identities::OwnershipScope::External,
            true,
        );
    }

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "request_agent_friend",
                "arguments": {
                    "target_agent_did": remote_identity.agent_did,
                    "remote_node_id": "stale-nearby-node",
                    "message": {
                        "kind": "friend_request",
                        "text": "hello known agent"
                    }
                }
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let commands = bridge.relationship_commands.lock().await;
    assert_eq!(commands.len(), 1);
    let command = &commands[0];
    assert_eq!(command.remote_node_id, "12D3KooTargetPeer");
    assert_eq!(
        command.agent_envelope.target_agent_id.as_deref(),
        Some(remote_identity.agent_did.as_str())
    );
    assert_eq!(
        command
            .agent_envelope
            .message
            .get("target_public_id")
            .and_then(Value::as_str),
        Some(remote_public_id.as_str())
    );
}

#[tokio::test]
async fn mcp_request_agent_friend_resolves_counterpart_public_id_from_discovery() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let remote_identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let remote_public_id = scoped_id("broker-discovery", &remote_identity.agent_did);
    let remote_node_id = "12D3KooDiscoveryPeer".to_string();
    let bridge = Arc::new(MockSwarmBridge {
        discovered_agents: [(
            remote_public_id.clone(),
            SwarmDiscoveredAgent {
                public_id: remote_public_id.clone(),
                remote_node_id: remote_node_id.clone(),
                target_agent_did: remote_identity.agent_did.clone(),
                display_name: Some("Broker Discovery".to_string()),
                source_agent_card: Some(discovered_source_agent_card(
                    &remote_public_id,
                    "Broker Discovery",
                    &remote_identity.agent_did,
                    &remote_node_id,
                    "broker-discovery-card",
                )),
            },
        )]
        .into_iter()
        .collect(),
        ..MockSwarmBridge::default_for(identity.agent_did.clone())
    });
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, app, token, _policy, _state) =
        build_test_app_with_bridge(100, dir, identity.clone(), event_log, bridge_handle);
    let _local_public_id =
        bootstrap_broker_identity(app.clone(), &token, &identity.agent_did).await;

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "request_agent_friend",
                "arguments": {
                    "counterpart_public_id": remote_public_id,
                    "message": {
                        "kind": "friend_request",
                        "text": "hello discovered public id"
                    }
                }
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let commands = bridge.relationship_commands.lock().await;
    assert_eq!(commands.len(), 1);
    let command = &commands[0];
    assert_eq!(command.remote_node_id, remote_node_id);
    assert_eq!(
        command.agent_envelope.target_agent_id.as_deref(),
        Some(remote_identity.agent_did.as_str())
    );
    assert_eq!(
        command
            .agent_envelope
            .message
            .get("target_public_id")
            .and_then(Value::as_str),
        Some(remote_public_id.as_str())
    );
}

#[tokio::test]
async fn mcp_request_agent_friend_resolves_display_name_from_discovery() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let remote_identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let remote_public_id = scoped_id("broker-display", &remote_identity.agent_did);
    let remote_node_id = "12D3KooDisplayPeer".to_string();
    let bridge = Arc::new(MockSwarmBridge {
        discovered_agents: [(
            remote_public_id.clone(),
            SwarmDiscoveredAgent {
                public_id: remote_public_id.clone(),
                remote_node_id: remote_node_id.clone(),
                target_agent_did: remote_identity.agent_did.clone(),
                display_name: Some("Broker Display".to_string()),
                source_agent_card: Some(discovered_source_agent_card(
                    &remote_public_id,
                    "Broker Display",
                    &remote_identity.agent_did,
                    &remote_node_id,
                    "broker-display-card",
                )),
            },
        )]
        .into_iter()
        .collect(),
        ..MockSwarmBridge::default_for(identity.agent_did.clone())
    });
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, app, token, _policy, _state) =
        build_test_app_with_bridge(100, dir, identity.clone(), event_log, bridge_handle);
    let _local_public_id =
        bootstrap_broker_identity(app.clone(), &token, &identity.agent_did).await;

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "request_agent_friend",
                "arguments": {
                    "display_name": "@Broker Display",
                    "message": {
                        "kind": "friend_request",
                        "text": "hello discovered display name"
                    }
                }
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let commands = bridge.relationship_commands.lock().await;
    assert_eq!(commands.len(), 1);
    let command = &commands[0];
    assert_eq!(command.remote_node_id, remote_node_id);
    assert_eq!(
        command.agent_envelope.target_agent_id.as_deref(),
        Some(remote_identity.agent_did.as_str())
    );
    assert_eq!(
        command
            .agent_envelope
            .message
            .get("target_public_id")
            .and_then(Value::as_str),
        Some(remote_public_id.as_str())
    );
}

#[tokio::test]
async fn mcp_request_agent_friend_rejects_display_name_before_discovery() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let remote_identity_a = Identity::new_random();
    let remote_identity_b = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let remote_public_id_a = scoped_id("broker-display-a", &remote_identity_a.agent_did);
    let remote_public_id_b = scoped_id("broker-display-b", &remote_identity_b.agent_did);
    let bridge = Arc::new(MockSwarmBridge {
        discovered_agents: [
            (
                remote_public_id_a.clone(),
                SwarmDiscoveredAgent {
                    public_id: remote_public_id_a.clone(),
                    remote_node_id: "12D3KooDisplayPeerA".to_string(),
                    target_agent_did: remote_identity_a.agent_did.clone(),
                    display_name: Some("Broker Display".to_string()),
                    source_agent_card: Some(discovered_source_agent_card(
                        &remote_public_id_a,
                        "Broker Display",
                        &remote_identity_a.agent_did,
                        "12D3KooDisplayPeerA",
                        "broker-display-a-card",
                    )),
                },
            ),
            (
                remote_public_id_b.clone(),
                SwarmDiscoveredAgent {
                    public_id: remote_public_id_b.clone(),
                    remote_node_id: "12D3KooDisplayPeerB".to_string(),
                    target_agent_did: remote_identity_b.agent_did.clone(),
                    display_name: Some("Broker Display".to_string()),
                    source_agent_card: Some(discovered_source_agent_card(
                        &remote_public_id_b,
                        "Broker Display",
                        &remote_identity_b.agent_did,
                        "12D3KooDisplayPeerB",
                        "broker-display-b-card",
                    )),
                },
            ),
        ]
        .into_iter()
        .collect(),
        ..MockSwarmBridge::default_for(identity.agent_did.clone())
    });
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, app, token, _policy, _state) =
        build_test_app_with_bridge(100, dir, identity.clone(), event_log, bridge_handle);
    let _local_public_id =
        bootstrap_broker_identity(app.clone(), &token, &identity.agent_did).await;

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "request_agent_friend",
                "arguments": {
                    "display_name": "Broker Display",
                    "message": "hello ambiguous display name"
                }
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"].as_bool(), Some(true));
    assert!(
        response["result"]["structuredContent"]["error"]
            .as_str()
            .is_some_and(|error| error.contains("multiple discovery records matched display_name"))
    );
    assert!(bridge.relationship_commands.lock().await.is_empty());
}

#[tokio::test]
async fn mcp_search_agents_resolves_public_id_from_discovery() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let remote_identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let remote_public_id = scoped_id("broker-search-public", &remote_identity.agent_did);
    let remote_node_id = "12D3KooSearchPublicPeer".to_string();
    let source_agent_card = discovered_source_agent_card(
        &remote_public_id,
        "Broker Search Public",
        &remote_identity.agent_did,
        &remote_node_id,
        "broker-search-public",
    );
    let bridge = Arc::new(MockSwarmBridge {
        discovered_agents: [(
            remote_public_id.clone(),
            SwarmDiscoveredAgent {
                public_id: remote_public_id.clone(),
                remote_node_id: remote_node_id.clone(),
                target_agent_did: remote_identity.agent_did.clone(),
                display_name: Some("Broker Search Public".to_string()),
                source_agent_card: Some(source_agent_card.clone()),
            },
        )]
        .into_iter()
        .collect(),
        ..MockSwarmBridge::default_for(identity.agent_did.clone())
    });
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, app, token, _policy, _state) =
        build_test_app_with_bridge(100, dir, identity.clone(), event_log, bridge_handle);
    let _local_public_id =
        bootstrap_broker_identity(app.clone(), &token, &identity.agent_did).await;

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "search_agents",
                "arguments": {
                    "public_id": remote_public_id.clone()
                }
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let content = &response["result"]["structuredContent"];
    assert_eq!(content["result_count"].as_u64(), Some(1));
    assert_eq!(
        content["query"]["public_id"].as_str(),
        Some(remote_public_id.as_str())
    );
    let item = &content["items"].as_array().unwrap()[0];
    assert_eq!(item["public_id"].as_str(), Some(remote_public_id.as_str()));
    assert_eq!(item["display_name"].as_str(), Some("Broker Search Public"));
    assert_eq!(
        item["remote_node_id"].as_str(),
        Some(remote_node_id.as_str())
    );
    assert_eq!(
        item["target_agent_did"].as_str(),
        Some(remote_identity.agent_did.as_str())
    );
    assert_eq!(
        item["card_hash"].as_str(),
        Some(source_agent_card.card_hash.as_str())
    );
    assert!(item.get("agent_card").is_none());
    assert!(item.get("source_agent_card").is_none());
    assert!(bridge.relationship_commands.lock().await.is_empty());
}

#[tokio::test]
async fn mcp_search_agents_returns_repeated_display_name_candidates() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let remote_identity_a = Identity::new_random();
    let remote_identity_b = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let remote_public_id_a = scoped_id("broker-search-a", &remote_identity_a.agent_did);
    let remote_public_id_b = scoped_id("broker-search-b", &remote_identity_b.agent_did);
    let bridge = Arc::new(MockSwarmBridge {
        discovered_agents: [
            (
                remote_public_id_a.clone(),
                SwarmDiscoveredAgent {
                    public_id: remote_public_id_a.clone(),
                    remote_node_id: "12D3KooSearchPeerA".to_string(),
                    target_agent_did: remote_identity_a.agent_did.clone(),
                    display_name: Some("Broker Search".to_string()),
                    source_agent_card: Some(discovered_source_agent_card(
                        &remote_public_id_a,
                        "Broker Search",
                        &remote_identity_a.agent_did,
                        "12D3KooSearchPeerA",
                        "broker-search-a",
                    )),
                },
            ),
            (
                remote_public_id_b.clone(),
                SwarmDiscoveredAgent {
                    public_id: remote_public_id_b.clone(),
                    remote_node_id: "12D3KooSearchPeerB".to_string(),
                    target_agent_did: remote_identity_b.agent_did.clone(),
                    display_name: Some("Broker Search".to_string()),
                    source_agent_card: Some(discovered_source_agent_card(
                        &remote_public_id_b,
                        "Broker Search",
                        &remote_identity_b.agent_did,
                        "12D3KooSearchPeerB",
                        "broker-search-b",
                    )),
                },
            ),
        ]
        .into_iter()
        .collect(),
        ..MockSwarmBridge::default_for(identity.agent_did.clone())
    });
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, app, token, _policy, _state) =
        build_test_app_with_bridge(100, dir, identity.clone(), event_log, bridge_handle);
    let _local_public_id =
        bootstrap_broker_identity(app.clone(), &token, &identity.agent_did).await;

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "search_agents",
                "arguments": {
                    "display_name": "@Broker Search"
                }
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let content = &response["result"]["structuredContent"];
    assert_eq!(content["result_count"].as_u64(), Some(2));
    assert_eq!(
        content["query"]["display_name"].as_str(),
        Some("Broker Search")
    );
    let items = content["items"].as_array().unwrap();
    let public_ids = items
        .iter()
        .filter_map(|item| item["public_id"].as_str())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        public_ids,
        BTreeSet::from([remote_public_id_a.as_str(), remote_public_id_b.as_str()])
    );
    assert!(items.iter().all(|item| item.get("agent_card").is_none()));
    assert!(
        items
            .iter()
            .all(|item| item.get("source_agent_card").is_none())
    );
    assert!(bridge.relationship_commands.lock().await.is_empty());
}

#[tokio::test]
async fn mcp_get_agent_card_resolves_public_id_from_discovery() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let remote_identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let remote_public_id = scoped_id("broker-card-public", &remote_identity.agent_did);
    let remote_node_id = "12D3KooCardPublicPeer".to_string();
    let source_agent_card = discovered_source_agent_card(
        &remote_public_id,
        "Broker Card Public",
        &remote_identity.agent_did,
        &remote_node_id,
        "broker-card-public",
    );
    let bridge = Arc::new(MockSwarmBridge {
        discovered_agents: [(
            remote_public_id.clone(),
            SwarmDiscoveredAgent {
                public_id: remote_public_id.clone(),
                remote_node_id: remote_node_id.clone(),
                target_agent_did: remote_identity.agent_did.clone(),
                display_name: Some("Broker Card Public".to_string()),
                source_agent_card: Some(source_agent_card.clone()),
            },
        )]
        .into_iter()
        .collect(),
        ..MockSwarmBridge::default_for(identity.agent_did.clone())
    });
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, app, token, _policy, _state) =
        build_test_app_with_bridge(100, dir, identity.clone(), event_log, bridge_handle);
    let _local_public_id =
        bootstrap_broker_identity(app.clone(), &token, &identity.agent_did).await;

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "get_agent_card",
                "arguments": {
                    "public_id": remote_public_id.clone()
                }
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let content = &response["result"]["structuredContent"];
    assert_eq!(
        content["agent_card"]["metadata"]["public_id"].as_str(),
        Some(remote_public_id.as_str())
    );
    assert_eq!(
        content["source_agent_card"]["card_hash"].as_str(),
        Some(source_agent_card.card_hash.as_str())
    );
    assert!(bridge.relationship_commands.lock().await.is_empty());
}

#[tokio::test]
async fn mcp_get_agent_card_resolves_display_name_from_discovery() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let remote_identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let remote_public_id = scoped_id("broker-card", &remote_identity.agent_did);
    let remote_node_id = "12D3KooCardPeer".to_string();
    let source_agent_card = discovered_source_agent_card(
        &remote_public_id,
        "Broker Card",
        &remote_identity.agent_did,
        &remote_node_id,
        "broker-card",
    );
    let bridge = Arc::new(MockSwarmBridge {
        discovered_agents: [(
            remote_public_id.clone(),
            SwarmDiscoveredAgent {
                public_id: remote_public_id.clone(),
                remote_node_id: remote_node_id.clone(),
                target_agent_did: remote_identity.agent_did.clone(),
                display_name: Some("Broker Card".to_string()),
                source_agent_card: Some(source_agent_card.clone()),
            },
        )]
        .into_iter()
        .collect(),
        ..MockSwarmBridge::default_for(identity.agent_did.clone())
    });
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, app, token, _policy, _state) =
        build_test_app_with_bridge(100, dir, identity.clone(), event_log, bridge_handle);
    let _local_public_id =
        bootstrap_broker_identity(app.clone(), &token, &identity.agent_did).await;

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "get_agent_card",
                "arguments": {
                    "display_name": "@Broker Card"
                }
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let content = &response["result"]["structuredContent"];
    assert_eq!(
        content["agent_card"]["metadata"]["public_id"].as_str(),
        Some(remote_public_id.as_str())
    );
    assert_eq!(
        content["source_agent_card"]["card_hash"].as_str(),
        Some(source_agent_card.card_hash.as_str())
    );
    assert!(bridge.relationship_commands.lock().await.is_empty());
}

#[tokio::test]
async fn mcp_get_agent_card_rejects_display_name_before_discovery() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let remote_identity_a = Identity::new_random();
    let remote_identity_b = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let remote_public_id_a = scoped_id("broker-card-a", &remote_identity_a.agent_did);
    let remote_public_id_b = scoped_id("broker-card-b", &remote_identity_b.agent_did);
    let bridge = Arc::new(MockSwarmBridge {
        discovered_agents: [
            (
                remote_public_id_a.clone(),
                SwarmDiscoveredAgent {
                    public_id: remote_public_id_a.clone(),
                    remote_node_id: "12D3KooCardPeerA".to_string(),
                    target_agent_did: remote_identity_a.agent_did.clone(),
                    display_name: Some("Broker Card".to_string()),
                    source_agent_card: Some(discovered_source_agent_card(
                        &remote_public_id_a,
                        "Broker Card",
                        &remote_identity_a.agent_did,
                        "12D3KooCardPeerA",
                        "broker-card-a",
                    )),
                },
            ),
            (
                remote_public_id_b.clone(),
                SwarmDiscoveredAgent {
                    public_id: remote_public_id_b.clone(),
                    remote_node_id: "12D3KooCardPeerB".to_string(),
                    target_agent_did: remote_identity_b.agent_did.clone(),
                    display_name: Some("Broker Card".to_string()),
                    source_agent_card: Some(discovered_source_agent_card(
                        &remote_public_id_b,
                        "Broker Card",
                        &remote_identity_b.agent_did,
                        "12D3KooCardPeerB",
                        "broker-card-b",
                    )),
                },
            ),
        ]
        .into_iter()
        .collect(),
        ..MockSwarmBridge::default_for(identity.agent_did.clone())
    });
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, app, token, _policy, _state) =
        build_test_app_with_bridge(100, dir, identity.clone(), event_log, bridge_handle);
    let _local_public_id =
        bootstrap_broker_identity(app.clone(), &token, &identity.agent_did).await;

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "get_agent_card",
                "arguments": {
                    "display_name": "Broker Card"
                }
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"].as_bool(), Some(true));
    let content = &response["result"]["structuredContent"];
    assert!(
        content["error"]
            .as_str()
            .is_some_and(|error| error.contains("multiple discovery records matched display_name"))
    );
    assert!(bridge.relationship_commands.lock().await.is_empty());
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn mcp_remove_agent_friend_sends_relationship_action_and_soft_deletes_friendship() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let remote_identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge::default_for(identity.agent_did.clone()));
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, app, token, _policy, state) =
        build_test_app_with_bridge(100, dir, identity.clone(), event_log, bridge_handle);
    bootstrap_broker_identity(app.clone(), &token, &identity.agent_did).await;
    let context = crate::routes::identity::resolve_identity_context(&state, None, None).await;
    let local_public_id = context
        .public_memory_owner
        .public
        .unwrap_or(context.public_memory_owner.controller);
    let remote_public_id = scoped_id("broker-remove", &remote_identity.agent_did);
    {
        let mut identities = state.public_identity_registry.lock().await;
        identities
            .upsert(
                &remote_public_id,
                "Broker Remove".to_string(),
                Some(remote_identity.agent_did.clone()),
                true,
            )
            .unwrap();
    }
    {
        let mut bindings = state.controller_binding_registry.lock().await;
        bindings.upsert(
            &remote_public_id,
            wattetheria_kernel::civilization::identities::ControllerKind::ExternalRuntime,
            "remote-runtime".to_string(),
            Some("12D3KooRemovePeer".to_string()),
            wattetheria_kernel::civilization::identities::OwnershipScope::External,
            true,
        );
    }
    friendship_service::upsert_friendship(
        &*state.social_store,
        &wattetheria_social::domain::friendships::Friendship {
            friendship_id: format!("friendship:{local_public_id}:{remote_public_id}"),
            local_public_id: local_public_id.clone(),
            remote_public_id: remote_public_id.clone(),
            display_name: Some("Broker Remove".to_string()),
            state: wattetheria_social::domain::friendships::FriendshipState::Active,
            established_from_request_id: Some("request-remove-1".to_string()),
            thread_id: Some(format!("dm:{local_public_id}:{remote_public_id}")),
            created_at: 1,
            updated_at: 1,
        },
    )
    .expect("seed active friendship");

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "remove_agent_friend",
                "arguments": {
                    "display_name": "Broker Remove",
                    "message": {
                        "kind": "friend_remove",
                        "text": "remove friend"
                    }
                }
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let commands = bridge.relationship_commands.lock().await;
    assert_eq!(commands.len(), 1);
    let command = &commands[0];
    assert_eq!(command.remote_node_id, "12D3KooRemovePeer");
    assert_eq!(
        serde_json::to_value(&command.action).unwrap().as_str(),
        Some("remove")
    );
    assert_eq!(
        command.agent_envelope.capability.as_deref(),
        Some("social.friend.remove")
    );
    assert_eq!(
        command.agent_envelope.target_agent_id.as_deref(),
        Some(remote_identity.agent_did.as_str())
    );
    assert_eq!(
        command
            .agent_envelope
            .message
            .get("source_public_id")
            .and_then(Value::as_str),
        Some(local_public_id.as_str())
    );
    assert_eq!(
        command
            .agent_envelope
            .message
            .get("target_public_id")
            .and_then(Value::as_str),
        Some(remote_public_id.as_str())
    );
    drop(commands);

    let friendships = friendship_service::list_friendships(&*state.social_store, &local_public_id)
        .expect("list friendships after remove");
    assert_eq!(friendships.len(), 1);
    assert_eq!(
        friendships[0].friendship_id,
        format!("friendship:{local_public_id}:{remote_public_id}")
    );
    assert_eq!(friendships[0].remote_public_id, remote_public_id);
    assert_eq!(
        friendships[0].state,
        wattetheria_social::domain::friendships::FriendshipState::Removed
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn mcp_send_agent_dm_message_sends_signed_direct_message_to_friend() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let remote_identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge::default_for(identity.agent_did.clone()));
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, app, token, _policy, state) =
        build_test_app_with_bridge(100, dir, identity.clone(), event_log, bridge_handle);
    bootstrap_broker_identity(app.clone(), &token, &identity.agent_did).await;
    let context = crate::routes::identity::resolve_identity_context(&state, None, None).await;
    let local_public_id = context
        .public_memory_owner
        .public
        .unwrap_or(context.public_memory_owner.controller);
    let remote_public_id = scoped_id("broker-dm", &remote_identity.agent_did);
    wattetheria_social::application::remote_identity_service::upsert_remote_identity(
        &*state.social_store,
        &wattetheria_social::domain::identities::RemoteIdentityProfile {
            public_id: remote_public_id.clone(),
            agent_did: remote_identity.agent_did.clone(),
            display_name: "Broker DM".to_string(),
            description: None,
            capabilities: Vec::new(),
            skills: Vec::new(),
            did_document_json: None,
            active: true,
            last_profile_fetched_at: Some(1),
            created_at: 1,
            updated_at: 1,
        },
    )
    .expect("seed remote identity");
    wattetheria_social::application::transport_binding_service::upsert_transport_binding(
        &*state.social_store,
        &wattetheria_social::domain::transport_bindings::RemoteTransportBinding {
            public_id: remote_public_id.clone(),
            agent_did: Some(remote_identity.agent_did.clone()),
            transport_kind:
                wattetheria_social::domain::transport_bindings::TransportKind::Wattswarm,
            transport_node_id: "12D3KooDmPeer".to_string(),
            binding_source: "friendship".to_string(),
            binding_confidence: 90,
            binding_proof_json: None,
            binding_verified: true,
            binding_verified_at: Some(1),
            updated_at: 1,
        },
    )
    .expect("seed remote transport binding");
    friendship_service::upsert_friendship(
        &*state.social_store,
        &wattetheria_social::domain::friendships::Friendship {
            friendship_id: format!("friendship:{local_public_id}:{remote_public_id}"),
            local_public_id: local_public_id.clone(),
            remote_public_id: remote_public_id.clone(),
            display_name: Some("Broker DM".to_string()),
            state: wattetheria_social::domain::friendships::FriendshipState::Active,
            established_from_request_id: None,
            thread_id: None,
            created_at: 1,
            updated_at: 1,
        },
    )
    .expect("seed active friendship");
    wattetheria_social::application::thread_service::upsert_thread(
        &*state.social_store,
        &wattetheria_social::domain::threads::DirectThread {
            thread_id: "dm:existing-ms-thread".to_string(),
            local_public_id: local_public_id.clone(),
            remote_public_id: remote_public_id.clone(),
            transport_thread_id: "dm:existing-ms-thread".to_string(),
            state: wattetheria_social::domain::threads::ThreadState::Ready,
            last_message_at: Some(1_780_801_347_838),
            created_at: 1_780_801_347_838,
            updated_at: 1_780_801_347_838,
        },
    )
    .expect("seed existing millisecond dm thread");

    let response = mcp_request(
        app.clone(),
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "send_agent_dm_message",
                "arguments": {
                    "display_name": "Broker DM",
                    "content": {
                        "type": "text",
                        "text": "hello over private group dm"
                    }
                }
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let commands = bridge.dm_commands.lock().await;
    assert_eq!(commands.len(), 1);
    let command = &commands[0];
    assert_eq!(command.remote_node_id, "12D3KooDmPeer");
    assert_eq!(
        command.agent_envelope.capability.as_deref(),
        Some("social.dm.send")
    );
    assert_eq!(
        command.content["text"].as_str(),
        Some("hello over private group dm")
    );
    let thread = wattetheria_social::application::thread_service::find_thread(
        &*state.social_store,
        &local_public_id,
        &remote_public_id,
    )
    .expect("find dm thread")
    .expect("dm thread exists");
    assert!(thread.updated_at >= thread.created_at);
    assert!(thread.updated_at >= 1_000_000_000_000);

    let friends_response = mcp_request(
        app.clone(),
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "list_friends",
                "arguments": {
                    "display_name": "Broker DM"
                }
            }
        }),
    )
    .await;
    let friends = &friends_response["result"]["structuredContent"];
    assert_eq!(friends["items"].as_array().unwrap().len(), 1);
    assert_eq!(
        friends["items"][0]["counterpart_public_id"].as_str(),
        Some(remote_public_id.as_str())
    );

    let threads_response = mcp_request(
        app.clone(),
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "list_agent_dm_threads",
                "arguments": {
                    "display_name": "Broker DM"
                }
            }
        }),
    )
    .await;
    let threads = &threads_response["result"]["structuredContent"];
    assert_eq!(threads["items"].as_array().unwrap().len(), 1);
    assert_eq!(
        threads["items"][0]["counterpart_public_id"].as_str(),
        Some(remote_public_id.as_str())
    );

    let messages_response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": {
                "name": "list_agent_dm_messages",
                "arguments": {
                    "display_name": "Broker DM"
                }
            }
        }),
    )
    .await;
    let messages = &messages_response["result"]["structuredContent"];
    assert_eq!(messages["items"].as_array().unwrap().len(), 1);
    assert_eq!(
        messages["items"][0]["counterpart_public_id"].as_str(),
        Some(remote_public_id.as_str())
    );
    assert_eq!(
        messages["items"][0]["content"]["text"].as_str(),
        Some("hello over private group dm")
    );
}

#[tokio::test]
async fn mcp_send_agent_dm_message_rejects_missing_or_ambiguous_display_name() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge::default_for(identity.agent_did.clone()));
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, app, token, _policy, state) =
        build_test_app_with_bridge(100, dir, identity.clone(), event_log, bridge_handle);
    bootstrap_broker_identity(app.clone(), &token, &identity.agent_did).await;
    let context = crate::routes::identity::resolve_identity_context(&state, None, None).await;
    let local_public_id = context
        .public_memory_owner
        .public
        .unwrap_or(context.public_memory_owner.controller);

    let missing_target_response = mcp_request(
        app.clone(),
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "send_agent_dm_message",
                "arguments": {
                    "content": {
                        "type": "text",
                        "text": "hello without target"
                    }
                }
            }
        }),
    )
    .await;
    assert_eq!(
        missing_target_response["result"]["structuredContent"]["error"].as_str(),
        Some("display_name or counterpart_public_id is required")
    );

    for remote_public_id in ["broker-duplicate-a", "broker-duplicate-b"] {
        friendship_service::upsert_friendship(
            &*state.social_store,
            &wattetheria_social::domain::friendships::Friendship {
                friendship_id: format!("friendship:{local_public_id}:{remote_public_id}"),
                local_public_id: local_public_id.clone(),
                remote_public_id: remote_public_id.to_string(),
                display_name: Some("Duplicate Broker".to_string()),
                state: wattetheria_social::domain::friendships::FriendshipState::Active,
                established_from_request_id: None,
                thread_id: None,
                created_at: 1,
                updated_at: 1,
            },
        )
        .expect("seed duplicate active friendship");
    }

    let duplicate_response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "send_agent_dm_message",
                "arguments": {
                    "display_name": "Duplicate Broker",
                    "content": {
                        "type": "text",
                        "text": "hello duplicate"
                    }
                }
            }
        }),
    )
    .await;
    assert_eq!(
        duplicate_response["result"]["structuredContent"]["error"].as_str(),
        Some("multiple active friends matched display_name; provide counterpart_public_id")
    );
    let commands = bridge.dm_commands.lock().await;
    assert!(commands.is_empty());
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn mcp_invite_private_hive_participant_sends_key_share_without_exposing_secret() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let remote_identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge::default_for(identity.agent_did.clone()));
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, app, token, _policy, state) =
        build_test_app_with_bridge(100, dir, identity.clone(), event_log, bridge_handle);
    bootstrap_broker_identity(app.clone(), &token, &identity.agent_did).await;
    let context = crate::routes::identity::resolve_identity_context(&state, None, None).await;
    let local_public_id = context
        .public_memory_owner
        .public
        .unwrap_or(context.public_memory_owner.controller);
    let remote_public_id = scoped_id("broker-private-hive", &remote_identity.agent_did);
    let hive_id = "mainnet:watt-etheria@private.hive@group:dm-private-hive-test";
    {
        let mut hives = state.hive_registry.lock().await;
        hives.upsert_hive(wattetheria_kernel::civilization::topics::TopicCreateSpec {
            network_id: Some("mainnet:watt-etheria".to_owned()),
            feed_key: "private.hive".to_owned(),
            scope_hint: "group:dm-private-hive-test".to_owned(),
            display_name: "Private Hive".to_owned(),
            summary: None,
            projection_kind:
                wattetheria_kernel::civilization::topics::TopicProjectionKind::ChatRoom,
            organization_id: None,
            mission_id: None,
            participant_public_ids: Vec::new(),
            created_by_public_id: local_public_id.clone(),
            why_this_exists: None,
            public_geo: None,
            active: true,
        });
    }
    wattetheria_social::application::transport_binding_service::upsert_transport_binding(
        &*state.social_store,
        &wattetheria_social::domain::transport_bindings::RemoteTransportBinding {
            public_id: remote_public_id.clone(),
            agent_did: Some(remote_identity.agent_did.clone()),
            transport_kind:
                wattetheria_social::domain::transport_bindings::TransportKind::Wattswarm,
            transport_node_id: "12D3KooPrivateHivePeer".to_string(),
            binding_source: "friendship".to_string(),
            binding_confidence: 90,
            binding_proof_json: None,
            binding_verified: true,
            binding_verified_at: Some(1),
            updated_at: 1,
        },
    )
    .expect("seed remote transport binding");
    friendship_service::upsert_friendship(
        &*state.social_store,
        &wattetheria_social::domain::friendships::Friendship {
            friendship_id: format!("friendship:{local_public_id}:{remote_public_id}"),
            local_public_id: local_public_id.clone(),
            remote_public_id: remote_public_id.clone(),
            display_name: Some("Private Hive Peer".to_string()),
            state: wattetheria_social::domain::friendships::FriendshipState::Active,
            established_from_request_id: None,
            thread_id: None,
            created_at: 1,
            updated_at: 1,
        },
    )
    .expect("seed active friendship");

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "invite_private_hive_participant",
                "arguments": {
                    "hive_id": hive_id,
                    "counterpart_public_id": remote_public_id,
                    "display_name": "Private Hive Peer",
                    "hive_name": "Private Hive"
                }
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let result_text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("tool result text");
    let result_json: Value = serde_json::from_str(result_text).expect("tool result parses");
    assert_eq!(
        result_json["remote_node_id"].as_str(),
        Some("12D3KooPrivateHivePeer")
    );
    assert_eq!(result_json["feed_key"].as_str(), Some("private.hive"));
    assert_eq!(
        result_json["scope_hint"].as_str(),
        Some("group:dm-private-hive-test")
    );
    assert_eq!(
        result_json["display_name"].as_str(),
        Some("Private Hive Peer")
    );
    assert_eq!(result_json["hive_name"].as_str(), Some("Private Hive"));
    assert_eq!(
        result_json["shared_secret_b64_redacted"].as_bool(),
        Some(true)
    );
    let key_share_commands = bridge.private_hive_key_share_commands.lock().await;
    assert_eq!(key_share_commands.len(), 1);
    assert_eq!(
        key_share_commands[0].display_name.as_str(),
        "Private Hive Peer"
    );
    assert_eq!(key_share_commands[0].hive_name.as_str(), "Private Hive");
    assert_eq!(
        key_share_commands[0].invite_text.as_str(),
        "Hi Private Hive Peer, you are invited to join the private Hive \"Private Hive\". This encrypted message includes the private Hive key share so your node can unlock the Hive messages."
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn mcp_accept_and_reject_friend_requests_send_relationship_actions() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let accept_identity = Identity::new_random();
    let reject_identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge::default_for(identity.agent_did.clone()));
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, app, token, _policy, state) =
        build_test_app_with_bridge(100, dir, identity.clone(), event_log, bridge_handle);
    bootstrap_broker_identity(app.clone(), &token, &identity.agent_did).await;
    let context = crate::routes::identity::resolve_identity_context(&state, None, None).await;
    let local_public_id = context
        .public_memory_owner
        .public
        .unwrap_or(context.public_memory_owner.controller);
    let accept_public_id = scoped_id("broker-accept", &accept_identity.agent_did);
    let reject_public_id = scoped_id("broker-reject", &reject_identity.agent_did);
    {
        let mut identities = state.public_identity_registry.lock().await;
        identities
            .upsert(
                &accept_public_id,
                "Broker Accept".to_string(),
                Some(accept_identity.agent_did.clone()),
                true,
            )
            .unwrap();
        identities
            .upsert(
                &reject_public_id,
                "Broker Reject".to_string(),
                Some(reject_identity.agent_did.clone()),
                true,
            )
            .unwrap();
    }
    {
        let mut bindings = state.controller_binding_registry.lock().await;
        bindings.upsert(
            &accept_public_id,
            wattetheria_kernel::civilization::identities::ControllerKind::ExternalRuntime,
            "accept-runtime".to_string(),
            Some("12D3KooAcceptPeer".to_string()),
            wattetheria_kernel::civilization::identities::OwnershipScope::External,
            true,
        );
        bindings.upsert(
            &reject_public_id,
            wattetheria_kernel::civilization::identities::ControllerKind::ExternalRuntime,
            "reject-runtime".to_string(),
            Some("12D3KooRejectPeer".to_string()),
            wattetheria_kernel::civilization::identities::OwnershipScope::External,
            true,
        );
    }
    for (request_id, remote_public_id, remote_node_id, correlation_id) in [
        (
            "req-accept-1",
            accept_public_id.as_str(),
            "12D3KooAcceptPeer",
            "corr-accept-1",
        ),
        (
            "req-reject-1",
            reject_public_id.as_str(),
            "12D3KooRejectPeer",
            "corr-reject-1",
        ),
    ] {
        friend_request_service::upsert_friend_request(
            &*state.social_store,
            &FriendRequest {
                request_id: request_id.to_string(),
                local_public_id: local_public_id.clone(),
                remote_public_id: remote_public_id.to_string(),
                remote_node_id: Some(remote_node_id.to_string()),
                direction: FriendRequestDirection::Inbound,
                state: FriendRequestState::Pending,
                decision_reason: None,
                correlation_id: Some(correlation_id.to_string()),
                created_at: 1,
                updated_at: 1,
                expires_at: None,
            },
        )
        .expect("save inbound friend request");
    }

    let accept_response = mcp_request(
        app.clone(),
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "accept_friend_request",
                "arguments": {"display_name": "Broker Accept"}
            }
        }),
    )
    .await;
    let reject_response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "reject_friend_request",
                "arguments": {"display_name": "Broker Reject"}
            }
        }),
    )
    .await;

    assert_eq!(accept_response["result"]["isError"].as_bool(), Some(false));
    assert_eq!(reject_response["result"]["isError"].as_bool(), Some(false));
    let commands = bridge.relationship_commands.lock().await;
    assert_eq!(commands.len(), 2);
    assert_eq!(
        commands[0].action,
        wattetheria_kernel::swarm_bridge::SwarmRelationshipAction::Accept
    );
    assert_eq!(commands[0].remote_node_id, "12D3KooAcceptPeer");
    assert_eq!(
        commands[0]
            .agent_envelope
            .message
            .get("request_id")
            .and_then(Value::as_str),
        Some("req-accept-1")
    );
    assert_eq!(
        commands[0]
            .agent_envelope
            .message
            .get("correlation_id")
            .and_then(Value::as_str),
        Some("corr-accept-1")
    );
    assert_eq!(
        commands[1].action,
        wattetheria_kernel::swarm_bridge::SwarmRelationshipAction::Reject
    );
    assert_eq!(commands[1].remote_node_id, "12D3KooRejectPeer");
    assert_eq!(
        commands[1]
            .agent_envelope
            .message
            .get("request_id")
            .and_then(Value::as_str),
        Some("req-reject-1")
    );
}

#[tokio::test]
async fn mcp_list_nearby_returns_compact_peer_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge {
        peers: vec![SwarmPeerView {
            node_id: "peer-nearby-1".to_owned(),
            connected: Some(true),
            recently_seen: Some(true),
            stale: Some(false),
            last_seen_age_ms: None,
            discovery: Some(json!({
                "source_kind": "bootstrap"
            })),
            metadata: Some(json!({
                "endpoint_id": "iroh-endpoint-nearby",
                "network_id": "mainnet:watt-galaxy",
                "protocol_version": "wattswarm/1.0.0",
                "handshake_status": "identified",
                "observed_addr": "198.51.100.2:4001",
                "listen_addrs": ["203.0.113.10:4001"]
            })),
            relationship: None,
        }],
        ..MockSwarmBridge::default_for(identity.agent_did.clone())
    });
    let bridge_handle: Arc<dyn SwarmBridge> = bridge;
    let (_dir, app, token, _policy, _state) =
        build_test_app_with_bridge(100, dir, identity, event_log, bridge_handle);

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "list_nearby",
                "arguments": {}
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let content = &response["result"]["structuredContent"];
    assert_eq!(content["ok"].as_bool(), Some(true));
    assert_eq!(content["count"].as_u64(), Some(1));
    let item = &content["items"][0];
    assert_eq!(item["remote_node_id"].as_str(), Some("peer-nearby-1"));
    assert_eq!(item["status"].as_str(), Some("online"));
    assert_eq!(item["connected"].as_bool(), Some(true));
    assert_eq!(item["endpoint"].as_str(), Some("iroh-endpoint-nearby"));
    assert_eq!(item["discovery"]["source_kind"].as_str(), Some("bootstrap"));
    assert_eq!(
        item["metadata"]["observed_addr"].as_str(),
        Some("198.51.100.2:4001")
    );
    assert_eq!(
        item["metadata"]["listen_addrs"][0].as_str(),
        Some("203.0.113.10:4001")
    );
    assert!(item.get("node_id").is_none());
    assert!(item.get("source_kind").is_none());
    assert!(item.get("request_agent_friend_arguments").is_none());
    assert!(item.get("target_agent_did").is_none());
    assert!(item.get("counterpart_public_id").is_none());
    assert!(item.get("relationship_state").is_none());
    assert!(item.get("relationship").is_none());
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn mcp_friend_request_tools_split_list_and_detail_views() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let remote_identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let remote_public_id = scoped_id("broker-inbound", &remote_identity.agent_did);
    let remote_node_id = "12D3KooInboundPeer".to_string();
    let bridge = Arc::new(MockSwarmBridge {
        peers: vec![SwarmPeerView {
            node_id: remote_node_id.clone(),
            connected: Some(true),
            recently_seen: Some(true),
            stale: Some(false),
            last_seen_age_ms: None,
            discovery: Some(json!({"source_kind": "bootstrap"})),
            metadata: Some(json!({
                "endpoint_id": "iroh-endpoint-inbound",
                "network_id": "mainnet:watt-etheria",
                "protocol_version": "wattswarm/1.0.0",
                "handshake_status": "identified",
                "observed_addr": "198.51.100.2:4001",
                "listen_addrs": ["203.0.113.10:4001"]
            })),
            relationship: None,
        }],
        relationship_views: Mutex::new(vec![
            SwarmPeerRelationshipView {
                remote_node_id: remote_node_id.clone(),
                relationship_state: "requested".to_string(),
                last_action: "request".to_string(),
                initiated_by: "remote".to_string(),
                agent_envelope: Some(SwarmAgentEnvelope {
                    protocol: "google_a2a".to_string(),
                    transport_profile: None,
                    source_agent_id: Some(remote_identity.agent_did.clone()),
                    target_agent_id: Some(identity.agent_did.clone()),
                    source_node_id: Some(remote_node_id.clone()),
                    target_node_id: None,
                    capability: Some("peer.relationship.request".to_string()),
                    source_agent_card: Some(SwarmSourceAgentCard {
                        agent_id: remote_identity.agent_did.clone(),
                        node_id: Some(remote_node_id.clone()),
                        card_hash: "sha256:alice-display-card".to_string(),
                        issued_at: 1_710_000_100,
                        card: json!({
                            "name": "Agent Alice Display",
                            "metadata": {
                                "display_name": "Agent Alice Display"
                            },
                            "skills": [
                                {
                                    "id": "social-direct-message",
                                    "name": "Social direct message",
                                    "description": "Can send and receive signed peer relationship and direct message events."
                                }
                            ]
                        }),
                        signature: Some("sig-alice-display-card".to_string()),
                    }),
                    message: json!({
                        "kind": "friend_request",
                        "text": "hello, I am Alice from node X",
                        "request_id": "req-inbound-1",
                        "correlation_id": "corr-inbound-1",
                        "sent_at": 1_710_000_100
                    }),
                    extensions: None,
                    signature: Some("sig-inbound".to_string()),
                }),
                requested_at: Some(1_710_000_100),
                responded_at: None,
                blocked_at: None,
                cleared_at: None,
                updated_at: 1_710_000_105,
            },
            SwarmPeerRelationshipView {
                remote_node_id: "12D3KooOutboundPeer".to_string(),
                relationship_state: "requested".to_string(),
                last_action: "request".to_string(),
                initiated_by: "local".to_string(),
                agent_envelope: Some(SwarmAgentEnvelope {
                    protocol: "google_a2a".to_string(),
                    transport_profile: None,
                    source_agent_id: Some(identity.agent_did.clone()),
                    target_agent_id: Some(remote_identity.agent_did.clone()),
                    source_node_id: None,
                    target_node_id: Some("12D3KooOutboundPeer".to_string()),
                    capability: Some("peer.relationship.request".to_string()),
                    source_agent_card: None,
                    message: json!({
                        "kind": "friend_request",
                        "text": "outbound hello",
                        "request_id": "req-outbound-1",
                        "correlation_id": "corr-outbound-1",
                        "sent_at": 1_710_000_090
                    }),
                    extensions: None,
                    signature: Some("sig-outbound".to_string()),
                }),
                requested_at: Some(1_710_000_090),
                responded_at: None,
                blocked_at: None,
                cleared_at: None,
                updated_at: 1_710_000_095,
            },
        ]),
        ..MockSwarmBridge::default_for(identity.agent_did.clone())
    });
    let bridge_handle: Arc<dyn SwarmBridge> = bridge;
    let (_dir, app, token, _policy, state) =
        build_test_app_with_bridge(100, dir, identity.clone(), event_log, bridge_handle);
    let _local_public_id =
        bootstrap_broker_identity(app.clone(), &token, &identity.agent_did).await;
    {
        let mut identities = state.public_identity_registry.lock().await;
        identities
            .upsert(
                &remote_public_id,
                "Agent Alice".to_string(),
                Some(remote_identity.agent_did.clone()),
                true,
            )
            .unwrap();
    }
    {
        let mut bindings = state.controller_binding_registry.lock().await;
        bindings.upsert(
            &remote_public_id,
            wattetheria_kernel::civilization::identities::ControllerKind::ExternalRuntime,
            "remote-runtime".to_string(),
            Some(remote_node_id.clone()),
            wattetheria_kernel::civilization::identities::OwnershipScope::External,
            true,
        );
    }

    let list_response = mcp_request(
        app.clone(),
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "list_friend_requests",
                "arguments": {}
            }
        }),
    )
    .await;
    let list_content = &list_response["result"]["structuredContent"];
    assert_eq!(list_content["ok"].as_bool(), Some(true));
    assert_eq!(list_content["count"].as_u64(), Some(1));
    assert_eq!(
        list_content["items"][0]["request_id"].as_str(),
        Some("req-inbound-1")
    );
    assert_eq!(
        list_content["items"][0]["from"].as_str(),
        Some("Agent Alice Display")
    );
    assert_eq!(
        list_content["items"][0]["preview"].as_str(),
        Some("hello, I am Alice from node X")
    );
    assert_eq!(
        list_content["items"][0]["counterpart_skills"][0].as_str(),
        Some("Social direct message")
    );
    assert_eq!(
        list_content["items"][0]["direction"].as_str(),
        Some("inbound")
    );
    assert_eq!(list_content["items"][0]["state"].as_str(), Some("pending"));
    assert_eq!(
        list_content["items"][0]["remote_node_id"].as_str(),
        Some(remote_node_id.as_str())
    );
    assert_eq!(
        list_content["items"][0]["counterpart_agent_did"].as_str(),
        Some(remote_identity.agent_did.as_str())
    );
    assert_eq!(
        list_content["items"][0]["agent"]["display_name"].as_str(),
        Some("Agent Alice Display")
    );
    assert_eq!(
        list_content["items"][0]["agent"]["agent_card"]["name"].as_str(),
        Some("Agent Alice Display")
    );
    assert_eq!(
        list_content["items"][0]["agent_card"]["metadata"]["display_name"].as_str(),
        Some("Agent Alice Display")
    );
    assert_eq!(
        list_content["items"][0]["source_agent_card"]["card_hash"].as_str(),
        Some("sha256:alice-display-card")
    );
    assert!(list_content["items"][0].get("agent_envelope").is_none());
    assert_eq!(
        list_content["items"][0]["message"]["text"].as_str(),
        Some("hello, I am Alice from node X")
    );
    assert_eq!(
        list_content["items"][0]["message"]["request_id"].as_str(),
        Some("req-inbound-1")
    );
    assert_eq!(
        list_content["items"][0]["network"]["remote_node_id"].as_str(),
        Some(remote_node_id.as_str())
    );
    assert_eq!(
        list_content["items"][0]["network"]["metadata"]["network_id"].as_str(),
        Some("mainnet:watt-etheria")
    );

    let sent_response = mcp_request(
        app.clone(),
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "list_sent_friend_requests",
                "arguments": {}
            }
        }),
    )
    .await;
    let sent_content = &sent_response["result"]["structuredContent"];
    assert_eq!(sent_content["count"].as_u64(), Some(1));
    assert_eq!(
        sent_content["items"][0]["request_id"].as_str(),
        Some("req-outbound-1")
    );
    assert_eq!(sent_content["items"][0]["state"].as_str(), Some("pending"));

    let get_response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "get_friend_request",
                "arguments": {
                    "display_name": "Agent Alice Display"
                }
            }
        }),
    )
    .await;
    let detail = &get_response["result"]["structuredContent"];
    assert_eq!(detail["ok"].as_bool(), Some(true));
    assert_eq!(
        detail["agent"]["display_name"].as_str(),
        Some("Agent Alice Display")
    );
    assert_eq!(
        detail["agent"]["skills"][0].as_str(),
        Some("Social direct message")
    );
    assert_eq!(
        detail["agent"]["counterpart_skills"][0].as_str(),
        Some("Social direct message")
    );
    assert_eq!(
        detail["agent"]["agent_did"].as_str(),
        Some(remote_identity.agent_did.as_str())
    );
    assert_eq!(detail["message"]["kind"].as_str(), Some("friend_request"));
    assert_eq!(
        detail["message"]["text"].as_str(),
        Some("hello, I am Alice from node X")
    );
    assert_eq!(
        detail["network"]["remote_node_id"].as_str(),
        Some(remote_node_id.as_str())
    );
    assert_eq!(detail["network"]["status"].as_str(), Some("online"));
    assert_eq!(
        detail["network"]["metadata"]["observed_addr"].as_str(),
        Some("198.51.100.2:4001")
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn mcp_tools_list_surfaces_precise_input_schemas_for_agent_tools() {
    let (_dir, app, token, _policy, _state) = build_test_app(100);

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/list"
        }),
    )
    .await;
    let tools = response["result"]["tools"].as_array().unwrap();
    assert!(
        tools
            .iter()
            .all(|tool| tool["name"] != "upsert_local_friend")
    );

    let publish_mission = find_tool(tools, "publish_mission");
    assert_schema_requires(
        publish_mission,
        &["title", "description", "domain", "payload"],
    );
    assert_eq!(
        publish_mission["inputSchema"]["properties"]["title"]["type"].as_str(),
        Some("string")
    );
    assert_eq!(
        publish_mission["inputSchema"]["properties"]["scope"]["enum"][0].as_str(),
        Some("real_world")
    );
    assert_eq!(
        publish_mission["inputSchema"]["properties"]["scope"]["enum"][1].as_str(),
        Some("in_world")
    );
    assert_schema_omits(
        publish_mission,
        &[
            "publisher",
            "publisher_kind",
            "lat",
            "lng",
            "coordinate_source",
            "reward",
        ],
    );
    assert_eq!(
        publish_mission["inputSchema"]["properties"]
            .get("body")
            .and_then(Value::as_object),
        None
    );
    assert!(
        !publish_mission["inputSchema"]["properties"]
            .as_object()
            .unwrap()
            .contains_key("settlement_delegation")
    );

    let publish_delegated_mission = find_tool(tools, "publish_delegated_mission");
    assert_schema_requires(
        publish_delegated_mission,
        &[
            "title",
            "description",
            "domain",
            "payload",
            "settlement_delegation",
        ],
    );
    assert_schema_omits(
        publish_delegated_mission,
        &["publisher", "publisher_kind", "reward"],
    );
    assert!(
        publish_delegated_mission["inputSchema"]["properties"]["settlement_delegation"]
            ["description"]
            .as_str()
            .is_some_and(|description| description.contains("servicenet-agent"))
    );

    let publish_collective_mission = find_tool(tools, "publish_collective_mission");
    assert_schema_requires(
        publish_collective_mission,
        &[
            "hive_id",
            "title",
            "description",
            "domain",
            "payload",
            "mode",
            "min_participants",
        ],
    );
    assert_schema_omits(
        publish_collective_mission,
        &[
            "publisher",
            "publisher_kind",
            "lat",
            "lng",
            "coordinate_source",
            "reward",
        ],
    );
    let collective_required = publish_collective_mission["inputSchema"]["required"]
        .as_array()
        .unwrap();
    assert!(!collective_required.iter().any(|field| field == "agents"));
    assert!(!collective_required.iter().any(|field| field == "scope"));
    assert_eq!(
        publish_collective_mission["inputSchema"]["properties"]["scope"]["enum"][0].as_str(),
        Some("real_world")
    );
    assert_eq!(
        publish_collective_mission["inputSchema"]["properties"]["scope"]["enum"][1].as_str(),
        Some("in_world")
    );
    assert_eq!(
        publish_collective_mission["inputSchema"]["properties"]["mode"]["enum"][1].as_str(),
        Some("stigmergy")
    );
    assert!(
        publish_collective_mission["inputSchema"]["properties"]["mode"]["description"]
            .as_str()
            .is_some_and(|description| description.contains("Defaults to committee"))
    );
    assert_eq!(
        publish_collective_mission["inputSchema"]["properties"]["mode"]["default"].as_str(),
        Some("committee")
    );
    assert_eq!(
        publish_collective_mission["inputSchema"]["properties"]["skills"]["type"].as_str(),
        Some("array")
    );
    assert!(
        publish_collective_mission["inputSchema"]["properties"]
            .as_object()
            .unwrap()
            .get("agents")
            .is_none()
    );
    assert!(
        publish_collective_mission["inputSchema"]["properties"]
            .as_object()
            .unwrap()
            .contains_key("min_participants")
    );
    assert!(
        publish_collective_mission["inputSchema"]["properties"]["min_participants"]["description"]
            .as_str()
            .is_some_and(|description| !description.contains("stigmergy mode"))
    );
    assert!(
        publish_collective_mission["inputSchema"]["properties"]
            .as_object()
            .unwrap()
            .contains_key("join_window_ms")
    );
    assert!(
        publish_collective_mission["inputSchema"]["properties"]["join_window_ms"]["description"]
            .as_str()
            .is_some_and(|description| description.contains("committee join window"))
    );
    assert_eq!(
        publish_collective_mission["inputSchema"]["properties"]["kickoff"]["type"].as_str(),
        Some("boolean")
    );
    assert!(
        publish_collective_mission["inputSchema"]["properties"]["kickoff"]["description"]
            .as_str()
            .is_some_and(|description| description.contains("never starts Wattswarm"))
    );

    let start_collective_mission = find_tool(tools, "start_collective_mission");
    assert_schema_requires(start_collective_mission, &["run_id"]);
    assert_schema_omits(
        start_collective_mission,
        &["joined_count", "participant_count"],
    );
    assert_eq!(
        start_collective_mission["inputSchema"]["properties"]["force"]["type"].as_str(),
        Some("boolean")
    );

    let collective_result = find_tool(tools, "get_collective_mission_result");
    assert!(
        collective_result["inputSchema"]["properties"]
            .as_object()
            .unwrap()
            .contains_key("mission_id")
    );
    assert!(
        collective_result["inputSchema"]["properties"]
            .as_object()
            .unwrap()
            .contains_key("run_id")
    );

    let list_payments = find_tool(tools, "list_agent_payments");
    assert_eq!(
        list_payments["inputSchema"]["properties"]["target_kind"]["enum"][0].as_str(),
        Some("network_agent")
    );
    assert_eq!(
        list_payments["inputSchema"]["properties"]["target_kind"]["enum"][1].as_str(),
        Some("service_agent")
    );
    assert_eq!(
        list_payments["inputSchema"]["properties"]["target_kind"]["enum"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        list_payments["inputSchema"]["properties"]["target_address"]["type"].as_str(),
        Some("string")
    );
    assert_schema_omits(list_payments, &["counterpart_public_id", "display_name"]);

    let propose_payment = find_tool(tools, "propose_agent_payment");
    assert_schema_requires(
        propose_payment,
        &[
            "target_kind",
            "target_address",
            "amount",
            "currency",
            "rail",
        ],
    );
    assert_schema_omits(
        propose_payment,
        &[
            "public_id",
            "display_name",
            "counterpart_public_id",
            "agent_id",
            "recipient_address",
        ],
    );
    assert_eq!(
        propose_payment["inputSchema"]["properties"]["target_kind"]["enum"][1].as_str(),
        Some("service_agent")
    );
    assert_eq!(
        propose_payment["inputSchema"]["properties"]["target_kind"]["enum"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        propose_payment["inputSchema"]["properties"]["layer"]["enum"][1].as_str(),
        Some("web3")
    );

    let create_hive = find_tool(tools, "create_hive");
    assert_schema_omits(
        create_hive,
        &[
            "public_id",
            "initial_message",
            "lat",
            "lng",
            "coordinate_source",
        ],
    );
    assert_eq!(
        create_hive["inputSchema"]["properties"]["scope_hint"]["description"].as_str(),
        Some(
            "Wattswarm scope hint. Valid values are `global`, `region:<id>`, `node:<id>`, `local:<id>`, or `group:<id>`. For Hives, use `group:<hive-or-topic-id>`; do not use `topic:<id>`."
        )
    );
    let list_private_hives = find_tool(tools, "list_private_hives");
    assert_schema_requires(list_private_hives, &[]);
    assert!(
        list_private_hives["inputSchema"]["properties"]
            .as_object()
            .unwrap()
            .contains_key("include_inactive")
    );
    assert_eq!(
        list_private_hives["_meta"]["wattetheria"]["path"].as_str(),
        Some("/v1/wattetheria/hives")
    );

    let create_private_hive = find_tool(tools, "create_private_hive");
    assert_schema_requires(create_private_hive, &["feed_key", "display_name"]);
    assert_schema_omits(
        create_private_hive,
        &[
            "public_id",
            "initial_message",
            "lat",
            "lng",
            "coordinate_source",
        ],
    );
    assert_eq!(
        create_private_hive["inputSchema"]["properties"]["scope_hint"]["description"].as_str(),
        Some(
            "Optional private Wattswarm scope hint. Defaults to a unique `group:dm-<id>` value suitable for sharing out of band with invited friends."
        )
    );
    let post_hive_message = find_tool(tools, "post_hive_message");
    assert_schema_omits(post_hive_message, &["public_id"]);
    let subscribe_hive = find_tool(tools, "subscribe_hive");
    assert_schema_omits(subscribe_hive, &["public_id"]);
    let unsubscribe_hive = find_tool(tools, "unsubscribe_hive");
    assert_schema_requires(unsubscribe_hive, &["hive_id"]);
    assert_schema_omits(unsubscribe_hive, &["public_id", "active"]);
    let invite_private_hive_participant = find_tool(tools, "invite_private_hive_participant");
    assert_schema_requires(
        invite_private_hive_participant,
        &[
            "hive_id",
            "counterpart_public_id",
            "display_name",
            "hive_name",
        ],
    );
    assert_schema_omits(
        invite_private_hive_participant,
        &["public_id", "shared_secret_b64"],
    );
    let list_friends = find_tool(tools, "list_friends");
    assert_eq!(
        list_friends["_meta"]["wattetheria"]["path"].as_str(),
        Some("/v1/wattetheria/social/agent-friends")
    );
    assert!(
        list_friends["inputSchema"]["properties"]
            .as_object()
            .unwrap()
            .contains_key("display_name")
    );
    let list_nearby = find_tool(tools, "list_nearby");
    assert_eq!(
        list_nearby["_meta"]["wattetheria"]["path"].as_str(),
        Some("/v1/wattetheria/social/nearby")
    );
    assert!(
        list_nearby["inputSchema"]["properties"]
            .as_object()
            .unwrap()
            .is_empty()
    );
    let list_friend_requests = find_tool(tools, "list_friend_requests");
    assert_eq!(
        list_friend_requests["_meta"]["wattetheria"]["path"].as_str(),
        Some("/v1/wattetheria/social/friend-requests")
    );
    assert!(
        list_friend_requests["inputSchema"]["properties"]
            .as_object()
            .unwrap()
            .contains_key("limit")
    );
    assert_schema_omits(list_friend_requests, &["direction", "state"]);
    let list_sent_friend_requests = find_tool(tools, "list_sent_friend_requests");
    assert_eq!(
        list_sent_friend_requests["_meta"]["wattetheria"]["path"].as_str(),
        Some("/v1/wattetheria/social/sent-friend-requests")
    );
    let get_friend_request = find_tool(tools, "get_friend_request");
    assert_schema_requires(get_friend_request, &[]);
    assert_schema_optional(get_friend_request, "request_id");
    assert_schema_optional(get_friend_request, "display_name");
    assert_eq!(
        get_friend_request["_meta"]["wattetheria"]["path"].as_str(),
        Some("/v1/wattetheria/social/friend-requests/{request_id}")
    );
    let accept_friend_request = find_tool(tools, "accept_friend_request");
    assert_schema_requires(accept_friend_request, &[]);
    assert_schema_optional(accept_friend_request, "request_id");
    assert_schema_optional(accept_friend_request, "display_name");
    assert_eq!(
        accept_friend_request["_meta"]["wattetheria"]["path"].as_str(),
        Some("/v1/wattetheria/social/friend-requests/{request_id}/accept")
    );
    let reject_friend_request = find_tool(tools, "reject_friend_request");
    assert_schema_requires(reject_friend_request, &[]);
    assert_schema_optional(reject_friend_request, "request_id");
    assert_schema_optional(reject_friend_request, "display_name");
    assert_eq!(
        reject_friend_request["_meta"]["wattetheria"]["path"].as_str(),
        Some("/v1/wattetheria/social/friend-requests/{request_id}/reject")
    );
    let request_agent_friend = find_tool(tools, "request_agent_friend");
    assert!(
        request_agent_friend["inputSchema"]["properties"]
            .as_object()
            .unwrap()
            .contains_key("target_agent_did")
    );
    assert!(
        !request_agent_friend["inputSchema"]["required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|field| field.as_str() == Some("remote_node_id"))
    );
    assert_schema_optional(request_agent_friend, "display_name");
    assert_schema_omits(request_agent_friend, &["public_id", "action"]);
    let search_agents = find_tool(tools, "search_agents");
    assert_schema_requires(search_agents, &[]);
    assert_schema_optional(search_agents, "public_id");
    assert_schema_optional(search_agents, "display_name");
    assert_eq!(
        search_agents["_meta"]["wattetheria"]["path"].as_str(),
        Some("/v1/wattetheria/social/agents/search")
    );
    assert_eq!(
        search_agents["_meta"]["wattetheria"]["readOnly"].as_bool(),
        Some(true)
    );
    let get_agent_card = find_tool(tools, "get_agent_card");
    assert_schema_requires(get_agent_card, &[]);
    assert_schema_optional(get_agent_card, "public_id");
    assert_schema_optional(get_agent_card, "display_name");
    assert_eq!(
        get_agent_card["_meta"]["wattetheria"]["path"].as_str(),
        Some("/v1/wattetheria/social/agent-card")
    );
    assert_eq!(
        get_agent_card["_meta"]["wattetheria"]["readOnly"].as_bool(),
        Some(true)
    );
    let remove_agent_friend = find_tool(tools, "remove_agent_friend");
    assert!(
        remove_agent_friend["inputSchema"]["properties"]
            .as_object()
            .unwrap()
            .contains_key("target_agent_did")
    );
    assert!(
        remove_agent_friend["inputSchema"]["properties"]
            .as_object()
            .unwrap()
            .contains_key("display_name")
    );
    assert!(
        !remove_agent_friend["inputSchema"]["required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|field| field.as_str() == Some("remote_node_id"))
    );
    assert_schema_omits(remove_agent_friend, &["public_id", "action"]);
    assert_eq!(
        remove_agent_friend["_meta"]["wattetheria"]["path"].as_str(),
        Some("/v1/wattetheria/social/agent-friends")
    );
    let list_agent_dm_threads = find_tool(tools, "list_agent_dm_threads");
    assert_eq!(
        list_agent_dm_threads["_meta"]["wattetheria"]["path"].as_str(),
        Some("/v1/wattetheria/social/agent-dm/threads")
    );
    assert!(
        list_agent_dm_threads["inputSchema"]["properties"]
            .as_object()
            .unwrap()
            .contains_key("display_name")
    );
    let list_agent_dm_messages = find_tool(tools, "list_agent_dm_messages");
    assert_eq!(
        list_agent_dm_messages["_meta"]["wattetheria"]["path"].as_str(),
        Some("/v1/wattetheria/social/agent-dm/messages")
    );
    assert!(
        list_agent_dm_messages["inputSchema"]["properties"]
            .as_object()
            .unwrap()
            .contains_key("display_name")
    );
    let send_agent_dm_message = find_tool(tools, "send_agent_dm_message");
    assert_schema_requires(send_agent_dm_message, &["content"]);
    assert!(
        send_agent_dm_message["inputSchema"]["properties"]
            .as_object()
            .unwrap()
            .contains_key("display_name")
    );
    assert_schema_omits(send_agent_dm_message, &["public_id"]);
    assert_eq!(
        send_agent_dm_message["_meta"]["wattetheria"]["path"].as_str(),
        Some("/v1/wattetheria/social/agent-dm/messages")
    );

    let settle_payment = find_tool(tools, "settle_agent_payment");
    assert_schema_requires(settle_payment, &["payment_id", "settlement_receipt"]);
    assert_schema_omits(
        settle_payment,
        &["target_kind", "target_address", "recipient_address"],
    );

    let submit_payment = find_tool(tools, "submit_agent_payment");
    assert_schema_requires(submit_payment, &["payment_id"]);
    assert!(
        submit_payment["inputSchema"]["properties"]
            .as_object()
            .unwrap()
            .contains_key("settlement_receipt")
    );
    assert!(
        !submit_payment["inputSchema"]["required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|field| field.as_str() == Some("settlement_receipt"))
    );
    assert_schema_omits(
        submit_payment,
        &["target_kind", "target_address", "recipient_address"],
    );

    let get_payment = find_tool(tools, "get_agent_payment");
    assert_schema_requires(get_payment, &["payment_id"]);
    assert_schema_omits(
        get_payment,
        &["target_kind", "target_address", "recipient_address"],
    );

    let authorize_payment = find_tool(tools, "authorize_agent_payment");
    assert_schema_requires(authorize_payment, &["payment_id"]);
    assert_schema_optional(authorize_payment, "sender_address");
    assert_schema_omits(
        authorize_payment,
        &["target_kind", "target_address", "recipient_address"],
    );

    let reject_payment = find_tool(tools, "reject_agent_payment");
    assert_schema_requires(reject_payment, &["payment_id", "reject_reason"]);
    assert_schema_omits(
        reject_payment,
        &["target_kind", "target_address", "recipient_address"],
    );

    let cancel_payment = find_tool(tools, "cancel_agent_payment");
    assert_schema_requires(cancel_payment, &["payment_id"]);
    assert_schema_omits(
        cancel_payment,
        &["target_kind", "target_address", "recipient_address"],
    );

    let get_servicenet_receipt = find_tool(tools, "get_servicenet_receipt");
    assert_schema_requires(get_servicenet_receipt, &["receipt_id"]);

    let get_servicenet_agent = find_tool(tools, "get_servicenet_agent");
    assert_schema_requires(get_servicenet_agent, &["service_address"]);
    assert_schema_omits(get_servicenet_agent, &["agent_id"]);
    assert_eq!(
        get_servicenet_agent["_meta"]["wattetheria"]["path"].as_str(),
        Some("/v1/wattetheria/servicenet/agents/{service_address}")
    );

    let delete_servicenet_agent = find_tool(tools, "delete_servicenet_agent");
    assert_schema_requires(delete_servicenet_agent, &["service_address"]);
    assert_schema_omits(delete_servicenet_agent, &["agent_id"]);
    assert_eq!(
        delete_servicenet_agent["_meta"]["wattetheria"]["path"].as_str(),
        Some("/v1/wattetheria/servicenet/agents/{service_address}/unpublish")
    );

    let invoke_servicenet_agent = find_tool(tools, "invoke_servicenet_agent_sync");
    assert_schema_requires(invoke_servicenet_agent, &["service_address"]);
    assert!(
        !invoke_servicenet_agent["inputSchema"]["properties"]
            .as_object()
            .unwrap()
            .contains_key("agent_id")
    );
    assert!(
        !invoke_servicenet_agent["inputSchema"]["properties"]
            .as_object()
            .unwrap()
            .contains_key("agent_name")
    );
    assert_eq!(
        invoke_servicenet_agent["_meta"]["wattetheria"]["path"].as_str(),
        Some("/v1/wattetheria/servicenet/agents/{service_address}/invoke")
    );

    let invoke_servicenet_agent_async = find_tool(tools, "invoke_servicenet_agent_async");
    assert_schema_requires(invoke_servicenet_agent_async, &["service_address"]);
    assert_schema_omits(invoke_servicenet_agent_async, &["agent_id", "agent_name"]);
    assert_eq!(
        invoke_servicenet_agent_async["_meta"]["wattetheria"]["path"].as_str(),
        Some("/v1/wattetheria/servicenet/agents/{service_address}/invoke-async")
    );

    let get_servicenet_agent_task = find_tool(tools, "get_servicenet_agent_task");
    assert_schema_requires(get_servicenet_agent_task, &["service_address", "task_id"]);
    assert_schema_omits(get_servicenet_agent_task, &["agent_id"]);
    assert_eq!(
        get_servicenet_agent_task["_meta"]["wattetheria"]["path"].as_str(),
        Some("/v1/wattetheria/servicenet/agents/{service_address}/tasks/{task_id}/get")
    );

    for hidden_tool in [
        "send_mailbox_message",
        "list_mailbox_messages",
        "ack_mailbox_message",
    ] {
        assert!(tools.iter().all(|tool| tool["name"] != hidden_tool));
    }

    let list_missions = find_tool(tools, "list_missions");
    assert_eq!(
        list_missions["description"].as_str(),
        Some("Browse the bounded Wattetheria network mission market from the configured gateway.")
    );
    assert_eq!(
        list_missions["inputSchema"]["properties"]["limit"]["type"].as_str(),
        Some("integer")
    );
    assert_eq!(
        list_missions["inputSchema"]["properties"]["offset"]["type"].as_str(),
        Some("integer")
    );

    let claim_mission = find_tool(tools, "claim_mission");
    assert_schema_requires(claim_mission, &["mission_id", "agent_did"]);
    assert_eq!(
        claim_mission["inputSchema"]["properties"]["claim_route"]["description"].as_str(),
        Some("Claim route object returned by list_missions.")
    );
    assert_eq!(
        claim_mission["inputSchema"]["properties"]["mission_scope_hint"]["type"].as_str(),
        Some("string")
    );
    let complete_mission = find_tool(tools, "complete_mission");
    assert_schema_requires(complete_mission, &["mission_id", "agent_did"]);
    assert_eq!(
        complete_mission["inputSchema"]["properties"]["result"]["description"].as_str(),
        Some(
            "Ordinary mission completion result to publish in the mission_completed lifecycle notice."
        )
    );
    assert_eq!(
        complete_mission["inputSchema"]["properties"]["claim_route"]["description"].as_str(),
        Some("Claim route object returned by list_missions for network missions.")
    );
    let settle_mission = find_tool(tools, "settle_mission");
    assert_schema_requires(settle_mission, &["mission_id"]);
    assert_eq!(
        settle_mission["inputSchema"]["properties"]["candidate_id"]["description"].as_str(),
        Some(
            "Explicit Wattswarm candidate ID to accept before settling candidate-backed task results."
        )
    );
}

#[tokio::test]
async fn mcp_complete_mission_publishes_ordinary_lifecycle_notice_without_candidate() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge::default_for(identity.agent_did.clone()));
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, app, token, _policy, state) =
        build_test_app_with_bridge(100, dir, identity, event_log, bridge_handle);
    let agent_did = state.agent_did.clone();
    let public_id = bootstrap_broker_identity(app.clone(), &token, &agent_did).await;
    let mission = authed_post_json(
        app.clone(),
        &token,
        "/v1/wattetheria/missions",
        json!({
            "title": "MCP ordinary complete",
            "description": "MCP complete_mission stays on ordinary mission lifecycle.",
            "publisher": public_id,
            "publisher_kind": "player",
            "domain": "trade",
            "reward": {
                "agent_watt": 1,
                "reputation": 0,
                "capacity": 0,
                "treasury_share_watt": 0
            },
            "payload": {"objective": "ordinary-mcp-complete"}
        }),
    )
    .await;
    let mission_id = mission["mission_id"].as_str().expect("mission_id");
    let _claimed = authed_post_json(
        app.clone(),
        &token,
        &format!("/v1/wattetheria/missions/{mission_id}/claim"),
        json!({
            "mission_id": mission_id,
            "agent_did": agent_did,
        }),
    )
    .await;

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "complete_mission",
                "arguments": {
                    "mission_id": mission_id,
                    "agent_did": agent_did,
                    "result": {"ok": true, "summary": "done"}
                }
            }
        }),
    )
    .await;

    let completed = &response["result"]["structuredContent"];
    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    assert_eq!(completed["status"].as_str(), Some("completed"));
    assert_eq!(
        completed["mission_lifecycle_notice"]["kind"].as_str(),
        Some("mission_completed")
    );
    assert!(completed.get("candidate_id").is_none());
    assert!(completed.get("swarm_candidate").is_none());

    let messages = bridge.messages.lock().await;
    assert_eq!(messages.len(), 2);
    assert_eq!(
        messages[0].content["kind"].as_str(),
        Some("mission_claim_approved")
    );
    assert_eq!(
        messages[1].content["kind"].as_str(),
        Some("mission_completed")
    );
}

#[tokio::test]
async fn mcp_publish_mission_uses_current_local_public_identity() {
    let (_dir, app, token, _policy, _state) = build_test_app(100);
    let self_json = authed_get_json(app.clone(), &token, "/v1/client/self").await;
    let local_public_id = self_json["id"].as_str().unwrap();

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "publish_mission",
                "arguments": {
                    "title": "MCP local publisher",
                    "description": "Publisher should be injected by the local MCP server.",
                    "publisher": "wrong-manual-value",
                    "publisher_kind": "system",
                    "domain": "trade",
                    "payload": {"objective": "identity-default"}
                }
            }
        }),
    )
    .await;

    let mission = &response["result"]["structuredContent"];
    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    assert_eq!(mission["publisher"].as_str(), Some(local_public_id));
    assert_eq!(mission["publisher_kind"].as_str(), Some("player"));
    let mission_id = mission["mission_id"].as_str().expect("mission id");
    assert_eq!(mission["task_id"].as_str(), Some(mission_id));
    assert_eq!(mission["task_type"].as_str(), Some("wattetheria.mission"));
    assert_eq!(mission["scope"].as_str(), Some("real_world"));
    assert_eq!(
        mission["mission_scope_hint"].as_str(),
        Some(format!("group:{mission_id}").as_str())
    );
    assert_eq!(
        mission["swarm_scope"],
        json!({"kind": "group", "id": mission_id})
    );
    assert_eq!(
        mission["task_contract"]["task_id"].as_str(),
        Some(mission_id)
    );
    assert_eq!(
        mission["task_contract"]["inputs"]["swarm_scope"],
        json!({"kind": "group", "id": mission_id})
    );
    assert_eq!(
        mission["task_contract"]["inputs"]["mission_scope_hint"].as_str(),
        mission["mission_scope_hint"].as_str()
    );
    assert_eq!(
        mission["task_contract"]["inputs"]["scope"].as_str(),
        Some("real_world")
    );
    assert!(mission.get("reward").is_none());
    assert!(mission["task_contract"]["inputs"].get("reward").is_none());
    assert_public_geo_projection(mission);
    assert_public_geo_projection(&mission["task_contract"]["inputs"]);
}

#[tokio::test]
async fn mcp_publish_delegated_mission_surfaces_servicenet_settlement_details() {
    let (_dir, app, token, _policy, _state) = build_test_app(100);
    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "publish_delegated_mission",
                "arguments": {
                    "title": "Funded ServiceNet escrow task",
                    "description": "Publish a real reward mission with third-party settlement metadata.",
                    "domain": "trade",
                    "payload": {"objective": "escrow-backed"},
                    "settlement_delegation": {
                        "enabled": true,
                        "layer": "web3",
                        "provider": "servicenet-agent",
                        "provider_agent_id": "escrow-agent-123",
                        "provider_agent_name": "Some Escrow Agent",
                        "network": "base-sepolia",
                        "asset": "USDC",
                        "amount": "10000000",
                        "funding_proof": {
                            "type": "evm_tx",
                            "tx_hash": "0x3333333333333333333333333333333333333333333333333333333333333333",
                            "chain_id": 84532,
                            "to": "0x1111111111111111111111111111111111111111"
                        },
                        "provider_receipt": {
                            "receipt_id": "receipt-servicenet-1",
                            "status": "funded",
                            "task_id": "provider-task-1",
                            "raw": {"provider_rule": "external"}
                        },
                        "terms": {
                            "summary": "Provider-defined settlement rules.",
                            "url": "https://escrow.example/terms",
                            "raw": {"max_revision_count": 1}
                        }
                    }
                }
            }
        }),
    )
    .await;

    let mission = &response["result"]["structuredContent"];
    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let mission_id = mission["mission_id"].as_str().expect("mission id");
    let delegation = &mission["settlement_delegation"];
    assert_eq!(delegation["provider"].as_str(), Some("servicenet-agent"));
    assert_eq!(delegation["layer"].as_str(), Some("web3"));
    assert_eq!(
        delegation["provider_agent_id"].as_str(),
        Some("escrow-agent-123")
    );
    assert_eq!(
        delegation["provider_agent_name"].as_str(),
        Some("Some Escrow Agent")
    );
    assert_eq!(delegation["network"].as_str(), Some("base-sepolia"));
    assert_eq!(delegation["status"].as_str(), Some("funded"));
    assert_eq!(delegation["asset"].as_str(), Some("USDC"));
    assert_eq!(
        delegation["provider_receipt"]["receipt_id"].as_str(),
        Some("receipt-servicenet-1")
    );
    assert_eq!(
        mission["payload"]["settlement_delegation"],
        mission["settlement_delegation"]
    );
    assert_eq!(
        mission["task_contract"]["inputs"]["settlement_delegation"],
        mission["settlement_delegation"]
    );
    assert_eq!(
        mission["task_contract"]["inputs"]["mission_id"].as_str(),
        Some(mission_id)
    );
    assert!(mission.get("reward").is_none());
    assert!(mission["task_contract"]["inputs"].get("reward").is_none());
}

fn create_collective_hive_request() -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "create_hive",
            "arguments": {
                "feed_key": "mcp-collective-feed",
                "scope_hint": "group:mcp-collective-feed",
                "display_name": "MCP Collective Hive",
                "projection_kind": "chat_room",
                "include_public_geo": false
            }
        }
    })
}

fn create_collective_hive_response(response: &Value) -> &str {
    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    response["result"]["structuredContent"]["hive"]["topic_id"]
        .as_str()
        .expect("hive id")
}

async fn create_collective_hive(app: axum::Router, token: &str) -> String {
    let response = mcp_request(app, token, create_collective_hive_request()).await;
    create_collective_hive_response(&response).to_owned()
}

async fn create_private_collective_hive(app: axum::Router, token: &str) -> String {
    let response = mcp_request(
        app,
        token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "create_private_hive",
                "arguments": {
                    "feed_key": "wattetheria.private.collective",
                    "scope_hint": "group:dm-private-collective",
                    "display_name": "Private Collective Hive"
                }
            }
        }),
    )
    .await;
    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    response["result"]["structuredContent"]["hive"]["topic_id"]
        .as_str()
        .expect("private hive id")
        .to_owned()
}

fn collective_mission_request(hive_id: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "publish_collective_mission",
            "arguments": {
                "mode": "committee",
                "hive_id": hive_id,
                "title": "Collective MCP mission",
                "description": "Run several agents through Wattswarm.",
                "publisher": "wrong-manual-value",
                "publisher_kind": "system",
                "domain": "trade",
                "scope": "in_world",
                "required_faction": "freeport",
                "required_role": "broker",
                "payload": {"objective": "collective-intel"},
                "min_participants": 2,
                "threshold_percent": 60,
                "round_timeout_ms": 30000,
                "max_rounds": 3,
                "aggregation": {"mode": "MAJORITY"},
                "kickoff": true
            }
        }
    })
}

fn collective_stigmergy_mission_request(hive_id: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {
            "name": "publish_collective_mission",
            "arguments": {
                "mode": "stigmergy",
                "hive_id": hive_id,
                "title": "Open collective MCP mission",
                "description": "Let subscribed agents decide whether to contribute.",
                "domain": "trade",
                "payload": {"objective": "open-collective-intel"},
                "min_participants": 2,
                "join_window_ms": 60000,
                "threshold_percent": 60,
                "round_timeout_ms": 30000,
                "max_rounds": 3,
                "fallback_decision": "abstain",
                "aggregation": {"mode": "MAJORITY"},
                "kickoff": true
            }
        }
    })
}

fn start_collective_mission_request(run_id: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {
            "name": "start_collective_mission",
            "arguments": {
                "run_id": run_id
            }
        }
    })
}

fn assert_collective_publish_result<'a>(
    response: &'a Value,
    local_public_id: &str,
    hive_id: &str,
) -> (&'a str, &'a str) {
    let content = &response["result"]["structuredContent"];
    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    assert_eq!(
        content["mission"]["publisher"].as_str(),
        Some(local_public_id)
    );
    assert_eq!(
        content["mission"]["publisher_kind"].as_str(),
        Some("player")
    );
    assert_eq!(
        content["mission"]["task_type"].as_str(),
        Some("wattetheria.collective_mission")
    );
    assert_eq!(
        content["mission"]["kind"].as_str(),
        Some("collective_mission")
    );
    assert_eq!(content["mission"]["lifecycle"].as_str(), Some("collective"));
    assert_eq!(content["mission"]["hive_id"].as_str(), Some(hive_id));
    assert_eq!(content["mission"]["scope"].as_str(), Some("in_world"));
    assert_public_geo_projection(&content["mission"]);
    assert!(content["mission"].get("task_contract").is_none());
    assert_eq!(
        content["mission"]["payload"]["task_type"].as_str(),
        Some("wattetheria.collective_mission")
    );
    assert_eq!(
        content["mission"]["payload"]["objective"].as_str(),
        Some("collective-intel")
    );
    let mission_id = content["mission_id"].as_str().expect("mission id");
    let run_id = content["run_id"].as_str().expect("run id");
    assert_eq!(content["kicked_off"].as_bool(), Some(false));
    assert_eq!(content["phase"].as_str(), Some("joining"));
    assert_eq!(content["wattswarm_run"]["submitted"].as_bool(), Some(false));
    assert_eq!(
        content["wattswarm_run"]["kicked_off"].as_bool(),
        Some(false)
    );
    assert_eq!(
        content["run_spec"]["task_type"].as_str(),
        Some("wattetheria.collective_mission")
    );
    assert_eq!(
        content["run_spec"]["shared_inputs"]["mission_id"].as_str(),
        Some(mission_id)
    );
    assert_eq!(
        content["run_spec"]["shared_inputs"]["hive_id"].as_str(),
        Some(hive_id)
    );
    assert_eq!(
        content["run_spec"]["shared_inputs"]["mission"]["scope"].as_str(),
        Some("in_world")
    );
    assert_eq!(
        content["run_spec"]["round_policy"]["min_participants"].as_u64(),
        Some(2)
    );
    assert_eq!(
        content["run_spec"]["round_policy"]["threshold_percent"].as_u64(),
        Some(60)
    );
    assert_eq!(
        content["run_spec"]["join_policy"]["join_window_ms"].as_u64(),
        Some(1_800_000)
    );
    assert_public_geo_projection(&content["run_spec"]["shared_inputs"]["mission"]);
    assert_eq!(
        content["run_spec"]["agents"].as_array().map(Vec::len),
        Some(0)
    );
    assert_eq!(
        content["hive_message"]["type"].as_str(),
        Some("collective_mission")
    );
    assert!(
        content["hive_message"].get("contribution").is_none(),
        "collective Hive messages must not expose coordinator contact material"
    );
    assert!(
        content["hive_message"]["coordinator"]["agent_did"]
            .as_str()
            .is_some_and(|agent_did| !agent_did.trim().is_empty())
    );
    assert_eq!(
        content["hive_message"]["mission_id"].as_str(),
        Some(mission_id)
    );
    assert_eq!(content["hive_message"]["run_id"].as_str(), Some(run_id));
    assert_eq!(content["hive_message"]["kickoff"].as_bool(), Some(false));
    (mission_id, run_id)
}

#[tokio::test]
async fn mcp_publish_collective_mission_creates_joining_link_without_submitting_run() {
    let (_dir, app, token, _policy, state) = build_test_app(100);
    let self_json = authed_get_json(app.clone(), &token, "/v1/client/self").await;
    let local_public_id = self_json["id"].as_str().unwrap();
    let hive_id = create_collective_hive(app.clone(), &token).await;
    let mission_count_before = state.mission_board.lock().await.list(None).len();

    let response = mcp_request(app.clone(), &token, collective_mission_request(&hive_id)).await;
    let (mission_id, run_id) =
        assert_collective_publish_result(&response, local_public_id, &hive_id);
    let mission_count_after = state.mission_board.lock().await.list(None).len();
    assert_eq!(mission_count_after, mission_count_before);

    let persisted: Value = state
        .local_db
        .load_domain(wattetheria_kernel::local_db::domain::COLLECTIVE_MISSION_RUNS)
        .unwrap()
        .unwrap();
    assert_eq!(
        persisted["runs"][mission_id]["run_id"].as_str(),
        Some(run_id)
    );
    assert_eq!(
        persisted["runs"][mission_id]["hive_id"].as_str(),
        Some(hive_id.as_str())
    );

    assert_eq!(
        persisted["runs"][mission_id]["wattswarm_run"]["submitted"].as_bool(),
        Some(false)
    );
    assert!(
        persisted["runs"][mission_id]["task_prompt"]
            .as_str()
            .is_some_and(|prompt| prompt.contains("Apply your own available skills"))
    );
    assert_eq!(
        persisted["runs"][mission_id]["participants"]
            .as_object()
            .map(serde_json::Map::len),
        Some(0)
    );
}

#[tokio::test]
async fn collective_participation_dm_records_joined_participant_in_run_link() {
    let (_dir, app, token, _policy, state) = build_test_app(100);
    let self_json = authed_get_json(app.clone(), &token, "/v1/client/self").await;
    let local_public_id = self_json["id"].as_str().unwrap();
    let hive_id = create_collective_hive(app.clone(), &token).await;

    let response = mcp_request(app, &token, collective_mission_request(&hive_id)).await;
    let (mission_id, run_id) =
        assert_collective_publish_result(&response, local_public_id, &hive_id);
    let view = SwarmPeerDmMessageView {
        thread_id: "thread-alpha".to_owned(),
        message_id: "dm-alpha".to_owned(),
        remote_node_id: "node-alpha".to_owned(),
        message_kind: "agent".to_owned(),
        direction: "inbound".to_owned(),
        delivery_state: "delivered".to_owned(),
        a2a_protocol: "wattetheria.agent.v1".to_owned(),
        agent_envelope: None,
        content: json!({
            "type": "collective_participation",
            "version": 1,
            "status": "join",
            "mission_id": mission_id,
            "run_id": run_id,
            "event_id": "event-alpha",
            "decision_id": "decision-alpha",
            "participant_agent_did": "did:key:alpha",
            "participant_node_id": "node-alpha",
            "payload": {
                "public_id": "agent-alpha"
            }
        }),
        encrypted_body: None,
        content_encoding: None,
        created_at: 1,
        acknowledged_at: None,
    };

    let recorded =
        crate::routes::mcp::collective::record_collective_participation_from_dm(&state, &view)
            .unwrap()
            .expect("collective participation record");
    assert_eq!(recorded["recorded"].as_bool(), Some(true));
    assert_eq!(recorded["inserted"].as_bool(), Some(true));
    assert_eq!(recorded["joined_count"].as_u64(), Some(1));
    let duplicate =
        crate::routes::mcp::collective::record_collective_participation_from_dm(&state, &view)
            .unwrap()
            .expect("duplicate collective participation record");
    assert_eq!(duplicate["recorded"].as_bool(), Some(true));
    assert_eq!(duplicate["inserted"].as_bool(), Some(false));
    assert_eq!(duplicate["joined_count"].as_u64(), Some(1));

    let persisted: Value = state
        .local_db
        .load_domain(wattetheria_kernel::local_db::domain::COLLECTIVE_MISSION_RUNS)
        .unwrap()
        .unwrap();
    let participant = &persisted["runs"][mission_id]["participants"]["public:agent-alpha"];
    assert_eq!(participant["agent_id"].as_str(), Some("agent-alpha"));
    assert_eq!(participant["executor"].as_str(), Some("remote:node-alpha"));
    assert!(
        participant["prompt"]
            .as_str()
            .is_some_and(|prompt| prompt.contains("Apply your own available skills"))
    );
}

#[tokio::test]
async fn mcp_start_collective_mission_submits_joined_participants_as_committee_agents() {
    let (_dir, app, token, _policy, state) = build_test_app(100);
    let self_json = authed_get_json(app.clone(), &token, "/v1/client/self").await;
    let local_public_id = self_json["id"].as_str().unwrap();
    let hive_id = create_collective_hive(app.clone(), &token).await;

    let response = mcp_request(app.clone(), &token, collective_mission_request(&hive_id)).await;
    let (mission_id, run_id) =
        assert_collective_publish_result(&response, local_public_id, &hive_id);

    let mut persisted: Value = state
        .local_db
        .load_domain(wattetheria_kernel::local_db::domain::COLLECTIVE_MISSION_RUNS)
        .unwrap()
        .unwrap();
    persisted["runs"][mission_id]["join_deadline_ms"] = json!(0);
    persisted["runs"][mission_id]["participants"] = json!({
        "public:agent-alpha": {
            "agent_id": "agent-alpha",
            "executor": "remote:node-alpha",
            "prompt": "Use alpha expertise for this collective mission.",
            "participant_agent_did": "did:key:alpha",
            "participant_node_id": "node-alpha",
            "public_id": "agent-alpha",
            "joined_at": "2026-06-24T00:00:00Z",
            "payload": {}
        },
        "public:agent-beta": {
            "agent_id": "agent-beta",
            "executor": "remote:node-beta",
            "prompt": persisted["runs"][mission_id]["task_prompt"].clone(),
            "participant_agent_did": "did:key:beta",
            "participant_node_id": "node-beta",
            "public_id": "agent-beta",
            "joined_at": "2026-06-24T00:00:00Z",
            "payload": {}
        }
    });
    state
        .local_db
        .save_domain(
            wattetheria_kernel::local_db::domain::COLLECTIVE_MISSION_RUNS,
            &persisted,
        )
        .unwrap();

    let response = mcp_request(app, &token, start_collective_mission_request(run_id)).await;
    let content = &response["result"]["structuredContent"];
    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    assert_eq!(content["mission_id"].as_str(), Some(mission_id));
    assert_eq!(content["run_id"].as_str(), Some(run_id));
    assert_eq!(content["kicked_off"].as_bool(), Some(true));
    assert_eq!(content["wattswarm_run"]["kicked_off"].as_bool(), Some(true));
    assert!(content["run_spec"].get("round_policy").is_none());
    assert_eq!(
        content["run_spec"]["collective_policy"]["min_participants"].as_u64(),
        Some(2)
    );
    let agents = content["run_spec"]["agents"].as_array().expect("agents");
    assert_eq!(agents.len(), 2);
    assert!(
        agents
            .iter()
            .any(|agent| agent["agent_id"].as_str() == Some("agent-alpha")
                && agent["prompt"].as_str()
                    == Some("Use alpha expertise for this collective mission."))
    );
    assert!(agents.iter().any(|agent| {
        agent["agent_id"].as_str() == Some("agent-beta")
            && agent["prompt"]
                .as_str()
                .is_some_and(|prompt| prompt.contains("Apply your own available skills"))
    }));
    assert_eq!(
        content["link"]["participants"]
            .as_object()
            .map(serde_json::Map::len),
        Some(2)
    );
}

#[tokio::test]
async fn mcp_publish_collective_mission_defaults_to_joining_without_kickoff() {
    let (_dir, app, token, _policy, state) = build_test_app(100);
    let hive_id = create_collective_hive(app.clone(), &token).await;
    let mut request = collective_mission_request(&hive_id);
    request["params"]["arguments"]
        .as_object_mut()
        .expect("collective arguments")
        .remove("kickoff");

    let response = mcp_request(app, &token, request).await;
    let content = &response["result"]["structuredContent"];
    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let mission_id = content["mission_id"].as_str().expect("mission id");
    assert_eq!(content["kicked_off"].as_bool(), Some(false));
    assert_eq!(content["phase"].as_str(), Some("joining"));
    assert_eq!(
        content["wattswarm_run"]["kicked_off"].as_bool(),
        Some(false)
    );
    assert_eq!(content["hive_message"]["kickoff"].as_bool(), Some(false));

    let persisted: Value = state
        .local_db
        .load_domain(wattetheria_kernel::local_db::domain::COLLECTIVE_MISSION_RUNS)
        .unwrap()
        .unwrap();
    assert_eq!(
        persisted["runs"][mission_id]["kicked_off"].as_bool(),
        Some(false)
    );
}

#[tokio::test]
async fn mcp_publish_collective_mission_omits_contact_material_for_private_hive() {
    let (_dir, app, token, _policy, _state) = build_test_app(101);
    let self_json = authed_get_json(app.clone(), &token, "/v1/client/self").await;
    let local_public_id = self_json["id"].as_str().unwrap();
    let hive_id = create_private_collective_hive(app.clone(), &token).await;

    let response = mcp_request(app, &token, collective_mission_request(&hive_id)).await;
    let content = &response["result"]["structuredContent"];
    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    assert_eq!(
        content["mission"]["publisher"].as_str(),
        Some(local_public_id)
    );
    assert!(
        content["hive_message"].get("contribution").is_none(),
        "private collective Hive messages must not expose coordinator contact material"
    );
}

#[tokio::test]
async fn mcp_get_collective_mission_result_allows_locally_linked_run_id() {
    let (_dir, app, token, _policy, _state) = build_test_app(100);
    let hive_id = create_collective_hive(app.clone(), &token).await;
    let response = mcp_request(app.clone(), &token, collective_mission_request(&hive_id)).await;
    let mission_id = response["result"]["structuredContent"]["mission_id"]
        .as_str()
        .expect("mission id");
    let run_id = response["result"]["structuredContent"]["run_id"]
        .as_str()
        .expect("run id");

    let result_response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "get_collective_mission_result",
                "arguments": {
                    "run_id": run_id
                }
            }
        }),
    )
    .await;

    let result = &result_response["result"]["structuredContent"];
    assert_eq!(result_response["result"]["isError"].as_bool(), Some(false));
    assert_eq!(result["mission_id"].as_str(), Some(mission_id));
    assert_eq!(result["run_id"].as_str(), Some(run_id));
    assert_eq!(result["link"]["mission_id"].as_str(), Some(mission_id));
}

#[tokio::test]
async fn mcp_get_collective_mission_result_rejects_unlinked_run_id() {
    let (_dir, app, token, _policy, _state) = build_test_app(100);

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "get_collective_mission_result",
                "arguments": {
                    "run_id": "external-run-1"
                }
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"].as_bool(), Some(true));
    assert_eq!(
        response["result"]["structuredContent"]["error"].as_str(),
        Some("collective mission run link not found for run_id: external-run-1")
    );
}

#[tokio::test]
async fn mcp_publish_collective_mission_committee_persists_policy_and_skills() {
    let (_dir, app, token, _policy, state) = build_test_app(100);
    let hive_id = create_collective_hive(app.clone(), &token).await;
    let mut request = collective_mission_request(&hive_id);
    let arguments = request["params"]["arguments"]
        .as_object_mut()
        .expect("collective arguments");
    arguments.insert(
        "skills".to_owned(),
        json!(["climate response", "supply-chain analysis"]),
    );

    let response = mcp_request(app, &token, request).await;
    let content = &response["result"]["structuredContent"];
    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let mission_id = content["mission_id"].as_str().expect("mission id");
    assert_eq!(
        content["run_spec"]["agents"].as_array().map(Vec::len),
        Some(0)
    );
    assert!(content["run_spec"].get("market_task_id").is_none());
    assert_eq!(
        content["run_spec"]["round_policy"]["min_participants"].as_u64(),
        Some(2)
    );
    assert_eq!(
        content["run_spec"]["join_policy"]["join_window_ms"].as_u64(),
        Some(1_800_000)
    );
    assert_eq!(
        content["mission"]["skills"].as_array().map(Vec::len),
        Some(2)
    );
    assert_eq!(
        content["hive_message"]["mission"]["skills"][0].as_str(),
        Some("climate response")
    );
    assert_eq!(
        content["run_spec"]["shared_inputs"]["mission"]["skills"][1].as_str(),
        Some("supply-chain analysis")
    );
    let persisted: Value = state
        .local_db
        .load_domain(wattetheria_kernel::local_db::domain::COLLECTIVE_MISSION_RUNS)
        .unwrap()
        .unwrap();
    assert_eq!(
        persisted["runs"][mission_id]["mission"]["skills"][0].as_str(),
        Some("climate response")
    );
}

#[tokio::test]
async fn mcp_publish_collective_mission_requires_collective_policy_and_filters() {
    let (_dir, app, token, _policy, _state) = build_test_app(100);
    let hive_id = create_collective_hive(app.clone(), &token).await;
    let mut missing_mode = collective_mission_request(&hive_id);
    missing_mode["params"]["arguments"]
        .as_object_mut()
        .expect("collective arguments")
        .remove("mode");

    let response = mcp_request(app.clone(), &token, missing_mode).await;
    assert_eq!(response["result"]["isError"].as_bool(), Some(true));
    assert_eq!(
        response["result"]["structuredContent"]["error"].as_str(),
        Some("mode is required for collective mission")
    );

    let mut missing_min_participants = collective_mission_request(&hive_id);
    missing_min_participants["params"]["arguments"]
        .as_object_mut()
        .expect("collective arguments")
        .remove("min_participants");

    let response = mcp_request(app.clone(), &token, missing_min_participants).await;
    assert_eq!(response["result"]["isError"].as_bool(), Some(true));
    assert_eq!(
        response["result"]["structuredContent"]["error"].as_str(),
        Some("min_participants is required for collective mission")
    );

    let response = mcp_request(app, &token, collective_mission_request(&hive_id)).await;
    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
}

#[tokio::test]
async fn mcp_publish_collective_mission_rejects_stigmergy_until_supported() {
    let (_dir, app, token, _policy, _state) = build_test_app(100);
    let hive_id = create_collective_hive(app.clone(), &token).await;

    let response = mcp_request(app, &token, collective_stigmergy_mission_request(&hive_id)).await;
    assert_eq!(response["result"]["isError"].as_bool(), Some(true));
    assert_eq!(
        response["result"]["structuredContent"]["error"].as_str(),
        Some(
            "collective stigmergy mode is temporarily unsupported; use committee mode. Stigmergy collective missions will be opened later."
        )
    );
}

#[tokio::test]
async fn mcp_create_hive_uses_current_local_public_identity() {
    let (_dir, app, token, _policy, _state) = build_test_app(100);
    let self_json = authed_get_json(app.clone(), &token, "/v1/client/self").await;
    let local_public_id = self_json["id"].as_str().unwrap();

    let response = mcp_request(
        app.clone(),
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "create_hive",
                "arguments": {
                    "public_id": "wrong-manual-value",
                    "feed_key": "mcp-topic-feed",
                    "scope_hint": "group:mcp-topic-feed",
                    "display_name": "MCP Hive",
                    "projection_kind": "chat_room",
                    "include_public_geo": false
                }
            }
        }),
    )
    .await;

    let content = &response["result"]["structuredContent"];
    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    assert_eq!(
        content["hive"]["created_by_public_id"].as_str(),
        Some(local_public_id)
    );
    assert_public_geo_projection(&content["hive"]);
    let topic_id = content["hive"]["topic_id"].as_str().unwrap();
    let export_json = public_get_json(
        app,
        &format!(
            "/v1/wattetheria/client/export?public_id={local_public_id}&peer_limit=1&task_limit=1&organization_limit=1&rpc_log_limit=1&leaderboard_limit=1"
        ),
    )
    .await;
    let public_topic = export_json["payload"]["public_topics"]
        .as_array()
        .unwrap()
        .iter()
        .find(|topic| topic["topic_id"].as_str() == Some(topic_id))
        .unwrap();
    assert_public_geo_projection(public_topic);
}

#[tokio::test]
async fn mcp_create_private_hive_defaults_to_unique_group_dm_chat_room_scope() {
    let (_dir, app, token, _policy, _state) = build_test_app(100);

    let first = mcp_request(
        app.clone(),
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "create_private_hive",
                "arguments": {
                    "feed_key": "wattetheria.private.hives",
                    "display_name": "Private Hive",
                    "participant_public_ids": ["friend-public-1"]
                }
            }
        }),
    )
    .await;
    let second = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "create_private_hive",
                "arguments": {
                    "feed_key": "wattetheria.private.hives",
                    "display_name": "Second Private Hive"
                }
            }
        }),
    )
    .await;

    assert_eq!(first["result"]["isError"].as_bool(), Some(false));
    assert_eq!(second["result"]["isError"].as_bool(), Some(false));
    let first_hive = &first["result"]["structuredContent"]["hive"];
    let second_hive = &second["result"]["structuredContent"]["hive"];
    let first_scope = first_hive["scope_hint"].as_str().unwrap();
    let second_scope = second_hive["scope_hint"].as_str().unwrap();
    assert!(first_scope.starts_with("group:dm-"));
    assert!(second_scope.starts_with("group:dm-"));
    assert_ne!(first_scope, second_scope);
    assert_eq!(first_hive["projection_kind"].as_str(), Some("chat_room"));
    assert_eq!(
        first_hive["participant_public_ids"][0].as_str(),
        Some("friend-public-1")
    );
    assert_public_geo_omitted(first_hive);
    assert_public_geo_omitted(second_hive);
}

#[tokio::test]
async fn mcp_create_private_hive_rejects_non_private_scope_hint() {
    let (_dir, app, token, _policy, _state) = build_test_app(100);

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "create_private_hive",
                "arguments": {
                    "feed_key": "wattetheria.private.hives",
                    "scope_hint": "group:public-room",
                    "display_name": "Not Private"
                }
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"].as_bool(), Some(true));
    assert_eq!(
        response["result"]["structuredContent"]["error"].as_str(),
        Some("create_private_hive scope_hint must use group:dm-<id>")
    );
}

#[tokio::test]
async fn mcp_create_hive_rejects_invalid_scope_hint_with_actionable_error() {
    let (_dir, app, token, _policy, _state) = build_test_app(100);

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "create_hive",
                "arguments": {
                    "feed_key": "wattetheria.hives",
                    "scope_hint": "topic:bad-hive",
                    "display_name": "Bad Hive Scope",
                    "projection_kind": "chat_room"
                }
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"].as_bool(), Some(true));
    assert_eq!(
        response["result"]["_meta"]["httpStatus"].as_u64(),
        Some(400)
    );
    let content = &response["result"]["structuredContent"];
    assert_eq!(content["field"].as_str(), Some("scope_hint"));
    assert_eq!(content["received"].as_str(), Some("topic:bad-hive"));
    assert_eq!(
        content["error"].as_str(),
        Some(
            "invalid scope_hint: expected global, region:<id>, node:<id>, local:<id>, or group:<id>; for Hives use group:<id>"
        )
    );
    assert!(
        response["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("group:<id>")
    );
}

#[tokio::test]
async fn mcp_post_hive_message_requires_local_subscription() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge::default_for(identity.agent_did.clone()));
    let (_dir, app, token, _policy, _state) =
        build_test_app_with_bridge(100, dir, identity, event_log, bridge.clone());

    let blocked = mcp_request(
        app.clone(),
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "post_hive_message",
                "arguments": {
                    "hive_id": "mainnet:test@crew.chat@group:crew-7",
                    "network_id": "mainnet:test",
                    "feed_key": "crew.chat",
                    "scope_hint": "group:crew-7",
                    "content": {"text": "blocked"}
                }
            }
        }),
    )
    .await;

    assert_eq!(blocked["result"]["isError"].as_bool(), Some(true));
    assert_eq!(blocked["result"]["_meta"]["httpStatus"].as_u64(), Some(403));
    assert_eq!(
        blocked["result"]["structuredContent"]["error"].as_str(),
        Some("hive subscription required")
    );
    assert!(bridge.messages.lock().await.is_empty());

    let create_response = mcp_request(
        app.clone(),
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "create_hive",
                "arguments": {
                    "feed_key": "crew.chat",
                    "scope_hint": "group:crew-7",
                    "display_name": "Crew Seven",
                    "projection_kind": "chat_room",
                    "network_id": "mainnet:test"
                }
            }
        }),
    )
    .await;
    assert_eq!(create_response["result"]["isError"].as_bool(), Some(false));

    let posted = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "post_hive_message",
                "arguments": {
                    "hive_id": "mainnet:test@crew.chat@group:crew-7",
                    "content": {"text": "allowed"}
                }
            }
        }),
    )
    .await;

    assert_eq!(posted["result"]["isError"].as_bool(), Some(false));
    let messages = bridge.messages.lock().await;
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].content["text"].as_str(), Some("allowed"));
}

#[tokio::test]
async fn mcp_unsubscribe_hive_uses_current_local_public_identity_and_removes_local_subscription() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge::default_for(identity.agent_did.clone()));
    let (_dir, app, token, _policy, _state) =
        build_test_app_with_bridge(100, dir, identity, event_log, bridge.clone());

    let create_response = mcp_request(
        app.clone(),
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "create_hive",
                "arguments": {
                    "public_id": "wrong-manual-value",
                    "feed_key": "codex_topic_smoke_test",
                    "scope_hint": "group:codex-topic-smoke-test",
                    "display_name": "Codex Hive",
                    "projection_kind": "chat_room"
                }
            }
        }),
    )
    .await;
    let hive_id = create_response["result"]["structuredContent"]["hive"]["topic_id"]
        .as_str()
        .unwrap();

    let response = mcp_request(
        app.clone(),
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "unsubscribe_hive",
                "arguments": {
                    "hive_id": hive_id,
                    "active": true
                }
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let subscriptions = bridge.subscriptions.lock().await;
    assert_eq!(subscriptions.len(), 2);
    assert_eq!(subscriptions[1].2, "codex_topic_smoke_test");
    assert_eq!(subscriptions[1].3, "group:codex-topic-smoke-test");
    assert!(!subscriptions[1].4);

    let hives = authed_get_json(app, &token, "/v1/wattetheria/hives?include_inactive=true").await;
    assert!(
        hives["hives"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["topic_id"].as_str() != Some(hive_id))
    );
}

#[tokio::test]
async fn mcp_list_hives_reads_configured_gateway_hives() {
    let gateway_url = spawn_gateway_hives_server(gateway_hives_fixture()).await;
    let (dir, app, token, _policy, _state) = build_test_app(100);
    std::fs::write(
        dir.path().join("config.json"),
        json!({"gateway_urls": [gateway_url]}).to_string(),
    )
    .unwrap();

    let response = mcp_request(
        app.clone(),
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "list_hives",
                "arguments": {
                    "limit": 1,
                    "offset": 1,
                    "projection_kind": "working_group"
                }
            }
        }),
    )
    .await;

    assert_eq!(response["jsonrpc"].as_str(), Some("2.0"));
    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let content = &response["result"]["structuredContent"];
    assert_eq!(
        content["source"].as_str(),
        Some("wattetheria-gateway.api_hives")
    );
    assert_eq!(content["scope"].as_str(), Some("network"));
    assert_eq!(
        content["pagination"].as_str(),
        Some("gateway_limit_client_offset")
    );
    assert_eq!(content["limit"].as_u64(), Some(1));
    assert_eq!(content["offset"].as_u64(), Some(1));
    assert_eq!(content["known_count"].as_u64(), Some(1));
    assert_eq!(content["has_more"].as_bool(), Some(false));
    let hives = content["hives"].as_array().unwrap();
    assert_eq!(hives.len(), 0);

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "list_hives",
                "arguments": {
                    "limit": 2,
                    "projection_kind": "working_group"
                }
            }
        }),
    )
    .await;
    assert_gateway_hive_topic(&response);
}

#[tokio::test]
async fn mcp_list_private_hives_reads_local_private_hives_only() {
    let (_dir, app, token, _policy, state) = build_test_app(100);
    {
        let mut hives = state.hive_registry.lock().await;
        hives.upsert_hive(test_hive_spec(
            "private.hive",
            "group:dm-private-active",
            "Private Active",
            true,
        ));
        hives.upsert_hive(test_hive_spec(
            "wattetheria.hives",
            "group:public-topic",
            "Public Hive",
            true,
        ));
        hives.upsert_hive(test_hive_spec(
            "private.hive",
            "group:dm-private-inactive",
            "Private Inactive",
            false,
        ));
    }

    let response = mcp_request(
        app.clone(),
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "list_private_hives",
                "arguments": {
                    "network_id": "mainnet:watt-etheria"
                }
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let content = &response["result"]["structuredContent"];
    assert_eq!(
        content["source"].as_str(),
        Some("wattetheria.local_hive_registry")
    );
    assert_eq!(content["scope"].as_str(), Some("local_private"));
    assert_eq!(content["known_count"].as_u64(), Some(1));
    let hives = content["hives"].as_array().unwrap();
    assert_eq!(hives.len(), 1);
    assert_eq!(hives[0]["display_name"].as_str(), Some("Private Active"));
    assert_eq!(hives[0]["feed_key"].as_str(), Some("private.hive"));
    assert_eq!(
        hives[0]["scope_hint"].as_str(),
        Some("group:dm-private-active")
    );
    assert!(
        !serde_json::to_string(content)
            .unwrap()
            .contains("shared_secret_b64")
    );

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "list_private_hives",
                "arguments": {
                    "include_inactive": true
                }
            }
        }),
    )
    .await;
    assert_eq!(
        response["result"]["structuredContent"]["known_count"].as_u64(),
        Some(2)
    );
}

fn test_hive_spec(
    feed_key: &str,
    scope_hint: &str,
    display_name: &str,
    active: bool,
) -> wattetheria_kernel::civilization::topics::TopicCreateSpec {
    wattetheria_kernel::civilization::topics::TopicCreateSpec {
        network_id: Some("mainnet:watt-etheria".to_owned()),
        feed_key: feed_key.to_owned(),
        scope_hint: scope_hint.to_owned(),
        display_name: display_name.to_owned(),
        summary: None,
        projection_kind: wattetheria_kernel::civilization::topics::TopicProjectionKind::ChatRoom,
        organization_id: None,
        mission_id: None,
        participant_public_ids: Vec::new(),
        created_by_public_id: "local-public".to_owned(),
        why_this_exists: None,
        public_geo: None,
        active,
    }
}

#[tokio::test]
async fn mcp_subscribe_hive_uses_gateway_subscribe_route_when_hive_is_not_local() {
    let gateway_url = spawn_gateway_hives_server(gateway_hives_fixture()).await;
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge::default_for(identity.agent_did.clone()));
    let (dir, app, token, _policy, _state) =
        build_test_app_with_bridge(100, dir, identity, event_log, bridge.clone());
    std::fs::write(
        dir.path().join("config.json"),
        json!({"gateway_urls": [gateway_url]}).to_string(),
    )
    .unwrap();

    let list_response = mcp_request(
        app.clone(),
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "list_hives",
                "arguments": {
                    "limit": 2,
                    "projection_kind": "working_group"
                }
            }
        }),
    )
    .await;
    let hive = &list_response["result"]["structuredContent"]["hives"][0];
    let route = &hive["subscribe_route"];

    let response = mcp_request(
        app.clone(),
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "subscribe_hive",
                "arguments": {
                    "hive_id": hive["hive_id"],
                    "network_id": route["network_id"],
                    "feed_key": route["feed_key"],
                    "scope_hint": route["scope_hint"],
                    "display_name": hive["display_name"],
                    "summary": hive["summary"],
                    "projection_kind": hive["projection_kind"],
                    "organization_id": hive["organization_id"]
                }
            }
        }),
    )
    .await;

    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let subscriptions = bridge.subscriptions.lock().await;
    assert_eq!(subscriptions.len(), 1);
    assert_eq!(subscriptions[0].2, "wattetheria.hives");
    assert_eq!(subscriptions[0].3, "group:hive-two");
    assert!(subscriptions[0].4);

    let client_hives = authed_get_json(app, &token, "/v1/client/hives?limit=10").await;
    let hives = client_hives.as_array().unwrap();
    let subscribed = hives
        .iter()
        .find(|item| {
            item["feed_key"].as_str() == Some("wattetheria.hives")
                && item["scope_hint"].as_str() == Some("group:hive-two")
        })
        .expect("subscribed gateway Hive is persisted locally");
    assert_eq!(
        subscribed["display_name"].as_str(),
        Some("Gateway Hive Two")
    );
    assert_eq!(
        subscribed["summary"].as_str(),
        Some("Gateway Hive Two summary")
    );
    assert_eq!(
        subscribed["projection_kind"].as_str(),
        Some("working_group")
    );
}

async fn spawn_gateway_hives_server(payload: Value) -> String {
    let gateway_app = axum::Router::new().route(
        "/api/hives",
        axum::routing::get(move || async move { axum::Json(payload) }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gateway_url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, gateway_app).await.unwrap();
    });
    gateway_url
}

fn gateway_hives_fixture() -> Value {
    json!([
        {
            "topic_id": "hive-gateway-1",
            "display_name": "Gateway Hive One",
            "projection_kind": "guild",
            "status": "active",
            "feed_key": "wattetheria.hives",
            "scope_hint": "group:hive-one",
            "source_node_id": "node-alpha"
        },
        {
            "topic_id": "hive-gateway-2",
            "display_name": "Gateway Hive Two",
            "summary": "Gateway Hive Two summary",
            "projection_kind": "working_group",
            "status": "active",
            "feed_key": "wattetheria.hives",
            "scope_hint": "group:hive-two",
            "source_node_id": "node-beta",
            "organization_id": "org-filter"
        },
        {
            "topic_id": "hive-inactive",
            "display_name": "Inactive Gateway Hive",
            "projection_kind": "guild",
            "status": "inactive",
            "feed_key": "wattetheria.hives",
            "scope_hint": "group:hive-inactive"
        }
    ])
}

fn assert_gateway_hive_topic(response: &Value) {
    let content = &response["result"]["structuredContent"];
    let hives = content["hives"].as_array().unwrap();
    assert_eq!(hives.len(), 1);
    assert_eq!(hives[0]["topic_id"].as_str(), Some("hive-gateway-2"));
    assert_eq!(hives[0]["hive_id"].as_str(), Some("hive-gateway-2"));
    assert_eq!(hives[0]["source_node_id"].as_str(), Some("node-beta"));
    assert_eq!(
        hives[0]["subscribe_route"]["feed_key"].as_str(),
        Some("wattetheria.hives")
    );
    assert_eq!(
        hives[0]["subscribe_route"]["scope_hint"].as_str(),
        Some("group:hive-two")
    );
    assert_eq!(
        hives[0]["subscribe_route"]["subscribe_ready"].as_bool(),
        Some(true)
    );
}

#[tokio::test]
async fn mcp_list_missions_reads_configured_gateway_tasks() {
    let gateway_app = axum::Router::new().route(
        "/api/missions",
        axum::routing::get(|| async { axum::Json(gateway_missions_fixture()) }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gateway_url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, gateway_app).await.unwrap();
    });

    let (dir, app, token, _policy, _state) = build_test_app(100);
    std::fs::write(
        dir.path().join("config.json"),
        json!({"gateway_urls": [gateway_url]}).to_string(),
    )
    .unwrap();

    let response = mcp_request(
        app,
        &token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "list_missions",
                "arguments": {
                    "limit": 1,
                    "offset": 1,
                    "status": "open"
                }
            }
        }),
    )
    .await;

    assert_eq!(response["jsonrpc"].as_str(), Some("2.0"));
    assert_eq!(response["result"]["isError"].as_bool(), Some(false));
    let content = &response["result"]["structuredContent"];
    assert_eq!(
        content["source"].as_str(),
        Some("wattetheria-gateway.api_missions")
    );
    assert_eq!(content["scope"].as_str(), Some("network"));
    assert_eq!(
        content["pagination"].as_str(),
        Some("gateway_limit_client_offset")
    );
    assert_eq!(content["limit"].as_u64(), Some(1));
    assert_eq!(content["offset"].as_u64(), Some(1));
    assert_eq!(content["known_count"].as_u64(), Some(2));
    assert_eq!(content["has_more"].as_bool(), Some(false));
    let missions = content["missions"].as_array().unwrap();
    assert_eq!(missions.len(), 1);
    assert_eq!(
        missions[0]["mission_id"].as_str(),
        Some("mission-gateway-2")
    );
    assert_eq!(missions[0]["task_id"].as_str(), Some("mission-gateway-2"));
    assert_eq!(missions[0]["source_node_id"].as_str(), Some("node-beta"));
    assert_eq!(missions[0]["status"].as_str(), Some("published"));
    assert_gateway_settlement_summary(&missions[0]);
    assert_gateway_claim_route(&missions[0], "mission-gateway-2", "node-beta");
}

fn gateway_missions_fixture() -> Value {
    json!([
        {
            "id": "mission-gateway-1",
            "title": "Gateway Mission One",
            "status": "published",
            "source_node_id": "node-alpha",
            "mission_scope_hint": "group:mission-gateway-1",
            "task_contract": {
                "task_id": "mission-gateway-1",
                "inputs": {
                    "swarm_scope": {"kind": "group", "id": "mission-gateway-1"}
                }
            }
        },
        {
            "task_id": "not-a-mission",
            "task_type": "topic_consensus",
            "terminal_state": "open"
        },
        {
            "id": "mission-gateway-2",
            "title": "Gateway Mission Two",
            "status": "published",
            "source_node_id": "node-beta",
            "mission_scope_hint": "group:mission-gateway-2",
            "task_contract": {
                "task_id": "mission-gateway-2",
                "inputs": {
                    "swarm_scope": {"kind": "group", "id": "mission-gateway-2"},
                    "settlement_delegation": gateway_servicenet_settlement_delegation()
                }
            }
        },
        {
            "id": "mission-gateway-settled",
            "title": "Settled Gateway Mission",
            "status": "settled",
            "source_node_id": "node-gamma"
        }
    ])
}

fn gateway_servicenet_settlement_delegation() -> Value {
    json!({
        "enabled": true,
        "layer": "web3",
        "provider": "servicenet-agent",
        "provider_agent_id": "escrow-agent-123",
        "provider_agent_name": "Some Escrow Agent",
        "network": "base-sepolia",
        "status": "funded",
        "asset": "USDC",
        "amount": "2500000",
        "funding_proof": {
            "type": "evm_tx",
            "tx_hash": "0x3333333333333333333333333333333333333333333333333333333333333333",
            "chain_id": 84532,
            "to": "0x1111111111111111111111111111111111111111"
        },
        "provider_receipt": {
            "receipt_id": "receipt-gateway-2",
            "status": "funded",
            "task_id": "provider-task-2"
        },
        "terms": {
            "summary": "Provider-defined settlement rules.",
            "url": "https://escrow.example/terms"
        }
    })
}

fn assert_gateway_settlement_summary(mission: &Value) {
    assert_eq!(mission["reward_type"].as_str(), Some("delegated"));
    assert_eq!(mission["has_settlement_delegation"].as_bool(), Some(true));
    assert_eq!(mission["settlement_layer"].as_str(), Some("web3"));
    assert_eq!(
        mission["settlement_provider"].as_str(),
        Some("servicenet-agent")
    );
    assert_eq!(
        mission["settlement_provider_agent_id"].as_str(),
        Some("escrow-agent-123")
    );
    assert_eq!(
        mission["settlement_provider_agent_name"].as_str(),
        Some("Some Escrow Agent")
    );
    assert_eq!(mission["settlement_network"].as_str(), Some("base-sepolia"));
    assert_eq!(mission["settlement_chain_id"].as_u64(), Some(84532));
    assert_eq!(mission["settlement_status"].as_str(), Some("funded"));
    assert_eq!(
        mission["settlement_receipt_id"].as_str(),
        Some("receipt-gateway-2")
    );
    assert_eq!(mission["settlement_asset"].as_str(), Some("USDC"));
    assert_eq!(mission["settlement_amount"].as_str(), Some("2500000"));
    assert_eq!(
        mission["settlement_funding_tx"].as_str(),
        Some("0x3333333333333333333333333333333333333333333333333333333333333333")
    );
    assert_eq!(
        mission["settlement_terms_url"].as_str(),
        Some("https://escrow.example/terms")
    );
    assert_eq!(
        mission["settlement_delegation"],
        mission["task_contract"]["inputs"]["settlement_delegation"]
    );
}

#[tokio::test]
async fn mcp_claim_mission_reports_duplicate_network_claim() {
    let (dir, app, token, _policy, state) = build_test_app(100);
    let mission_id = "mission-mcp-duplicate-claim";
    let agent_did = state.agent_did.clone();
    seed_mcp_gateway_remote_mission(dir.path(), &state, mission_id).await;
    append_bad_audit_row(dir.path());

    let first = mcp_claim_mission(app.clone(), &token, mission_id, &agent_did).await;
    assert_eq!(first["result"]["isError"].as_bool(), Some(false));
    assert_eq!(
        first["result"]["structuredContent"]["status"].as_str(),
        Some("network_claim_submitted")
    );
    let registry: NetworkMissionClaimRegistry = state
        .local_db
        .load_domain_or_default(wattetheria_kernel::local_db::domain::NETWORK_MISSION_CLAIMS)
        .unwrap();
    let saved_claim = registry
        .records()
        .into_iter()
        .find(|claim| claim.mission_id == mission_id)
        .expect("network claim saved");
    assert_eq!(
        saved_claim.metadata.title.as_deref(),
        Some("Remote mission")
    );
    assert_eq!(saved_claim.metadata.domain.as_deref(), Some("trade"));
    assert_eq!(
        saved_claim.metadata.publisher_id.as_deref(),
        Some("publisher-public")
    );
    assert_eq!(saved_claim.status.as_deref(), Some("published"));
    assert_eq!(saved_claim.metadata.reward_watt, Some(10));

    let second = mcp_claim_mission(app, &token, mission_id, &agent_did).await;
    assert_eq!(second["result"]["isError"].as_bool(), Some(true));
    let content = &second["result"]["structuredContent"];
    assert_eq!(content["code"].as_str(), Some("mission_already_claimed"));
    assert_eq!(content["claim_status"].as_str(), Some("already_claimed"));
    assert_eq!(content["mission_id"].as_str(), Some(mission_id));
    assert_eq!(content["task_id"].as_str(), Some(mission_id));
    assert_eq!(content["agent_did"].as_str(), Some(agent_did.as_str()));
    assert_eq!(second["result"]["_meta"]["httpStatus"].as_u64(), Some(409));
}

#[tokio::test]
async fn mcp_claim_mission_reports_gateway_claimed_status() {
    let (dir, app, token, _policy, state) = build_test_app(100);
    let mission_id = "mission-mcp-gateway-claimed";
    let agent_did = state.agent_did.clone();
    seed_mcp_gateway_remote_mission_with_status(dir.path(), &state, mission_id, "claimed").await;

    let response = mcp_claim_mission(app, &token, mission_id, &agent_did).await;
    assert_eq!(response["result"]["isError"].as_bool(), Some(true));
    let content = &response["result"]["structuredContent"];
    assert_eq!(content["code"].as_str(), Some("mission_already_claimed"));
    assert_eq!(content["claim_status"].as_str(), Some("already_claimed"));
    assert_eq!(content["mission_id"].as_str(), Some(mission_id));
    assert_eq!(
        response["result"]["_meta"]["httpStatus"].as_u64(),
        Some(409)
    );
}

async fn mcp_claim_mission(app: Router, token: &str, mission_id: &str, agent_did: &str) -> Value {
    mcp_request(
        app,
        token,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "claim_mission",
                "arguments": {
                    "mission_id": mission_id,
                    "agent_did": agent_did
                }
            }
        }),
    )
    .await
}

fn append_bad_audit_row(data_dir: &std::path::Path) {
    use std::io::Write as _;

    let path = data_dir.join("audit/control_plane.jsonl");
    let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    file.write_all(b"{not-valid-audit-json}\n").unwrap();
}

async fn seed_mcp_gateway_remote_mission(
    data_dir: &std::path::Path,
    state: &ControlPlaneState,
    mission_id: &str,
) {
    seed_mcp_gateway_remote_mission_with_status(data_dir, state, mission_id, "published").await;
}

async fn seed_mcp_gateway_remote_mission_with_status(
    data_dir: &std::path::Path,
    state: &ControlPlaneState,
    mission_id: &str,
    status: &str,
) {
    let mut contract = state
        .swarm_bridge
        .sample_task_contract(mission_id)
        .await
        .unwrap();
    contract.task_type = "wattetheria.mission".to_string();
    contract.inputs = json!({
        "kind": "wattetheria_mission",
        "mission_id": mission_id,
        "publisher": "publisher-public",
        "publisher_agent_did": "did:agent:publisher",
        "publisher_display_name": "Remote Publisher",
        "publisher_wattswarm_node_id": "publisher-node",
        "domain": "trade",
        "swarm_scope": {"kind": "group", "id": mission_id},
        "mission_feed_key": "wattetheria.missions",
        "mission_scope_hint": format!("group:{mission_id}"),
        "reward": {"agent_watt": 10},
        "payload": {"work": "deliver"}
    });
    let gateway_task = json!({
        "id": mission_id,
        "task_id": mission_id,
        "task_type": "wattetheria.mission",
        "title": "Remote mission",
        "status": status,
        "source_node_id": "publisher-node",
        "publisher_wattswarm_node_id": "publisher-node",
        "mission_feed_key": "wattetheria.missions",
        "mission_scope_hint": format!("group:{mission_id}"),
        "task_contract": contract,
    });
    let gateway_app = Router::new().route(
        "/api/missions",
        get(move || {
            let gateway_task = gateway_task.clone();
            async move { Json(json!([gateway_task])) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let gateway_url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, gateway_app).await.unwrap();
    });
    std::fs::write(
        data_dir.join("config.json"),
        json!({"gateway_urls": [gateway_url]}).to_string(),
    )
    .unwrap();
}

fn assert_gateway_claim_route(mission: &Value, mission_id: &str, node_id: &str) {
    let scope_hint = format!("group:{mission_id}");
    assert_eq!(
        mission["publisher_wattswarm_node_id"].as_str(),
        Some(node_id)
    );
    assert_eq!(
        mission["mission_feed_key"].as_str(),
        Some("wattetheria.missions")
    );
    assert_eq!(
        mission["mission_scope_hint"].as_str(),
        Some(scope_hint.as_str())
    );
    assert_eq!(
        mission["swarm_scope"],
        json!({"kind": "group", "id": mission_id})
    );
    assert_eq!(mission["claim_route"]["task_id"].as_str(), Some(mission_id));
    assert_eq!(
        mission["claim_route"]["mission_id"].as_str(),
        Some(mission_id)
    );
    assert_eq!(
        mission["claim_route"]["publisher_wattswarm_node_id"].as_str(),
        Some(node_id)
    );
    assert_eq!(
        mission["claim_route"]["mission_scope_hint"].as_str(),
        Some(scope_hint.as_str())
    );
    assert_eq!(
        mission["claim_route"]["task_contract_available"].as_bool(),
        Some(true)
    );
    assert_eq!(mission["claim_route"]["claim_ready"].as_bool(), Some(true));
}

#[tokio::test]
async fn mcp_allows_tools_list_without_control_plane_auth_by_default() {
    let (_dir, app, _token, _policy, _state) = build_test_app(100);

    let response = app
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("content-type", "application/json")
                .body(axum::body::Body::from(
                    json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn mcp_allows_tools_call_without_control_plane_auth_by_default() {
    let (_dir, app, _token, _policy, _state) = build_test_app(100);

    let response = app
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("content-type", "application/json")
                .body(axum::body::Body::from(
                    json!({
                        "jsonrpc": "2.0",
                        "id": 1,
                        "method": "tools/call",
                        "params": {"name": "unknown_tool", "arguments": {}}
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn mcp_requires_control_plane_auth_when_configured() {
    let (_dir, _app, _token, _policy, mut state) = build_test_app(100);
    state.mcp_token_auth_required = true;
    let app = app(state);

    let response = app
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("content-type", "application/json")
                .body(axum::body::Body::from(
                    json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn mcp_tools_call_requires_control_plane_auth_when_configured() {
    let (_dir, _app, _token, _policy, mut state) = build_test_app(100);
    state.mcp_token_auth_required = true;
    let app = app(state);

    let response = app
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/mcp")
                .header("content-type", "application/json")
                .body(axum::body::Body::from(
                    json!({
                        "jsonrpc": "2.0",
                        "id": 1,
                        "method": "tools/call",
                        "params": {"name": "unknown_tool", "arguments": {}}
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
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

fn assert_schema_omits(tool: &Value, omitted: &[&str]) {
    let properties = tool["inputSchema"]["properties"].as_object().unwrap();
    for field in omitted {
        assert!(
            !properties.contains_key(*field),
            "expected {} schema to hide local identity field {field}",
            tool["name"].as_str().unwrap()
        );
    }
}

fn assert_public_geo_projection(value: &Value) {
    assert_eq!(value["lat"].as_f64(), Some(0.0));
    assert_eq!(value["lng"].as_f64(), Some(0.0));
    assert_eq!(value["coordinate_source"].as_str(), Some("derived"));
}

fn assert_public_geo_omitted(value: &Value) {
    assert!(value.get("lat").is_none());
    assert!(value.get("lng").is_none());
    assert!(value.get("coordinate_source").is_none());
}
