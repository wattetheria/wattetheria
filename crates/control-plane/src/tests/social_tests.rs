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

#[allow(clippy::too_many_lines)]
#[tokio::test]
async fn agent_social_routes_sign_and_forward_friend_and_dm_commands() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let remote_identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge {
        fail_accept_and_finalize: false,
        local_node_id: identity.agent_did.clone(),
        agent_stats: BTreeMap::new(),
        network_status: SwarmNetworkStatusView {
            running: true,
            mode: "network".to_string(),
            peer_protocol_distribution: BTreeMap::new(),
        },
        peers: Vec::new(),
        subscriptions: Mutex::new(Vec::new()),
        messages: Mutex::new(Vec::new()),
        relationship_views: Mutex::new(Vec::new()),
        relationship_commands: Mutex::new(Vec::new()),
        dm_threads: Mutex::new(Vec::new()),
        dm_messages: Mutex::new(BTreeMap::new()),
        dm_commands: Mutex::new(Vec::new()),
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

    let relationship_response = authed_post_json(
        app.clone(),
        &token,
        "/v1/wattetheria/social/agent-friends",
        json!({
            "public_id": local_public_id,
            "counterpart_public_id": remote_public_id,
            "action": "request",
            "message": {
                "kind": "friend_request",
                "text": "connect with me"
            },
            "extensions": {
                "source": "product"
            }
        }),
    )
    .await;
    assert_eq!(relationship_response["ok"].as_bool(), Some(true));

    let relationship_commands = bridge.relationship_commands.lock().await;
    assert_eq!(relationship_commands.len(), 1);
    let relationship_command = &relationship_commands[0];
    assert_eq!(relationship_command.remote_node_id, "12D3KooRemotePeer");
    assert_eq!(
        relationship_command.agent_envelope.capability.as_deref(),
        Some("social.friend.request")
    );
    assert_eq!(
        relationship_command
            .agent_envelope
            .source_agent_id
            .as_deref(),
        Some(identity.agent_did.as_str())
    );
    assert_eq!(
        relationship_command
            .agent_envelope
            .source_agent_card
            .as_ref()
            .and_then(|card| card.card["metadata"]["display_name"].as_str()),
        Some("Captain Aurora")
    );
    assert_eq!(
        relationship_command
            .agent_envelope
            .source_agent_card
            .as_ref()
            .and_then(|card| card.card["metadata"]["agent_id"].as_str()),
        Some(identity.agent_did.as_str())
    );
    assert_eq!(
        relationship_command
            .agent_envelope
            .target_agent_id
            .as_deref(),
        Some(remote_identity.agent_did.as_str())
    );
    assert_envelope_signature_valid(
        &relationship_command.agent_envelope,
        &state.identity.public_key,
    );
    drop(relationship_commands);

    friendship_service::upsert_friendship(
        &*state.social_store,
        &wattetheria_social::domain::friendships::Friendship {
            friendship_id: format!("friendship:{local_public_id}:{remote_public_id}"),
            local_public_id: local_public_id.clone(),
            remote_public_id: remote_public_id.clone(),
            state: wattetheria_social::domain::friendships::FriendshipState::Active,
            established_from_request_id: None,
            thread_id: None,
            created_at: 1,
            updated_at: 1,
        },
    )
    .expect("seed active friendship for dm policy");

    let dm_response = authed_post_json(
        app.clone(),
        &token,
        "/v1/wattetheria/social/agent-dm/messages",
        json!({
            "public_id": local_public_id,
            "counterpart_public_id": remote_public_id,
            "content": {
                "type": "text",
                "text": "hello from wattetheria"
            },
            "extensions": {
                "conversation_hint": "friendship"
            }
        }),
    )
    .await;
    assert_eq!(dm_response["ok"].as_bool(), Some(true));

    let dm_commands = bridge.dm_commands.lock().await;
    assert_eq!(dm_commands.len(), 1);
    let dm_command = &dm_commands[0];
    assert_eq!(dm_command.remote_node_id, "12D3KooRemotePeer");
    assert_eq!(
        dm_command.agent_envelope.capability.as_deref(),
        Some("social.dm.send")
    );
    assert_eq!(
        dm_command
            .agent_envelope
            .source_agent_card
            .as_ref()
            .and_then(|card| card.card["metadata"]["display_name"].as_str()),
        Some("Captain Aurora")
    );
    assert_eq!(
        dm_command.agent_envelope.source_agent_id.as_deref(),
        Some(identity.agent_did.as_str())
    );
    assert_envelope_signature_valid(&dm_command.agent_envelope, &state.identity.public_key);

    let friend_requests =
        friend_request_service::list_friend_requests(&*state.social_store, &local_public_id)
            .expect("list persisted friend requests");
    assert_eq!(friend_requests.len(), 1);
    assert_eq!(friend_requests[0].remote_public_id, remote_public_id);

    let threads = thread_service::list_threads(&*state.social_store, &local_public_id)
        .expect("list persisted dm threads");
    assert_eq!(threads.len(), 1);
    assert_eq!(threads[0].remote_public_id, remote_public_id);

    let messages =
        message_service::list_thread_messages(&*state.social_store, &threads[0].thread_id)
            .expect("list persisted dm messages");
    assert_eq!(messages.len(), 1);
    assert_eq!(
        messages[0].content_json["text"].as_str(),
        Some("hello from wattetheria")
    );

    let receipts =
        receipt_service::list_message_receipts(&*state.social_store, &messages[0].message_id)
            .expect("list persisted dm receipts");
    assert_eq!(receipts.len(), 1);

    let relationship_items = authed_get_json(
        app.clone(),
        &token,
        &format!("/v1/wattetheria/social/agent-friends?public_id={local_public_id}"),
    )
    .await;
    assert_eq!(relationship_items.as_array().unwrap().len(), 1);
    assert_eq!(
        relationship_items[0]["counterpart_public_id"].as_str(),
        Some(remote_public_id.as_str())
    );

    let thread_items = authed_get_json(
        app.clone(),
        &token,
        &format!("/v1/wattetheria/social/agent-dm/threads?public_id={local_public_id}"),
    )
    .await;
    assert_eq!(thread_items.as_array().unwrap().len(), 1);
    assert_eq!(
        thread_items[0]["counterpart_public_id"].as_str(),
        Some(remote_public_id.as_str())
    );

    let message_items = authed_get_json(
        app,
        &token,
        &format!("/v1/wattetheria/social/agent-dm/messages?public_id={local_public_id}"),
    )
    .await;
    assert_eq!(message_items.as_array().unwrap().len(), 1);
    assert_eq!(
        message_items[0]["content"]["text"].as_str(),
        Some("hello from wattetheria")
    );
}
#[tokio::test]
async fn agent_payment_propose_persists_and_dispatches_direct_message() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let remote_identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge {
        fail_accept_and_finalize: false,
        local_node_id: identity.agent_did.clone(),
        agent_stats: BTreeMap::new(),
        network_status: SwarmNetworkStatusView {
            running: true,
            mode: "network".to_string(),
            peer_protocol_distribution: BTreeMap::new(),
        },
        peers: Vec::new(),
        subscriptions: Mutex::new(Vec::new()),
        messages: Mutex::new(Vec::new()),
        relationship_views: Mutex::new(Vec::new()),
        relationship_commands: Mutex::new(Vec::new()),
        dm_threads: Mutex::new(Vec::new()),
        dm_messages: Mutex::new(BTreeMap::new()),
        dm_commands: Mutex::new(Vec::new()),
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
        fail_accept_and_finalize: false,
        local_node_id: identity.agent_did.clone(),
        agent_stats: BTreeMap::new(),
        network_status: SwarmNetworkStatusView {
            running: true,
            mode: "network".to_string(),
            peer_protocol_distribution: BTreeMap::new(),
        },
        peers: Vec::new(),
        subscriptions: Mutex::new(Vec::new()),
        messages: Mutex::new(Vec::new()),
        relationship_views: Mutex::new(Vec::new()),
        relationship_commands: Mutex::new(Vec::new()),
        dm_threads: Mutex::new(Vec::new()),
        dm_messages: Mutex::new(BTreeMap::new()),
        dm_commands: Mutex::new(Vec::new()),
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
        fail_accept_and_finalize: false,
        local_node_id: identity.agent_did.clone(),
        agent_stats: BTreeMap::new(),
        network_status: SwarmNetworkStatusView {
            running: true,
            mode: "network".to_string(),
            peer_protocol_distribution: BTreeMap::new(),
        },
        peers: Vec::new(),
        subscriptions: Mutex::new(Vec::new()),
        messages: Mutex::new(Vec::new()),
        relationship_views: Mutex::new(Vec::new()),
        relationship_commands: Mutex::new(Vec::new()),
        dm_threads: Mutex::new(Vec::new()),
        dm_messages: Mutex::new(BTreeMap::new()),
        dm_commands: Mutex::new(Vec::new()),
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
        fail_accept_and_finalize: false,
        local_node_id: identity.agent_did.clone(),
        agent_stats: BTreeMap::new(),
        network_status: SwarmNetworkStatusView {
            running: true,
            mode: "network".to_string(),
            peer_protocol_distribution: BTreeMap::new(),
        },
        peers: Vec::new(),
        subscriptions: Mutex::new(Vec::new()),
        messages: Mutex::new(Vec::new()),
        relationship_views: Mutex::new(Vec::new()),
        relationship_commands: Mutex::new(Vec::new()),
        dm_threads: Mutex::new(Vec::new()),
        dm_messages: Mutex::new(BTreeMap::new()),
        dm_commands: Mutex::new(Vec::new()),
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

#[tokio::test]
async fn agent_action_commit_routes_social_block_to_wattetheria_state() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let remote_identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge::default_for(identity.agent_did.clone()));
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

    let committed = authed_post_json_with_headers(
        app.clone(),
        &token,
        "/v1/agent-actions/commit",
        json!({
            "event": {
                "event_id": "evt-friend-1",
                "event_type": "friend_request",
                "source_kind": "peer_relationship",
                "source_node_id": "12D3KooRemotePeer",
                "target_agent_id": identity.agent_did,
                "payload": {
                    "agent_envelope": {
                        "message": {
                            "source_public_id": remote_public_id,
                            "target_public_id": local_public_id,
                        }
                    }
                },
                "requires_commit": true
            },
            "decision": {
                "decision_id": "dec-friend-1",
                "action": "block",
                "route": "wattetheria_commit",
                "payload": {
                    "message": {"kind": "friend_request", "text": "blocked"}
                }
            }
        }),
        &[
            ("x-agent-event-id", "evt-friend-1"),
            ("x-agent-decision-id", "dec-friend-1"),
        ],
    )
    .await;

    assert_eq!(committed["ok"].as_bool(), Some(true));
    let blocks = block_service::list_blocks(&*state.social_store, &local_public_id)
        .expect("list social blocks");
    assert_eq!(blocks.len(), 1);
    assert_eq!(blocks[0].blocked_public_id, remote_public_id);

    let relationship_commands = bridge.relationship_commands.lock().await;
    assert_eq!(relationship_commands.len(), 1);
    assert_eq!(
        relationship_commands[0].action,
        wattetheria_kernel::swarm_bridge::SwarmRelationshipAction::Block
    );
}

