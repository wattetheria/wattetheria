use super::*;

const TEST_X402_TX_HASH: &str =
    "0x89c91c789e57059b17285e7ba1716a1f5ff4c5dace0ea5a5135f26158d0421b9";

const TEST_X402_RECIPIENT: &str = "0x2222222222222222222222222222222222222222";

const TEST_X402_BASE_SEPOLIA_USDC: &str = "0x036CbD53842c5426634e7929541eC2318f3dCF7e";

const TEST_X402_TRANSFER_TOPIC: &str =
    "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef";

async fn mock_x402_settle_rpc(
    axum::extract::State(sender_address): axum::extract::State<String>,
    Json(payload): Json<Value>,
) -> Json<Value> {
    let result = match payload["method"].as_str().unwrap_or_default() {
        "eth_chainId" => json!("0x14a34"),
        "eth_getTransactionReceipt" => json!({
            "transactionHash": TEST_X402_TX_HASH,
            "status": "0x1",
            "to": TEST_X402_BASE_SEPOLIA_USDC,
            "logs": [{
                "address": TEST_X402_BASE_SEPOLIA_USDC,
                "topics": [
                    TEST_X402_TRANSFER_TOPIC,
                    indexed_address_topic(&sender_address),
                    indexed_address_topic(TEST_X402_RECIPIENT)
                ],
                "data": u256_hex(2_500_000)
            }]
        }),
        method => json!({"unexpected": method}),
    };
    Json(json!({
        "jsonrpc": "2.0",
        "id": payload["id"].clone(),
        "result": result,
    }))
}

fn indexed_address_topic(address: &str) -> String {
    format!("0x{}{}", "0".repeat(24), address.trim_start_matches("0x"))
}

fn u256_hex(value: u128) -> String {
    format!("0x{value:064x}")
}

