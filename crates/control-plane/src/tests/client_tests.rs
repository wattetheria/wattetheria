use super::*;

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
        fail_accept_and_finalize: false,
        local_node_id: identity.agent_did.clone(),
        agent_stats,
        network_status: SwarmNetworkStatusView {
            running: true,
            mode: "network".to_string(),
            peer_protocol_distribution: [("v0.1".to_string(), 1_u64)].into_iter().collect(),
        },
        peers: vec![SwarmPeerView {
            node_id: "peer-a".to_string(),
        }],
        subscriptions: Mutex::new(Vec::new()),
        messages: Mutex::new(Vec::new()),
        relationship_views: Mutex::new(Vec::new()),
        relationship_commands: Mutex::new(Vec::new()),
        dm_threads: Mutex::new(Vec::new()),
        dm_messages: Mutex::new(BTreeMap::new()),
        dm_commands: Mutex::new(Vec::new()),
        payment_commands: Mutex::new(Vec::new()),
    });
    let (_dir, app, token, _, _state) =
        build_test_app_with_bridge(20, dir, identity.clone(), event_log, bridge);

    let captain = bootstrap_broker_identity(app.clone(), &token, &identity.agent_did).await;
    let _published = publish_trade_mission(
        app.clone(),
        &token,
        TradeMissionSpec {
            title: "Calibrate relay",
            description: "Tune the frontier relay.",
            reward_watt: 24,
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
    assert!(peers_json[0].get("lat").is_none());
    assert!(peers_json[0].get("lng").is_none());

    let self_json = authed_get_json(
        app.clone(),
        &token,
        &format!("/v1/client/self?public_id={captain}"),
    )
    .await;
    assert_eq!(self_json["id"].as_str(), Some(captain.as_str()));
    assert_eq!(self_json["display_name"].as_str(), Some("Captain Aurora"));
    assert_eq!(self_json["watt_balance"].as_i64(), Some(0));
    assert_eq!(self_json["reward_policy_version"].as_u64(), Some(1));
    assert_eq!(
        self_json["wallet_bound_agent_did"].as_str(),
        Some(identity.agent_did.as_str())
    );
    assert!(self_json["active_payment_account"].is_null());

    let tasks_json = authed_get_json(app.clone(), &token, "/v1/client/tasks").await;
    assert_eq!(tasks_json.as_array().unwrap().len(), 1);
    assert_eq!(tasks_json[0]["title"].as_str(), Some("Calibrate relay"));
    assert_eq!(tasks_json[0]["status"].as_str(), Some("published"));
    assert_eq!(tasks_json[0]["reward_watt"].as_i64(), Some(24));
    assert_eq!(tasks_json[0]["task_bounty_watt"].as_i64(), Some(24));
    assert_eq!(tasks_json[0]["executor_bounty_watt"].as_i64(), Some(24));
    assert_eq!(
        tasks_json[0]["network_publish_reward_watt"].as_i64(),
        Some(1)
    );
    assert_eq!(
        tasks_json[0]["network_settle_publisher_reward_watt"].as_i64(),
        Some(2)
    );
    assert_eq!(
        tasks_json[0]["publisher_network_reward_watt"].as_i64(),
        Some(1)
    );

    let organizations_json = authed_get_json(app.clone(), &token, "/v1/client/organizations").await;
    assert_eq!(organizations_json.as_array().unwrap().len(), 1);
    assert_eq!(organizations_json[0]["name"].as_str(), Some("Aurora Guild"));
    assert_eq!(organizations_json[0]["treasury_watt"].as_i64(), Some(55));
    assert_eq!(organizations_json[0]["member_count"].as_u64(), Some(1));

    let leaderboard_json = authed_get_json(
        app.clone(),
        &token,
        "/v1/client/leaderboard?category=wealth",
    )
    .await;
    assert_eq!(leaderboard_json.as_array().unwrap().len(), 1);
    assert_eq!(leaderboard_json[0]["rank"].as_u64(), Some(1));
    assert_eq!(
        leaderboard_json[0]["display_name"].as_str(),
        Some("Captain Aurora")
    );

    let rpc_logs_json = authed_get_json(app, &token, "/v1/client/rpc-logs?limit=5").await;
    assert!(!rpc_logs_json.as_array().unwrap().is_empty());
    assert!(rpc_logs_json[0]["timestamp"].is_string());
    assert!(rpc_logs_json[0]["message"].is_string());
    assert!(rpc_logs_json[0]["level"].is_string());
}

#[tokio::test]
async fn client_self_reports_wallet_bound_mission_rewards() {
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
        "/v1/missions",
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
                "agent_watt": 24,
                "reputation": 3,
                "capacity": 1,
                "treasury_share_watt": 0
            },
            "payload": {"objective": "wallet-bound-balance"}
        }),
    )
    .await;
    let mission_id = mission["mission_id"].as_str().unwrap();
    let _ = authed_post_json(
        app.clone(),
        &token,
        "/v1/missions/claim",
        json!({"mission_id": mission_id, "agent_did": agent_did}),
    )
    .await;
    let _ = authed_post_json(
        app.clone(),
        &token,
        "/v1/missions/complete",
        json!({"mission_id": mission_id, "agent_did": agent_did}),
    )
    .await;
    let _ = authed_post_json(
        app.clone(),
        &token,
        "/v1/missions/settle",
        json!({"mission_id": mission_id}),
    )
    .await;

    let tasks_json = authed_get_json(app.clone(), &token, "/v1/client/tasks").await;
    assert_eq!(tasks_json.as_array().unwrap().len(), 1);
    assert_eq!(tasks_json[0]["reward_watt"].as_i64(), Some(24));
    assert_eq!(tasks_json[0]["executor_bounty_watt"].as_i64(), Some(24));
    assert_eq!(
        tasks_json[0]["publisher_network_reward_watt"].as_i64(),
        Some(3)
    );

    let balance_state: wattetheria_kernel::economy::WalletBalanceState = state
        .local_db
        .load_domain(wattetheria_kernel::local_db::domain::WATT_BALANCE_STATE)
        .unwrap()
        .unwrap();
    let stored = balance_state.get(&agent_did, Some(&public_id)).unwrap();
    assert_eq!(stored.watt_balance, 27);
    assert_eq!(stored.policy_version, 1);

    let self_json = authed_get_json(
        app,
        &token,
        &format!("/v1/client/self?public_id={public_id}"),
    )
    .await;
    assert_eq!(self_json["watt_balance"].as_i64(), Some(27));
    assert_eq!(self_json["reward_policy_version"].as_u64(), Some(1));
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
        "/v1/missions",
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
                "agent_watt": 8,
                "reputation": 1,
                "capacity": 0,
                "treasury_share_watt": 0
            },
            "payload": {"objective": "remote-result"}
        }),
    )
    .await;
    let mission_id = mission["mission_id"].as_str().unwrap();
    let worker = "did:key:worker";

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
                    "inspect_task",
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
                    "agent_did": worker
                }
            }
        }),
    )
    .await;
    assert_eq!(committed["status"].as_str(), Some("settled"));

    let board = state.mission_board.lock().await;
    let settled = board.get(mission_id).unwrap();
    assert_eq!(
        settled.status,
        wattetheria_kernel::civilization::missions::MissionStatus::Settled
    );
    assert_eq!(settled.completed_by.as_deref(), Some(worker));
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
        "/v1/missions",
        json!({
            "title": "Fail path",
            "description": "Task finalize failure must not change mission state.",
            "publisher": public_id,
            "publisher_kind": "player",
            "domain": "trade",
            "reward": {"agent_watt": 1, "reputation": 0, "capacity": 0, "treasury_share_watt": 0},
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
                "allowed_actions": ["inspect_task", "accept_result", "reject_result", "request_retry"],
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

    let board = state.mission_board.lock().await;
    let stuck = board.get(mission_id).unwrap();
    assert_eq!(
        stuck.status,
        wattetheria_kernel::civilization::missions::MissionStatus::Open,
        "mission must remain Open when task finalize fails"
    );
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
        fail_accept_and_finalize: false,
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
        }],
        subscriptions: Mutex::new(Vec::new()),
        messages: Mutex::new(Vec::new()),
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
        payment_commands: Mutex::new(Vec::new()),
    });
    let (_dir, app, token, _, state) =
        build_test_app_with_bridge(20, dir, identity.clone(), event_log, bridge);
    let local_public_id = bootstrap_broker_identity(app.clone(), &token, &identity.agent_did).await;
    crate::swarm_sync::save_cached_task_run_projection(
        &state.local_db,
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
        let mut topics = state.topic_registry.lock().await;
        topics.upsert_topic(wattetheria_kernel::civilization::topics::TopicCreateSpec {
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
            active: true,
        });
    }

    let export_json = public_get_json(
            app,
            &format!("/v1/client/export?public_id={local_public_id}&peer_limit=10&task_limit=10&organization_limit=10&rpc_log_limit=5&leaderboard_limit=5"),
        )
        .await;
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
        export_json["payload"]["swarm_task_activity"]["tasks"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        export_json["payload"]["swarm_task_activity"]["runs"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        export_json["payload"]["swarm_task_activity"]["tasks"][0]["task_type"].as_str(),
        Some("topic_consensus")
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
        fail_accept_and_finalize: false,
        local_node_id: identity.agent_did.clone(),
        agent_stats,
        network_status: SwarmNetworkStatusView {
            running: true,
            mode: "network".to_string(),
            peer_protocol_distribution: [("v0.1".to_string(), 1_u64)].into_iter().collect(),
        },
        peers: vec![SwarmPeerView {
            node_id: "peer-a".to_string(),
        }],
        subscriptions: Mutex::new(Vec::new()),
        messages: Mutex::new(Vec::new()),
        relationship_views: Mutex::new(Vec::new()),
        relationship_commands: Mutex::new(Vec::new()),
        dm_threads: Mutex::new(Vec::new()),
        dm_messages: Mutex::new(BTreeMap::new()),
        dm_commands: Mutex::new(Vec::new()),
        payment_commands: Mutex::new(Vec::new()),
    });
    let (_dir, app, token, _, _state) =
        build_test_app_with_bridge(20, dir, identity.clone(), event_log, bridge);
    let captain = bootstrap_broker_identity(app.clone(), &token, &identity.agent_did).await;

    let export_json = public_get_json(
            app,
            &format!("/v1/client/export?public_id={captain}&peer_limit=1&task_limit=10&organization_limit=10&rpc_log_limit=5&leaderboard_limit=5"),
        )
        .await;
    assert_eq!(
        export_json["payload"]["operator"]["display_name"].as_str(),
        Some("Captain Aurora")
    );
    assert_eq!(
        export_json["payload"]["network_status"]["total_nodes"].as_u64(),
        Some(2)
    );
    assert_eq!(export_json["payload"]["peers"].as_array().unwrap().len(), 1);
    assert!(export_json["payload"]["peers"][0].get("lat").is_none());
    assert!(export_json["payload"]["peers"][0].get("lng").is_none());
    let verified = verify_payload(
        &export_json["payload"],
        export_json["signature"].as_str().unwrap(),
        export_json["payload"]["public_key"].as_str().unwrap(),
    )
    .unwrap();
    assert!(verified);
}