#[allow(clippy::too_many_lines)]
#[tokio::test]
async fn agent_action_commit_routes_payment_authorize_to_ledger_update() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let remote_identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge {
        fail_accept_and_finalize: false,
        local_node_id: identity.agent_did.clone(),
        agent_stats: BTreeMap::new(),
        network_status: SwarmNetworkStatusView {
            running: true,
            mode: "network".to_string(),
            peer_protocol_distribution: BTreeMap::new(),
        },
        peers: Vec::new(),
        subscriptions: Mutex::new(Vec::new()),
        messages: Mutex::new(Vec::new()),
        relationship_views: Mutex::new(Vec::new()),
        relationship_commands: Mutex::new(Vec::new()),
        dm_threads: Mutex::new(Vec::new()),
        dm_messages: Mutex::new(BTreeMap::new()),
        dm_commands: Mutex::new(Vec::new()),
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

#[tokio::test]
async fn agent_action_commit_routes_topic_reply_through_swarm_bridge() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge::default_for(identity.agent_did.clone()));
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, app, token, _, _state) =
        build_test_app_with_bridge(20, dir, identity.clone(), event_log, bridge_handle);

    let committed = authed_post_json_with_headers(
        app.clone(),
        &token,
        "/v1/agent-actions/commit",
        json!({
            "event": {
                "event_id": "evt-topic-1",
                "event_type": "topic_message_requires_reply",
                "source_kind": "topic_message",
                "source_node_id": "12D3KooRemotePeer",
                "payload": {
                    "network_id": "local:test",
                    "feed_key": "crew.chat",
                    "scope_hint": "group:crew-7",
                    "message_id": "msg-remote-1"
                },
                "requires_commit": false
            },
            "decision": {
                "decision_id": "dec-topic-1",
                "action": "reply",
                "route": "wattetheria_commit",
                "payload": {
                    "content": {
                        "kind": "message",
                        "text": "roger that"
                    }
                }
            }
        }),
        &[
            ("x-agent-event-id", "evt-topic-1"),
            ("x-agent-decision-id", "dec-topic-1"),
        ],
    )
    .await;

    assert_eq!(committed["ok"].as_bool(), Some(true));
    let messages = bridge.messages.lock().await;
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].feed_key, "crew.chat");
    assert_eq!(
        messages[0].reply_to_message_id.as_deref(),
        Some("msg-remote-1")
    );
    assert_eq!(messages[0].content["text"].as_str(), Some("roger that"));
}

