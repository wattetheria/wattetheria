use super::*;

#[derive(Clone)]
struct PaymentBindingFixture {
    agent_did: String,
    payment_address: String,
    binding: Value,
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
            "runtime": "wattetheria_adapter",
            "endpoint": {
                "url": "https://binding.example.com/a2a",
                "interaction_protocol": "a2a_v1",
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
