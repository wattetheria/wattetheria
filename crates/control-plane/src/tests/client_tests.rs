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

fn network_claim_test_metadata(title: &str, mission_id: &str) -> NetworkMissionClaimMetadata {
    NetworkMissionClaimMetadata {
        title: Some(title.to_string()),
        publisher_id: Some("publisher-public".to_string()),
        publisher_agent_did: Some("did:agent:publisher".to_string()),
        publisher_display_name: Some("Remote Publisher".to_string()),
        publisher_wattswarm_node_id: Some("publisher-node".to_string()),
        domain: Some("trade".to_string()),
        scope: Some("real_world".to_string()),
        task_status: None,
        mission_feed_key: Some("wattetheria.missions".to_string()),
        mission_scope_hint: Some(format!("group:{mission_id}")),
        reward: Some(json!({"agent_watt": 0})),
        reward_watt: Some(10),
        executor_bounty_watt: Some(10),
        publisher_network_reward_watt: Some(1),
    }
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

    let tasks_json = authed_get_json(app.clone(), &token, "/v1/client/tasks").await;
    assert_eq!(tasks_json.as_array().unwrap().len(), 1);
    assert_eq!(tasks_json[0]["title"].as_str(), Some("Calibrate relay"));
    assert_eq!(tasks_json[0]["status"].as_str(), Some("published"));
    // Fixtures publish without a gateway ledger, so they carry no Watt bounty.
    assert_eq!(tasks_json[0]["reward_watt"].as_i64(), Some(0));
    assert_eq!(tasks_json[0]["task_bounty_watt"].as_i64(), Some(0));
    assert_eq!(tasks_json[0]["executor_bounty_watt"].as_i64(), Some(0));
    assert!(tasks_json[0].get("network_publish_reward_watt").is_none());
    assert!(
        tasks_json[0]
            .get("network_settle_publisher_reward_watt")
            .is_none()
    );
    assert!(tasks_json[0].get("publisher_network_reward_watt").is_none());

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
    assert_eq!(leaderboard_json[0]["compute_score"].as_i64(), Some(1));
    assert_eq!(leaderboard_json[0]["prestige"].as_i64(), Some(0));
    assert_eq!(leaderboard_json[0]["score"].as_f64(), Some(10.0));
    assert_eq!(leaderboard_json[0]["score_tenths"].as_i64(), Some(100));

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
    let (_dir, app, token, _, _state) = build_test_app(20);
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

    let tasks_json = authed_get_json(app.clone(), &token, "/v1/client/tasks").await;
    assert_eq!(tasks_json.as_array().unwrap().len(), 1);
    assert_eq!(tasks_json[0]["reward_watt"].as_i64(), Some(0));
    assert_eq!(tasks_json[0]["executor_bounty_watt"].as_i64(), Some(0));
    assert!(tasks_json[0].get("publisher_network_reward_watt").is_none());
    assert!(
        tasks_json[0]["created_by_agent_identity"]
            .as_str()
            .is_some()
    );
    assert!(tasks_json[0]["claimer_agent_identity"].as_str().is_some());
    assert!(tasks_json[0]["claimer_public_id"].as_str().is_some());

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
async fn system_puzzle_settle_rejects_self_verified_receipt() {
    use wattetheria_kernel::civilization::missions::MissionStatus;
    use wattetheria_kernel::tasks::system_puzzle::{
        PROOF_SCHEME_ZK_HASHCASH_V1, SYSTEM_PUZZLE_CHALLENGE_TASK_KIND,
        SYSTEM_PUZZLE_VERIFICATION_TASK_KIND, SystemPuzzleChallenge, SystemPuzzleProofEnvelope,
        SystemPuzzleRewardPolicy, SystemPuzzleVerificationMissionPayload,
        SystemPuzzleVerificationReceipt, VERIFICATION_VERDICT_VALID, proof_hash,
    };

    let (_dir, app, token, _, state) = build_test_app(20);
    let identity = authed_get_json(app.clone(), &token, "/v1/client/self").await;
    let agent_did = identity["wallet_bound_agent_did"]
        .as_str()
        .unwrap()
        .to_string();
    let solver_public_id = bootstrap_broker_identity(app.clone(), &token, &agent_did).await;
    let challenge = SystemPuzzleChallenge {
        task_kind: SYSTEM_PUZZLE_CHALLENGE_TASK_KIND.to_string(),
        challenge_id: "challenge-2026-06-01T00-00Z".to_string(),
        slot_id: "slot-2026-06-01T00-00Z".to_string(),
        template_id: "zk-hashcash-demo".to_string(),
        challenge_seed: "seed-2026-06-01T00-00Z".to_string(),
        difficulty_bits: 0,
        proof_scheme: PROOF_SCHEME_ZK_HASHCASH_V1.to_string(),
        proposer_public_id: "system-proposer".to_string(),
        proposer_agent_identity: Some("System-Proposer".to_string()),
        issued_at: 1,
        reward_policy: SystemPuzzleRewardPolicy::default(),
    };
    let nonce = "nonce-1";
    let digest = hex::encode(sha2::Sha256::digest(
        format!(
            "{}:{}:{}",
            challenge.challenge_seed, solver_public_id, nonce
        )
        .as_bytes(),
    ));
    let proof = SystemPuzzleProofEnvelope {
        proof_scheme: PROOF_SCHEME_ZK_HASHCASH_V1.to_string(),
        public_inputs: json!({
            "challenge_id": challenge.challenge_id,
            "challenge_seed": challenge.challenge_seed,
            "difficulty_bits": challenge.difficulty_bits,
            "solver_public_id": solver_public_id,
        }),
        public_output: json!({"digest": digest}),
        proof: json!({"nonce": nonce, "digest": digest}),
    };
    let payload = SystemPuzzleVerificationMissionPayload {
        task_kind: SYSTEM_PUZZLE_VERIFICATION_TASK_KIND.to_string(),
        challenge: challenge.clone(),
        solver_public_id: solver_public_id.clone(),
        solver_agent_identity: Some("Agent-Solver".to_string()),
        solution_id: "solution-1".to_string(),
        proof: proof.clone(),
    };
    let self_verified_receipt = SystemPuzzleVerificationReceipt {
        task_kind: SYSTEM_PUZZLE_VERIFICATION_TASK_KIND.to_string(),
        challenge_id: challenge.challenge_id.clone(),
        solution_id: "solution-1".to_string(),
        solver_public_id: solver_public_id.clone(),
        verifier_public_id: solver_public_id.clone(),
        verifier_agent_identity: Some("Agent-Solver".to_string()),
        verdict: VERIFICATION_VERDICT_VALID.to_string(),
        proof_hash: proof_hash(&proof).unwrap(),
        verified_at: 2,
        verifier_notes: None,
    };
    let mission = authed_post_json(
        app.clone(),
        &token,
        "/v1/wattetheria/missions",
        json!({
            "title": "Verify system puzzle solution",
            "description": "Verify a submitted system puzzle proof.",
            "publisher": solver_public_id,
            "publisher_kind": "system",
            "domain": "power",
            "subnet_id": null,
            "zone_id": null,
            "required_role": null,
            "required_faction": null,
            "reward": {
                "agent_watt": 0,
                "reputation": 0,
                "capacity": 0
            },
            "payload": payload
        }),
    )
    .await;
    let mission_id = mission["mission_id"].as_str().unwrap();
    let _claimed = authed_post_json(
        app.clone(),
        &token,
        &format!("/v1/wattetheria/missions/{mission_id}/claim"),
        json!({"mission_id": mission_id, "agent_did": solver_public_id}),
    )
    .await;
    let _completed = authed_post_json(
        app.clone(),
        &token,
        &format!("/v1/wattetheria/missions/{mission_id}/complete"),
        json!({
            "mission_id": mission_id,
            "agent_did": solver_public_id,
            "result": self_verified_receipt
        }),
    )
    .await;

    let status = authed_post(
        app,
        &token,
        &format!("/v1/wattetheria/missions/{mission_id}/settle"),
        json!({"mission_id": mission_id}),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    let board = state.mission_board.lock().await;
    assert_eq!(
        board.get(mission_id).unwrap().status,
        MissionStatus::Completed
    );
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

    let board = state.mission_board.lock().await;
    let settled = board.get(mission_id).unwrap();
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

    let board = state.mission_board.lock().await;
    let stuck = board.get(mission_id).unwrap();
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
            &format!("/v1/wattetheria/client/export?public_id={local_public_id}&peer_limit=10&task_limit=10&organization_limit=10&rpc_log_limit=5&leaderboard_limit=5"),
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
    assert_eq!(
        export_json["payload"]["public_topics"][0]["source_agent_identity"].as_str(),
        Some("Captain Aurora")
    );
    assert_eq!(
        export_json["payload"]["swarm_task_activity"]["tasks"][0]["source_agent_identity"].as_str(),
        Some("Captain Aurora")
    );
    assert_eq!(
        export_json["payload"]["swarm_task_activity"]["runs"][0]["source_agent_identity"].as_str(),
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
    });
    let (_dir, app, token, _, _state) =
        build_test_app_with_bridge(20, dir, identity.clone(), event_log, bridge);
    let captain = bootstrap_broker_identity(app.clone(), &token, &identity.agent_did).await;

    let export_json = public_get_json(
            app,
            &format!("/v1/wattetheria/client/export?public_id={captain}&node_limit=1&task_limit=10&organization_limit=10&rpc_log_limit=5&leaderboard_limit=5"),
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
async fn client_export_includes_task_contract_for_network_mission_claims() {
    let (_dir, app, token, _, state) = build_test_app(20);
    let created = authed_post_json(
        app.clone(),
        &token,
        "/v1/wattetheria/missions",
        json!({
            "title": "Network cargo",
            "description": "Move cargo for a remote claimer",
            "publisher": "publisher-public",
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

    let export_json = public_get_json(app, "/v1/wattetheria/client/export?task_limit=10").await;
    let task = export_json["payload"]["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|task| task["id"].as_str() == Some(mission_id))
        .unwrap();

    assert_eq!(task["task_id"].as_str(), Some(mission_id));
    assert_eq!(task["task_type"].as_str(), Some("wattetheria.mission"));
    assert_eq!(
        task["publisher_wattswarm_node_id"].as_str(),
        Some(state.agent_did.as_str())
    );
    assert_eq!(
        task["mission_scope_hint"].as_str(),
        Some(format!("group:{mission_id}").as_str())
    );
    assert_eq!(task["task_contract"]["task_id"].as_str(), Some(mission_id));
    assert_eq!(
        task["task_contract"]["inputs"]["mission_id"].as_str(),
        Some(mission_id)
    );
    assert!(task.get("expiry_ms").is_none());
    assert!(task.get("expires_at").is_none());
    assert!(task.get("expired").is_none());
}

#[tokio::test]
async fn client_export_separates_published_and_claimed_mission_tasks() {
    let (_dir, app, token, _, state) = build_test_app(20);
    let created = authed_post_json(
        app.clone(),
        &token,
        "/v1/wattetheria/missions",
        json!({
            "title": "Published cargo",
            "description": "Local publisher mission",
            "publisher": "publisher-public",
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
    let published_id = created["mission_id"].as_str().unwrap();
    let mut claims = NetworkMissionClaimRegistry::default();
    claims.record(
        "remote-mission-1",
        "remote-task-1",
        &state.agent_did,
        "mission-remote-mission-1-agent",
        Some("network_claim_submitted".to_string()),
        network_claim_test_metadata("Remote cargo", "remote-mission-1"),
    );
    claims.record(
        "remote-mission-2",
        "remote-task-2",
        &state.agent_did,
        "mission-remote-mission-2-agent",
        Some("claimed".to_string()),
        network_claim_test_metadata("Approved remote cargo", "remote-mission-2"),
    );
    state
        .local_db
        .save_domain(
            wattetheria_kernel::local_db::domain::NETWORK_MISSION_CLAIMS,
            &claims,
        )
        .unwrap();

    let export_json = public_get_json(app, "/v1/wattetheria/client/export?task_limit=10").await;
    let tasks = export_json["payload"]["tasks"].as_array().unwrap();
    let published = tasks
        .iter()
        .find(|task| task["id"].as_str() == Some(published_id))
        .unwrap();
    let claimed = tasks
        .iter()
        .find(|task| task["id"].as_str() == Some("remote-mission-1"))
        .unwrap();
    let approved_claim = tasks
        .iter()
        .find(|task| task["id"].as_str() == Some("remote-mission-2"))
        .unwrap();

    assert_eq!(published["task_origin"].as_str(), Some("published"));
    assert_eq!(published["status"].as_str(), Some("published"));
    assert_eq!(published.get("mission_tab"), None);
    assert_eq!(claimed["task_origin"].as_str(), Some("claimed"));
    assert_eq!(claimed["mission_tab"].as_str(), Some("claim_submitted"));
    assert_eq!(claimed["status"].as_str(), Some("network_claim_submitted"));
    assert_eq!(claimed["node_claim_status"].as_str(), Some("claimed"));
    assert_eq!(claimed["task_id"].as_str(), Some("remote-task-1"));
    assert_eq!(claimed["title"].as_str(), Some("Remote cargo"));
    assert_eq!(claimed["domain"].as_str(), Some("trade"));
    assert_eq!(claimed["publisher_id"].as_str(), Some("publisher-public"));
    assert_eq!(
        claimed["created_by_agent_identity"].as_str(),
        Some("Remote Publisher")
    );
    assert_eq!(claimed["reward_watt"].as_i64(), Some(10));
    assert_eq!(claimed["publisher_network_reward_watt"].as_i64(), Some(1));
    assert_eq!(
        claimed["claimer_id"].as_str(),
        Some(state.agent_did.as_str())
    );
    assert!(claimed["claimed_at"].as_str().is_some());
    assert_eq!(approved_claim["task_origin"].as_str(), Some("claimed"));
    assert_eq!(approved_claim["mission_tab"].as_str(), Some("claimed"));
    assert_eq!(approved_claim["status"].as_str(), Some("claimed"));
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

type MockLedgerCalls = Arc<Mutex<Vec<(String, Value)>>>;

#[derive(Clone)]
struct MockLedger {
    calls: MockLedgerCalls,
    balance: i64,
    /// Commit the lock but answer with a server error, like a gateway whose reply was lost.
    lose_lock_response: bool,
    /// Answer every lock with a server error without committing anything.
    lock_hard_fail: bool,
    /// While set, refunds answer with a server error.
    refund_unavailable: Arc<std::sync::atomic::AtomicBool>,
}

impl MockLedger {
    fn with_balance(calls: MockLedgerCalls, balance: i64) -> Self {
        Self {
            calls,
            balance,
            lose_lock_response: false,
            lock_hard_fail: false,
            refund_unavailable: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }
}
type MockLedgerState = axum::extract::State<MockLedger>;

/// A stand-in for the gateway ledger: records every signed request, refuses locks above the
/// configured balance and remembers which escrows exist, like the real `/api/ledger/*` routes.
#[allow(clippy::too_many_lines)]
fn mock_ledger_router(ledger: MockLedger) -> Router {
    fn escrow_status(calls: &[(String, Value)], escrow_id: &str) -> Option<(String, Value)> {
        let mut found = None;
        for (kind, body) in calls {
            if body["payload"]["escrow_id"].as_str() != Some(escrow_id) {
                continue;
            }
            match kind.as_str() {
                "lock" => found = Some(("locked".to_string(), body["payload"].clone())),
                "release" => {
                    if let Some((status, _)) = found.as_mut() {
                        *status = "released".to_string();
                    }
                }
                "refund" => {
                    if let Some((status, _)) = found.as_mut() {
                        *status = "refunded".to_string();
                    }
                }
                _ => {}
            }
        }
        found
    }
    async fn lock(
        axum::extract::State(ledger): MockLedgerState,
        Json(body): Json<Value>,
    ) -> axum::response::Response {
        let amount = body["payload"]["amount"].as_i64().unwrap_or(0);
        if amount > ledger.balance {
            ledger
                .calls
                .lock()
                .await
                .push(("lock_refused".to_string(), body.clone()));
            return (
                StatusCode::PAYMENT_REQUIRED,
                Json(json!({"error": format!("insufficient balance: needs {amount}")})),
            )
                .into_response();
        }
        if ledger.lock_hard_fail {
            ledger
                .calls
                .lock()
                .await
                .push(("lock_failed".to_string(), body.clone()));
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "database unavailable"})),
            )
                .into_response();
        }
        ledger
            .calls
            .lock()
            .await
            .push(("lock".to_string(), body.clone()));
        if ledger.lose_lock_response {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "connection reset"})),
            )
                .into_response();
        }
        Json(json!({"escrow_id": body["payload"]["escrow_id"], "status": "locked"})).into_response()
    }
    async fn get_escrow(
        axum::extract::State(ledger): MockLedgerState,
        Path(escrow_id): Path<String>,
    ) -> axum::response::Response {
        let calls = ledger.calls.lock().await;
        match escrow_status(&calls, &escrow_id) {
            Some((status, lock_payload)) => Json(json!({
                "escrow_id": escrow_id,
                "payer_account": lock_payload["payer"],
                "amount": lock_payload["amount"],
                "status": status,
            }))
            .into_response(),
            None => (
                StatusCode::NOT_FOUND,
                Json(json!({"error": format!("escrow {escrow_id} not found")})),
            )
                .into_response(),
        }
    }
    async fn release(
        axum::extract::State(ledger): MockLedgerState,
        Path(escrow_id): Path<String>,
        Json(body): Json<Value>,
    ) -> axum::response::Response {
        ledger
            .calls
            .lock()
            .await
            .push(("release".to_string(), body));
        Json(json!({"escrow_id": escrow_id, "status": "released"})).into_response()
    }
    async fn refund(
        axum::extract::State(ledger): MockLedgerState,
        Path(escrow_id): Path<String>,
        Json(body): Json<Value>,
    ) -> axum::response::Response {
        let mut calls = ledger.calls.lock().await;
        if escrow_status(&calls, &escrow_id).is_none() {
            return (
                StatusCode::NOT_FOUND,
                Json(json!({"error": format!("escrow {escrow_id} not found")})),
            )
                .into_response();
        }
        if ledger
            .refund_unavailable
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            calls.push(("refund_failed".to_string(), body));
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({"error": "database unavailable"})),
            )
                .into_response();
        }
        calls.push(("refund".to_string(), body));
        Json(json!({"escrow_id": escrow_id, "status": "refunded"})).into_response()
    }
    fn account_escrows(calls: &[(String, Value)], account_id: &str) -> Vec<Value> {
        let mut escrows: Vec<Value> = Vec::new();
        for (kind, body) in calls {
            let payload = &body["payload"];
            let Some(escrow_id) = payload["escrow_id"].as_str() else {
                continue;
            };
            match kind.as_str() {
                "lock" if payload["payer"].as_str() == Some(account_id) => escrows.push(json!({
                    "escrow_id": escrow_id,
                    "payer_account": payload["payer"],
                    "payee_account": Value::Null,
                    "amount": payload["amount"],
                    "reference": payload["reference"],
                    "payer_label": payload["payer_label"],
                    "payee_label": Value::Null,
                    "subject": payload["subject"],
                    "status": "locked",
                })),
                "release" | "refund" => {
                    if let Some(escrow) = escrows
                        .iter_mut()
                        .find(|escrow| escrow["escrow_id"].as_str() == Some(escrow_id))
                    {
                        escrow["status"] = json!(if kind == "release" {
                            "released"
                        } else {
                            "refunded"
                        });
                        escrow["payee_account"] = payload["payee"].clone();
                        escrow["payee_label"] = payload["payee_label"].clone();
                    }
                }
                _ => {}
            }
        }
        escrows
    }
    async fn account(
        axum::extract::State(ledger): MockLedgerState,
        Path(account_id): Path<String>,
    ) -> Json<Value> {
        let calls = ledger.calls.lock().await;
        let locked: i64 = account_escrows(&calls, &account_id)
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
        let mut calls = ledger.calls.lock().await;
        let status = query.get("status").cloned();
        let escrows: Vec<Value> = account_escrows(&calls, &account_id)
            .into_iter()
            .filter(|escrow| {
                status
                    .as_deref()
                    .is_none_or(|status| escrow["status"].as_str() == Some(status))
            })
            .collect();
        calls.push((
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
        .route("/api/ledger/escrows", post(lock))
        .route("/api/ledger/escrows/{escrow_id}", get(get_escrow))
        .route("/api/ledger/escrows/{escrow_id}/release", post(release))
        .route("/api/ledger/escrows/{escrow_id}/refund", post(refund))
        .route("/api/ledger/accounts/{account_id}", get(account))
        .route("/api/ledger/accounts/{account_id}/escrows", get(escrows))
        .route("/api/ledger/accounts/{account_id}/entries", get(entries))
        .with_state(ledger)
}

#[tokio::test]
async fn mcp_publish_mission_with_a_watt_bounty_locks_it_in_the_ledger() {
    let calls: MockLedgerCalls = Arc::new(Mutex::new(Vec::new()));
    let (app, state, token, _public_id, _dir) =
        ledger_backed_node(MockLedger::with_balance(calls.clone(), 100)).await;
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
    assert_eq!(mission["escrow_id"], json!(mission_id));
    {
        let recorded = calls.lock().await;
        let (kind, body) = recorded
            .iter()
            .find(|(kind, _)| kind == "lock")
            .expect("the MCP publish locks the bounty");
        assert_eq!(kind, "lock");
        assert_eq!(body["payload"]["escrow_id"], json!(mission_id));
        assert_eq!(body["payload"]["payer"], json!(state.agent_did));
        assert_eq!(body["payload"]["amount"], json!(70));
        assert_eq!(body["payload"]["payer_label"], json!(own_name));
        assert_eq!(
            body["payload"]["subject"],
            json!("Mission bounty: Chart the trade lane")
        );
    }
    let wallet = mcp_tool_call(app.clone(), &token, "get_watt_balance", json!({})).await;
    assert_eq!(wallet["locked_in_escrow"], json!(70), "{wallet}");
    assert_eq!(wallet["locked_escrows"][0]["escrow_id"], json!(mission_id));

    // Beyond the balance: refused as insufficient_watt and nothing stays open.
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
    let board = state.mission_board.lock().await;
    assert!(
        board
            .list(Some(
                &wattetheria_kernel::civilization::missions::MissionStatus::Open
            ))
            .into_iter()
            .all(|mission| mission.title != "Too rich")
    );
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
    let (app, state, token, public_id, _dir) =
        ledger_backed_node(MockLedger::with_balance(calls.clone(), 300)).await;
    let mission = authed_post_json(
        app.clone(),
        &token,
        "/v1/wattetheria/missions",
        escrowed_mission_body(&public_id, 120),
    )
    .await;
    let mission_id = mission["mission_id"].as_str().unwrap().to_string();

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
    assert_eq!(locked["payer_label"], json!(own_name));
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

/// Boots a node whose only gateway is the given mock ledger. Returns the app, its state, the
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
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ledger_addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, mock_ledger_router(ledger))
            .await
            .unwrap();
    });
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge: Arc<dyn SwarmBridge> =
        Arc::new(MockSwarmBridge::default_for(identity.agent_did.clone()));
    let (dir, mut state, token, _) =
        build_test_state_with_bridge(20, dir, identity, event_log, bridge);
    state.gateway_urls = vec![format!("http://{ledger_addr}/api/ingest/snapshot")];
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

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn mission_bounty_is_escrowed_and_released_through_the_gateway_ledger() {
    let calls: MockLedgerCalls = Arc::new(Mutex::new(Vec::new()));
    let (app, state, token, public_id, _dir) =
        ledger_backed_node(MockLedger::with_balance(calls.clone(), 150)).await;
    let payer_did = state.identity.agent_did.clone();
    let executor_did = "did:key:z6MkExecutorForEscrowTest".to_string();

    let publish = |bounty: i64| escrowed_mission_body(&public_id, bounty);
    let mission = authed_post_json(
        app.clone(),
        &token,
        "/v1/wattetheria/missions",
        publish(100),
    )
    .await;
    let mission_id = mission["mission_id"].as_str().unwrap().to_string();
    assert_eq!(
        mission["escrow_id"],
        json!(mission_id),
        "published mission records its ledger escrow: {mission}"
    );
    {
        let recorded = calls.lock().await;
        assert_eq!(recorded.len(), 1, "{recorded:?}");
        let (kind, body) = &recorded[0];
        assert_eq!(kind, "lock");
        assert_eq!(body["payload"]["escrow_id"], json!(mission_id));
        assert_eq!(body["payload"]["payer"], json!(payer_did));
        assert_eq!(body["payload"]["amount"], json!(100));
        assert_eq!(
            body["payload"]["payer_label"],
            json!(local_agent_display_name(app.clone(), &token).await),
            "the publisher's current display name travels with the lock"
        );
        assert_eq!(
            body["payload"]["subject"],
            json!("Mission bounty: Escrowed task")
        );
        assert!(
            verify_payload(
                &body["payload"],
                body["signature"].as_str().unwrap(),
                body["public_key"].as_str().unwrap(),
            )
            .unwrap(),
            "ledger request must be signed by the node identity"
        );
    }

    let response = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/v1/wattetheria/missions")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(axum::body::Body::from(publish(500).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYMENT_REQUIRED);
    let body: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(body["code"], json!("insufficient_watt"));
    {
        let board = state.mission_board.lock().await;
        let open: Vec<_> = board
            .list(Some(
                &wattetheria_kernel::civilization::missions::MissionStatus::Open,
            ))
            .into_iter()
            .map(|mission| mission.mission_id)
            .collect();
        assert_eq!(
            open,
            vec![mission_id.clone()],
            "unfunded mission must not stay open"
        );
        assert_eq!(
            board
                .list(Some(
                    &wattetheria_kernel::civilization::missions::MissionStatus::Cancelled
                ))
                .len(),
            1
        );
    }

    let claimed = authed_post_json(
        app.clone(),
        &token,
        &format!("/v1/wattetheria/missions/{mission_id}/claim"),
        json!({
            "mission_id": mission_id,
            "agent_did": executor_did,
            "claim_route": {"decision_payload": {"display_name": "Remote Rita"}}
        }),
    )
    .await;
    assert_eq!(
        claimed["claimed_by_display_name"],
        json!("Remote Rita"),
        "the claimer's name from the claim is kept on the mission: {claimed}"
    );
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
        let recorded = calls.lock().await;
        let (kind, body) = recorded.last().unwrap();
        assert_eq!(kind, "release");
        assert_eq!(body["payload"]["escrow_id"], json!(mission_id));
        assert_eq!(body["payload"]["payer"], json!(payer_did));
        assert_eq!(body["payload"]["payee"], json!(executor_did));
        assert_eq!(
            body["payload"]["payee_label"],
            json!("Remote Rita"),
            "the executor's name travels with the payout"
        );
        assert_eq!(
            recorded.iter().filter(|(kind, _)| kind == "lock").count(),
            1,
            "one funded lock"
        );
        assert_eq!(
            recorded
                .iter()
                .filter(|(kind, _)| kind == "lock_refused")
                .count(),
            1,
            "one refused lock"
        );
    }

    let self_json = authed_get_json(app, &token, "/v1/client/self").await;
    assert_eq!(self_json["watt_balance"].as_i64(), Some(150));
    assert_eq!(self_json["watt_balance_available"].as_bool(), Some(true));
}

#[tokio::test]
async fn cancelling_an_escrowed_mission_refunds_the_bounty() {
    let calls: MockLedgerCalls = Arc::new(Mutex::new(Vec::new()));
    let (app, state, token, public_id, _dir) =
        ledger_backed_node(MockLedger::with_balance(calls.clone(), 150)).await;
    let payer_did = state.identity.agent_did.clone();
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
        let recorded = calls.lock().await;
        let kinds: Vec<&str> = recorded.iter().map(|(kind, _)| kind.as_str()).collect();
        assert_eq!(kinds, vec!["lock", "refund"], "{recorded:?}");
        let (_, refund) = recorded.last().unwrap();
        assert_eq!(refund["payload"]["escrow_id"], json!(mission_id));
        assert_eq!(refund["payload"]["payer"], json!(payer_did));
    }
    // Cancelling again is idempotent and does not touch the ledger.
    let again = authed_post_json(
        app.clone(),
        &token,
        &format!("/v1/wattetheria/missions/{mission_id}/cancel"),
        json!({}),
    )
    .await;
    assert_eq!(again["status"], json!("cancelled"));
    assert_eq!(calls.lock().await.len(), 2);

    // A settled mission cannot be cancelled.
    let executor_did = "did:key:z6MkExecutorForCancelTest".to_string();
    let other = authed_post_json(
        app.clone(),
        &token,
        "/v1/wattetheria/missions",
        escrowed_mission_body(&public_id, 10),
    )
    .await;
    let other_id = other["mission_id"].as_str().unwrap().to_string();
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
    let response = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri(format!("/v1/wattetheria/missions/{other_id}/cancel"))
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(axum::body::Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::CONFLICT);
}