#[tokio::test]
async fn agent_friend_request_is_denied_when_counterpart_is_blocked() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let remote_identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge {
        fail_accept_and_finalize: false,
        local_node_id: identity.agent_did.clone(),
        agent_stats: BTreeMap::new(),
        network_status: SwarmNetworkStatusView {
            running: true,
            mode: "network".to_string(),
            peer_protocol_distribution: BTreeMap::new(),
        },
        peers: Vec::new(),
        subscriptions: Mutex::new(Vec::new()),
        messages: Mutex::new(Vec::new()),
        relationship_views: Mutex::new(Vec::new()),
        relationship_commands: Mutex::new(Vec::new()),
        dm_threads: Mutex::new(Vec::new()),
        dm_messages: Mutex::new(BTreeMap::new()),
        dm_commands: Mutex::new(Vec::new()),
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
    block_service::upsert_block(
        &*state.social_store,
        &wattetheria_social::domain::blocks::SocialBlock {
            block_id: "block:alice:borealis".to_string(),
            owner_public_id: local_public_id.clone(),
            blocked_public_id: remote_public_id.clone(),
            blocked_node_id: Some("12D3KooRemotePeer".to_string()),
            reason: Some("blocked".to_string()),
            created_at: 1,
            updated_at: 1,
        },
    )
    .unwrap();

    let status = authed_post(
        app,
        &token,
        "/v1/wattetheria/social/agent-friends",
        json!({
            "public_id": local_public_id,
            "counterpart_public_id": remote_public_id,
            "action": "request",
            "message": {
                "kind": "friend_request",
                "text": "connect with me"
            }
        }),
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(bridge.relationship_commands.lock().await.is_empty());
}

#[tokio::test]
async fn agent_friends_status_uses_social_transport_binding_for_remote_node() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let remote_identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let remote_public_id = scoped_id("broker-borealis", &remote_identity.agent_did);
    let remote_node_id = "12D3KooRemotePeer".to_string();
    let mut bridge = MockSwarmBridge::default_for(identity.agent_did.clone());
    bridge.network_status = SwarmNetworkStatusView {
        running: true,
        mode: "network".to_string(),
        peer_protocol_distribution: BTreeMap::new(),
    };
    bridge.peers = vec![SwarmPeerView {
        node_id: remote_node_id.clone(),
        connected: Some(true),
        discovery: None,
        metadata: Some(json!({"network_id": "mainnet:watt-etheria"})),
        relationship: None,
    }];
    let bridge_handle: Arc<dyn SwarmBridge> = Arc::new(bridge);
    let (_dir, app, token, _, state) =
        build_test_app_with_bridge(20, dir, identity.clone(), event_log, bridge_handle);

    let local_public_id = bootstrap_broker_identity(app.clone(), &token, &identity.agent_did).await;
    wattetheria_social::application::transport_binding_service::upsert_transport_binding(
        &*state.social_store,
        &wattetheria_social::domain::transport_bindings::RemoteTransportBinding {
            public_id: remote_public_id.clone(),
            agent_did: Some(remote_identity.agent_did.clone()),
            transport_kind:
                wattetheria_social::domain::transport_bindings::TransportKind::Wattswarm,
            transport_node_id: remote_node_id.clone(),
            binding_source: "friendship".to_string(),
            binding_confidence: 90,
            binding_proof_json: None,
            binding_verified: true,
            binding_verified_at: Some(1),
            updated_at: 1,
        },
    )
    .expect("seed social transport binding");
    friendship_service::upsert_friendship(
        &*state.social_store,
        &wattetheria_social::domain::friendships::Friendship {
            friendship_id: format!("friendship:{local_public_id}:{remote_public_id}"),
            local_public_id: local_public_id.clone(),
            remote_public_id: remote_public_id.clone(),
            state: wattetheria_social::domain::friendships::FriendshipState::Active,
            established_from_request_id: Some("req-accepted-1".to_string()),
            thread_id: None,
            created_at: 1,
            updated_at: 2,
        },
    )
    .expect("seed active friendship");

    let relationship_items = authed_get_json(
        app,
        &token,
        &format!("/v1/wattetheria/social/agent-friends?public_id={local_public_id}"),
    )
    .await;
    assert_eq!(relationship_items.as_array().unwrap().len(), 1);
    assert_eq!(
        relationship_items[0]["remote_node_id"].as_str(),
        Some(remote_node_id.as_str())
    );
    assert_eq!(relationship_items[0]["connected"].as_bool(), Some(true));
    assert_eq!(relationship_items[0]["status"].as_str(), Some("online"));
}

