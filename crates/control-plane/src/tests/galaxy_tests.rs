use super::*;

#[tokio::test]
async fn galaxy_event_publish_and_query_works() {
    let (_dir, app, token, _, _state) = build_test_app(20);
    let state_json = authed_get_json(app.clone(), &token, "/v1/state").await;
    let agent_did = state_json["agent_did"].as_str().unwrap();

    let publish_body = json!({
        "category": "economic",
        "zone_id": "genesis-core",
        "title": "Power Shortage",
        "description": "Grid instability is driving up maintenance demand.",
        "severity": 4,
        "expires_at": null,
        "tags": ["market", "supply"]
    });
    let publish_resp = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/v1/galaxy/events")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(axum::body::Body::from(publish_body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(publish_resp.status(), StatusCode::OK);

    let zones_resp = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .uri("/v1/galaxy/zones")
                .header("authorization", format!("Bearer {token}"))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(zones_resp.status(), StatusCode::OK);
    let zones_json: Value =
        serde_json::from_slice(&zones_resp.into_body().collect().await.unwrap().to_bytes())
            .unwrap();
    assert!(zones_json.as_array().unwrap().len() >= 3);

    let events_resp = app
        .oneshot(
            axum::http::Request::builder()
                .uri("/v1/galaxy/events?zone_id=genesis-core")
                .header("authorization", format!("Bearer {token}"))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(events_resp.status(), StatusCode::OK);
    let events_json: Value =
        serde_json::from_slice(&events_resp.into_body().collect().await.unwrap().to_bytes())
            .unwrap();
    assert_eq!(events_json["events"].as_array().unwrap().len(), 1);
    assert_eq!(events_json["events"][0]["title"], "Power Shortage");
    assert_eq!(
        events_json["public_memory_owner"]["controller_id"].as_str(),
        Some(agent_did)
    );
}

#[tokio::test]
async fn galaxy_map_endpoints_expose_official_genesis_map() {
    let (_dir, app, token, _, _state) = build_test_app(20);

    let map_list_json = authed_get_json(app.clone(), &token, "/v1/galaxy/maps").await;
    let maps = map_list_json["maps"].as_array().unwrap();
    assert_eq!(maps.len(), 1);
    assert_eq!(maps[0]["map_id"].as_str(), Some("genesis-base"));
    assert_eq!(maps[0]["system_count"].as_u64(), Some(3));

    let selected_map_json = authed_get_json(app, &token, "/v1/galaxy/map").await;
    assert_eq!(selected_map_json["map_id"].as_str(), Some("genesis-base"));
    assert_eq!(selected_map_json["systems"].as_array().unwrap().len(), 3);
    assert_eq!(selected_map_json["routes"].as_array().unwrap().len(), 2);
}

#[tokio::test]
async fn galaxy_travel_endpoints_expose_options_and_plans() {
    let (_dir, app, token, _, _state) = build_test_app(21);
    let state_json = authed_get_json(app.clone(), &token, "/v1/state").await;
    let agent_did = state_json["agent_did"].as_str().unwrap();
    let captain = bootstrap_broker_identity(app.clone(), &token, agent_did).await;

    let _event_json = authed_post_json(
        app.clone(),
        &token,
        "/v1/galaxy/events",
        json!({
            "category": "spatial",
            "zone_id": "frontier-belt",
            "title": "Frontier turbulence",
            "description": "Instability across the gate corridor.",
            "severity": 8,
            "tags": ["hazard"]
        }),
    )
    .await;

    let options_json = authed_get_json(
        app.clone(),
        &token,
        &format!("/v1/galaxy/travel/options?public_id={captain}"),
    )
    .await;
    assert_eq!(
        options_json["from_system_id"].as_str(),
        Some("frontier-gate")
    );
    let options = options_json["options"].as_array().unwrap();
    assert_eq!(options.len(), 2);
    let abyss_option = options
        .iter()
        .find(|option| option["to_system_id"].as_str() == Some("abyss-watch"))
        .unwrap();
    assert_eq!(abyss_option["risk_level"].as_str(), Some("volatile"));
    assert!(
        abyss_option["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning["code"].as_str() == Some("route_risk_high"))
    );

    let plan_json = authed_get_json(
        app,
        &token,
        &format!("/v1/galaxy/travel/plan?public_id={captain}&to_system_id=abyss-watch"),
    )
    .await;
    assert_eq!(plan_json["map_id"].as_str(), Some("genesis-base"));
    assert_eq!(plan_json["total_travel_cost"].as_u64(), Some(5));
    assert_eq!(plan_json["legs"].as_array().unwrap().len(), 1);
    assert_eq!(
        plan_json["traversed_system_ids"].as_array().unwrap().len(),
        2
    );
}

#[tokio::test]
async fn galaxy_travel_state_and_session_flow_work() {
    let (_dir, app, token, _, _state) = build_test_app(21);
    let state_json = authed_get_json(app.clone(), &token, "/v1/state").await;
    let agent_did = state_json["agent_did"].as_str().unwrap();
    let captain = bootstrap_broker_identity(app.clone(), &token, agent_did).await;
    let _ = publish_trade_mission(
        app.clone(),
        &token,
        TradeMissionSpec {
            title: "Deep watch market relay",
            description: "Unlock deep-space market visibility",
            reward_watt: 35,
            reward_reputation: 4,
            objective: "deep-watch",
            required_faction: None,
            subnet_id: None,
            zone_id: Some("deep-space"),
        },
    )
    .await;

    let initial_json = authed_get_json(
        app.clone(),
        &token,
        &format!("/v1/galaxy/travel/state?public_id={captain}"),
    )
    .await;
    assert_eq!(
        initial_json["travel_state"]["current_position"]["system_id"].as_str(),
        Some("frontier-gate")
    );
    assert!(initial_json["travel_state"]["active_session"].is_null());

    let departed_json = authed_post_json(
        app.clone(),
        &token,
        "/v1/galaxy/travel/depart",
        json!({
            "public_id": captain,
            "to_system_id": "abyss-watch"
        }),
    )
    .await;
    assert_eq!(
        departed_json["travel_state"]["active_session"]["to_system_id"].as_str(),
        Some("abyss-watch")
    );
    assert_eq!(
        departed_json["travel_state"]["active_session"]["status"].as_str(),
        Some("in_transit")
    );

    let arrived_json = authed_post_json(
        app,
        &token,
        "/v1/galaxy/travel/arrive",
        json!({
            "public_id": captain
        }),
    )
    .await;
    assert_eq!(
        arrived_json["travel_state"]["current_position"]["system_id"].as_str(),
        Some("abyss-watch")
    );
    assert!(arrived_json["travel_state"]["active_session"].is_null());
    assert_eq!(
        arrived_json["travel_state"]["current_position"]["zone_id"].as_str(),
        Some("deep-space")
    );
    assert_eq!(
        arrived_json["travel_state"]["last_consequence"]["mission_impact"]["eligible_local_count"]
            .as_u64(),
        Some(1)
    );
    assert_eq!(
        arrived_json["travel_state"]["last_consequence"]["route_risk_level"].as_str(),
        Some("volatile")
    );
    assert!(
        !arrived_json["travel_state"]["recent_consequences"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}
