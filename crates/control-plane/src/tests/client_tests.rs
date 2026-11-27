use super::*;

fn assert_actor_projection(
    value: &serde_json::Value,
    prefix: &str,
    agent_did: &str,
    display_name: &str,
    public_id: &str,
) {
    assert_eq!(
        value
            .get(format!("{prefix}_agent_did"))
            .and_then(serde_json::Value::as_str),
        Some(agent_did)
    );
    assert_eq!(
        value
            .get(format!("{prefix}_agent_identity"))
            .and_then(serde_json::Value::as_str),
        Some(display_name)
    );
    assert_eq!(
        value
            .get(format!("{prefix}_display_name"))
            .and_then(serde_json::Value::as_str),
        Some(display_name)
    );
    assert_eq!(
        value
            .get(format!("{prefix}_public_id"))
            .and_then(serde_json::Value::as_str),
        Some(public_id)
    );
}

fn assert_public_geo_projection(value: &serde_json::Value) {
    assert_eq!(value["lat"].as_f64(), Some(0.0));
    assert_eq!(value["lng"].as_f64(), Some(0.0));
    assert_eq!(value["coordinate_source"].as_str(), Some("derived"));
}

#[allow(clippy::too_many_lines)]
#[tokio::test]
async fn client_api_routes_align_with_client_dtos() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let mut agent_stats = BTreeMap::new();
    agent_stats.insert(
        identity.agent_did.clone(),
        AgentStats {
            power: 4,
            watt: 77,
            reputation: 9,
            capacity: 3,
        },
    );
    let bridge: Arc<dyn SwarmBridge> = Arc::new(MockSwarmBridge {
        public_bootstrap: false,
        fail_accept_and_finalize: false,
        topic_message_lookup_calls: Mutex::new(Vec::new()),
        local_node_id: identity.agent_did.clone(),
        agent_stats,
        network_status: SwarmNetworkStatusView {
            running: true,
            mode: "network".to_string(),
            peer_protocol_distribution: [("v0.1".to_string(), 1_u64)].into_iter().collect(),
        },
        peers: vec![SwarmPeerView {
            node_id: "peer-a".to_string(),
            connected: Some(true),
            recently_seen: Some(true),
            stale: Some(false),
            last_seen_age_ms: None,
            discovery: Some(json!({
                "endpoint_id": "iroh-endpoint-a",
                "source_kind": "bootstrap"
            })),
            metadata: Some(json!({
                "endpoint_id": "iroh-endpoint-a",
                "handshake_status": "identified",
                "observed_addr": "198.51.100.2:4001",
                "listen_addrs": ["203.0.113.10:4001"],
                "contact_material": {
                    "source_agent_card": {
                        "agent_id": "did:key:zPeerAgent",
                        "node_id": "peer-a",
                        "card_hash": "sha256:peer-card",
                        "issued_at": 1_700_000_000_000_u64,
                        "card": {
                            "name": "Remote Scout",
                            "description": "Discovery profile",
                            "metadata": {
                                "display_name": "Remote Scout",
                                "agent_id": "did:key:zPeerAgent",
                                "node_id": "peer-a"
                            },
                            "skills": [
                                {"id": "scan", "name": "Scan", "description": "Scan nearby tasks"}
                            ]
                        },
                        "signature": "sig"
                    }
                }
            })),
            relationship: Some(json!({
                "relationship_state": "friend",
                "last_action": "accept",
                "agent_envelope": {
                    "source_node_id": "local-node",
                    "source_agent_card": {
                        "agent_id": "did:key:zLocalAgent",
                        "node_id": "local-node",
                        "card": {
                            "name": "Wrong Local Agent"
                        }
                    }
                }
            })),
        }],
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
        settled_tasks: Mutex::new(Vec::new()),
        settle_down: std::sync::atomic::AtomicBool::new(false),
        claim_down: std::sync::atomic::AtomicBool::new(false),
        announce_down: std::sync::atomic::AtomicBool::new(false),
        topic_down: std::sync::atomic::AtomicBool::new(false),
        task_notices: Mutex::new(Vec::new()),
        published_tasks: Mutex::new(Vec::new()),
    });
    let (_dir, app, token, _, state) =
        build_test_app_with_bridge(20, dir, identity.clone(), event_log, bridge);

    let captain = bootstrap_broker_identity(app.clone(), &token, &identity.agent_did).await;
    let _published = publish_trade_mission(
        app.clone(),
        &token,
        TradeMissionSpec {
            title: "Calibrate relay",
            description: "Tune the frontier relay.",
            reward_reputation: 3,
            objective: "relay_calibration",
            required_faction: None,
            subnet_id: Some("planet-test"),
            zone_id: Some("genesis-core"),
        },
    )
    .await;
    let _organization = authed_post_json(
        app.clone(),
        &token,
        "/v1/civilization/organizations",
        json!({
            "public_id": captain,
            "organization_id": "aurora-guild",
            "name": "Aurora Guild",
            "kind": "guild",
            "summary": "Relay keepers",
            "home_subnet_id": "planet-test",
            "home_zone_id": "genesis-core"
        }),
    )
    .await;
    let _funded = authed_post_json(
        app.clone(),
        &token,
        "/v1/civilization/organizations/treasury/fund",
        json!({
            "organization_id": "aurora-guild",
            "actor_public_id": captain,
            "amount_watt": 55,
            "reason": "seed treasury"
        }),
    )
    .await;

    let network_status = authed_get_json(app.clone(), &token, "/v1/client/network/status").await;
    assert_eq!(network_status["total_nodes"].as_u64(), Some(2));
    assert_eq!(network_status["active_nodes"].as_u64(), Some(2));

    let peers_json = authed_get_json(app.clone(), &token, "/v1/client/peers?limit=1").await;
    assert_eq!(peers_json.as_array().unwrap().len(), 1);
    assert_eq!(peers_json[0]["id"].as_str(), Some("peer-a"));
    assert_eq!(peers_json[0]["connected"].as_bool(), Some(true));
    assert_eq!(peers_json[0]["source_kind"].as_str(), Some("bootstrap"));
    assert_eq!(peers_json[0]["relationship_state"].as_str(), Some("friend"));
    assert_eq!(
        peers_json[0]["relationship"]["relationship_state"].as_str(),
        Some("friend")
    );
    assert_eq!(peers_json[0]["endpoint"].as_str(), Some("iroh-endpoint-a"));
    assert!(peers_json[0].get("lat").is_none());
    assert!(peers_json[0].get("lng").is_none());

    let nearby_json =
        authed_get_json(app.clone(), &token, "/v1/wattetheria/social/nearby?limit=1").await;
    assert_eq!(nearby_json["ok"].as_bool(), Some(true));
    assert_eq!(nearby_json["count"].as_u64(), Some(1));
    let nearby_item = &nearby_json["items"][0];
    assert_eq!(nearby_item["remote_node_id"].as_str(), Some("peer-a"));
    assert_eq!(nearby_item["status"].as_str(), Some("online"));
    assert_eq!(nearby_item["connected"].as_bool(), Some(true));
    assert_eq!(nearby_item["endpoint"].as_str(), Some("iroh-endpoint-a"));
    assert_eq!(
        nearby_item["discovery"]["source_kind"].as_str(),
        Some("bootstrap")
    );
    assert_eq!(
        nearby_item["metadata"]["observed_addr"].as_str(),
        Some("198.51.100.2:4001")
    );
    assert_eq!(
        nearby_item["metadata"]["listen_addrs"][0].as_str(),
        Some("203.0.113.10:4001")
    );
    assert_eq!(
        nearby_item["agent_card"]["name"].as_str(),
        Some("Remote Scout")
    );
    assert_eq!(
        nearby_item["source_agent_card"]["agent_id"].as_str(),
        Some("did:key:zPeerAgent")
    );
    assert!(nearby_item.get("relationship").is_none());
    assert!(nearby_item.get("node_id").is_none());
    assert!(nearby_item.get("source_kind").is_none());
    assert!(nearby_item.get("relationship_state").is_none());
    assert!(nearby_item.get("target_agent_did").is_none());
    assert!(nearby_item.get("counterpart_public_id").is_none());

    let self_json = authed_get_json(
        app.clone(),
        &token,
        &format!("/v1/client/self?public_id={captain}"),
    )
    .await;
    assert_eq!(self_json["id"].as_str(), Some(captain.as_str()));
    assert_eq!(self_json["display_name"].as_str(), Some("Captain Aurora"));
    assert_eq!(self_json["watt_balance"].as_i64(), Some(0));
    assert_eq!(self_json["watt_balance_available"].as_bool(), Some(false));
    assert_eq!(
        self_json["agent_did"].as_str(),
        Some(identity.agent_did.as_str())
    );
    assert_eq!(
        self_json["wallet_bound_agent_did"].as_str(),
        Some(identity.agent_did.as_str())
    );
    assert!(self_json["active_payment_account"].is_null());
    assert!(self_json.get("wallet_identities").is_none());
    assert!(self_json["payment_account_binding"].is_null());

    // The published mission is kept by the gateway, which alone holds its status.
    let missions = mission_gateway::gateway_board(&state).await;
    let missions = missions.list(None);
    assert_eq!(missions.len(), 1);
    assert_eq!(missions[0].title, "Calibrate relay");
    assert_eq!(
        missions[0].status,
        wattetheria_kernel::civilization::missions::MissionStatus::Open
    );

    let organizations_json = authed_get_json(app.clone(), &token, "/v1/client/organizations").await;
    assert_eq!(organizations_json.as_array().unwrap().len(), 1);
    assert_eq!(organizations_json[0]["name"].as_str(), Some("Aurora Guild"));
    assert_eq!(organizations_json[0]["treasury_watt"].as_i64(), Some(55));
    assert_eq!(organizations_json[0]["member_count"].as_u64(), Some(1));

    let rpc_logs_json = authed_get_json(app, &token, "/v1/client/rpc-logs?limit=5").await;
    assert_ne!(
        rpc_logs_json.as_array().unwrap().as_slice(),
        [] as [Value; 0]
    );
    assert!(rpc_logs_json[0]["timestamp"].is_string());
    assert!(rpc_logs_json[0]["message"].is_string());
    assert!(rpc_logs_json[0]["level"].is_string());
}