#[tokio::test]
async fn agent_friend_request_allows_pending_retry_but_denies_active_friendship() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let remote_identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge::default_for(identity.agent_did.clone()));
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
    friend_request_service::upsert_friend_request(
        &*state.social_store,
        &wattetheria_social::domain::friend_requests::FriendRequest {
            request_id: "request-existing-pending".to_string(),
            local_public_id: local_public_id.clone(),
            remote_public_id: remote_public_id.clone(),
            remote_node_id: Some("12D3KooRemotePeer".to_string()),
            direction:
                wattetheria_social::domain::friend_requests::FriendRequestDirection::Outbound,
            state: wattetheria_social::domain::friend_requests::FriendRequestState::Pending,
            decision_reason: None,
            correlation_id: None,
            created_at: 1,
            updated_at: 1,
            expires_at: None,
        },
    )
    .unwrap();

    let retry_status = authed_post(
        app.clone(),
        &token,
        "/v1/wattetheria/social/agent-friends",
        json!({
            "public_id": local_public_id,
            "counterpart_public_id": remote_public_id,
            "action": "request",
            "message": {
                "kind": "friend_request",
                "text": "retry connect with me"
            }
        }),
    )
    .await;

    assert_eq!(retry_status, StatusCode::ACCEPTED);
    assert_eq!(bridge.relationship_commands.lock().await.len(), 1);

    friendship_service::upsert_friendship(
        &*state.social_store,
        &wattetheria_social::domain::friendships::Friendship {
            friendship_id: format!("friendship:{local_public_id}:{remote_public_id}"),
            local_public_id: local_public_id.clone(),
            remote_public_id: remote_public_id.clone(),
            state: wattetheria_social::domain::friendships::FriendshipState::Active,
            established_from_request_id: Some("request-existing-pending".to_string()),
            thread_id: Some("dm:alice:borealis".to_string()),
            created_at: 2,
            updated_at: 2,
        },
    )
    .unwrap();

    let active_friend_status = authed_post(
        app,
        &token,
        "/v1/wattetheria/social/agent-friends",
        json!({
            "public_id": local_public_id,
            "counterpart_public_id": remote_public_id,
            "action": "request",
            "message": {
                "kind": "friend_request",
                "text": "should not send to an active friend"
            }
        }),
    )
    .await;

    assert_eq!(active_friend_status, StatusCode::FORBIDDEN);
    assert_eq!(bridge.relationship_commands.lock().await.len(), 1);
}

