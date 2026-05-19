use super::*;

#[tokio::test]
async fn supervision_home_and_my_views_work() {
    let (_dir, app, token, _, _state) = build_test_app(20);
    let state_json = authed_get_json(app.clone(), &token, "/v1/state").await;
    let agent_did = state_json["agent_did"].as_str().unwrap();

    let captain = bootstrap_broker_identity(app.clone(), &token, agent_did).await;
    seed_client_view_missions(app.clone(), &token, agent_did).await;
    let supervision_json = authed_get_json(
        app.clone(),
        &token,
        &format!("/v1/supervision/home?public_id={captain}"),
    )
    .await;
    assert_supervision_home_game_block(&supervision_json, &captain);

    let my_missions_json = authed_get_json(
        app.clone(),
        &token,
        &format!("/v1/wattetheria/missions/my?public_id={captain}"),
    )
    .await;
    assert_client_mission_travel_views(&supervision_json, &my_missions_json);
    let supervision_missions_json = authed_get_json(
        app.clone(),
        &token,
        &format!("/v1/supervision/missions?public_id={captain}"),
    )
    .await;
    assert_eq!(
        supervision_missions_json["eligible_open"],
        my_missions_json["eligible_open"]
    );

    let my_governance_json = authed_get_json(
        app.clone(),
        &token,
        &format!("/v1/governance/my?public_id={captain}"),
    )
    .await;
    let supervision_governance_json = authed_get_json(
        app,
        &token,
        &format!("/v1/supervision/governance?public_id={captain}"),
    )
    .await;
    assert_eq!(
        supervision_governance_json["journey"],
        my_governance_json["journey"]
    );
    assert_eq!(
        my_governance_json["home_planet"]["subnet_id"].as_str(),
        Some("planet-test")
    );
    assert_eq!(
        my_governance_json["eligibility"]["has_valid_license"].as_bool(),
        Some(true)
    );
    assert_eq!(
        my_governance_json["governed_planets"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        my_governance_json["journey"]["next_gate"].as_str(),
        Some("influence_floor")
    );
    assert!(
        my_governance_json["qualification_tracks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|track| track["key"].as_str() == Some("civic_governance"))
    );
    assert!(
        !my_governance_json["next_actions"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}
#[tokio::test]
async fn network_routes_surface_bridge_read_models() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge: Arc<dyn SwarmBridge> = Arc::new(MockSwarmBridge {
        fail_accept_and_finalize: false,
        local_node_id: identity.agent_did.clone(),
        agent_stats: BTreeMap::new(),
        network_status: SwarmNetworkStatusView {
            running: true,
            mode: "network".to_string(),
            peer_protocol_distribution: [("v0.1".to_string(), 2_u64)].into_iter().collect(),
        },
        peers: vec![
            SwarmPeerView {
                node_id: "peer-a".to_string(),
                connected: Some(true),
                discovery: None,
                metadata: None,
                relationship: None,
            },
            SwarmPeerView {
                node_id: "peer-b".to_string(),
                connected: Some(true),
                discovery: None,
                metadata: None,
                relationship: None,
            },
        ],
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
        build_test_app_with_bridge(20, dir, identity, event_log, bridge);

    let status_json = authed_get_json(app.clone(), &token, "/v1/network/status").await;
    assert_eq!(status_json["running"].as_bool(), Some(true));
    assert_eq!(status_json["total_nodes"].as_u64(), Some(3));
    assert_eq!(status_json["active_nodes"].as_u64(), Some(3));

    let peers_json = authed_get_json(app, &token, "/v1/network/peers?limit=1").await;
    assert_eq!(peers_json["peers"].as_array().unwrap().len(), 1);
    assert_eq!(
        peers_json["peers"][0]["coordinate_source"].as_str(),
        Some("derived")
    );
}

#[tokio::test]
async fn claim_network_mission_subscribes_scope_and_claims_wattswarm_task() {
    let (dir, app, token, _, state) = build_test_app(20);
    let agent_did = state.agent_did.clone();
    seed_gateway_remote_mission(dir.path(), &state, "mission-remote-1").await;
    let response = authed_post_json(
        app,
        &token,
        "/v1/wattetheria/missions/mission-remote-1/claim",
        json!({
            "mission_id": "mission-remote-1",
            "agent_did": agent_did
        }),
    )
    .await;

    assert_eq!(response["ok"].as_bool(), Some(true));
    assert_eq!(response["status"].as_str(), Some("network_claim_submitted"));
    assert_eq!(response["task_id"].as_str(), Some("mission-remote-1"));
    assert_eq!(
        response["mission_scope_hint"].as_str(),
        Some("group:mission-remote-1")
    );
    assert_eq!(
        response["publisher_wattswarm_node_id"].as_str(),
        Some("publisher-node")
    );
    assert_eq!(
        response["swarm_claim"]["task_id"].as_str(),
        Some("mission-remote-1")
    );
    assert_eq!(
        response["task_contract_sync"]["task_id"].as_str(),
        Some("mission-remote-1")
    );
    assert_eq!(
        response["task_contract_sync"]["scope_hint"].as_str(),
        Some("group:mission-remote-1")
    );
    assert!(response.get("task_announcement_sync").is_none());
}

async fn seed_gateway_remote_mission(
    data_dir: &std::path::Path,
    state: &ControlPlaneState,
    mission_id: &str,
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
        "publisher_wattswarm_node_id": "publisher-node",
        "swarm_scope": {"kind": "group", "id": mission_id},
        "mission_feed_key": "wattetheria.missions",
        "mission_scope_hint": format!("group:{mission_id}"),
        "reward": {"agent_watt": 10},
        "payload": {"work": "deliver"}
    });
    contract.output_schema = json!({
        "type": "object",
        "required": ["mission_id", "agent_did", "result"],
        "properties": {
            "mission_id": {"type": "string"},
            "agent_did": {"type": "string"},
            "result": {}
        }
    });
    let gateway_task = json!({
        "id": mission_id,
        "task_id": mission_id,
        "task_type": "wattetheria.mission",
        "title": "Remote mission",
        "status": "published",
        "source_node_id": "publisher-node",
        "publisher_wattswarm_node_id": "publisher-node",
        "mission_feed_key": "wattetheria.missions",
        "mission_scope_hint": format!("group:{mission_id}"),
        "task_contract": contract,
    });
    let gateway_app = Router::new().route(
        "/v1/wattetheria/missions",
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

#[tokio::test]
async fn complete_network_mission_syncs_contract_and_proposes_candidate() {
    let (dir, app, token, _, state) = build_test_app(20);
    let agent_did = state.agent_did.clone();
    seed_gateway_remote_mission(dir.path(), &state, "mission-remote-2").await;
    let response = authed_post_json(
        app,
        &token,
        "/v1/wattetheria/missions/mission-remote-2/complete",
        json!({
            "mission_id": "mission-remote-2",
            "agent_did": agent_did,
            "claim_route": {
                "task_id": "mission-remote-2",
                "mission_feed_key": "wattetheria.missions",
                "mission_scope_hint": "group:mission-remote-2",
                "publisher_wattswarm_node_id": "publisher-node"
            },
            "result": {"delivered": true}
        }),
    )
    .await;

    assert_eq!(response["ok"].as_bool(), Some(true));
    assert_eq!(
        response["status"].as_str(),
        Some("network_complete_submitted")
    );
    assert_eq!(response["task_id"].as_str(), Some("mission-remote-2"));
    assert_eq!(
        response["candidate_id"].as_str(),
        Some(format!("wattetheria-candidate-mission-remote-2-{agent_did}").as_str())
    );
    assert_eq!(
        response["mission_scope_hint"].as_str(),
        Some("group:mission-remote-2")
    );
    assert_eq!(
        response["publisher_wattswarm_node_id"].as_str(),
        Some("publisher-node")
    );
    assert_eq!(
        response["swarm_candidate"]["task_id"].as_str(),
        Some("mission-remote-2")
    );
    assert_eq!(
        response["task_contract_sync"]["task_id"].as_str(),
        Some("mission-remote-2")
    );
    assert_eq!(
        response["task_contract_sync"]["scope_hint"].as_str(),
        Some("group:mission-remote-2")
    );
    assert!(response.get("task_announcement_sync").is_none());
}

#[tokio::test]
async fn settle_local_publisher_mission_finalizes_wattswarm_candidate() {
    let (_dir, app, token, _, state) = build_test_app(20);
    let agent_did = state.agent_did.clone();
    let mission = authed_post_json(
        app.clone(),
        &token,
        "/v1/wattetheria/missions",
        json!({
            "title": "Publisher mission",
            "description": "Validate direct settlement finalization",
            "publisher": "publisher-public",
            "publisher_kind": "player",
            "domain": "trade",
            "reward": {
                "agent_watt": 10,
                "reputation": 1,
                "capacity": 0,
                "treasury_share_watt": 0
            },
            "payload": {"work": "inspect"}
        }),
    )
    .await;
    let mission_id = mission["mission_id"].as_str().unwrap();
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
    let _completed = authed_post_json(
        app.clone(),
        &token,
        &format!("/v1/wattetheria/missions/{mission_id}/complete"),
        json!({
            "mission_id": mission_id,
            "agent_did": agent_did,
        }),
    )
    .await;

    let settled = authed_post_json(
        app,
        &token,
        &format!("/v1/wattetheria/missions/{mission_id}/settle"),
        json!({
            "mission_id": mission_id,
        }),
    )
    .await;

    assert_eq!(settled["status"].as_str(), Some("settled"));
    assert_eq!(
        settled["swarm_finalize"]["task_id"].as_str(),
        Some(mission_id)
    );
    assert_eq!(
        settled["swarm_finalize"]["candidate_id"].as_str(),
        Some(format!("wattetheria-candidate-{mission_id}-{agent_did}").as_str())
    );
}

#[tokio::test]
async fn topic_routes_persist_product_metadata_and_proxy_bridge_calls() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge {
        fail_accept_and_finalize: false,
        local_node_id: identity.agent_did.clone(),
        agent_stats: BTreeMap::new(),
        network_status: SwarmNetworkStatusView {
            running: true,
            mode: "local".to_string(),
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
    let (_dir, app, token, _, _state) =
        build_test_app_with_bridge(20, dir, identity, event_log, bridge_handle);

    let created = authed_post_json(
        app.clone(),
        &token,
        "/v1/wattetheria/hives",
        json!({
            "network_id": "mainnet:test",
            "feed_key": "crew.chat",
            "scope_hint": "group:crew-7",
            "display_name": "Crew Seven",
            "projection_kind": "working_group",
            "summary": "Operations thread",
            "why_this_exists": "Mission pressure",
            "initial_message": {"text": "hello crew"}
        }),
    )
    .await;
    assert_eq!(
        created["hive"]["topic_id"].as_str(),
        Some("mainnet:test@crew.chat@group:crew-7")
    );
    assert_eq!(created["hive"]["network_id"].as_str(), Some("mainnet:test"));

    let hives_json = authed_get_json(app.clone(), &token, "/v1/wattetheria/hives").await;
    assert_eq!(hives_json["hives"].as_array().unwrap().len(), 1);

    let messages_json = authed_get_json(
        app,
        &token,
        "/v1/wattetheria/hives/mainnet:test@crew.chat@group:crew-7/messages?network_id=mainnet:test",
    )
    .await;
    assert_eq!(messages_json["network_id"].as_str(), Some("mainnet:test"));
    assert_eq!(messages_json["messages"].as_array().unwrap().len(), 1);
    assert_eq!(
        messages_json["messages"][0]["network_id"].as_str(),
        Some("mainnet:test")
    );
    assert_eq!(
        messages_json["messages"][0]["author_public_id"].as_str(),
        Some(created["hive"]["created_by_public_id"].as_str().unwrap())
    );

    let subscriptions = bridge.subscriptions.lock().await;
    assert_eq!(subscriptions.len(), 1);
    assert_eq!(subscriptions[0].0.as_deref(), Some("mainnet:test"));
    assert_eq!(subscriptions[0].2, "crew.chat");
    drop(subscriptions);
    assert_eq!(bridge.messages.lock().await.len(), 1);
}