#[tokio::test]
async fn lost_escrow_lock_response_is_reconciled_against_the_ledger() {
    let calls: MockLedgerCalls = Arc::new(Mutex::new(Vec::new()));
    let (app, state, token, public_id, _dir) = ledger_backed_node(MockLedger {
        lose_lock_response: true,
        ..MockLedger::with_balance(calls.clone(), 150)
    })
    .await;
    let mission = authed_post_json(
        app.clone(),
        &token,
        "/v1/wattetheria/missions",
        escrowed_mission_body(&public_id, 60),
    )
    .await;
    let mission_id = mission["mission_id"].as_str().unwrap().to_string();
    // The ledger committed the lock even though its reply was an error: the node must find the
    // escrow and keep the mission funded instead of cancelling it and stranding the Watt.
    assert_eq!(mission["status"], json!("open"), "{mission}");
    assert_eq!(mission["escrow_id"], json!(mission_id), "{mission}");
    {
        let board = state.mission_board.lock().await;
        let stored = board.get(&mission_id).unwrap();
        assert_eq!(stored.escrow_id.as_deref(), Some(mission_id.as_str()));
    }
    let kinds: Vec<String> = calls
        .lock()
        .await
        .iter()
        .map(|(kind, _)| kind.clone())
        .collect();
    assert_eq!(
        kinds,
        vec!["lock".to_string()],
        "no refund for a lock that stands"
    );
}

