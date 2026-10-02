use super::*;

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