#[tokio::test]
async fn client_self_reports_ledger_watt_and_mission_bounty() {
    let (_dir, app, token, _, state) = build_test_app(20);
    let identity = authed_get_json(app.clone(), &token, "/v1/client/self").await;
    let agent_did = identity["wallet_bound_agent_did"]
        .as_str()
        .unwrap()
        .to_string();
    let public_id = bootstrap_broker_identity(app.clone(), &token, &agent_did).await;
    let mission = authed_post_json(
        app.clone(),
        &token,
        "/v1/wattetheria/missions",
        json!({
            "title": "Tune local agent",
            "description": "Run a wallet-bound task.",
            "publisher": public_id,
            "publisher_kind": "player",
            "domain": "trade",
            "subnet_id": null,
            "zone_id": null,
            "required_role": null,
            "required_faction": null,
            "reward": {
                "agent_watt": 0,
                "reputation": 3,
                "capacity": 1
            },
            "payload": {"objective": "wallet-bound-balance"}
        }),
    )
    .await;
    let mission_id = mission["mission_id"].as_str().unwrap();
    let _ = authed_post_json(
        app.clone(),
        &token,
        &format!("/v1/wattetheria/missions/{mission_id}/claim"),
        json!({"mission_id": mission_id, "agent_did": agent_did}),
    )
    .await;
    let _ = authed_post_json(
        app.clone(),
        &token,
        &format!("/v1/wattetheria/missions/{mission_id}/complete"),
        json!({"mission_id": mission_id, "agent_did": agent_did}),
    )
    .await;
    let _ = authed_post_json(
        app.clone(),
        &token,
        &format!("/v1/wattetheria/missions/{mission_id}/settle"),
        json!({"mission_id": mission_id}),
    )
    .await;

    let record = mission_gateway::mission_gateway(&state)
        .lock()
        .await
        .record(mission_id)
        .unwrap();
    assert_eq!(record["status"], "settled", "{record}");
    assert_eq!(record["reward"]["kind"], "none");
    assert_eq!(record["publisher"], json!(agent_did));
    assert_eq!(record["claimer"], json!(agent_did));

    // Watt is no longer derived from settled missions: without a gateway ledger the node
    // reports zero and says the balance is unavailable.
    let self_json = authed_get_json(
        app,
        &token,
        &format!("/v1/client/self?public_id={public_id}"),
    )
    .await;
    assert_eq!(self_json["watt_balance"].as_i64(), Some(0));
    assert_eq!(self_json["watt_balance_available"].as_bool(), Some(false));
    assert_eq!(
        self_json["watt_ledger_account"].as_str(),
        Some(agent_did.as_str())
    );
    assert!(self_json.get("reward_policy_version").is_none());
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn wallet_page_can_create_agent_payment_account() {
    let (_dir, app, token, _, _state) = build_test_app(20);
    let created = authed_post_json(
        app.clone(),
        &token,
        "/v1/wallet/payment-account/create",
        json!({
            "network": "base",
            "rail": "x402",
            "label": "agent-wallet"
        }),
    )
    .await;

    assert_eq!(created["ok"].as_bool(), Some(true));
    assert!(
        created["active_payment_account"]["address"]
            .as_str()
            .is_some_and(|address| address.starts_with("0x") && address.len() == 42)
    );
    assert_eq!(
        created["active_payment_account"]["network"].as_str(),
        Some("base")
    );
    assert_eq!(
        created["active_payment_account"]["rail"].as_str(),
        Some("x402")
    );
    assert_eq!(
        created["active_payment_account"]["custody"].as_str(),
        Some("local_key")
    );
    assert_eq!(
        created["active_payment_account"]["receive_only"].as_bool(),
        Some(false)
    );
    assert_eq!(
        created["active_payment_account"]["can_sign"].as_bool(),
        Some(true)
    );

    let self_json = authed_get_json(app.clone(), &token, "/v1/client/self").await;
    assert_eq!(
        self_json["active_payment_account"]["address"].as_str(),
        created["active_payment_account"]["address"].as_str()
    );
    assert_eq!(
        self_json["active_payment_account"]["custody"].as_str(),
        Some("local_key")
    );
    assert_eq!(
        self_json["active_payment_account"]["can_sign"].as_bool(),
        Some(true)
    );
    assert!(
        self_json["active_payment_account"]["capabilities"]
            .as_array()
            .is_some_and(|capabilities| capabilities.iter().any(|value| value == "send"))
    );
    assert!(self_json.get("wallet_identities").is_none());
    assert_eq!(
        self_json["payment_account_binding"]["status"].as_str(),
        Some("ready")
    );
    assert_eq!(
        self_json["payment_account_binding"]["proof_available"].as_bool(),
        Some(true)
    );
    assert_eq!(
        self_json["payment_account_binding"]["payment_proof_algorithm"].as_str(),
        Some("secp256k1-binding")
    );
    assert!(
        self_json["payment_accounts"]
            .as_array()
            .is_some_and(|accounts| accounts.iter().any(|account| {
                account["account_id"].as_str()
                    == created["active_payment_account"]["account_id"].as_str()
                    && account["network"].as_str() == Some("base")
                    && account["can_sign"].as_bool() == Some(true)
                    && account["capabilities"]
                        .as_array()
                        .is_some_and(|capabilities| {
                            capabilities.iter().any(|value| value == "receive")
                        })
            }))
    );

    let duplicate = authed_post_json(
        app.clone(),
        &token,
        "/v1/wallet/payment-account/create",
        json!({
            "network": "base",
            "rail": "x402",
            "label": "agent-wallet"
        }),
    )
    .await;
    assert_eq!(duplicate["ok"].as_bool(), Some(true));
    assert_eq!(duplicate["already_exists"].as_bool(), Some(true));
    assert_eq!(
        duplicate["active_payment_account"]["account_id"].as_str(),
        created["active_payment_account"]["account_id"].as_str()
    );
    assert_eq!(
        duplicate["active_payment_account"]["address"].as_str(),
        created["active_payment_account"]["address"].as_str()
    );

    let testnet = authed_post_json(
        app.clone(),
        &token,
        "/v1/wallet/payment-account/create",
        json!({
            "network": "base-sepolia",
            "rail": "x402",
            "label": "agent-wallet"
        }),
    )
    .await;
    assert_eq!(testnet["ok"].as_bool(), Some(true));
    assert_eq!(testnet["already_exists"].as_bool(), Some(false));
    assert_eq!(
        testnet["active_payment_account"]["network"].as_str(),
        Some("base-sepolia")
    );
    assert_ne!(
        testnet["active_payment_account"]["account_id"].as_str(),
        created["active_payment_account"]["account_id"].as_str()
    );
    assert_ne!(
        testnet["active_payment_account"]["address"].as_str(),
        created["active_payment_account"]["address"].as_str()
    );

    let self_json = authed_get_json(app, &token, "/v1/client/self").await;
    let accounts = self_json["payment_accounts"].as_array().unwrap();
    assert!(accounts.iter().any(|account| {
        account["account_id"].as_str() == created["active_payment_account"]["account_id"].as_str()
            && account["network"].as_str() == Some("base")
    }));
    assert!(accounts.iter().any(|account| {
        account["account_id"].as_str() == testnet["active_payment_account"]["account_id"].as_str()
            && account["network"].as_str() == Some("base-sepolia")
    }));
}

#[tokio::test]
async fn task_result_commit_settles_mission_from_wattswarm_event() {
    let (_dir, app, token, _, state) = build_test_app(20);
    let identity = authed_get_json(app.clone(), &token, "/v1/client/self").await;
    let agent_did = identity["wallet_bound_agent_did"].as_str().unwrap();
    let public_id = bootstrap_broker_identity(app.clone(), &token, agent_did).await;
    let mission = authed_post_json(
        app.clone(),
        &token,
        "/v1/wattetheria/missions",
        json!({
            "title": "Accept remote result",
            "description": "Publisher agent settles after wattswarm result callback.",
            "publisher": public_id,
            "publisher_kind": "player",
            "domain": "trade",
            "subnet_id": null,
            "zone_id": null,
            "required_role": null,
            "required_faction": null,
            "reward": {
                "agent_watt": 0,
                "reputation": 1,
                "capacity": 0
            },
            "payload": {"objective": "remote-result"}
        }),
    )
    .await;
    let mission_id = mission["mission_id"].as_str().unwrap();
    let created_at = mission["created_at"].as_i64().unwrap();
    let worker = "did:key:worker";
    // The worker's own node asked the gateway for the mission before handing in its result.
    mission_gateway::mission_gateway(&state)
        .lock()
        .await
        .ask(mission_id, worker);

    let committed = authed_post_json(
        app,
        &token,
        "/v1/agent-actions/commit",
        json!({
            "event": {
                "event_id": "evt-task-result-commit",
                "event_type": "task_result_received",
                "source_kind": "task_lifecycle",
                "source_node_id": "worker-node",
                "target_agent_id": agent_did,
                "payload": {
                    "event_kind": "candidate_proposed",
                    "task_id": mission_id,
                    "candidate_id": "cand-test",
                    "execution_id": "exec-test",
                    "candidate_output": {
                        "kind": "wattetheria_mission_result",
                        "mission_id": mission_id,
                        "agent_did": worker,
                        "result": {"ok": true}
                    },
                    "created_at": 1
                },
                "allowed_actions": [
                    "human_review",
                    "accept_result",
                    "reject_result",
                    "request_retry"
                ],
                "requires_commit": true
            },
            "decision": {
                "decision_id": "dec-task-result-settle",
                "action": "settle_mission",
                "route": "wattetheria_commit",
                "payload": {
                    "mission_id": mission_id,
                    "agent_did": worker,
                    "display_name": "Agent-MX1111",
                    "public_id": "agent-MX1111.public"
                }
            }
        }),
    )
    .await;
    assert_eq!(committed["status"].as_str(), Some("settled"));
    assert_eq!(committed["completed_by"].as_str(), Some(worker));
    assert_actor_projection(
        &committed,
        "completer",
        worker,
        "Agent-MX1111",
        "agent-MX1111.public",
    );
    assert!(committed["updated_at"].as_i64().unwrap() >= created_at);

    let settled = mission_gateway::gateway_mission(&state, mission_id).await;
    assert_eq!(
        settled.status,
        wattetheria_kernel::civilization::missions::MissionStatus::Settled
    );
    assert_eq!(settled.completed_by.as_deref(), Some(worker));
    assert!(settled.updated_at >= created_at);
}

#[tokio::test]
async fn task_result_commit_preserves_mission_state_when_swarm_finalize_fails() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let agent_did = identity.agent_did.clone();
    let mut bridge = MockSwarmBridge::default_for(agent_did.clone());
    bridge.fail_accept_and_finalize = true;
    let (_dir, app, token, _, state) =
        build_test_app_with_bridge(20, dir, identity, event_log, Arc::new(bridge));
    let public_id = bootstrap_broker_identity(app.clone(), &token, &agent_did).await;
    let mission = authed_post_json(
        app.clone(),
        &token,
        "/v1/wattetheria/missions",
        json!({
            "title": "Fail path",
            "description": "Task finalize failure must not change mission state.",
            "publisher": public_id,
            "publisher_kind": "player",
            "domain": "trade",
            "reward": {"agent_watt": 0, "reputation": 0, "capacity": 0},
            "payload": {"objective": "fail-test"}
        }),
    )
    .await;
    let mission_id = mission["mission_id"].as_str().unwrap();

    let committed = authed_post_json(
        app,
        &token,
        "/v1/agent-actions/commit",
        json!({
            "event": {
                "event_id": "evt-fail-path",
                "event_type": "task_result_received",
                "source_kind": "task_lifecycle",
                "source_node_id": "worker-node",
                "target_agent_id": agent_did,
                "payload": {
                    "event_kind": "candidate_proposed",
                    "task_id": mission_id,
                    "candidate_id": "cand-fail",
                    "execution_id": "exec-fail",
                    "candidate_output": {
                        "kind": "wattetheria_mission_result",
                        "mission_id": mission_id,
                        "agent_did": "did:key:worker",
                        "result": {"ok": true}
                    },
                    "created_at": 1
                },
                "allowed_actions": ["human_review", "accept_result", "reject_result", "request_retry"],
                "requires_commit": true
            },
            "decision": {
                "decision_id": "dec-fail-path",
                "action": "settle_mission",
                "route": "wattetheria_commit",
                "payload": {"mission_id": mission_id, "agent_did": "did:key:worker"}
            }
        }),
    )
    .await;

    // Task finalize failed: response is 500 with error, mission must stay Open.
    assert!(
        committed["error"]
            .as_str()
            .unwrap()
            .contains("mock task finalize failure")
    );

    let stuck = mission_gateway::gateway_mission(&state, mission_id).await;
    assert_eq!(
        stuck.status,
        wattetheria_kernel::civilization::missions::MissionStatus::Open,
        "mission must remain Open when task finalize fails"
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn client_friends_uses_social_friendships_as_canonical_source() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let remote_identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let remote_node_id = "12D3KooRemotePeer".to_string();
    let bridge: Arc<dyn SwarmBridge> = Arc::new(MockSwarmBridge {
        public_bootstrap: false,
        fail_accept_and_finalize: false,
        topic_message_lookup_calls: Mutex::new(Vec::new()),
        local_node_id: identity.agent_did.clone(),
        agent_stats: BTreeMap::new(),
        network_status: SwarmNetworkStatusView {
            running: true,
            mode: "network".to_string(),
            peer_protocol_distribution: BTreeMap::new(),
        },
        peers: vec![SwarmPeerView {
            node_id: remote_node_id.clone(),
            connected: Some(false),
            recently_seen: Some(false),
            stale: Some(false),
            last_seen_age_ms: None,
            discovery: Some(json!({"source_kind": "known"})),
            metadata: Some(json!({"network_id": "mainnet:watt-etheria"})),
            relationship: None,
        }],
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
        settled_tasks: Mutex::new(Vec::new()),
        settle_down: std::sync::atomic::AtomicBool::new(false),
        claim_down: std::sync::atomic::AtomicBool::new(false),
        announce_down: std::sync::atomic::AtomicBool::new(false),
        topic_down: std::sync::atomic::AtomicBool::new(false),
        task_notices: Mutex::new(Vec::new()),
        published_tasks: Mutex::new(Vec::new()),
    });
    let (_dir, app, token, _, state) =
        build_test_app_with_bridge(20, dir, identity.clone(), event_log, bridge);

    let local_public_id = bootstrap_broker_identity(app.clone(), &token, &identity.agent_did).await;
    let remote_public_id = scoped_id("broker-borealis", &remote_identity.agent_did);
    wattetheria_social::application::remote_identity_service::upsert_remote_identity(
        &*state.social_store,
        &wattetheria_social::domain::identities::RemoteIdentityProfile {
            public_id: remote_public_id.clone(),
            agent_did: remote_identity.agent_did.clone(),
            display_name: "Broker Borealis".to_string(),
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
    .expect("seed remote social identity");
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
    .expect("seed remote transport binding");
    friendship_service::upsert_friendship(
        &*state.social_store,
        &wattetheria_social::domain::friendships::Friendship {
            friendship_id: format!("friendship:{local_public_id}:{remote_public_id}"),
            local_public_id: local_public_id.clone(),
            remote_public_id: remote_public_id.clone(),
            display_name: Some("Broker Borealis".to_string()),
            state: wattetheria_social::domain::friendships::FriendshipState::Active,
            established_from_request_id: None,
            thread_id: None,
            created_at: 1,
            updated_at: 1,
        },
    )
    .expect("seed social friendship");
    {
        let mut registry = state.relationship_registry.lock().await;
        registry.upsert(
            &local_public_id,
            "legacy-remote-node-id",
            wattetheria_kernel::civilization::relationships::RelationshipKind::Friend,
            true,
        );
    }

    let friends = authed_get_json(
        app.clone(),
        &token,
        &format!("/v1/client/friends?public_id={local_public_id}"),
    )
    .await;

    let items = friends.as_array().expect("client friends array");
    assert_eq!(items.len(), 1);
    assert_eq!(
        items[0]["public_id"].as_str(),
        Some(remote_public_id.as_str())
    );
    assert_eq!(items[0]["display_name"].as_str(), Some("Broker Borealis"));
    assert_eq!(
        items[0]["counterpart_agent_public_id"].as_str(),
        Some(remote_public_id.as_str())
    );
    assert_eq!(
        items[0]["counterpart_agent_did"].as_str(),
        Some(remote_identity.agent_did.as_str())
    );
    assert_eq!(
        items[0]["counterpart_agent_name"].as_str(),
        Some("Broker Borealis")
    );
    assert_eq!(
        items[0]["remote_node_id"].as_str(),
        Some(remote_node_id.as_str())
    );
    assert_eq!(
        items[0]["network_id"].as_str(),
        Some("mainnet:watt-etheria")
    );
    assert_eq!(items[0]["relationship_kind"].as_str(), Some("friend"));

    let mut removed_friendship =
        friendship_service::list_friendships(&*state.social_store, &local_public_id)
            .expect("list seeded friendship")
            .into_iter()
            .find(|friendship| friendship.remote_public_id == remote_public_id)
            .expect("seeded friendship");
    removed_friendship.state = wattetheria_social::domain::friendships::FriendshipState::Removed;
    removed_friendship.updated_at = 2;
    friendship_service::upsert_friendship(&*state.social_store, &removed_friendship)
        .expect("mark friendship removed");

    let friends_after_remove = authed_get_json(
        app.clone(),
        &token,
        &format!("/v1/client/friends?public_id={local_public_id}"),
    )
    .await;
    assert_eq!(friends_after_remove.as_array().unwrap().len(), 0);

    let agent_friends_after_remove = authed_get_json(
        app,
        &token,
        &format!("/v1/wattetheria/social/agent-friends?public_id={local_public_id}"),
    )
    .await;
    assert_eq!(agent_friends_after_remove.as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn client_alias_routes_cover_console_social_reads() {
    let (_dir, app, token, _, _state) = build_test_app(20);

    let export_json = public_get_json(app.clone(), "/v1/client/export?node_limit=1").await;
    assert!(export_json["payload"].is_object());
    assert!(export_json["signature"].is_string());

    let friend_requests = authed_get_json(app, &token, "/v1/client/friend-requests").await;
    assert_eq!(friend_requests["items"].as_array().map(Vec::len), Some(0));
    assert_eq!(friend_requests["count"].as_u64(), Some(0));
}

#[allow(clippy::too_many_lines)]
#[tokio::test]
async fn client_export_excludes_local_friends_and_dm() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let remote_identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let remote_public_id = scoped_id("broker-borealis", &remote_identity.agent_did);
    let remote_node_id = "12D3KooRemotePeer".to_string();
    let thread_id = format!("dm:{remote_node_id}");
    let bridge: Arc<dyn SwarmBridge> = Arc::new(MockSwarmBridge {
        public_bootstrap: false,
        fail_accept_and_finalize: false,
        topic_message_lookup_calls: Mutex::new(Vec::new()),
        local_node_id: identity.agent_did.clone(),
        agent_stats: [(identity.agent_did.clone(), AgentStats::default())]
            .into_iter()
            .collect(),
        network_status: SwarmNetworkStatusView {
            running: true,
            mode: "network".to_string(),
            peer_protocol_distribution: BTreeMap::new(),
        },
        peers: vec![SwarmPeerView {
            node_id: remote_node_id.clone(),
            connected: Some(true),
            recently_seen: Some(true),
            stale: Some(false),
            last_seen_age_ms: None,
            discovery: None,
            metadata: None,
            relationship: None,
        }],
        discovered_agents: BTreeMap::new(),
        subscriptions: Mutex::new(Vec::new()),
        messages: Mutex::new(vec![
            SwarmTopicMessageView {
                message_id: "topic-msg-1".to_string(),
                network_id: "local:test".to_string(),
                feed_key: "hive.general".to_string(),
                scope_hint: "org:crew".to_string(),
                author_node_id: remote_node_id.clone(),
                agent_envelope: None,
                content: json!({"text": "remote crew update"}),
                reply_to_message_id: None,
                created_at: 1_710_000_115,
            },
            SwarmTopicMessageView {
                message_id: "dm-topic-msg-1".to_string(),
                network_id: "local:test".to_string(),
                feed_key: "wattswarm.dm".to_string(),
                scope_hint: "group:dm-private".to_string(),
                author_node_id: remote_node_id.clone(),
                agent_envelope: None,
                content: json!({"text": "private dm topic update"}),
                reply_to_message_id: None,
                created_at: 1_710_000_116,
            },
        ]),
        relationship_views: Mutex::new(vec![SwarmPeerRelationshipView {
            remote_node_id: remote_node_id.clone(),
            relationship_state: "requested".to_string(),
            last_action: "request".to_string(),
            initiated_by: "remote".to_string(),
            agent_envelope: None,
            requested_at: Some(1_710_000_100),
            responded_at: None,
            blocked_at: None,
            cleared_at: None,
            updated_at: 1_710_000_100,
        }]),
        relationship_commands: Mutex::new(Vec::new()),
        dm_threads: Mutex::new(vec![SwarmPeerDmThreadView {
            remote_node_id: remote_node_id.clone(),
            thread_id: thread_id.clone(),
            thread_kind: "direct".to_string(),
            session_state: "ready".to_string(),
            relationship_established_at: Some(1_710_000_090),
            created_at: 1_710_000_090,
            updated_at: 1_710_000_110,
            last_message_at: Some(1_710_000_110),
        }]),
        dm_messages: Mutex::new(BTreeMap::from([(
            thread_id.clone(),
            vec![SwarmPeerDmMessageView {
                thread_id: thread_id.clone(),
                message_id: "dm-msg-1".to_string(),
                remote_node_id: remote_node_id.clone(),
                message_kind: "direct".to_string(),
                direction: "inbound".to_string(),
                delivery_state: "delivered".to_string(),
                a2a_protocol: "google_a2a".to_string(),
                agent_envelope: None,
                content: json!({"type":"text","text":"hello"}),
                encrypted_body: None,
                content_encoding: None,
                created_at: 1_710_000_110,
                acknowledged_at: Some(1_710_000_111),
            }],
        )])),
        dm_commands: Mutex::new(Vec::new()),
        private_hive_key_share_commands: Mutex::new(Vec::new()),
        payment_commands: Mutex::new(Vec::new()),
        settled_tasks: Mutex::new(Vec::new()),
        settle_down: std::sync::atomic::AtomicBool::new(false),
        claim_down: std::sync::atomic::AtomicBool::new(false),
        announce_down: std::sync::atomic::AtomicBool::new(false),
        topic_down: std::sync::atomic::AtomicBool::new(false),
        task_notices: Mutex::new(Vec::new()),
        published_tasks: Mutex::new(Vec::new()),
    });
    let (_dir, app, token, _, state) =
        build_test_app_with_bridge(20, dir, identity.clone(), event_log, bridge);
    let local_public_id = bootstrap_broker_identity(app.clone(), &token, &identity.agent_did).await;
    crate::swarm_sync::save_cached_task_run_projection(
        &state.data_dir,
        wattetheria_kernel::swarm_sync::SwarmTaskRunProjectionSnapshot {
            generated_at: 1_710_000_120,
            recent_tasks: vec![wattetheria_kernel::swarm_sync::SwarmTaskProjectionSummary {
                task_id: "task-swarm-1".to_string(),
                task_type: "topic_consensus".to_string(),
                epoch: 1,
                terminal_state: "open".to_string(),
                committed_candidate_id: None,
                finalized_candidate_id: None,
                retry_attempt: 0,
            }],
            recent_runs: vec![json!({
                "run_id": "run-swarm-1",
                "task_id": "task-swarm-1",
                "status": "QUEUED",
                "task_type": "topic_consensus",
                "created_at": 1_710_000_120_i64,
                "updated_at": 1_710_000_120_i64,
                "started_at": Value::Null,
                "finished_at": Value::Null,
                "counts": {
                    "created": 0,
                    "queued": 1,
                    "leased": 0,
                    "succeeded": 0,
                    "failed": 0,
                    "retry_wait": 0,
                    "cancelled": 0,
                    "remote_dispatched": 0
                }
            })],
        },
    )
    .await
    .unwrap();
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
        let mut topics = state.hive_registry.lock().await;
        topics.upsert_hive(wattetheria_kernel::civilization::topics::TopicCreateSpec {
            network_id: None,
            feed_key: "hive.general".to_string(),
            scope_hint: "org:crew".to_string(),
            display_name: "Crew Hive".to_string(),
            summary: Some("crew coordination".to_string()),
            projection_kind:
                wattetheria_kernel::civilization::topics::TopicProjectionKind::ChatRoom,
            organization_id: Some("crew-org".to_string()),
            mission_id: None,
            participant_public_ids: vec![local_public_id.clone(), remote_public_id.clone()],
            created_by_public_id: local_public_id.clone(),
            why_this_exists: Some("coordination".to_string()),
            public_geo: None,
            active: true,
        });
        topics.upsert_hive(wattetheria_kernel::civilization::topics::TopicCreateSpec {
            network_id: None,
            feed_key: "wattswarm.dm".to_string(),
            scope_hint: "group:dm-private".to_string(),
            display_name: "Private DM".to_string(),
            summary: Some("private direct conversation".to_string()),
            projection_kind:
                wattetheria_kernel::civilization::topics::TopicProjectionKind::DirectConversation,
            organization_id: None,
            mission_id: None,
            participant_public_ids: vec![local_public_id.clone(), remote_public_id.clone()],
            created_by_public_id: local_public_id.clone(),
            why_this_exists: Some("private conversation".to_string()),
            public_geo: None,
            active: true,
        });
    }

    let export_json = public_get_json(
            app,
            &format!("/v1/wattetheria/client/export?public_id={local_public_id}&peer_limit=10&task_limit=10&organization_limit=10&rpc_log_limit=5"),
        )
        .await;
    // The gateway ranks Agents itself; the node keeps the field but sends no rows.
    assert_eq!(export_json["payload"]["leaderboard"], json!([]));
    assert!(export_json["payload"]["friend_relationships"].is_null());
    assert!(export_json["payload"]["pending_friend_requests"].is_null());
    assert_eq!(
        export_json["payload"]["public_blocks"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    assert!(export_json["payload"]["dm_threads"].is_null());
    assert!(export_json["payload"]["dm_messages"].is_null());
    assert_eq!(
        export_json["payload"]["public_topics"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        export_json["payload"]["public_topics"][0]["last_message_author_agent_identity"].as_str(),
        Some("Broker Borealis")
    );
    assert_eq!(
        export_json["payload"]["public_topics"][0]["last_message_author"].as_str(),
        Some("Broker Borealis")
    );
    assert_eq!(
        export_json["payload"]["public_topics"][0]["created_by_agent_identity"].as_str(),
        Some("Captain Aurora")
    );
    assert_eq!(
        export_json["payload"]["public_topic_messages"][0]["author_agent_identity"].as_str(),
        Some("Broker Borealis")
    );
    let public_topic_messages = export_json["payload"]["public_topic_messages"]
        .as_array()
        .unwrap();
    assert_eq!(public_topic_messages.len(), 1);
    assert_eq!(
        public_topic_messages[0]["content"]["text"].as_str(),
        Some("remote crew update")
    );
    // Nodes no longer report task activity; the gateway keeps every mission itself.
    assert!(
        export_json["payload"].get("swarm_task_activity").is_none(),
        "{export_json}"
    );
    assert!(
        export_json["payload"].get("tasks").is_none(),
        "{export_json}"
    );
    assert_eq!(
        export_json["payload"]["public_topics"][0]["source_agent_identity"].as_str(),
        Some("Captain Aurora")
    );
}

#[tokio::test]
async fn client_export_is_public_and_signed() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let mut agent_stats = BTreeMap::new();
    agent_stats.insert(
        identity.agent_did.clone(),
        AgentStats {
            power: 3,
            watt: 42,
            reputation: 5,
            capacity: 2,
        },
    );
    let bridge: Arc<dyn SwarmBridge> = Arc::new(MockSwarmBridge {
        public_bootstrap: false,
        fail_accept_and_finalize: false,
        topic_message_lookup_calls: Mutex::new(Vec::new()),
        local_node_id: identity.agent_did.clone(),
        agent_stats,
        network_status: SwarmNetworkStatusView {
            running: true,
            mode: "network".to_string(),
            peer_protocol_distribution: [("v0.1".to_string(), 1_u64)].into_iter().collect(),
        },
        peers: vec![SwarmPeerView {
            node_id: "peer-a".to_string(),
            connected: Some(true),
            recently_seen: Some(true),
            stale: Some(false),
            last_seen_age_ms: None,
            discovery: None,
            metadata: None,
            relationship: None,
        }],
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
        settled_tasks: Mutex::new(Vec::new()),
        settle_down: std::sync::atomic::AtomicBool::new(false),
        claim_down: std::sync::atomic::AtomicBool::new(false),
        announce_down: std::sync::atomic::AtomicBool::new(false),
        topic_down: std::sync::atomic::AtomicBool::new(false),
        task_notices: Mutex::new(Vec::new()),
        published_tasks: Mutex::new(Vec::new()),
    });
    let (_dir, app, token, _, _state) =
        build_test_app_with_bridge(20, dir, identity.clone(), event_log, bridge);
    let captain = bootstrap_broker_identity(app.clone(), &token, &identity.agent_did).await;

    let export_json = public_get_json(
            app,
            &format!("/v1/wattetheria/client/export?public_id={captain}&node_limit=1&task_limit=10&organization_limit=10&rpc_log_limit=5"),
        )
        .await;
    assert_eq!(
        export_json["payload"]["agent_identity"].as_str(),
        Some("Captain Aurora")
    );
    assert_eq!(
        export_json["payload"]["operator"]["display_name"].as_str(),
        Some("Captain Aurora")
    );
    assert_eq!(
        export_json["payload"]["network_status"]["total_nodes"].as_u64(),
        Some(2)
    );
    assert_eq!(export_json["payload"]["nodes"].as_array().unwrap().len(), 1);
    assert_eq!(
        export_json["payload"]["nodes"][0]["source_agent_identity"].as_str(),
        Some("Captain Aurora")
    );
    assert!(export_json["payload"]["nodes"][0].get("lat").is_none());
    assert!(export_json["payload"]["nodes"][0].get("lng").is_none());
    let verified = verify_payload(
        &export_json["payload"],
        export_json["signature"].as_str().unwrap(),
        export_json["payload"]["public_key"].as_str().unwrap(),
    )
    .unwrap();
    assert!(verified);
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn mission_lifecycle_events_keep_network_task_projection_shape() {
    let (_dir, app, token, _, state) = build_test_app(20);
    let publisher_public_id =
        bootstrap_broker_identity(app.clone(), &token, &state.agent_did).await;
    let mut events = state.stream_tx.subscribe();
    let created = authed_post_json(
        app.clone(),
        &token,
        "/v1/wattetheria/missions",
        json!({
            "title": "Network lifecycle",
            "description": "Keep gateway lifecycle payloads complete",
            "publisher": publisher_public_id,
            "publisher_kind": "player",
            "domain": "trade",
            "reward": {
                "agent_watt": 0,
                "reputation": 1,
                "capacity": 0
            },
            "payload": {"cargo": "ore"}
        }),
    )
    .await;
    let mission_id = created["mission_id"].as_str().unwrap();
    let agent_did = "agent-worker";
    mission_gateway::mission_gateway(&state)
        .lock()
        .await
        .ask(mission_id, agent_did);
    let actor_claim_route = json!({
        "decision_payload": {
            "display_name": "Agent-MX1111",
            "public_id": "agent-MX1111.public"
        }
    });

    let published = tokio::time::timeout(std::time::Duration::from_secs(2), events.recv())
        .await
        .expect("publish event timeout")
        .expect("publish event");
    assert_eq!(published.kind, "mission.published");
    assert_public_geo_projection(&published.payload);
    assert_public_geo_projection(&published.payload["task_contract"]["inputs"]);
    assert_eq!(
        published.payload["created_by_agent_identity"].as_str(),
        Some("Captain Aurora")
    );
    assert!(
        published.payload["source_agent_identity"]
            .as_str()
            .is_some()
    );

    let _ = authed_post_json(
        app.clone(),
        &token,
        &format!("/v1/wattetheria/missions/{mission_id}/claim"),
        json!({
            "mission_id": mission_id,
            "agent_did": agent_did,
            "claim_route": actor_claim_route
        }),
    )
    .await;
    let claimed = tokio::time::timeout(std::time::Duration::from_secs(2), events.recv())
        .await
        .expect("claim event timeout")
        .expect("claim event");
    assert_eq!(claimed.kind, "mission.claimed");
    assert_mission_gateway_projection_payload(&claimed.payload, mission_id, "claimed", agent_did);
    assert_actor_projection(
        &claimed.payload,
        "claimer",
        agent_did,
        "Agent-MX1111",
        "agent-MX1111.public",
    );

    let _ = authed_post_json(
        app.clone(),
        &token,
        &format!("/v1/wattetheria/missions/{mission_id}/complete"),
        json!({
            "mission_id": mission_id,
            "agent_did": agent_did,
            "claim_route": actor_claim_route
        }),
    )
    .await;
    let completed = tokio::time::timeout(std::time::Duration::from_secs(2), events.recv())
        .await
        .expect("complete event timeout")
        .expect("complete event");
    assert_eq!(completed.kind, "mission.completed");
    assert_mission_gateway_projection_payload(
        &completed.payload,
        mission_id,
        "completed",
        agent_did,
    );
    assert_eq!(completed.payload["completed_by"].as_str(), Some(agent_did));
    assert_actor_projection(
        &completed.payload,
        "claimer",
        agent_did,
        "Agent-MX1111",
        "agent-MX1111.public",
    );
    assert_actor_projection(
        &completed.payload,
        "completer",
        agent_did,
        "Agent-MX1111",
        "agent-MX1111.public",
    );

    let _ = authed_post_json(
        app,
        &token,
        &format!("/v1/wattetheria/missions/{mission_id}/settle"),
        json!({
            "mission_id": mission_id,
            "agent_did": agent_did,
            "claim_route": actor_claim_route
        }),
    )
    .await;
    let settled = tokio::time::timeout(std::time::Duration::from_secs(2), events.recv())
        .await
        .expect("settle event timeout")
        .expect("settle event");
    assert_eq!(settled.kind, "mission.settled");
    assert_mission_gateway_projection_payload(&settled.payload, mission_id, "settled", agent_did);
    assert!(settled.payload["settled_at"].as_i64().is_some());
    assert_actor_projection(
        &settled.payload,
        "claimer",
        agent_did,
        "Agent-MX1111",
        "agent-MX1111.public",
    );
    assert_actor_projection(
        &settled.payload,
        "completer",
        agent_did,
        "Agent-MX1111",
        "agent-MX1111.public",
    );
}

fn assert_mission_gateway_projection_payload(
    payload: &Value,
    mission_id: &str,
    status: &str,
    claimed_by: &str,
) {
    assert_eq!(payload["mission_id"].as_str(), Some(mission_id));
    assert_eq!(payload["task_id"].as_str(), Some(mission_id));
    assert_eq!(payload["task_type"].as_str(), Some("wattetheria.mission"));
    assert_eq!(payload["scope"].as_str(), Some("real_world"));
    assert_eq!(payload["status"].as_str(), Some(status));
    assert_eq!(payload["claimed_by"].as_str(), Some(claimed_by));
    assert_eq!(payload["claimer_agent_did"].as_str(), Some(claimed_by));
    assert_eq!(
        payload["publisher_wattswarm_node_id"].as_str(),
        payload["task_contract"]["inputs"]["publisher_wattswarm_node_id"].as_str()
    );
    assert_eq!(
        payload["mission_scope_hint"].as_str(),
        Some(format!("group:{mission_id}").as_str())
    );
    assert_eq!(
        payload["task_contract"]["inputs"]["mission_scope_hint"].as_str(),
        Some(format!("group:{mission_id}").as_str())
    );
    assert_eq!(
        payload["task_contract"]["inputs"]["scope"].as_str(),
        Some("real_world")
    );
    assert_public_geo_projection(payload);
    assert_public_geo_projection(&payload["task_contract"]["inputs"]);
    assert_eq!(
        payload["swarm_scope"],
        json!({
            "kind": "group",
            "id": mission_id,
        })
    );
}

#[tokio::test]
async fn client_snapshot_can_be_pushed_to_gateway_ingest() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge: Arc<dyn SwarmBridge> = Arc::new(MockSwarmBridge {
        public_bootstrap: false,
        fail_accept_and_finalize: false,
        topic_message_lookup_calls: Mutex::new(Vec::new()),
        local_node_id: identity.agent_did.clone(),
        agent_stats: [(identity.agent_did.clone(), AgentStats::default())]
            .into_iter()
            .collect(),
        network_status: SwarmNetworkStatusView {
            running: true,
            mode: "network".to_string(),
            peer_protocol_distribution: BTreeMap::new(),
        },
        peers: vec![SwarmPeerView {
            node_id: "peer-a".to_string(),
            connected: Some(true),
            recently_seen: Some(true),
            stale: Some(false),
            last_seen_age_ms: None,
            discovery: None,
            metadata: None,
            relationship: None,
        }],
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
        settled_tasks: Mutex::new(Vec::new()),
        settle_down: std::sync::atomic::AtomicBool::new(false),
        claim_down: std::sync::atomic::AtomicBool::new(false),
        announce_down: std::sync::atomic::AtomicBool::new(false),
        topic_down: std::sync::atomic::AtomicBool::new(false),
        task_notices: Mutex::new(Vec::new()),
        published_tasks: Mutex::new(Vec::new()),
    });
    let (_dir, state, token, _) =
        build_test_state_with_bridge(20, dir, identity.clone(), event_log, bridge);
    let app = app(state.clone());
    let captain = bootstrap_broker_identity(app, &token, &identity.agent_did).await;

    let received = Arc::new(Mutex::new(Vec::<Value>::new()));
    let ingest_app = axum::Router::new().route(
        "/api/ingest/snapshot",
        post({
            let received = Arc::clone(&received);
            move |Json(payload): Json<Value>| {
                let received = Arc::clone(&received);
                async move {
                    received.lock().await.push(payload);
                    Json(json!({"status":"ok"}))
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, ingest_app).await.unwrap();
    });

    let client = reqwest::Client::new();
    let pushed = push_signed_public_client_snapshot(
        &client,
        &format!("http://{addr}"),
        &state,
        &ClientExportQuery {
            public_id: Some(captain),
            node_limit: Some(1),
            task_limit: Some(5),
            organization_limit: Some(5),
            rpc_log_limit: Some(5),
            ..ClientExportQuery::default()
        },
    )
    .await
    .unwrap();

    let received = received.lock().await;
    assert_eq!(received.len(), 1);
    assert_eq!(
        received[0]["payload"]["node_id"].as_str(),
        Some(pushed.payload.node_id.as_str())
    );
    assert_eq!(
        received[0]["signature"].as_str(),
        Some(pushed.signature.as_str())
    );

    server.abort();
}

#[tokio::test]
async fn gateway_node_event_can_be_pushed_to_event_ingest() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge: Arc<dyn SwarmBridge> = Arc::new(MockSwarmBridge {
        public_bootstrap: false,
        fail_accept_and_finalize: false,
        topic_message_lookup_calls: Mutex::new(Vec::new()),
        local_node_id: identity.agent_did.clone(),
        agent_stats: [(identity.agent_did.clone(), AgentStats::default())]
            .into_iter()
            .collect(),
        network_status: SwarmNetworkStatusView {
            running: true,
            mode: "network".to_string(),
            peer_protocol_distribution: BTreeMap::new(),
        },
        peers: vec![],
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
        settled_tasks: Mutex::new(Vec::new()),
        settle_down: std::sync::atomic::AtomicBool::new(false),
        claim_down: std::sync::atomic::AtomicBool::new(false),
        announce_down: std::sync::atomic::AtomicBool::new(false),
        topic_down: std::sync::atomic::AtomicBool::new(false),
        task_notices: Mutex::new(Vec::new()),
        published_tasks: Mutex::new(Vec::new()),
    });
    let (_dir, state, _token, _) =
        build_test_state_with_bridge(20, dir, identity.clone(), event_log, bridge);

    let event = StreamEvent {
        kind: "organization.created".to_string(),
        timestamp: Utc::now().timestamp(),
        payload: json!({
            "organization_id": "org-1",
            "name": "Aurora Guild",
        }),
    };
    let signed = build_signed_node_event(&state, &event)
        .unwrap()
        .expect("gateway event");

    let received = Arc::new(Mutex::new(Vec::<Value>::new()));
    let ingest_app = axum::Router::new().route(
        "/api/ingest/event",
        post({
            let received = Arc::clone(&received);
            move |Json(payload): Json<Value>| {
                let received = Arc::clone(&received);
                async move {
                    received.lock().await.push(payload);
                    Json(json!({"status":"ok"}))
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, ingest_app).await.unwrap();
    });

    let client = reqwest::Client::new();
    let receipt = push_signed_node_event(&client, &format!("http://{addr}"), &signed)
        .await
        .unwrap();
    assert_eq!(receipt, json!({"status":"ok"}));

    let received = received.lock().await;
    assert_eq!(received.len(), 1);
    assert_eq!(
        received[0]["payload"]["event_kind"].as_str(),
        Some("organization.created")
    );
    assert_eq!(
        received[0]["payload"]["data_kind"].as_str(),
        Some("organization_summary")
    );

    server.abort();
}

type MockLedgerCalls = Arc<Mutex<Vec<(String, Value)>>>;

#[derive(Clone)]
struct MockLedger {
    calls: MockLedgerCalls,
    balance: i64,
    /// Escrows the gateway holds for this account; the gateway locks them when it publishes a
    /// mission with a Watt bounty.
    held: Vec<Value>,
}

impl MockLedger {
    fn with_balance(calls: MockLedgerCalls, balance: i64) -> Self {
        Self {
            calls,
            balance,
            held: Vec::new(),
        }
    }

    fn holding(mut self, escrow: Value) -> Self {
        self.held.push(escrow);
        self
    }
}
type MockLedgerState = axum::extract::State<MockLedger>;

/// A stand-in for the gateway ledger's read routes: balance, held escrows and history.
#[allow(clippy::too_many_lines)]
fn mock_ledger_router(ledger: MockLedger) -> Router {
    fn account_escrows(ledger: &MockLedger, account_id: &str) -> Vec<Value> {
        ledger
            .held
            .iter()
            .map(|escrow| {
                let mut escrow = escrow.clone();
                escrow["payer_account"] = json!(account_id);
                escrow
            })
            .collect()
    }
    async fn account(
        axum::extract::State(ledger): MockLedgerState,
        Path(account_id): Path<String>,
    ) -> Json<Value> {
        let locked: i64 = account_escrows(&ledger, &account_id)
            .iter()
            .filter(|escrow| escrow["status"] == "locked")
            .filter_map(|escrow| escrow["amount"].as_i64())
            .sum();
        Json(json!({
            "account_id": account_id,
            "balance": ledger.balance,
            "locked_in_escrow": locked,
            "exists": true,
        }))
    }
    async fn escrows(
        axum::extract::State(ledger): MockLedgerState,
        Path(account_id): Path<String>,
        Query(query): Query<BTreeMap<String, String>>,
    ) -> Json<Value> {
        let status = query.get("status").cloned();
        let escrows: Vec<Value> = account_escrows(&ledger, &account_id)
            .into_iter()
            .filter(|escrow| {
                status
                    .as_deref()
                    .is_none_or(|status| escrow["status"].as_str() == Some(status))
            })
            .collect();
        ledger.calls.lock().await.push((
            "escrows".to_string(),
            json!({"account_id": account_id, "query": query}),
        ));
        Json(json!({"account_id": account_id, "escrows": escrows}))
    }
    /// A fixed five-entry history (seq 1..=5) so cursor handling is observable.
    async fn entries(
        axum::extract::State(ledger): MockLedgerState,
        Path(account_id): Path<String>,
        Query(query): Query<BTreeMap<String, String>>,
    ) -> Json<Value> {
        let limit: usize = query
            .get("limit")
            .and_then(|limit| limit.parse().ok())
            .unwrap_or(100);
        let before_seq: i64 = query
            .get("before_seq")
            .and_then(|seq| seq.parse().ok())
            .unwrap_or(i64::MAX);
        let history = [
            (
                5,
                "escrow_release",
                "in",
                "did:key:z6MkPublisherPat",
                Some("Pat"),
                "Mission bounty: Map the reef",
            ),
            (4, "transfer", "out", "did:key:z6MkFriendFay", None, "Tip"),
            (
                3,
                "escrow_lock",
                "out",
                "system:escrow",
                None,
                "Mission bounty: Escrowed task",
            ),
            (
                2,
                "transfer",
                "in",
                "did:key:z6MkFriendFay",
                Some("Fay"),
                "Loan",
            ),
            (1, "welcome", "in", "system:issuer", None, "Welcome bonus"),
        ];
        let page: Vec<Value> = history
            .iter()
            .filter(|(seq, ..)| *seq < before_seq)
            .take(limit)
            .map(|(seq, kind, direction, counterparty, label, subject)| {
                let mut entry = json!({
                    "seq": seq,
                    "kind": kind,
                    "direction": direction,
                    "amount": 10 * seq,
                    "counterparty_account": counterparty,
                    "counterparty_label": label,
                    "subject": subject,
                });
                if *kind == "escrow_lock" {
                    entry["escrow_id"] = json!("mission-lock-3");
                    entry["escrow_status"] = json!("released");
                    entry["escrow_payee_account"] = json!("did:key:z6MkWorkerWen");
                    entry["escrow_payee_label"] = json!("Wen");
                    entry["escrow_resolved_at_ms"] = json!(1_700_000_000_000_i64);
                }
                entry
            })
            .collect();
        let next_before_seq = (page.len() == limit)
            .then(|| page.last().and_then(|entry| entry["seq"].as_i64()))
            .flatten();
        ledger.calls.lock().await.push((
            "entries".to_string(),
            json!({"account_id": account_id, "query": query}),
        ));
        Json(json!({
            "account_id": account_id,
            "entries": page,
            "next_before_seq": next_before_seq,
        }))
    }
    Router::new()
        .route("/api/ledger/accounts/{account_id}", get(account))
        .route("/api/ledger/accounts/{account_id}/escrows", get(escrows))
        .route("/api/ledger/accounts/{account_id}/entries", get(entries))
        .with_state(ledger)
}

#[tokio::test]
async fn mcp_publish_mission_sends_its_watt_bounty_to_the_gateway() {
    let calls: MockLedgerCalls = Arc::new(Mutex::new(Vec::new()));
    let (app, state, token, _public_id, _dir) =
        ledger_backed_node(MockLedger::with_balance(calls, 100)).await;
    let own_name = local_agent_display_name(app.clone(), &token).await;

    let mission = mcp_tool_call(
        app.clone(),
        &token,
        "publish_mission",
        json!({
            "title": "Chart the trade lane",
            "description": "Published through MCP with a Watt bounty.",
            "domain": "trade",
            "payload": {"objective": "chart"},
            "reward": {"agent_watt": 70}
        }),
    )
    .await;
    assert_eq!(mission["_is_error"], json!(false), "{mission}");
    let mission_id = mission["mission_id"].as_str().unwrap().to_string();
    assert_eq!(mission["reward"]["agent_watt"], json!(70));
    let gateway = mission_gateway::mission_gateway(&state);
    {
        let gateway = gateway.lock().await;
        let published = gateway.commands("publish");
        assert_eq!(published.len(), 1, "{published:?}");
        assert!(published[0].get("mission_id").is_none(), "{}", published[0]);
        assert!(published[0]["request_id"].is_string(), "{}", published[0]);
        assert_eq!(gateway.status(&mission_id).as_deref(), Some("published"));
        assert_eq!(published[0]["publisher"], json!(state.agent_did));
        assert_eq!(published[0]["publisher_label"], json!(own_name));
        assert_eq!(published[0]["title"], json!("Chart the trade lane"));
        assert_eq!(published[0]["reward"]["kind"], json!("watt"));
        assert_eq!(published[0]["reward"]["amount"], json!(70));
    }

    // Beyond the balance: refused as insufficient_watt and the gateway keeps no such mission.
    let refused = mcp_tool_call(
        app.clone(),
        &token,
        "publish_mission",
        json!({
            "title": "Too rich",
            "description": "More than the publisher holds.",
            "domain": "trade",
            "payload": {"objective": "overdraw"},
            "reward": {"agent_watt": 500}
        }),
    )
    .await;
    assert_eq!(refused["_is_error"], json!(true), "{refused}");
    assert_eq!(refused["code"], json!("insufficient_watt"));
    let gateway = gateway.lock().await;
    assert!(
        gateway
            .missions
            .values()
            .all(|mission| mission["title"] != "Too rich")
    );
    assert_eq!(gateway.missions.len(), 1);
}

/// Display name of the identity the node itself reports as current.
async fn local_agent_display_name(app: Router, token: &str) -> String {
    let identity = authed_get_json(app, token, "/v1/civilization/public-identity").await;
    identity["public_identity"]["display_name"]
        .as_str()
        .unwrap_or_else(|| panic!("node reports no current public identity: {identity}"))
        .to_owned()
}

async fn mcp_tool_call(app: Router, token: &str, name: &str, arguments: Value) -> Value {
    let response = authed_post_json(
        app,
        token,
        "/mcp",
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {"name": name, "arguments": arguments}
        }),
    )
    .await;
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("MCP tool {name} returned no text content: {response}"));
    let mut payload: Value = serde_json::from_str(text).unwrap();
    payload["_is_error"] = response["result"]["isError"].clone();
    payload
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn wallet_reads_own_balance_locked_bounties_and_history_from_the_ledger() {
    let calls: MockLedgerCalls = Arc::new(Mutex::new(Vec::new()));
    let mission_id = "mission-held-1";
    let ledger = MockLedger::with_balance(calls.clone(), 300).holding(json!({
        "escrow_id": mission_id,
        "amount": 120,
        "reference": mission_id,
        "payer_label": "Captain Aurora",
        "subject": "Mission bounty: Escrowed task",
        "status": "locked",
    }));
    let (app, state, token, _public_id, _dir) = ledger_backed_node(ledger).await;

    // The wallet is labelled with the same identity `get_agent_identity` reports.
    let own_name = local_agent_display_name(app.clone(), &token).await;
    let wallet = authed_get_json(app.clone(), &token, "/v1/wattetheria/wallet").await;
    assert_eq!(wallet["agent_did"], json!(state.agent_did), "{wallet}");
    assert_eq!(wallet["display_name"], json!(own_name));
    assert_eq!(wallet["balance"], json!(300));
    assert_eq!(wallet["locked_in_escrow"], json!(120));
    assert_eq!(wallet["locked_escrows"].as_array().unwrap().len(), 1);
    let locked = &wallet["locked_escrows"][0];
    assert_eq!(locked["escrow_id"], json!(mission_id));
    assert_eq!(locked["subject"], json!("Mission bounty: Escrowed task"));
    assert_eq!(locked["payer_label"], json!("Captain Aurora"));
    assert_eq!(locked["payer_account"], json!(state.agent_did));
    {
        let recorded = calls.lock().await;
        let (kind, query) = recorded
            .iter()
            .find(|(kind, _)| kind == "escrows")
            .expect("wallet asks the ledger for escrows");
        assert_eq!(kind, "escrows");
        assert_eq!(query["account_id"], json!(state.agent_did));
        assert_eq!(query["query"]["status"], json!("locked"));
    }

    let page = authed_get_json(
        app.clone(),
        &token,
        "/v1/wattetheria/wallet/entries?limit=2",
    )
    .await;
    assert_eq!(page["agent_did"], json!(state.agent_did), "{page}");
    let kinds: Vec<&str> = page["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["escrow_release", "transfer"]);
    assert_eq!(page["entries"][0]["direction"], json!("in"));
    assert_eq!(page["entries"][0]["counterparty_label"], json!("Pat"));
    assert_eq!(page["next_before_seq"], json!(4));
    let next = authed_get_json(
        app.clone(),
        &token,
        "/v1/wattetheria/wallet/entries?limit=2&before_seq=4",
    )
    .await;
    let kinds: Vec<&str> = next["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["escrow_lock", "transfer"], "{next}");
    assert_eq!(next["next_before_seq"], json!(2));
    // The publisher's lock entry carries the escrow outcome the ledger attached to it.
    let lock = &next["entries"][0];
    assert_eq!(lock["escrow_status"], json!("released"));
    assert_eq!(lock["escrow_payee_account"], json!("did:key:z6MkWorkerWen"));
    assert_eq!(lock["escrow_payee_label"], json!("Wen"));
    assert_eq!(lock["escrow_resolved_at_ms"], json!(1_700_000_000_000_i64));
    assert!(next["entries"][1].get("escrow_status").is_none());
    {
        let recorded = calls.lock().await;
        let entry_queries: Vec<&Value> = recorded
            .iter()
            .filter(|(kind, _)| kind == "entries")
            .map(|(_, query)| query)
            .collect();
        assert_eq!(entry_queries.len(), 2);
        assert_eq!(entry_queries[0]["account_id"], json!(state.agent_did));
        assert_eq!(entry_queries[0]["query"]["limit"], json!("2"));
        assert_eq!(entry_queries[1]["query"]["before_seq"], json!("4"));
    }
    let bad_cursor = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("GET")
                .uri("/v1/wattetheria/wallet/entries?before_seq=0")
                .header("authorization", format!("Bearer {token}"))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(bad_cursor.status(), StatusCode::BAD_REQUEST);

    // The same two views through the MCP tools agents actually use.
    let balance = mcp_tool_call(app.clone(), &token, "get_watt_balance", json!({})).await;
    assert_eq!(balance["_is_error"], json!(false), "{balance}");
    assert_eq!(balance["balance"], json!(300));
    assert_eq!(balance["locked_in_escrow"], json!(120));
    assert_eq!(balance["agent_did"], json!(state.agent_did));
    let history = mcp_tool_call(
        app.clone(),
        &token,
        "list_watt_ledger_entries",
        json!({"limit": 1, "before_seq": 2}),
    )
    .await;
    assert_eq!(history["_is_error"], json!(false), "{history}");
    assert_eq!(history["entries"].as_array().unwrap().len(), 1);
    assert_eq!(history["entries"][0]["kind"], json!("welcome"));
    assert_eq!(
        history["entries"][0]["counterparty_account"],
        json!("system:issuer")
    );
    // A full page always carries a cursor; the next call returns an empty page.
    assert_eq!(history["next_before_seq"], json!(1));
    let end = mcp_tool_call(
        app.clone(),
        &token,
        "list_watt_ledger_entries",
        json!({"limit": 1, "before_seq": 1}),
    )
    .await;
    assert_eq!(end["entries"], json!([]), "{end}");
    assert_eq!(end["next_before_seq"], Value::Null);
}

/// Boots a node whose gateway is the mission stand-in with the given mock ledger, the
/// publisher's balance limiting the bounties it may publish. Returns the app, its state, the
/// control token and the bootstrapped broker `public_id`.
async fn ledger_backed_node(
    ledger: MockLedger,
) -> (
    Router,
    crate::state::ControlPlaneState,
    String,
    String,
    tempfile::TempDir,
) {
    let balance = ledger.balance;
    let (url, gateway) =
        mission_gateway::spawn_mission_gateway(mock_ledger_router(ledger)).unwrap();
    gateway.lock().await.balance = Some(balance);
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge: Arc<dyn SwarmBridge> =
        Arc::new(MockSwarmBridge::default_for(identity.agent_did.clone()));
    let (dir, mut state, token, _) =
        build_test_state_with_bridge(20, dir, identity, event_log, bridge);
    state.gateway_urls = vec![format!("{url}/api/ingest/snapshot")];
    let app = crate::app(state.clone());
    let public_id = bootstrap_broker_identity(app.clone(), &token, &state.agent_did).await;
    (app, state, token, public_id, dir)
}

fn escrowed_mission_body(public_id: &str, bounty: i64) -> Value {
    json!({
        "title": "Escrowed task",
        "description": "Bounty locked in the ledger.",
        "publisher": public_id,
        "publisher_kind": "player",
        "domain": "trade",
        "subnet_id": null,
        "zone_id": null,
        "required_role": null,
        "required_faction": null,
        "reward": {"agent_watt": bounty, "reputation": 1, "capacity": 1},
        "payload": {"objective": "escrow"}
    })
}

async fn post_status(app: Router, token: &str, uri: &str, body: &Value) -> (StatusCode, Value) {
    let response = app
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri(uri)
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(axum::body::Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let body = serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes())
        .unwrap_or(Value::Null);
    (status, body)
}

#[tokio::test]
async fn a_watt_bounty_is_published_and_settled_through_signed_gateway_commands() {
    let calls: MockLedgerCalls = Arc::new(Mutex::new(Vec::new()));
    let (app, state, token, public_id, _dir) =
        ledger_backed_node(MockLedger::with_balance(calls, 150)).await;
    let payer_did = state.identity.agent_did.clone();
    let executor_did = "did:key:z6MkExecutorForEscrowTest".to_string();
    let gateway = mission_gateway::mission_gateway(&state);

    let mission = authed_post_json(
        app.clone(),
        &token,
        "/v1/wattetheria/missions",
        escrowed_mission_body(&public_id, 100),
    )
    .await;
    let mission_id = mission["mission_id"].as_str().unwrap().to_string();
    {
        let gateway = gateway.lock().await;
        let published = gateway.commands("publish");
        assert_eq!(published.len(), 1, "{published:?}");
        assert!(published[0].get("mission_id").is_none(), "{}", published[0]);
        assert!(published[0]["request_id"].is_string(), "{}", published[0]);
        assert_eq!(gateway.status(&mission_id).as_deref(), Some("published"));
        assert_eq!(published[0]["publisher"], json!(payer_did));
        assert_eq!(
            published[0]["reward"],
            json!({"kind": "watt", "amount": 100, "reputation": 1, "capacity": 1})
        );
        assert!(published[0]["nonce"].is_string(), "{}", published[0]);
        assert!(published[0]["issued_at"].is_string(), "{}", published[0]);
    }

    let (status, body) = post_status(
        app.clone(),
        &token,
        "/v1/wattetheria/missions",
        &escrowed_mission_body(&public_id, 500),
    )
    .await;
    assert_eq!(status, StatusCode::PAYMENT_REQUIRED, "{body}");
    assert_eq!(body["code"], json!("insufficient_watt"));
    assert_eq!(
        gateway.lock().await.missions.len(),
        1,
        "a refused bounty leaves no mission"
    );

    gateway.lock().await.ask(&mission_id, &executor_did);
    let claimed = authed_post_json(
        app.clone(),
        &token,
        &format!("/v1/wattetheria/missions/{mission_id}/claim"),
        json!({"mission_id": mission_id, "agent_did": executor_did}),
    )
    .await;
    assert_eq!(claimed["claimed_by"], json!(executor_did), "{claimed}");
    let _ = authed_post_json(
        app.clone(),
        &token,
        &format!("/v1/wattetheria/missions/{mission_id}/complete"),
        json!({"mission_id": mission_id, "agent_did": executor_did}),
    )
    .await;
    let settled = authed_post_json(
        app.clone(),
        &token,
        &format!("/v1/wattetheria/missions/{mission_id}/settle"),
        json!({"mission_id": mission_id}),
    )
    .await;
    assert_eq!(settled["status"], json!("settled"), "{settled}");
    {
        let gateway = gateway.lock().await;
        assert_eq!(gateway.status(&mission_id).as_deref(), Some("settled"));
        let settle = gateway.commands("settle");
        assert_eq!(settle.len(), 1);
        assert_eq!(settle[0]["publisher"], json!(payer_did));
        assert_eq!(
            gateway.commands("approve")[0]["claimer"],
            json!(executor_did)
        );
    }

    let self_json = authed_get_json(app, &token, "/v1/client/self").await;
    assert_eq!(self_json["watt_balance"].as_i64(), Some(150));
    assert_eq!(self_json["watt_balance_available"].as_bool(), Some(true));
}

#[tokio::test]
async fn cancelling_a_mission_is_a_gateway_command() {
    let calls: MockLedgerCalls = Arc::new(Mutex::new(Vec::new()));
    let (app, state, token, public_id, _dir) =
        ledger_backed_node(MockLedger::with_balance(calls, 150)).await;
    let payer_did = state.identity.agent_did.clone();
    let gateway = mission_gateway::mission_gateway(&state);
    let mission = authed_post_json(
        app.clone(),
        &token,
        "/v1/wattetheria/missions",
        escrowed_mission_body(&public_id, 40),
    )
    .await;
    let mission_id = mission["mission_id"].as_str().unwrap().to_string();

    let cancelled = authed_post_json(
        app.clone(),
        &token,
        &format!("/v1/wattetheria/missions/{mission_id}/cancel"),
        json!({"reason": "publisher withdrew"}),
    )
    .await;
    assert_eq!(cancelled["status"], json!("cancelled"), "{cancelled}");
    {
        let gateway = gateway.lock().await;
        assert_eq!(gateway.status(&mission_id).as_deref(), Some("cancelled"));
        let cancel = gateway.commands("cancel");
        assert_eq!(cancel.len(), 1);
        assert_eq!(cancel[0]["publisher"], json!(payer_did));
        assert_eq!(cancel[0]["reason"], json!("publisher withdrew"));
    }
    // A closed mission cannot be cancelled again.
    let (status, body) = post_status(
        app.clone(),
        &token,
        &format!("/v1/wattetheria/missions/{mission_id}/cancel"),
        &json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], json!("mission_closed"));

    // Neither can a settled one.
    let executor_did = "did:key:z6MkExecutorForCancelTest".to_string();
    let other = authed_post_json(
        app.clone(),
        &token,
        "/v1/wattetheria/missions",
        escrowed_mission_body(&public_id, 10),
    )
    .await;
    let other_id = other["mission_id"].as_str().unwrap().to_string();
    gateway.lock().await.ask(&other_id, &executor_did);
    for step in ["claim", "complete"] {
        let _ = authed_post_json(
            app.clone(),
            &token,
            &format!("/v1/wattetheria/missions/{other_id}/{step}"),
            json!({"mission_id": other_id, "agent_did": executor_did}),
        )
        .await;
    }
    let _ = authed_post_json(
        app.clone(),
        &token,
        &format!("/v1/wattetheria/missions/{other_id}/settle"),
        json!({"mission_id": other_id}),
    )
    .await;
    assert_eq!(
        gateway.lock().await.status(&other_id).as_deref(),
        Some("settled")
    );
    let (status, body) = post_status(
        app,
        &token,
        &format!("/v1/wattetheria/missions/{other_id}/cancel"),
        &json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
}

#[tokio::test]
async fn publishing_without_a_gateway_is_refused() {
    let (_dir, _app, token, _, state) = build_test_app(20);
    let mut state = state;
    state.gateway_urls = Vec::new();
    let app = crate::app(state.clone());
    let public_id = bootstrap_broker_identity(app.clone(), &token, &state.agent_did).await;
    for bounty in [25, 0] {
        let (status, body) = post_status(
            app.clone(),
            &token,
            "/v1/wattetheria/missions",
            &escrowed_mission_body(&public_id, bounty),
        )
        .await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
        assert_eq!(body["code"], json!("gateway_unavailable"));
    }
}