#[tokio::test]
async fn publishing_a_bounty_without_a_gateway_is_rejected() {
    let (_dir, app, token, _, state) = build_test_app(20);
    assert_eq!(state.gateway_urls, Vec::<String>::new());
    let public_id = bootstrap_broker_identity(app.clone(), &token, &state.agent_did).await;
    let response = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/v1/wattetheria/missions")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(axum::body::Body::from(
                    escrowed_mission_body(&public_id, 25).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body: Value = serde_json::from_slice(
        &axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(body["code"], json!("ledger_unavailable"));
    {
        let board = state.mission_board.lock().await;
        assert!(
            board
                .list(Some(
                    &wattetheria_kernel::civilization::missions::MissionStatus::Open
                ))
                .is_empty(),
            "an unfunded bounty mission must not stay open"
        );
    }
    // Missions without a bounty still publish without a ledger.
    let free = authed_post_json(
        app,
        &token,
        "/v1/wattetheria/missions",
        escrowed_mission_body(&public_id, 0),
    )
    .await;
    assert_eq!(free["status"], json!("open"), "{free}");
    assert!(free.get("escrow_id").is_none());
}

#[tokio::test]
async fn failed_refund_keeps_the_escrow_pending_until_a_retry_succeeds() {
    let calls: MockLedgerCalls = Arc::new(Mutex::new(Vec::new()));
    let refund_unavailable = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let (app, state, token, public_id, _dir) = ledger_backed_node(MockLedger {
        refund_unavailable: refund_unavailable.clone(),
        ..MockLedger::with_balance(calls.clone(), 150)
    })
    .await;
    let mission = authed_post_json(
        app.clone(),
        &token,
        "/v1/wattetheria/missions",
        escrowed_mission_body(&public_id, 30),
    )
    .await;
    let mission_id = mission["mission_id"].as_str().unwrap().to_string();

    let mut events = state.stream_tx.subscribe();
    let response = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri(format!("/v1/wattetheria/missions/{mission_id}/cancel"))
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(axum::body::Body::from("{}"))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    // The cancellation is announced even though the refund failed, so the gateway learns of
    // it now rather than only once a retry succeeds.
    let mut announced = Vec::new();
    while let Ok(event) = events.try_recv() {
        if event.kind == "mission.cancelled" {
            announced.push(event.payload);
        }
    }
    assert_eq!(announced.len(), 1, "{announced:?}");
    assert_eq!(announced[0]["mission_id"], json!(mission_id));
    {
        // Cancelled on the board first, so nobody can complete it, with the refund pending.
        let board = state.mission_board.lock().await;
        let stored = board.get(&mission_id).unwrap();
        assert_eq!(
            stored.status,
            wattetheria_kernel::civilization::missions::MissionStatus::Cancelled
        );
        assert_eq!(stored.escrow_id.as_deref(), Some(mission_id.as_str()));
    }
    // A complete racing in after the cancel is refused.
    let complete = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri(format!("/v1/wattetheria/missions/{mission_id}/complete"))
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(axum::body::Body::from(
                    json!({"mission_id": mission_id, "agent_did": "did:key:z6MkLateWorker"})
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        !complete.status().is_success(),
        "complete after cancel must fail: {}",
        complete.status()
    );

    // The sweep retries while the ledger is down and keeps the refund pending.
    let report = crate::reconcile_mission_escrows(&state, 0).await;
    assert_eq!(report.refunds_pending, 1);
    assert_eq!(report.refunds_settled, 0);
    refund_unavailable.store(false, std::sync::atomic::Ordering::SeqCst);
    let report = crate::reconcile_mission_escrows(&state, 0).await;
    assert_eq!(report.refunds_settled, 1);
    assert_eq!(report.refunds_pending, 0);
    {
        let board = state.mission_board.lock().await;
        assert!(board.get(&mission_id).unwrap().escrow_id.is_none());
    }
    let kinds: Vec<String> = calls
        .lock()
        .await
        .iter()
        .map(|(kind, _)| kind.clone())
        .collect();
    assert_eq!(
        kinds,
        vec!["lock", "refund_failed", "refund_failed", "refund"]
    );
}

#[tokio::test]
async fn unconfirmed_lock_is_tracked_as_a_pending_refund_until_stale() {
    let calls: MockLedgerCalls = Arc::new(Mutex::new(Vec::new()));
    let (app, state, token, public_id, _dir) = ledger_backed_node(MockLedger {
        lock_hard_fail: true,
        ..MockLedger::with_balance(calls.clone(), 150)
    })
    .await;
    let response = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/v1/wattetheria/missions")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(axum::body::Body::from(
                    escrowed_mission_body(&public_id, 70).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let (mission_id, stored) = {
        let board = state.mission_board.lock().await;
        let cancelled = board.list(Some(
            &wattetheria_kernel::civilization::missions::MissionStatus::Cancelled,
        ));
        assert_eq!(cancelled.len(), 1);
        (cancelled[0].mission_id.clone(), cancelled[0].clone())
    };
    // A 5xx is not proof that nothing was locked: the escrow stays recorded as pending.
    assert_eq!(stored.escrow_id.as_deref(), Some(mission_id.as_str()));
    // Within the grace period the ledger's "not found" is not trusted yet.
    let report = crate::reconcile_mission_escrows(&state, 3600).await;
    assert_eq!(report.refunds_pending, 1);
    // Once stale, a lock that never landed is forgotten.
    let report = crate::reconcile_mission_escrows(&state, 0).await;
    assert_eq!(report.refunds_settled, 1);
    let board = state.mission_board.lock().await;
    assert!(board.get(&mission_id).unwrap().escrow_id.is_none());
}

#[tokio::test]
async fn unconfirmed_escrow_the_ledger_holds_is_confirmed_by_the_sweep() {
    let calls: MockLedgerCalls = Arc::new(Mutex::new(Vec::new()));
    let (app, state, token, public_id, _dir) =
        ledger_backed_node(MockLedger::with_balance(calls.clone(), 150)).await;
    let mission = authed_post_json(
        app.clone(),
        &token,
        "/v1/wattetheria/missions",
        escrowed_mission_body(&public_id, 30),
    )
    .await;
    let mission_id = mission["mission_id"].as_str().unwrap().to_string();
    {
        let board = state.mission_board.lock().await;
        assert!(board.get(&mission_id).unwrap().escrow_confirmed);
    }
    // Simulate the node dying between the lock request and persisting its outcome: the escrow
    // id is on the mission (recorded before the request) but unconfirmed.
    {
        let mut board = state.mission_board.lock().await;
        board
            .record_bounty_escrow_intent(&mission_id, &mission_id)
            .unwrap();
    }
    let report = crate::reconcile_mission_escrows(&state, 0).await;
    assert_eq!(report.escrows_confirmed, 1);
    assert_eq!(report.escrows_abandoned, 0);
    {
        let board = state.mission_board.lock().await;
        let stored = board.get(&mission_id).unwrap();
        assert_eq!(
            stored.status,
            wattetheria_kernel::civilization::missions::MissionStatus::Open
        );
        assert!(stored.escrow_confirmed);
        assert_eq!(stored.escrow_id.as_deref(), Some(mission_id.as_str()));
    }
    let kinds: Vec<String> = calls
        .lock()
        .await
        .iter()
        .map(|(kind, _)| kind.clone())
        .collect();
    assert_eq!(kinds, vec!["lock"], "no refund or second lock");
    // The confirmed escrow then pays out on settlement as usual.
    let worker = "did:key:z6MkSweepWorker";
    authed_post_json(
        app.clone(),
        &token,
        &format!("/v1/wattetheria/missions/{mission_id}/claim"),
        json!({"mission_id": mission_id, "agent_did": worker}),
    )
    .await;
    authed_post_json(
        app.clone(),
        &token,
        &format!("/v1/wattetheria/missions/{mission_id}/complete"),
        json!({"mission_id": mission_id, "agent_did": worker}),
    )
    .await;
    authed_post_json(
        app.clone(),
        &token,
        &format!("/v1/wattetheria/missions/{mission_id}/settle"),
        json!({"mission_id": mission_id, "agent_did": worker}),
    )
    .await;
    let kinds: Vec<String> = calls
        .lock()
        .await
        .iter()
        .map(|(kind, _)| kind.clone())
        .collect();
    assert_eq!(kinds, vec!["lock", "release"]);
}

#[tokio::test]
async fn unconfirmed_escrow_the_ledger_never_saw_cancels_the_mission_once_stale() {
    let calls: MockLedgerCalls = Arc::new(Mutex::new(Vec::new()));
    let (app, state, token, public_id, _dir) =
        ledger_backed_node(MockLedger::with_balance(calls.clone(), 150)).await;
    // Publish without a bounty, then pretend the node recorded an escrow intent and died before
    // the lock request ever reached the ledger.
    let mission = authed_post_json(
        app.clone(),
        &token,
        "/v1/wattetheria/missions",
        escrowed_mission_body(&public_id, 0),
    )
    .await;
    let mission_id = mission["mission_id"].as_str().unwrap().to_string();
    {
        let mut board = state.mission_board.lock().await;
        board
            .record_bounty_escrow_intent(&mission_id, &mission_id)
            .unwrap();
    }
    let mut events = state.stream_tx.subscribe();
    // Within the grace period the request may still land: nothing changes.
    let report = crate::reconcile_mission_escrows(&state, 3600).await;
    assert_eq!(report, crate::MissionEscrowSweepReport::default());
    {
        let board = state.mission_board.lock().await;
        let stored = board.get(&mission_id).unwrap();
        assert_eq!(
            stored.status,
            wattetheria_kernel::civilization::missions::MissionStatus::Open
        );
        assert!(stored.has_unconfirmed_escrow());
    }
    // Once stale, the unfunded mission is cancelled, announced, and the escrow forgotten.
    let report = crate::reconcile_mission_escrows(&state, 0).await;
    assert_eq!(report.escrows_abandoned, 1);
    assert_eq!(report.escrows_confirmed, 0);
    {
        let board = state.mission_board.lock().await;
        let stored = board.get(&mission_id).unwrap();
        assert_eq!(
            stored.status,
            wattetheria_kernel::civilization::missions::MissionStatus::Cancelled
        );
        assert!(stored.escrow_id.is_none());
    }
    let mut announced = Vec::new();
    while let Ok(event) = events.try_recv() {
        if event.kind == "mission.cancelled" {
            announced.push(event.payload);
        }
    }
    assert_eq!(announced.len(), 1, "{announced:?}");
    assert_eq!(announced[0]["cancel_reason"], json!("escrow_unconfirmed"));
    assert!(calls.lock().await.is_empty(), "nothing was ever locked");
    let report = crate::reconcile_mission_escrows(&state, 0).await;
    assert_eq!(report, crate::MissionEscrowSweepReport::default());
}

fn cancelled_events(
    events: &mut tokio::sync::broadcast::Receiver<crate::StreamEvent>,
) -> Vec<Value> {
    let mut announced = Vec::new();
    while let Ok(event) = events.try_recv() {
        if event.kind == "mission.cancelled" {
            announced.push(event.payload);
        }
    }
    announced
}

async fn authed_post_status(
    app: Router,
    token: &str,
    uri: &str,
    body: Value,
) -> (StatusCode, Value) {
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
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, json)
}

#[tokio::test]
async fn claiming_a_mission_whose_escrow_never_landed_cancels_it_as_unfunded() {
    let calls: MockLedgerCalls = Arc::new(Mutex::new(Vec::new()));
    let (app, state, token, public_id, _dir) =
        ledger_backed_node(MockLedger::with_balance(calls.clone(), 150)).await;
    // The node recorded the escrow intent and died before the lock request went out; after a
    // restart the mission is Open with an unconfirmed escrow.
    let mission = authed_post_json(
        app.clone(),
        &token,
        "/v1/wattetheria/missions",
        escrowed_mission_body(&public_id, 0),
    )
    .await;
    let mission_id = mission["mission_id"].as_str().unwrap().to_string();
    {
        let mut board = state.mission_board.lock().await;
        board
            .record_bounty_escrow_intent(&mission_id, &mission_id)
            .unwrap();
    }
    let mut events = state.stream_tx.subscribe();
    let (status, body) = authed_post_status(
        app.clone(),
        &token,
        &format!("/v1/wattetheria/missions/{mission_id}/claim"),
        json!({"mission_id": mission_id, "agent_did": "did:key:z6MkEagerWorker"}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], json!("mission_unfunded"));
    {
        let board = state.mission_board.lock().await;
        let stored = board.get(&mission_id).unwrap();
        assert_eq!(
            stored.status,
            wattetheria_kernel::civilization::missions::MissionStatus::Cancelled
        );
        assert!(stored.claimed_by.is_none());
        // The ledger's 404 is not trusted yet: the escrow id stays as a pending refund.
        assert_eq!(stored.escrow_id.as_deref(), Some(mission_id.as_str()));
    }
    let announced = cancelled_events(&mut events);
    assert_eq!(announced.len(), 1, "{announced:?}");
    assert_eq!(announced[0]["cancel_reason"], json!("escrow_unconfirmed"));
    assert!(calls.lock().await.is_empty(), "nothing was ever locked");
    // A repeated claim is simply refused; the cancellation is not announced twice.
    let (status, _) = authed_post_status(
        app.clone(),
        &token,
        &format!("/v1/wattetheria/missions/{mission_id}/claim"),
        json!({"mission_id": mission_id, "agent_did": "did:key:z6MkEagerWorker"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(cancelled_events(&mut events), Vec::<Value>::new());
    // Once stale the sweep forgets the escrow that never existed.
    let report = crate::reconcile_mission_escrows(&state, 0).await;
    assert_eq!(report.refunds_settled, 1);
    let board = state.mission_board.lock().await;
    assert!(board.get(&mission_id).unwrap().escrow_id.is_none());
}

#[tokio::test]
async fn claiming_a_mission_with_an_unconfirmed_but_held_escrow_confirms_it_first() {
    let calls: MockLedgerCalls = Arc::new(Mutex::new(Vec::new()));
    let (app, state, token, public_id, _dir) =
        ledger_backed_node(MockLedger::with_balance(calls.clone(), 150)).await;
    let mission = authed_post_json(
        app.clone(),
        &token,
        "/v1/wattetheria/missions",
        escrowed_mission_body(&public_id, 30),
    )
    .await;
    let mission_id = mission["mission_id"].as_str().unwrap().to_string();
    // The lock landed but the node died before persisting the confirmation.
    {
        let mut board = state.mission_board.lock().await;
        board
            .record_bounty_escrow_intent(&mission_id, &mission_id)
            .unwrap();
    }
    let worker = "did:key:z6MkPatientWorker";
    let (status, body) = authed_post_status(
        app.clone(),
        &token,
        &format!("/v1/wattetheria/missions/{mission_id}/claim"),
        json!({"mission_id": mission_id, "agent_did": worker}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    {
        let board = state.mission_board.lock().await;
        let stored = board.get(&mission_id).unwrap();
        assert!(
            stored.escrow_confirmed,
            "confirmed against the ledger on claim"
        );
        assert_eq!(stored.claimed_by.as_deref(), Some(worker));
    }
    authed_post_json(
        app.clone(),
        &token,
        &format!("/v1/wattetheria/missions/{mission_id}/complete"),
        json!({"mission_id": mission_id, "agent_did": worker}),
    )
    .await;
    authed_post_json(
        app.clone(),
        &token,
        &format!("/v1/wattetheria/missions/{mission_id}/settle"),
        json!({"mission_id": mission_id, "agent_did": worker}),
    )
    .await;
    let kinds: Vec<String> = calls
        .lock()
        .await
        .iter()
        .map(|(kind, _)| kind.clone())
        .collect();
    assert_eq!(kinds, vec!["lock", "release"], "the worker is paid once");
    let release = &calls.lock().await[1].1;
    assert_eq!(release["payload"]["payee"], json!(worker));
}

#[tokio::test]
async fn settling_a_completed_mission_whose_escrow_never_landed_is_refused_and_cancels_it() {
    let calls: MockLedgerCalls = Arc::new(Mutex::new(Vec::new()));
    let (app, state, token, public_id, _dir) =
        ledger_backed_node(MockLedger::with_balance(calls.clone(), 150)).await;
    let mission = authed_post_json(
        app.clone(),
        &token,
        "/v1/wattetheria/missions",
        escrowed_mission_body(&public_id, 0),
    )
    .await;
    let mission_id = mission["mission_id"].as_str().unwrap().to_string();
    let worker = "did:key:z6MkLateWorker";
    authed_post_json(
        app.clone(),
        &token,
        &format!("/v1/wattetheria/missions/{mission_id}/claim"),
        json!({"mission_id": mission_id, "agent_did": worker}),
    )
    .await;
    authed_post_json(
        app.clone(),
        &token,
        &format!("/v1/wattetheria/missions/{mission_id}/complete"),
        json!({"mission_id": mission_id, "agent_did": worker}),
    )
    .await;
    // A completion synced from the network for a mission whose escrow never existed.
    {
        let mut board = state.mission_board.lock().await;
        board
            .record_bounty_escrow_intent(&mission_id, &mission_id)
            .unwrap();
    }
    let mut events = state.stream_tx.subscribe();
    let (status, body) = authed_post_status(
        app.clone(),
        &token,
        &format!("/v1/wattetheria/missions/{mission_id}/settle"),
        json!({"mission_id": mission_id, "agent_did": worker}),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], json!("mission_unfunded"));
    {
        let board = state.mission_board.lock().await;
        let stored = board.get(&mission_id).unwrap();
        assert_eq!(
            stored.status,
            wattetheria_kernel::civilization::missions::MissionStatus::Cancelled,
            "not settled for free"
        );
        assert!(stored.settled_at.is_none());
    }
    assert_eq!(cancelled_events(&mut events).len(), 1);
    assert!(calls.lock().await.is_empty(), "no release was attempted");
}

#[tokio::test]
async fn expired_missions_are_cancelled_and_refunded_by_the_sweep() {
    let calls: MockLedgerCalls = Arc::new(Mutex::new(Vec::new()));
    let (app, state, token, public_id, _dir) =
        ledger_backed_node(MockLedger::with_balance(calls.clone(), 150)).await;
    let mut body = escrowed_mission_body(&public_id, 20);
    body["expires_at"] = json!(chrono::Utc::now().timestamp() - 5);
    let response = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/v1/wattetheria/missions")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(axum::body::Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::BAD_REQUEST,
        "expiry in the past is rejected"
    );

    let mut body = escrowed_mission_body(&public_id, 20);
    let expires_at = chrono::Utc::now().timestamp() + 3600;
    body["expires_at"] = json!(expires_at);
    let mission = authed_post_json(app.clone(), &token, "/v1/wattetheria/missions", body).await;
    let mission_id = mission["mission_id"].as_str().unwrap().to_string();
    assert_eq!(mission["expires_at"], json!(expires_at));
    let report = crate::reconcile_mission_escrows(&state, 3600).await;
    assert_eq!(report.expired, 0, "not due yet");

    {
        let mut board = state.mission_board.lock().await;
        board
            .set_expiry(&mission_id, Some(chrono::Utc::now().timestamp() - 1))
            .unwrap();
    }
    let report = crate::reconcile_mission_escrows(&state, 3600).await;
    assert_eq!(report.expired, 1);
    assert_eq!(report.refunds_pending, 0);
    {
        let board = state.mission_board.lock().await;
        let stored = board.get(&mission_id).unwrap();
        assert_eq!(
            stored.status,
            wattetheria_kernel::civilization::missions::MissionStatus::Cancelled
        );
        assert!(stored.escrow_id.is_none(), "refund settled");
    }
    let kinds: Vec<String> = calls
        .lock()
        .await
        .iter()
        .map(|(kind, _)| kind.clone())
        .collect();
    assert_eq!(kinds, vec!["lock", "refund"]);
    // The second sweep finds nothing to do.
    let report = crate::reconcile_mission_escrows(&state, 3600).await;
    assert_eq!(report, crate::MissionEscrowSweepReport::default());
}