#[tokio::test]
async fn client_snapshot_can_be_pushed_to_gateway_ingest() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge: Arc<dyn SwarmBridge> = Arc::new(MockSwarmBridge {
        fail_accept_and_finalize: false,
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
        }],
        subscriptions: Mutex::new(Vec::new()),
        messages: Mutex::new(Vec::new()),
        relationship_views: Mutex::new(Vec::new()),
        relationship_commands: Mutex::new(Vec::new()),
        dm_threads: Mutex::new(Vec::new()),
        dm_messages: Mutex::new(BTreeMap::new()),
        dm_commands: Mutex::new(Vec::new()),
        payment_commands: Mutex::new(Vec::new()),
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
            peer_limit: Some(1),
            task_limit: Some(5),
            organization_limit: Some(5),
            rpc_log_limit: Some(5),
            leaderboard_limit: Some(5),
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
        fail_accept_and_finalize: false,
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
        subscriptions: Mutex::new(Vec::new()),
        messages: Mutex::new(Vec::new()),
        relationship_views: Mutex::new(Vec::new()),
        relationship_commands: Mutex::new(Vec::new()),
        dm_threads: Mutex::new(Vec::new()),
        dm_messages: Mutex::new(BTreeMap::new()),
        dm_commands: Mutex::new(Vec::new()),
        payment_commands: Mutex::new(Vec::new()),
    });
    let (_dir, state, _token, _) =
        build_test_state_with_bridge(20, dir, identity.clone(), event_log, bridge);

    let event = StreamEvent {
        kind: "mission.published".to_string(),
        timestamp: Utc::now().timestamp(),
        payload: json!({
            "mission_id": "mission-1",
            "publisher": "org-1",
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
    push_signed_node_event(&client, &format!("http://{addr}"), &signed)
        .await
        .unwrap();

    let received = received.lock().await;
    assert_eq!(received.len(), 1);
    assert_eq!(
        received[0]["payload"]["event_kind"].as_str(),
        Some("mission.published")
    );
    assert_eq!(
        received[0]["payload"]["data_kind"].as_str(),
        Some("mission_lifecycle")
    );

    server.abort();
}
