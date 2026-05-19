use super::*;

#[allow(clippy::too_many_lines)]
#[tokio::test]
async fn mission_lifecycle_settles_and_funds_treasury() {
    let (_dir, app, token, _, state) = build_test_app(30);

    let publish_body = json!({
        "title": "Stabilize the relay",
        "description": "Restore uptime on the frontier relay.",
        "publisher": "planet-test",
        "publisher_kind": "planetary_government",
        "domain": "security",
        "subnet_id": "planet-test",
        "zone_id": "frontier-ring",
        "required_role": "enforcer",
        "required_faction": null,
        "reward": {
            "agent_watt": 120,
            "reputation": 8,
            "capacity": 2,
            "treasury_share_watt": 30
        },
        "payload": {"objective": "relay_repair"}
    });
    let publish_resp = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/v1/wattetheria/missions")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(axum::body::Body::from(publish_body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(publish_resp.status(), StatusCode::CREATED);
    let publish_json: Value =
        serde_json::from_slice(&publish_resp.into_body().collect().await.unwrap().to_bytes())
            .unwrap();
    let mission_id = publish_json["mission_id"].as_str().unwrap().to_string();

    for (action, agent_did) in [("claim", "agent-enforcer"), ("complete", "agent-enforcer")] {
        let resp = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri(format!("/v1/wattetheria/missions/{mission_id}/{action}"))
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(
                        json!({
                            "mission_id": mission_id,
                            "agent_did": agent_did
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
    }

    let settle_resp = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri(format!("/v1/wattetheria/missions/{mission_id}/settle"))
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(axum::body::Body::from(
                    json!({"mission_id": mission_id}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(settle_resp.status(), StatusCode::OK);

    let list_resp = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .uri("/v1/wattetheria/missions?status=settled")
                .header("authorization", format!("Bearer {token}"))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(list_resp.status(), StatusCode::OK);
    let list_json: Value =
        serde_json::from_slice(&list_resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(list_json.as_array().unwrap().len(), 1);
    assert_eq!(list_json[0]["status"], "settled");

    let persisted = state
        .local_db
        .load_domain::<GovernanceEngine>(wattetheria_kernel::local_db::domain::GOVERNANCE)
        .unwrap()
        .unwrap();
    let planet = persisted.list_planets().remove(0);
    assert_eq!(planet.treasury_watt, 30);
}
#[tokio::test]
async fn governance_lifecycle_endpoints_work() {
    let (_dir, app, token, _, state) = build_test_app(40);
    let state_json = authed_get_json(app.clone(), &token, "/v1/state").await;
    let agent_did = state_json["agent_did"].as_str().unwrap();

    for (uri, body) in [
        (
            "/v1/governance/stability",
            json!({"subnet_id":"planet-test","delta":-80}),
        ),
        (
            "/v1/governance/recall",
            json!({
                "subnet_id":"planet-test",
                "initiated_by": agent_did,
                "reason":"stability collapse",
                "threshold":25
            }),
        ),
        (
            "/v1/governance/recall/resolve",
            json!({
                "subnet_id":"planet-test",
                "successor":"agent-challenger",
                "min_bond":100
            }),
        ),
        (
            "/v1/governance/custody",
            json!({
                "subnet_id":"planet-test",
                "reason":"civil emergency",
                "managed_by":"neutral-admin"
            }),
        ),
        (
            "/v1/governance/takeover",
            json!({
                "subnet_id":"planet-test",
                "challenger":"agent-challenger",
                "reason":"secured orbit",
                "min_bond":100
            }),
        ),
    ] {
        assert_eq!(
            authed_post(app.clone(), &token, uri, body).await,
            StatusCode::OK
        );
    }

    let persisted = state
        .local_db
        .load_domain::<GovernanceEngine>(wattetheria_kernel::local_db::domain::GOVERNANCE)
        .unwrap()
        .unwrap();
    let planet = persisted.planet("planet-test").unwrap();
    assert_eq!(planet.creator, "agent-challenger");
}

#[tokio::test]
async fn civilization_briefing_and_generated_galaxy_events_work() {
    let (_dir, app, token, _, _state) = build_test_app(40);
    let state_json = authed_get_json(app.clone(), &token, "/v1/state").await;
    let agent_did = state_json["agent_did"].as_str().unwrap();

    for (uri, body, expected) in [
        (
            "/v1/civilization/profile",
            json!({
                "agent_did": agent_did,
                "faction": "order",
                "role": "operator",
                "strategy": "conservative",
                "home_subnet_id": "planet-test",
                "home_zone_id": "genesis-core"
            }),
            StatusCode::OK,
        ),
        (
            "/v1/governance/stability",
            json!({"subnet_id":"planet-test","delta":-60}),
            StatusCode::OK,
        ),
        (
            "/v1/wattetheria/missions",
            json!({
                "title": "Defend gate",
                "description": "Interdict raiders",
                "publisher": "planet-test",
                "publisher_kind": "planetary_government",
                "domain": "security",
                "subnet_id": "planet-test",
                "zone_id": "genesis-core",
                "required_role": "enforcer",
                "required_faction": null,
                "reward": {"agent_watt": 20, "reputation": 3, "capacity": 1, "treasury_share_watt": 2},
                "payload": {}
            }),
            StatusCode::CREATED,
        ),
        (
            "/v1/galaxy/events/generate",
            json!({"max_events": 3}),
            StatusCode::OK,
        ),
    ] {
        assert_eq!(authed_post(app.clone(), &token, uri, body).await, expected);
    }

    let emergencies_json =
        authed_get_json(app.clone(), &token, "/v1/civilization/emergencies").await;
    assert!(
        !emergencies_json["emergencies"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    let briefing_json =
        authed_get_json(app.clone(), &token, "/v1/civilization/briefing?hours=12").await;
    assert!(
        briefing_json["briefing"]["emergencies"]
            .as_array()
            .is_some()
    );
    assert_eq!(
        briefing_json["public_memory_owner"]["controller_id"].as_str(),
        Some(agent_did)
    );

    let supervision_briefing_json =
        authed_get_json(app.clone(), &token, "/v1/supervision/briefing?hours=12").await;
    let briefing_emergencies = briefing_json["briefing"]["emergencies"].as_array().unwrap();
    let supervision_emergencies = supervision_briefing_json["briefing"]["emergencies"]
        .as_array()
        .unwrap();
    assert_eq!(supervision_emergencies.len(), briefing_emergencies.len());
    assert_eq!(
        supervision_emergencies
            .iter()
            .map(|item| item["title"].as_str().unwrap())
            .collect::<Vec<_>>(),
        briefing_emergencies
            .iter()
            .map(|item| item["title"].as_str().unwrap())
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn bootstrap_identity_returns_unified_identity_bundle_and_public_memory_owner() {
    let (dir, app, token, _, _state) = build_test_app(20);
    let state_json = authed_get_json(app.clone(), &token, "/v1/state").await;
    let agent_did = state_json["agent_did"].as_str().unwrap();

    let bootstrap_json = authed_post_json(
        app.clone(),
        &token,
        "/v1/civilization/bootstrap-identity",
        json!({
            "display_name": "Captain Aurora"
        }),
    )
    .await;

    // When no public_id is provided, the endpoint reuses the agent's
    // existing active identity (created during test setup) or generates a
    // new fingerprinted public_id from the display name.
    let returned_public_id = bootstrap_json["public_identity"]["public_id"]
        .as_str()
        .unwrap();
    assert!(
        extract_public_id_fingerprint(returned_public_id).is_some(),
        "auto-generated public_id should be fingerprinted: {returned_public_id}"
    );
    assert_eq!(
        bootstrap_json["controller_binding"]["controller_kind"].as_str(),
        Some("local_wattswarm")
    );
    assert_eq!(
        bootstrap_json["profile"]["agent_did"].as_str(),
        Some(agent_did)
    );
    assert_eq!(
        bootstrap_json["profile"]["faction"].as_str(),
        Some("freeport")
    );
    assert_eq!(bootstrap_json["profile"]["role"].as_str(), Some("broker"));
    assert_eq!(
        bootstrap_json["profile"]["strategy"].as_str(),
        Some("balanced")
    );
    assert_eq!(
        bootstrap_json["public_memory_owner"]["public_id"].as_str(),
        Some(returned_public_id)
    );

    let captain_alt = scoped_id("captain-aurora-alt", agent_did);
    let bootstrap_identity_json = authed_post_json(
        app.clone(),
        &token,
        "/v1/civilization/bootstrap-identity",
        json!({
            "public_id": captain_alt,
            "display_name": "Captain Aurora Alt",
            "faction": "freeport",
            "role": "broker",
            "strategy": "balanced",
            "home_subnet_id": "planet-test",
            "home_zone_id": "genesis-core"
        }),
    )
    .await;
    assert_eq!(
        bootstrap_identity_json["public_identity"]["public_id"].as_str(),
        Some(captain_alt.as_str())
    );

    let events = EventLog::new(dir.path().join("events.jsonl"))
        .unwrap()
        .get_all()
        .unwrap();
    let bootstrap_events: Vec<_> = events
        .iter()
        .filter(|event| event.event_type == "CIVILIZATION_IDENTITY_BOOTSTRAPPED")
        .collect();
    assert_eq!(bootstrap_events.len(), 2);
    let bootstrap_event = bootstrap_events
        .iter()
        .find(|event| {
            event.payload["public_memory"]["public_id"].as_str() == Some(returned_public_id)
        })
        .unwrap();
    assert_eq!(
        bootstrap_event.payload["public_memory"]["public_id"].as_str(),
        Some(returned_public_id)
    );
    assert_eq!(
        bootstrap_event.payload["public_memory"]["controller_id"].as_str(),
        Some(agent_did)
    );
}

#[tokio::test]
async fn bootstrap_identity_reuses_existing_public_id_for_same_agent_when_public_id_is_omitted() {
    let (_dir, app, token, _, _state) = build_test_app(20);

    let first_bootstrap = authed_post_json(
        app.clone(),
        &token,
        "/v1/civilization/bootstrap-identity",
        json!({
            "display_name": "Node Agent"
        }),
    )
    .await;
    let first_public_id = first_bootstrap["public_identity"]["public_id"]
        .as_str()
        .unwrap()
        .to_string();

    let second_bootstrap = authed_post_json(
        app,
        &token,
        "/v1/civilization/bootstrap-identity",
        json!({
            "display_name": "Node Agent Updated"
        }),
    )
    .await;

    assert_eq!(
        second_bootstrap["public_identity"]["public_id"].as_str(),
        Some(first_public_id.as_str())
    );
    assert_eq!(
        second_bootstrap["public_identity"]["display_name"].as_str(),
        Some("Node Agent Updated")
    );
}

#[tokio::test]
async fn public_identities_and_catalog_endpoints_work() {
    let (_dir, app, token, _, _state) = build_test_app(20);
    let state_json = authed_get_json(app.clone(), &token, "/v1/state").await;
    let agent_did = state_json["agent_did"].as_str().unwrap();

    let captain = scoped_id("captain-aurora", agent_did);
    let bootstrap_json = authed_post_json(
        app.clone(),
        &token,
        "/v1/civilization/bootstrap-identity",
        json!({
            "public_id": captain,
            "display_name": "Captain Aurora",
            "faction": "freeport",
            "role": "broker",
            "strategy": "balanced",
            "home_subnet_id": "planet-test",
            "home_zone_id": "genesis-core"
        }),
    )
    .await;
    assert_eq!(
        bootstrap_json["public_identity"]["public_id"].as_str(),
        Some(captain.as_str())
    );

    let identities_json = authed_get_json(app.clone(), &token, "/v1/civilization/identities").await;
    let public_identities = identities_json["public_identities"].as_array().unwrap();
    assert!(public_identities.len() >= 2);
    assert!(public_identities.iter().any(|item| {
        item["identity"]["public_identity"]["public_id"].as_str() == Some(captain.as_str())
            && item["identity"]["profile"]["role"].as_str() == Some("broker")
            && item["travel_state"]["current_position"]["system_id"].as_str()
                == Some("frontier-gate")
    }));
    let supervision_identities_json =
        authed_get_json(app.clone(), &token, "/v1/supervision/identities").await;
    assert_eq!(
        supervision_identities_json["public_identities"],
        identities_json["public_identities"]
    );

    let catalog_json = authed_get_json(app, &token, "/v1/catalog/bootstrap").await;
    assert!(
        catalog_json["roles"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item.as_str() == Some("broker"))
    );
    assert!(
        catalog_json["travel_risk_levels"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item.as_str() == Some("volatile"))
    );
    assert!(
        catalog_json["organization_kinds"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item.as_str() == Some("consortium"))
    );
    assert!(
        catalog_json["organization_permissions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item.as_str() == Some("publish_missions"))
    );
    assert!(
        catalog_json["organization_permissions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item.as_str() == Some("manage_governance"))
    );
    assert!(
        catalog_json["organization_proposal_kinds"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item.as_str() == Some("subnet_charter"))
    );
    assert_eq!(catalog_json["galaxy_zones"].as_array().unwrap().len(), 3);
}

#[tokio::test]
async fn game_catalog_and_status_endpoints_work() {
    let (_dir, app, token, _, _state) = build_test_app(20);
    let state_json = authed_get_json(app.clone(), &token, "/v1/state").await;
    let agent_did = state_json["agent_did"].as_str().unwrap();

    let (_, captain) = bootstrap_broker_game(app.clone(), &token, agent_did).await;
    let _ = settle_trade_mission_for_agent(app.clone(), &token, agent_did).await;

    let catalog_json = authed_get_json(app.clone(), &token, "/v1/game/catalog").await;
    assert_eq!(catalog_json["roles"].as_array().unwrap().len(), 4);
    assert_eq!(catalog_json["stages"].as_array().unwrap().len(), 4);
    let starter_list = authed_get_json(
        app.clone(),
        &token,
        &format!("/v1/game/starter-missions?public_id={captain}"),
    )
    .await;
    assert_starter_templates_with_anchor(&starter_list);
    let pack_bootstrap = authed_post_json(
        app.clone(),
        &token,
        "/v1/game/mission-pack/bootstrap",
        json!({"public_id": captain}),
    )
    .await;
    assert_eq!(pack_bootstrap["created"].as_array().unwrap().len(), 2);
    let mission_pack_json = authed_get_json(
        app.clone(),
        &token,
        &format!("/v1/game/mission-pack?public_id={captain}"),
    )
    .await;
    assert_game_mission_pack_payload(&mission_pack_json, &captain);
    let bootstrap_json = authed_get_json(
        app.clone(),
        &token,
        &format!("/v1/game/bootstrap?public_id={captain}"),
    )
    .await;
    assert_game_bootstrap_payload(&bootstrap_json, &captain);
    let supervision_bootstrap_json = authed_get_json(
        app.clone(),
        &token,
        &format!("/v1/supervision/bootstrap?public_id={captain}"),
    )
    .await;
    assert_eq!(
        supervision_bootstrap_json["bootstrap_flow"],
        bootstrap_json["bootstrap_flow"]
    );

    let status_json = authed_get_json(
        app.clone(),
        &token,
        &format!("/v1/game/status?public_id={captain}"),
    )
    .await;
    assert_game_status_payload(&status_json, &captain);
    let supervision_status_json = authed_get_json(
        app.clone(),
        &token,
        &format!("/v1/supervision/status?public_id={captain}"),
    )
    .await;
    assert_eq!(supervision_status_json["status"], status_json["status"]);
    assert_eq!(
        supervision_status_json["supervision"],
        status_json["supervision"]
    );
}