#[tokio::test]
async fn agent_payment_propose_persists_and_dispatches_direct_message() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let remote_identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge {
        public_bootstrap: false,
        fail_accept_and_finalize: false,
        local_node_id: identity.agent_did.clone(),
        agent_stats: BTreeMap::new(),
        network_status: SwarmNetworkStatusView {
            running: true,
            mode: "network".to_string(),
            peer_protocol_distribution: BTreeMap::new(),
        },
        peers: Vec::new(),
        discovered_agents: BTreeMap::new(),
        subscriptions: Mutex::new(Vec::new()),
        messages: Mutex::new(Vec::new()),
        relationship_views: Mutex::new(Vec::new()),
        relationship_commands: Mutex::new(Vec::new()),
        dm_threads: Mutex::new(Vec::new()),
        dm_messages: Mutex::new(BTreeMap::new()),
        dm_commands: Mutex::new(Vec::new()),
        private_hive_key_share_commands: Mutex::new(Vec::new()),
        payment_commands: Mutex::new(Vec::new()),
    });
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, app, token, _, state) =
        build_test_app_with_bridge(20, dir, identity.clone(), event_log, bridge_handle);

    let local_public_id = bootstrap_broker_identity(app.clone(), &token, &identity.agent_did).await;
    let remote_public_id = scoped_id("broker-borealis", &remote_identity.agent_did);
    {
        let mut identities = state.public_identity_registry.lock().await;
        identities
            .upsert(
                &remote_public_id,
                "Broker Borealis".to_string(),
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
            Some("12D3KooRemotePeer".to_string()),
            wattetheria_kernel::civilization::identities::OwnershipScope::External,
            true,
        );
    }

    let response = authed_post_json(
        app.clone(),
        &token,
        "/v1/wattetheria/payments/agent-payments/propose",
        json!({
            "public_id": local_public_id,
            "counterpart_public_id": remote_public_id,
            "amount": "2500000",
            "currency": "USDT",
            "rail": "x402",
            "layer": "web3",
            "network": "base-sepolia",
            "recipient_address": TEST_X402_RECIPIENT,
            "description": "task reward",
        }),
    )
    .await;

    assert_eq!(response["ok"].as_bool(), Some(true));
    assert_eq!(
        response["payment"]["recipient_public_id"].as_str(),
        Some(remote_public_id.as_str())
    );

    let ledger = state.payment_ledger.lock().await;
    assert_eq!(ledger.len(), 1);
    let payment_id = response["payment"]["payment_id"].as_str().unwrap();
    assert_eq!(
        ledger.get(payment_id).unwrap().status,
        wattetheria_kernel::payments::PaymentStatus::Proposed
    );
    drop(ledger);

    let payment_commands = bridge.payment_commands.lock().await;
    assert_eq!(payment_commands.len(), 1);
    assert_eq!(payment_commands[0].remote_node_id, "12D3KooRemotePeer");
    assert_eq!(payment_commands[0].message_kind, "payment_request");
    assert_eq!(
        payment_commands[0].payment["currency"].as_str(),
        Some("USDT")
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn agent_payment_authorize_signs_with_active_payment_account() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let remote_identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge {
        public_bootstrap: false,
        fail_accept_and_finalize: false,
        local_node_id: identity.agent_did.clone(),
        agent_stats: BTreeMap::new(),
        network_status: SwarmNetworkStatusView {
            running: true,
            mode: "network".to_string(),
            peer_protocol_distribution: BTreeMap::new(),
        },
        peers: Vec::new(),
        discovered_agents: BTreeMap::new(),
        subscriptions: Mutex::new(Vec::new()),
        messages: Mutex::new(Vec::new()),
        relationship_views: Mutex::new(Vec::new()),
        relationship_commands: Mutex::new(Vec::new()),
        dm_threads: Mutex::new(Vec::new()),
        dm_messages: Mutex::new(BTreeMap::new()),
        dm_commands: Mutex::new(Vec::new()),
        private_hive_key_share_commands: Mutex::new(Vec::new()),
        payment_commands: Mutex::new(Vec::new()),
    });
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, app, token, _, state) =
        build_test_app_with_bridge(20, dir, identity.clone(), event_log, bridge_handle);

    let local_public_id = bootstrap_broker_identity(app.clone(), &token, &identity.agent_did).await;
    let remote_public_id = scoped_id("broker-borealis", &remote_identity.agent_did);
    {
        let mut identities = state.public_identity_registry.lock().await;
        identities
            .upsert(
                &remote_public_id,
                "Broker Borealis".to_string(),
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
            Some("12D3KooRemotePeer".to_string()),
            wattetheria_kernel::civilization::identities::OwnershipScope::External,
            true,
        );
    }
    let sender_address = seed_active_payment_account(&state);

    let proposed = authed_post_json(
        app.clone(),
        &token,
        "/v1/wattetheria/payments/agent-payments/propose",
        json!({
            "public_id": local_public_id,
            "counterpart_public_id": remote_public_id,
            "amount": "2500000",
            "currency": "USDC",
            "rail": "x402",
            "layer": "web3",
            "network": "base-sepolia",
            "recipient_address": TEST_X402_RECIPIENT,
        }),
    )
    .await;
    let payment_id = proposed["payment"]["payment_id"]
        .as_str()
        .unwrap()
        .to_string();

    let authorized = authed_post_json(
        app.clone(),
        &token,
        &format!("/v1/wattetheria/payments/agent-payments/{payment_id}/authorize"),
        json!({}),
    )
    .await;

    assert_eq!(authorized["status"].as_str(), Some("authorized"));
    assert_eq!(
        authorized["sender_address"].as_str(),
        Some(sender_address.as_str())
    );
    assert!(authorized["authorization_signature"].is_string());
    assert!(authorized["authorization_public_key"].is_string());

    let payment_commands = bridge.payment_commands.lock().await;
    assert_eq!(payment_commands.len(), 2);
    assert_eq!(payment_commands[1].message_kind, "payment_authorized");
    assert_eq!(
        payment_commands[1].payment["sender_address"].as_str(),
        Some(sender_address.as_str())
    );
    drop(payment_commands);

    let submitted = authed_post_json(
        app.clone(),
        &token,
        &format!("/v1/wattetheria/payments/agent-payments/{payment_id}/submit"),
        json!({
            "settlement_receipt": {
                "success": true,
                "payer": sender_address,
                "transaction": "0x89c91c789e57059b17285e7ba1716a1f5ff4c5dace0ea5a5135f26158d0421b9",
                "network": "eip155:84532",
                "amount": "2500000"
            }
        }),
    )
    .await;

    assert_eq!(submitted["status"].as_str(), Some("submitted"));
    assert_eq!(
        submitted["settlement_receipt"]["transaction"].as_str(),
        Some("0x89c91c789e57059b17285e7ba1716a1f5ff4c5dace0ea5a5135f26158d0421b9")
    );
    let payment_commands = bridge.payment_commands.lock().await;
    assert_eq!(payment_commands.len(), 3);
    assert_eq!(payment_commands[2].message_kind, "payment_submitted");
    drop(payment_commands);

    let proposed_mismatch = authed_post_json(
        app.clone(),
        &token,
        "/v1/wattetheria/payments/agent-payments/propose",
        json!({
            "public_id": local_public_id,
            "counterpart_public_id": remote_public_id,
            "amount": "3000000",
            "currency": "USDT",
            "rail": "x402",
            "layer": "web3",
            "network": "base-sepolia",
            "recipient_address": TEST_X402_RECIPIENT,
        }),
    )
    .await;
    let mismatch_payment_id = proposed_mismatch["payment"]["payment_id"]
        .as_str()
        .unwrap()
        .to_string();
    let mismatch_status = authed_post(
        app,
        &token,
        &format!("/v1/wattetheria/payments/agent-payments/{mismatch_payment_id}/authorize"),
        json!({"sender_address": "0x0000000000000000000000000000000000000000"}),
    )
    .await;
    assert_eq!(mismatch_status, StatusCode::FORBIDDEN);

    let ledger = state.payment_ledger.lock().await;
    assert_eq!(
        ledger.get(&mismatch_payment_id).unwrap().status,
        wattetheria_kernel::payments::PaymentStatus::Proposed
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn agent_payment_settle_validates_x402_receipt_before_persisting() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let remote_identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge {
        public_bootstrap: false,
        fail_accept_and_finalize: false,
        local_node_id: identity.agent_did.clone(),
        agent_stats: BTreeMap::new(),
        network_status: SwarmNetworkStatusView {
            running: true,
            mode: "network".to_string(),
            peer_protocol_distribution: BTreeMap::new(),
        },
        peers: Vec::new(),
        discovered_agents: BTreeMap::new(),
        subscriptions: Mutex::new(Vec::new()),
        messages: Mutex::new(Vec::new()),
        relationship_views: Mutex::new(Vec::new()),
        relationship_commands: Mutex::new(Vec::new()),
        dm_threads: Mutex::new(Vec::new()),
        dm_messages: Mutex::new(BTreeMap::new()),
        dm_commands: Mutex::new(Vec::new()),
        private_hive_key_share_commands: Mutex::new(Vec::new()),
        payment_commands: Mutex::new(Vec::new()),
    });
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, app, token, _, state) =
        build_test_app_with_bridge(20, dir, identity.clone(), event_log, bridge_handle);

    let local_public_id = bootstrap_broker_identity(app.clone(), &token, &identity.agent_did).await;
    let remote_public_id = scoped_id("broker-borealis", &remote_identity.agent_did);
    {
        let mut identities = state.public_identity_registry.lock().await;
        identities
            .upsert(
                &remote_public_id,
                "Broker Borealis".to_string(),
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
            Some("12D3KooRemotePeer".to_string()),
            wattetheria_kernel::civilization::identities::OwnershipScope::External,
            true,
        );
    }
    let sender_address = seed_active_payment_account(&state);

    let proposed = authed_post_json(
        app.clone(),
        &token,
        "/v1/wattetheria/payments/agent-payments/propose",
        json!({
            "public_id": local_public_id,
            "counterpart_public_id": remote_public_id,
            "amount": "2500000",
            "currency": "USDC",
            "rail": "x402",
            "layer": "web3",
            "network": "base-sepolia",
            "recipient_address": TEST_X402_RECIPIENT,
        }),
    )
    .await;
    let payment_id = proposed["payment"]["payment_id"]
        .as_str()
        .unwrap()
        .to_string();

    let authorized = authed_post_json(
        app.clone(),
        &token,
        &format!("/v1/wattetheria/payments/agent-payments/{payment_id}/authorize"),
        json!({}),
    )
    .await;
    assert_eq!(authorized["status"].as_str(), Some("authorized"));

    let invalid_status = authed_post(
        app.clone(),
        &token,
        &format!("/v1/wattetheria/payments/agent-payments/{payment_id}/settle"),
        json!({
            "settlement_receipt": {
                "success": true,
                "payer": sender_address,
                "transaction": "0x89c91c789e57059b17285e7ba1716a1f5ff4c5dace0ea5a5135f26158d0421b9",
                "network": "base-sepolia",
                "amount": "1"
            }
        }),
    )
    .await;
    assert_eq!(invalid_status, StatusCode::BAD_REQUEST);
    {
        let ledger = state.payment_ledger.lock().await;
        assert_eq!(
            ledger.get(&payment_id).unwrap().status,
            wattetheria_kernel::payments::PaymentStatus::Authorized
        );
    }

    let rpc_app = Router::new()
        .route("/", post(mock_x402_settle_rpc))
        .with_state(sender_address.clone());
    let rpc_listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind x402 rpc listener");
    let rpc_addr = rpc_listener.local_addr().expect("x402 rpc addr");
    let rpc_server = tokio::spawn(async move {
        axum::serve(rpc_listener, rpc_app)
            .await
            .expect("serve x402 rpc mock");
    });
    let _rpc_url_guard =
        crate::routes::payment_chain::set_test_base_sepolia_rpc_url(format!("http://{rpc_addr}"));

    let settled = authed_post_json(
        app.clone(),
        &token,
        &format!("/v1/wattetheria/payments/agent-payments/{payment_id}/settle"),
        json!({
            "settlement_receipt": {
                "success": true,
                "payer": sender_address,
                "transaction": TEST_X402_TX_HASH,
                "network": "eip155:84532",
                "amount": "2500000",
                "payTo": TEST_X402_RECIPIENT
            }
        }),
    )
    .await;
    assert_eq!(
        settled["status"].as_str(),
        Some("settled"),
        "expected settled response, got {settled}"
    );

    let payment_commands = bridge.payment_commands.lock().await;
    assert_eq!(payment_commands.len(), 3);
    assert_eq!(payment_commands[2].message_kind, "payment_settled");
    rpc_server.abort();
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn agent_payments_list_reads_synced_inbound_payment_request() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let remote_identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let remote_node_id = "12D3KooRemotePeer".to_string();
    let local_public_id = scoped_id("captain-aurora", &identity.agent_did);
    let remote_public_id = scoped_id("broker-borealis", &remote_identity.agent_did);
    let bridge = Arc::new(MockSwarmBridge {
        public_bootstrap: false,
        fail_accept_and_finalize: false,
        local_node_id: identity.agent_did.clone(),
        agent_stats: BTreeMap::new(),
        network_status: SwarmNetworkStatusView {
            running: true,
            mode: "network".to_string(),
            peer_protocol_distribution: BTreeMap::new(),
        },
        peers: Vec::new(),
        discovered_agents: BTreeMap::new(),
        subscriptions: Mutex::new(Vec::new()),
        messages: Mutex::new(Vec::new()),
        relationship_views: Mutex::new(Vec::new()),
        relationship_commands: Mutex::new(Vec::new()),
        dm_threads: Mutex::new(Vec::new()),
        dm_messages: Mutex::new(BTreeMap::new()),
        dm_commands: Mutex::new(Vec::new()),
        private_hive_key_share_commands: Mutex::new(Vec::new()),
        payment_commands: Mutex::new(Vec::new()),
    });
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, app, token, _, state) =
        build_test_app_with_bridge(20, dir, identity.clone(), event_log, bridge_handle);
    let _bootstrapped = bootstrap_broker_identity(app.clone(), &token, &identity.agent_did).await;
    {
        let mut identities = state.public_identity_registry.lock().await;
        identities
            .upsert(
                &remote_public_id,
                "Broker Borealis".to_string(),
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
    {
        let mut ledger = state.payment_ledger.lock().await;
        let _ = ledger.merge_remote_transaction(wattetheria_kernel::payments::PaymentTransaction {
            payment_id: "payment-remote-1".to_string(),
            sender_did: remote_identity.agent_did.clone(),
            recipient_did: identity.agent_did.clone(),
            sender_public_id: remote_public_id.clone(),
            recipient_public_id: local_public_id.clone(),
            remote_node_id: remote_node_id.clone(),
            amount: "990000".to_string(),
            currency: "USDT".to_string(),
            rail: "x402".to_string(),
            layer: wattetheria_kernel::payments::SettlementLayer::Web3,
            network: Some("base-sepolia".to_string()),
            sender_address: None,
            recipient_address: Some("0xreceiver".to_string()),
            mission_id: None,
            task_id: Some("task-42".to_string()),
            description: Some("inbound reward".to_string()),
            metadata: None,
            status: wattetheria_kernel::payments::PaymentStatus::Proposed,
            authorization_signature: None,
            authorization_public_key: None,
            settlement_receipt: None,
            reject_reason: None,
            proposed_at: 10,
            authorized_at: None,
            settled_at: None,
            expires_at: None,
        });
    }

    let payments = authed_get_json(
        app.clone(),
        &token,
        &format!("/v1/wattetheria/payments/agent-payments?public_id={local_public_id}"),
    )
    .await;

    assert_eq!(payments["count"].as_u64(), Some(1));
    assert_eq!(
        payments["items"][0]["payment_id"].as_str(),
        Some("payment-remote-1")
    );
    assert_eq!(
        payments["items"][0]["recipient_public_id"].as_str(),
        Some(local_public_id.as_str())
    );

    let ledger = state.payment_ledger.lock().await;
    assert!(ledger.get("payment-remote-1").is_some());
}

#[allow(clippy::too_many_lines)]
#[tokio::test]
async fn agent_action_commit_routes_payment_authorize_to_ledger_update() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let remote_identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge {
        public_bootstrap: false,
        fail_accept_and_finalize: false,
        local_node_id: identity.agent_did.clone(),
        agent_stats: BTreeMap::new(),
        network_status: SwarmNetworkStatusView {
            running: true,
            mode: "network".to_string(),
            peer_protocol_distribution: BTreeMap::new(),
        },
        peers: Vec::new(),
        discovered_agents: BTreeMap::new(),
        subscriptions: Mutex::new(Vec::new()),
        messages: Mutex::new(Vec::new()),
        relationship_views: Mutex::new(Vec::new()),
        relationship_commands: Mutex::new(Vec::new()),
        dm_threads: Mutex::new(Vec::new()),
        dm_messages: Mutex::new(BTreeMap::new()),
        dm_commands: Mutex::new(Vec::new()),
        private_hive_key_share_commands: Mutex::new(Vec::new()),
        payment_commands: Mutex::new(Vec::new()),
    });
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, app, token, _, state) =
        build_test_app_with_bridge(20, dir, identity.clone(), event_log, bridge_handle);

    let local_public_id = bootstrap_broker_identity(app.clone(), &token, &identity.agent_did).await;
    let remote_public_id = scoped_id("broker-borealis", &remote_identity.agent_did);
    {
        let mut identities = state.public_identity_registry.lock().await;
        identities
            .upsert(
                &remote_public_id,
                "Broker Borealis".to_string(),
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
            Some("12D3KooRemotePeer".to_string()),
            wattetheria_kernel::civilization::identities::OwnershipScope::External,
            true,
        );
    }
    let sender_address = seed_active_payment_account(&state);
    let proposed = authed_post_json(
        app.clone(),
        &token,
        "/v1/wattetheria/payments/agent-payments/propose",
        json!({
            "public_id": local_public_id,
            "counterpart_public_id": remote_public_id,
            "amount": "2500000",
            "currency": "USDT",
            "rail": "x402",
            "layer": "web3",
            "network": "base-sepolia",
            "recipient_address": TEST_X402_RECIPIENT,
        }),
    )
    .await;
    let payment_id = proposed["payment"]["payment_id"]
        .as_str()
        .unwrap()
        .to_string();

    let committed = authed_post_json_with_headers(
        app.clone(),
        &token,
        "/v1/agent-actions/commit",
        json!({
            "event": {
                "event_id": "evt-payment-1",
                "event_type": "payment_request",
                "source_kind": "payment_summary",
                "source_node_id": "12D3KooRemotePeer",
                "payload": {
                    "payment": {
                        "payment_id": payment_id,
                    }
                },
                "requires_commit": true
            },
            "decision": {
                "decision_id": "dec-payment-1",
                "action": "authorize",
                "route": "wattetheria_commit",
                "payload": {}
            }
        }),
        &[
            ("x-agent-event-id", "evt-payment-1"),
            ("x-agent-decision-id", "dec-payment-1"),
        ],
    )
    .await;

    assert_eq!(committed["status"].as_str(), Some("authorized"));
    assert_eq!(
        committed["sender_address"].as_str(),
        Some(sender_address.as_str())
    );
    let ledger = state.payment_ledger.lock().await;
    assert_eq!(
        ledger.get(&payment_id).unwrap().status,
        wattetheria_kernel::payments::PaymentStatus::Authorized
    );
}

