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
        &format!("/v1/missions/my?public_id={captain}"),
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
            },
            SwarmPeerView {
                node_id: "peer-b".to_string(),
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
        "/v1/civilization/topics",
        json!({
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
        created["topic"]["topic_id"].as_str(),
        Some("crew.chat@group:crew-7")
    );

    let topics_json = authed_get_json(app.clone(), &token, "/v1/civilization/topics").await;
    assert_eq!(topics_json["topics"].as_array().unwrap().len(), 1);

    let messages_json = authed_get_json(
        app,
        &token,
        "/v1/civilization/topics/messages?feed_key=crew.chat&scope_hint=group:crew-7",
    )
    .await;
    assert_eq!(messages_json["messages"].as_array().unwrap().len(), 1);
    assert_eq!(
        messages_json["messages"][0]["author_public_id"].as_str(),
        Some(created["topic"]["created_by_public_id"].as_str().unwrap())
    );

    let subscriptions = bridge.subscriptions.lock().await;
    assert_eq!(subscriptions.len(), 1);
    assert_eq!(subscriptions[0].1, "crew.chat");
    drop(subscriptions);
    assert_eq!(bridge.messages.lock().await.len(), 1);
}