#[tokio::test]
async fn agent_dm_is_denied_when_counterpart_is_blocked() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let remote_identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge {
        fail_accept_and_finalize: false,
        local_node_id: identity.agent_did.clone(),
        agent_stats: BTreeMap::new(),
        network_status: SwarmNetworkStatusView {
            running: true,
            mode: "network".to_string(),
            peer_protocol_distribution: BTreeMap::new(),
        },
        peers: Vec::new(),
        subscriptions: Mutex::new(Vec::new()),
        messages: Mutex::new(Vec::new()),
        relationship_views: Mutex::new(Vec::new()),
        relationship_commands: Mutex::new(Vec::new()),
        dm_threads: Mutex::new(Vec::new()),
        dm_messages: Mutex::new(BTreeMap::new()),
        dm_commands: Mutex::new(Vec::new()),
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
    wattetheria_social::application::transport_binding_service::upsert_transport_binding(
        &*state.social_store,
        &wattetheria_social::domain::transport_bindings::RemoteTransportBinding {
            public_id: remote_public_id.clone(),
            agent_did: Some(remote_identity.agent_did.clone()),
            transport_kind:
                wattetheria_social::domain::transport_bindings::TransportKind::Wattswarm,
            transport_node_id: "12D3KooRemotePeer".to_string(),
            binding_source: "friendship".to_string(),
            binding_confidence: 90,
            binding_proof_json: None,
            binding_verified: true,
            binding_verified_at: Some(1),
            updated_at: 1,
        },
    )
    .unwrap();
    block_service::upsert_block(
        &*state.social_store,
        &wattetheria_social::domain::blocks::SocialBlock {
            block_id: "block:alice:borealis".to_string(),
            owner_public_id: local_public_id.clone(),
            blocked_public_id: remote_public_id.clone(),
            blocked_node_id: Some("12D3KooRemotePeer".to_string()),
            reason: Some("blocked".to_string()),
            created_at: 1,
            updated_at: 1,
        },
    )
    .unwrap();

    let status = authed_post(
        app,
        &token,
        "/v1/wattetheria/social/agent-dm/messages",
        json!({
            "public_id": local_public_id,
            "counterpart_public_id": remote_public_id,
            "content": {
                "type": "text",
                "text": "hello from wattetheria"
            }
        }),
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(bridge.dm_commands.lock().await.is_empty());
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn agent_social_queries_reconcile_inbound_swarm_views_into_social_store() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let remote_identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let remote_public_id = scoped_id("broker-borealis", &remote_identity.agent_did);
    let remote_node_id = "12D3KooRemotePeer".to_string();
    let transport_thread_id = "transport-thread-42".to_string();
    let bridge = Arc::new(MockSwarmBridge {
        fail_accept_and_finalize: false,
        local_node_id: identity.agent_did.clone(),
        agent_stats: BTreeMap::new(),
        network_status: SwarmNetworkStatusView {
            running: true,
            mode: "network".to_string(),
            peer_protocol_distribution: BTreeMap::new(),
        },
        peers: vec![SwarmPeerView {
            node_id: remote_node_id.clone(),
            connected: Some(true),
            discovery: None,
            metadata: Some(json!({"network_id": "mainnet:watt-etheria"})),
            relationship: None,
        }],
        subscriptions: Mutex::new(Vec::new()),
        messages: Mutex::new(Vec::new()),
        relationship_views: Mutex::new(vec![SwarmPeerRelationshipView {
            remote_node_id: remote_node_id.clone(),
            relationship_state: "accepted".to_string(),
            last_action: "accept".to_string(),
            initiated_by: "remote".to_string(),
            agent_envelope: Some(SwarmAgentEnvelope {
                protocol: "google_a2a".to_string(),
                transport_profile: None,
                source_agent_id: Some(remote_identity.agent_did.clone()),
                target_agent_id: Some(identity.agent_did.clone()),
                source_node_id: Some(remote_node_id.clone()),
                target_node_id: None,
                capability: Some("social.friend.accept".to_string()),
                source_agent_card: Some(SwarmSourceAgentCard {
                    agent_id: remote_identity.agent_did.clone(),
                    node_id: Some(remote_node_id.clone()),
                    card_hash: "sha256:remote-card".to_string(),
                    issued_at: 1_710_000_120,
                    card: json!({
                        "name": "Remote Agent Alice",
                        "description": "Remote agent profile from the accepted relationship action.",
                        "skills": [
                            {"id": "social-direct-message", "name": "Social direct message"},
                            {"id": "task-participation", "name": "Task participation"}
                        ]
                    }),
                    signature: Some("card-sig-1".to_string()),
                }),
                message: json!({
                    "request_id": "req-inbound-1",
                    "correlation_id": "corr-inbound-1",
                    "source_public_id": remote_public_id.clone()
                }),
                extensions: None,
                signature: Some("sig-1".to_string()),
            }),
            requested_at: Some(1_710_000_100),
            responded_at: Some(1_710_000_150),
            blocked_at: None,
            cleared_at: None,
            updated_at: 1_710_000_150,
        }]),
        relationship_commands: Mutex::new(Vec::new()),
        dm_threads: Mutex::new(vec![SwarmPeerDmThreadView {
            remote_node_id: remote_node_id.clone(),
            thread_id: transport_thread_id.clone(),
            thread_kind: "direct".to_string(),
            session_state: "ready".to_string(),
            relationship_established_at: Some(1_710_000_150),
            created_at: 1_710_000_150,
            updated_at: 1_710_000_180,
            last_message_at: Some(1_710_000_180),
        }]),
        dm_messages: Mutex::new(BTreeMap::from([(
            transport_thread_id.clone(),
            vec![SwarmPeerDmMessageView {
                thread_id: transport_thread_id.clone(),
                message_id: "dm-msg-1".to_string(),
                remote_node_id: remote_node_id.clone(),
                message_kind: "message".to_string(),
                direction: "inbound".to_string(),
                delivery_state: "acknowledged".to_string(),
                a2a_protocol: "google_a2a".to_string(),
                agent_envelope: Some(SwarmAgentEnvelope {
                    protocol: "google_a2a".to_string(),
                    transport_profile: None,
                    source_agent_id: Some(remote_identity.agent_did.clone()),
                    target_agent_id: Some(identity.agent_did.clone()),
                    source_node_id: None,
                    target_node_id: None,
                    capability: Some("social.dm.send".to_string()),
                    source_agent_card: None,
                    message: json!({
                        "thread_id": transport_thread_id,
                        "message_id": "dm-msg-1"
                    }),
                    extensions: None,
                    signature: Some("sig-2".to_string()),
                }),
                content: json!({"type":"text","text":"hello inbound"}),
                encrypted_body: None,
                content_encoding: None,
                created_at: 1_710_000_180,
                acknowledged_at: Some(1_710_000_181),
            }],
        )])),
        dm_commands: Mutex::new(Vec::new()),
        payment_commands: Mutex::new(Vec::new()),
    });
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, app, token, _, state) =
        build_test_app_with_bridge(20, dir, identity.clone(), event_log, bridge_handle);

    let local_public_id = bootstrap_broker_identity(app.clone(), &token, &identity.agent_did).await;
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
    friend_request_service::upsert_friend_request(
        &*state.social_store,
        &wattetheria_social::domain::friend_requests::FriendRequest {
            request_id: "req-inbound-1".to_string(),
            local_public_id: local_public_id.clone(),
            remote_public_id: remote_public_id.clone(),
            remote_node_id: Some(remote_node_id.clone()),
            direction: wattetheria_social::domain::friend_requests::FriendRequestDirection::Inbound,
            state: wattetheria_social::domain::friend_requests::FriendRequestState::Accepted,
            decision_reason: Some("accepted".to_string()),
            correlation_id: Some("corr-inbound-1".to_string()),
            created_at: 1_710_000_100,
            updated_at: 1_710_000_150,
            expires_at: None,
        },
    )
    .expect("seed accepted request alongside friendship");
    friendship_service::upsert_friendship(
        &*state.social_store,
        &wattetheria_social::domain::friendships::Friendship {
            friendship_id: format!("friendship:{local_public_id}:{remote_node_id}"),
            local_public_id: local_public_id.clone(),
            remote_public_id: remote_node_id.clone(),
            state: wattetheria_social::domain::friendships::FriendshipState::Active,
            established_from_request_id: Some("req-inbound-1".to_string()),
            thread_id: Some("legacy-thread".to_string()),
            created_at: 1_710_000_100,
            updated_at: 1_710_000_120,
        },
    )
    .expect("seed legacy node-id friendship");

    let relationship_items = authed_get_json(
        app.clone(),
        &token,
        &format!("/v1/wattetheria/social/agent-friends?public_id={local_public_id}"),
    )
    .await;
    assert_eq!(relationship_items.as_array().unwrap().len(), 1);
    assert_eq!(
        relationship_items[0]["relationship_state"].as_str(),
        Some("active")
    );
    assert_eq!(
        relationship_items[0]["counterpart_display_name"].as_str(),
        Some("Remote Agent Alice")
    );
    assert_eq!(
        relationship_items[0]["counterpart_agent_did"].as_str(),
        Some(remote_identity.agent_did.as_str())
    );
    assert_eq!(
        relationship_items[0]["counterpart_agent_public_id"].as_str(),
        Some(remote_public_id.as_str())
    );
    assert_eq!(
        relationship_items[0]["counterpart_skills"][0].as_str(),
        Some("Social direct message")
    );
    assert_eq!(
        relationship_items[0]["network_id"].as_str(),
        Some("mainnet:watt-etheria")
    );

    let thread_items = authed_get_json(
        app.clone(),
        &token,
        &format!("/v1/wattetheria/social/agent-dm/threads?public_id={local_public_id}"),
    )
    .await;
    assert_eq!(thread_items.as_array().unwrap().len(), 1);

    let message_items = authed_get_json(
        app.clone(),
        &token,
        &format!("/v1/wattetheria/social/agent-dm/messages?public_id={local_public_id}"),
    )
    .await;
    assert_eq!(message_items.as_array().unwrap().len(), 1);
    assert_eq!(
        message_items[0]["content"]["text"].as_str(),
        Some("hello inbound")
    );

    let friend_requests =
        friend_request_service::list_friend_requests(&*state.social_store, &local_public_id)
            .expect("list reconciled requests");
    assert_eq!(friend_requests.len(), 1);
    assert_eq!(friend_requests[0].request_id, "req-inbound-1");

    let friendships = friendship_service::list_friendships(&*state.social_store, &local_public_id)
        .expect("list reconciled friendships");
    let active_friendships = friendships
        .iter()
        .filter(|friendship| {
            friendship.state == wattetheria_social::domain::friendships::FriendshipState::Active
        })
        .collect::<Vec<_>>();
    assert_eq!(active_friendships.len(), 1);
    assert_eq!(active_friendships[0].remote_public_id, remote_public_id);
    assert!(friendships.iter().any(|friendship| {
        friendship.remote_public_id == remote_node_id
            && friendship.state == wattetheria_social::domain::friendships::FriendshipState::Removed
    }));

    let threads = thread_service::list_threads(&*state.social_store, &local_public_id)
        .expect("list reconciled threads");
    assert_eq!(threads.len(), 1);
    assert_eq!(threads[0].transport_thread_id, "transport-thread-42");

    let messages =
        message_service::list_thread_messages(&*state.social_store, &threads[0].thread_id)
            .expect("list reconciled messages");
    assert_eq!(messages.len(), 1);
    assert_eq!(
        messages[0].content_json["text"].as_str(),
        Some("hello inbound")
    );

    let receipts =
        receipt_service::list_message_receipts(&*state.social_store, &messages[0].message_id)
            .expect("list reconciled receipts");
    assert!(receipts.len() >= 2);

    bridge.relationship_views.lock().await.clear();
    bridge.dm_threads.lock().await.clear();
    bridge.dm_messages.lock().await.clear();

    let relationship_items_after_cache = authed_get_json(
        app.clone(),
        &token,
        &format!("/v1/wattetheria/social/agent-friends?public_id={local_public_id}"),
    )
    .await;
    assert_eq!(relationship_items_after_cache.as_array().unwrap().len(), 1);

    let thread_items_after_cache = authed_get_json(
        app.clone(),
        &token,
        &format!("/v1/wattetheria/social/agent-dm/threads?public_id={local_public_id}"),
    )
    .await;
    assert_eq!(thread_items_after_cache.as_array().unwrap().len(), 1);

    let message_items_after_cache = authed_get_json(
        app,
        &token,
        &format!("/v1/wattetheria/social/agent-dm/messages?public_id={local_public_id}"),
    )
    .await;
    assert_eq!(message_items_after_cache.as_array().unwrap().len(), 1);
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn social_host_adapters_use_active_identity_and_swarm_bridge() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let remote_identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge {
        fail_accept_and_finalize: false,
        local_node_id: identity.agent_did.clone(),
        agent_stats: BTreeMap::new(),
        network_status: SwarmNetworkStatusView {
            running: true,
            mode: "network".to_string(),
            peer_protocol_distribution: BTreeMap::new(),
        },
        peers: Vec::new(),
        subscriptions: Mutex::new(Vec::new()),
        messages: Mutex::new(Vec::new()),
        relationship_views: Mutex::new(Vec::new()),
        relationship_commands: Mutex::new(Vec::new()),
        dm_threads: Mutex::new(Vec::new()),
        dm_messages: Mutex::new(BTreeMap::new()),
        dm_commands: Mutex::new(Vec::new()),
        payment_commands: Mutex::new(Vec::new()),
    });
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, state, _token, _policy_engine) =
        build_test_state_with_bridge(20, dir, identity.clone(), event_log, bridge_handle);

    let local_public_id = {
        let registry = state.public_identity_registry.lock().await;
        registry
            .active_for_agent_did(&identity.agent_did)
            .expect("active public identity")
            .public_id
    };
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

    let identity_provider = WattetheriaLocalIdentityProvider::new(state.clone());
    let active_identity = identity_provider
        .active_identity()
        .await
        .expect("load active identity");
    assert_eq!(active_identity.public_id, local_public_id);
    assert_eq!(active_identity.agent_did, identity.agent_did);

    let transport = WattetheriaTransportAdapter::new(state.clone());
    transport
        .send_friend_request(
            "12D3KooRemotePeer",
            &json!({
                "counterpart_public_id": remote_public_id,
                "kind": "friend_request",
                "text": "connect with me"
            }),
        )
        .await
        .expect("send friend request");
    transport
        .send_friend_decision(
            "12D3KooRemotePeer",
            &json!({
                "counterpart_public_id": remote_public_id,
                "decision": "accept",
                "request_id": "request-1"
            }),
        )
        .await
        .expect("send friend decision");
    transport
        .send_direct_message(
            "12D3KooRemotePeer",
            &json!({
                "counterpart_public_id": remote_public_id,
                "content": {
                    "type": "text",
                    "text": "hello from adapter"
                }
            }),
        )
        .await
        .expect("send direct message");

    let relationship_commands = bridge.relationship_commands.lock().await;
    assert_eq!(relationship_commands.len(), 2);
    assert_eq!(
        relationship_commands[0].action,
        wattetheria_kernel::swarm_bridge::SwarmRelationshipAction::Request
    );
    assert_eq!(
        relationship_commands[1].action,
        wattetheria_kernel::swarm_bridge::SwarmRelationshipAction::Accept
    );
    assert_eq!(
        relationship_commands[0]
            .agent_envelope
            .capability
            .as_deref(),
        Some("social.friend.request")
    );
    assert_eq!(
        relationship_commands[1]
            .agent_envelope
            .capability
            .as_deref(),
        Some("social.friend.accept")
    );
    drop(relationship_commands);

    let dm_commands = bridge.dm_commands.lock().await;
    assert_eq!(dm_commands.len(), 1);
    assert_eq!(
        dm_commands[0].agent_envelope.capability.as_deref(),
        Some("social.dm.send")
    );
}