#[allow(clippy::too_many_lines)]
#[tokio::test]
async fn agent_action_commit_payment_authorize_uses_ledger_remote_node_without_controller_binding()
{
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let remote_identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge {
        public_bootstrap: false,
        fail_accept_and_finalize: false,
        local_node_id: identity.agent_did.clone(),
        agent_stats: BTreeMap::new(),
        network_status: SwarmNetworkStatusView {
            running: true,
            mode: "network".to_string(),
            peer_protocol_distribution: BTreeMap::new(),
        },
        peers: Vec::new(),
        discovered_agents: BTreeMap::new(),
        subscriptions: Mutex::new(Vec::new()),
        messages: Mutex::new(Vec::new()),
        relationship_views: Mutex::new(Vec::new()),
        relationship_commands: Mutex::new(Vec::new()),
        dm_threads: Mutex::new(Vec::new()),
        dm_messages: Mutex::new(BTreeMap::new()),
        dm_commands: Mutex::new(Vec::new()),
        private_hive_key_share_commands: Mutex::new(Vec::new()),
        payment_commands: Mutex::new(Vec::new()),
    });
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, app, token, _, state) =
        build_test_app_with_bridge(20, dir, identity.clone(), event_log, bridge_handle);

    let local_public_id = bootstrap_broker_identity(app.clone(), &token, &identity.agent_did).await;
    let remote_public_id = scoped_id("broker-borealis", &remote_identity.agent_did);
    let sender_address = seed_active_payment_account(&state);
    let payment_id = {
        let mut ledger = state.payment_ledger.lock().await;
        ledger
            .propose(
                &identity.agent_did,
                wattetheria_kernel::payments::ProposePaymentRequest {
                    sender_public_id: local_public_id.clone(),
                    remote_node_id: "12D3KooRemotePeer".to_string(),
                    recipient_public_id: remote_public_id.clone(),
                    recipient_did: remote_identity.agent_did.clone(),
                    amount: "2500000".to_string(),
                    currency: "USDT".to_string(),
                    rail: "x402".to_string(),
                    layer: wattetheria_kernel::payments::SettlementLayer::Web3,
                    network: Some("base-sepolia".to_string()),
                    recipient_address: None,
                    mission_id: None,
                    task_id: None,
                    description: None,
                    metadata: None,
                    expires_at: None,
                },
            )
            .unwrap()
            .payment_id
    };

    let committed = authed_post_json_with_headers(
        app.clone(),
        &token,
        "/v1/agent-actions/commit",
        json!({
            "event": {
                "event_id": "evt-payment-no-binding-1",
                "event_type": "payment_request",
                "source_kind": "payment_summary",
                "source_node_id": "12D3KooRemotePeer",
                "payload": {
                    "payment": {
                        "payment_id": payment_id,
                    }
                },
                "requires_commit": true
            },
            "decision": {
                "decision_id": "dec-payment-no-binding-1",
                "action": "authorize",
                "route": "wattetheria_commit",
                "payload": {}
            }
        }),
        &[
            ("x-agent-event-id", "evt-payment-no-binding-1"),
            ("x-agent-decision-id", "dec-payment-no-binding-1"),
        ],
    )
    .await;

    assert_eq!(committed["status"].as_str(), Some("authorized"));
    assert_eq!(
        committed["sender_address"].as_str(),
        Some(sender_address.as_str())
    );
    let payment_commands = bridge.payment_commands.lock().await;
    assert_eq!(payment_commands.len(), 1);
    assert_eq!(payment_commands[0].remote_node_id, "12D3KooRemotePeer");
    assert_eq!(
        payment_commands[0].agent_envelope.target_node_id.as_deref(),
        Some("12D3KooRemotePeer")
    );
    assert_eq!(
        payment_commands[0]
            .agent_envelope
            .target_agent_id
            .as_deref(),
        Some(remote_identity.agent_did.as_str())
    );
}
