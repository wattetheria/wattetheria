use super::*;
use axum::extract::{Path as AxumPath, Query, State as AxumState};
use std::sync::Arc;
use tokio::sync::Mutex;

/// Every signed farm command the mock gateway received.
#[derive(Clone, Default)]
struct MockFarm {
    actions: Arc<Mutex<Vec<Value>>>,
    /// Until when the gateway holds the agent, as its status reports it.
    busy_until_ms: Arc<Mutex<Option<i64>>>,
}

/// A season that has ended, with its parameters revealed.
async fn ended_season(AxumPath(season_no): AxumPath<i64>) -> Json<Value> {
    Json(json!({
        "season_no": season_no,
        "params_hash": "abc",
        "params": {"disabled": ["well"]},
        "seed": "00",
    }))
}

/// A gateway with the games catalog, the farm manual, one agent status, past seasons and the
/// action endpoint, plus the presence recovery lookup `enter_space` needs.
#[allow(clippy::too_many_lines)]
async fn spawn_mock_gateway() -> (String, MockFarm) {
    let mock = MockFarm::default();
    let router = axum::Router::new()
        .route(
            "/api/spaces/presence/{did}/record",
            axum::routing::get(|| async { axum::http::StatusCode::NOT_FOUND }),
        )
        .route(
            "/api/games",
            axum::routing::get(|| async {
                Json(json!({"games": [{"game_id": "farm", "summary": "Grow and sell crops.",
                                       "space_id": "the-ville", "field_place_id": "public-field/farmland",
                                       "action_secs": 12}]}))
            }),
        )
        .route(
            "/api/games/{game_id}",
            axum::routing::get(
                |AxumPath(game_id): AxumPath<String>, Query(query): Query<Value>| async move {
                    Json(json!({
                        "game_id": game_id,
                        "asked_for": query["agent_did"],
                        "availability": {"can_join": true, "free_plots": 700},
                    }))
                },
            ),
        )
        .route(
            "/api/farm/agents/{did}",
            axum::routing::get(
                |AxumState(mock): AxumState<MockFarm>, AxumPath(did): AxumPath<String>| async move {
                    Json(json!({
                        "agent_did": did,
                        "busy_until": mock.busy_until_ms.lock().await.and_then(chrono::DateTime::from_timestamp_millis)
                            .map(|at| at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true)),
                        "next": {"do": "claim_plots", "parcel": 1, "tile": [57, 86]},
                    }))
                },
            ),
        )
        .route(
            "/api/farm/seasons/{season_no}",
            axum::routing::get(ended_season),
        )
        .route(
            "/api/farm/actions",
            axum::routing::post(
                |AxumState(mock): AxumState<MockFarm>, Json(body): Json<Value>| async move {
                    mock.actions.lock().await.push(body.clone());
                    let payload = &body["payload"];
                    if payload["action_id"] == "lost" {
                        // Accepted, but the reply cannot be read.
                        return Ok((axum::http::StatusCode::OK, Json(Value::Null)));
                    }
                    if payload["action_id"] == "late" {
                        // The command reached the gateway late, so it holds the agent longer than
                        // the node assumed; then a proxy timed out in front of the reply.
                        *mock.busy_until_ms.lock().await =
                            Some(chrono::Utc::now().timestamp_millis() + 20_000);
                        return Err((axum::http::StatusCode::GATEWAY_TIMEOUT, "upstream timed out"));
                    }
                    if payload["action_id"] == "proxy" {
                        // A proxy's timeout page that happens to be JSON with an error code.
                        return Ok((
                            axum::http::StatusCode::GATEWAY_TIMEOUT,
                            Json(json!({"error": "upstream timeout"})),
                        ));
                    }
                    if payload["action_id"] == "commit" {
                        // The gateway's own error after its database may already have committed.
                        return Ok((
                            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
                            Json(json!({"error": "internal", "message": "connection reset"})),
                        ));
                    }
                    if payload["action"] == "sell" {
                        return Ok((
                            axum::http::StatusCode::CONFLICT,
                            Json(json!({"error": "not_at_place", "message": "walk into the buyback",
                                        "details": {"place_id": "the-willows-market-and-pharmacy"}})),
                        ));
                    }
                    if payload["action"] == "steal" {
                        let busy_until = (chrono::Utc::now() + chrono::Duration::seconds(20))
                            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
                        return Ok((
                            axum::http::StatusCode::OK,
                            Json(json!({"ok": true, "action": "steal", "units": 1,
                                        "busy_until": busy_until, "escape_tile": [61, 90]})),
                        ));
                    }
                    if payload["action"] == "water" {
                        return Ok((
                            axum::http::StatusCode::CONFLICT,
                            Json(json!({"error": "not_at_tile", "message": "stand on the plot",
                                        "details": {"tile": payload["tile"]}})),
                        ));
                    }
                    let busy_until = (chrono::Utc::now() + chrono::Duration::seconds(6))
                        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
                    Ok((
                        axum::http::StatusCode::OK,
                        Json(json!({"ok": true, "action": payload["action"], "busy_until": busy_until})),
                    ))
                },
            ),
        )
        .with_state(mock.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (url, mock)
}

async fn call(app: Router, token: &str, name: &str, arguments: Value) -> Value {
    mcp_request(
        app,
        token,
        json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":arguments}}),
    )
    .await
}

fn content(response: &Value) -> Value {
    let text = response["result"]["content"][0]["text"].as_str().unwrap();
    serde_json::from_str(text).unwrap()
}

fn with_gateway(state: ControlPlaneState, gateway_url: String) -> ControlPlaneState {
    ControlPlaneState {
        gateway_urls: vec![gateway_url],
        ..state
    }
}

/// The map before the farm field was added, as an old node still has it: the current map with
/// every field place removed, re-hashed so it validates as a distinct version.
fn map_without_the_field() -> wattetheria_kernel::space::SpaceCatalog {
    use sha2::Digest as _;
    let mut package = test_space_package();
    package
        .places
        .retain(|place| !place.place_id.starts_with("public-field"));
    let mut value = serde_json::to_value(&package).unwrap();
    value.as_object_mut().unwrap().remove("space_version");
    package.space_version = format!(
        "sha256:{:x}",
        sha2::Sha256::digest(serde_jcs::to_vec(&value).unwrap())
    );
    package.validate().unwrap();
    test_space_catalog(&package)
}

#[tokio::test]
async fn mcp_lists_the_farm_tools_with_precise_schemas() {
    let (_dir, app, token, _policy, _state) = build_test_app(100);
    let response = mcp_request(
        app,
        &token,
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
    )
    .await;
    let tools = response["result"]["tools"].as_array().unwrap();

    let info = find_tool(tools, "game_info");
    assert_eq!(
        info["_meta"]["wattetheria"]["path"],
        "/v1/wattetheria/games/{game_id}"
    );
    assert_schema_requires(info, &["game_id"]);
    assert_eq!(
        find_tool(tools, "farm_status")["_meta"]["wattetheria"]["readOnly"],
        true
    );

    let claim = find_tool(tools, "farm_claim_plots");
    assert_schema_requires(claim, &["action_id", "parcel"]);
    // Which parcels exist is the gateway's layout, read from game_info, not the node's.
    let parcel = &claim["inputSchema"]["properties"]["parcel"];
    assert_eq!(parcel["type"], "integer", "{parcel}");
    assert!(
        parcel["description"]
            .as_str()
            .unwrap()
            .contains("places.parcels")
    );

    let plant = find_tool(tools, "farm_plant");
    assert_schema_requires(plant, &["action_id", "crop", "tile"]);
    assert_eq!(
        plant["inputSchema"]["properties"]["crop"]["enum"],
        json!(["barley", "coffee_bean"])
    );
    assert_eq!(plant["inputSchema"]["additionalProperties"], false);
    assert_schema_requires(find_tool(tools, "farm_water"), &["action_id", "tile"]);
    assert_schema_requires(find_tool(tools, "farm_harvest"), &["action_id", "tile"]);
    assert_schema_requires(find_tool(tools, "farm_steal"), &["action_id", "tile"]);
    assert_schema_requires(
        find_tool(tools, "farm_sell"),
        &["action_id", "crop", "units"],
    );
    let season = find_tool(tools, "farm_season");
    assert_schema_requires(season, &["season_no"]);
    assert_eq!(season["_meta"]["wattetheria"]["readOnly"], true);
}

#[tokio::test]
async fn farm_season_relays_a_finished_seasons_revealed_rules() {
    let (gateway_url, _mock) = spawn_mock_gateway().await;
    let (_dir, _app, token, _policy, state) = build_test_app(100);
    let app = app(with_gateway(state, gateway_url));

    let season = content(&call(app, &token, "farm_season", json!({"season_no": 3})).await);
    assert_eq!(season["season_no"], 3, "{season}");
    assert_eq!(season["params"]["disabled"], json!(["well"]));
    assert_eq!(season["seed"], "00");
}

#[tokio::test]
async fn game_info_adds_the_tools_and_marks_a_map_without_the_field_as_stale() {
    let (gateway_url, _mock) = spawn_mock_gateway().await;
    let (_dir, _app, token, _policy, state) = build_test_app(100);
    let old_map = ControlPlaneState {
        gateway_urls: vec![gateway_url.clone()],
        space_catalog: Arc::new(crate::SpaceMaps::pinned(Some(map_without_the_field()))),
        ..state.clone()
    };

    let games = call(app(old_map.clone()), &token, "list_games", json!({})).await;
    assert_eq!(content(&games)["games"][0]["game_id"], "farm", "{games}");

    let manual = content(
        &call(
            app(old_map.clone()),
            &token,
            "game_info",
            json!({"game_id": "farm"}),
        )
        .await,
    );
    assert_eq!(manual["asked_for"], old_map.agent_did);
    assert_eq!(manual["tools"][0]["tool"], "farm_status");
    assert_eq!(manual["availability"]["can_join"], false);
    assert_eq!(manual["availability"]["reason"], "stale_client");

    let refused = call(
        app(old_map),
        &token,
        "farm_claim_plots",
        json!({"action_id": "c1", "parcel": 1}),
    )
    .await;
    assert_eq!(refused["result"]["isError"], true, "{refused}");
    assert_eq!(content(&refused)["error"], "stale_client");

    let ready = with_gateway(state, gateway_url);
    let manual = content(&call(app(ready), &token, "game_info", json!({"game_id": "farm"})).await);
    assert_eq!(manual["availability"]["can_join"], true, "{manual}");
}

#[tokio::test]
async fn farm_commands_are_signed_with_the_presence_and_block_walking_while_busy() {
    let (gateway_url, mock) = spawn_mock_gateway().await;
    let (_dir, _app, token, _policy, state) = build_test_app(100);
    let state = with_gateway(state, gateway_url);
    let app = app(state.clone());

    let outside = call(
        app.clone(),
        &token,
        "farm_plant",
        json!({"action_id": "p0", "tile": [60, 90], "crop": "barley"}),
    )
    .await;
    assert_eq!(content(&outside)["error"], "not_in_space", "{outside}");
    assert!(mock.actions.lock().await.is_empty());

    let entered = call(app.clone(), &token, "enter_space", json!({})).await;
    assert_eq!(entered["result"]["isError"], false, "{entered}");

    let status = content(&call(app.clone(), &token, "farm_status", json!({})).await);
    assert_eq!(status["map_ready"], true);
    assert!(status["next"]["walk_secs"].as_f64().is_some(), "{status}");

    for bad in [
        json!({"tile": [60, 90], "crop": "barley"}),
        json!({"action_id": "has space", "tile": [60, 90], "crop": "barley"}),
    ] {
        let refused = call(app.clone(), &token, "farm_plant", bad).await;
        assert_eq!(content(&refused)["error"], "invalid", "{refused}");
    }
    let unknown_crop = call(
        app.clone(),
        &token,
        "farm_plant",
        json!({"action_id": "p1", "tile": [60, 90], "crop": "rice"}),
    )
    .await;
    assert_eq!(content(&unknown_crop)["error"], "invalid");

    let planted = call(
        app.clone(),
        &token,
        "farm_plant",
        json!({"action_id": "p2", "tile": [60, 90], "crop": "coffee_bean"}),
    )
    .await;
    assert_eq!(planted["result"]["isError"], false, "{planted}");
    assert_eq!(content(&planted)["action_id"], "p2");
    let sent = mock.actions.lock().await.last().cloned().unwrap();
    assert!(sent["signature"].as_str().is_some_and(|s| !s.is_empty()));
    assert_eq!(sent["public_key"], state.identity.public_key);
    let payload = &sent["payload"];
    assert_eq!(payload["action"], "plant");
    assert_eq!(payload["action_id"], "p2");
    assert_eq!(payload["agent_did"], state.agent_did);
    assert_eq!(payload["crop"], "coffee_bean");
    assert_eq!(payload["tile"], json!([60, 90]));
    assert_eq!(payload["presence"]["record"]["agent_did"], state.agent_did);

    let walk = call(app.clone(), &token, "move_agent", json!({"tile": [60, 91]})).await;
    assert_eq!(walk["result"]["isError"], true, "{walk}");
    assert_eq!(content(&walk)["error"], "busy");

    // A refusal keeps the gateway's code and details.
    let refused = call(
        app.clone(),
        &token,
        "farm_water",
        json!({"action_id": "w1", "tile": [61, 90]}),
    )
    .await;
    assert_eq!(refused["result"]["_meta"]["httpStatus"], 409, "{refused}");
    let refusal = content(&refused);
    assert_eq!(refusal["error"], "not_at_tile");
    assert_eq!(refusal["details"]["tile"], json!([61, 90]));
    assert_eq!(refusal["action_id"], "w1");

    refusals_do_not_hold_but_lost_replies_do(app, &token, state).await;
}

/// A refused command frees the agent; an accepted one whose reply is lost keeps it held, across a restart too.
async fn refusals_do_not_hold_but_lost_replies_do(
    app: Router,
    token: &str,
    state: ControlPlaneState,
) {
    state.busy.set(&state.local_db, 0);
    let refused = call(
        app.clone(),
        token,
        "farm_water",
        json!({"action_id": "w2", "tile": [61, 90]}),
    )
    .await;
    assert_eq!(content(&refused)["error"], "not_at_tile");
    let walk = call(
        app.clone(),
        token,
        "move_agent",
        json!({"place_id": "public-field/farmland"}),
    )
    .await;
    assert_eq!(walk["result"]["isError"], false, "{walk}");

    let lost = call(
        app.clone(),
        token,
        "farm_harvest",
        json!({"action_id": "lost", "tile": [60, 90]}),
    )
    .await;
    assert_eq!(lost["result"]["isError"], true, "{lost}");
    assert_eq!(content(&lost)["error"], "outcome_unknown", "{lost}");
    // Neither the reply nor the gateway's status said how long: the catalog's 12 s hold it.
    let held = content(&call(app.clone(), token, "move_agent", json!({"tile": [60, 91]})).await);
    assert_eq!(held["error"], "busy", "{held}");
    assert!(
        held["details"]["remaining_secs"].as_i64().unwrap() > 6,
        "held for the gateway's 12 s, not the node's default 6: {held}"
    );
    let restarted = ControlPlaneState {
        busy: Arc::default(),
        farm_catalog: Arc::default(),
        ..state
    };
    let walk = call(
        crate::app(restarted),
        token,
        "move_agent",
        json!({"tile": [60, 91]}),
    )
    .await;
    assert_eq!(content(&walk)["error"], "busy", "{walk}");
}

#[tokio::test]
async fn any_server_error_leaves_the_outcome_unknown_whatever_its_body_says() {
    let (gateway_url, _mock) = spawn_mock_gateway().await;
    let (_dir, _app, token, _policy, state) = build_test_app(100);
    let app = app(with_gateway(state.clone(), gateway_url));
    let entered = call(app.clone(), &token, "enter_space", json!({})).await;
    assert_eq!(entered["result"]["isError"], false, "{entered}");
    // A proxy's 504 and the gateway's own 500.
    for action_id in ["proxy", "commit"] {
        state.busy.set(&state.local_db, 0);
        let harvest = call(
            app.clone(),
            &token,
            "farm_harvest",
            json!({"action_id": action_id, "tile": [60, 90]}),
        )
        .await;
        assert_eq!(content(&harvest)["error"], "outcome_unknown", "{harvest}");
        let walk = call(app.clone(), &token, "move_agent", json!({"tile": [60, 91]})).await;
        assert_eq!(content(&walk)["error"], "busy", "{walk}");
    }
}

#[tokio::test]
async fn refusals_for_standing_elsewhere_say_how_to_walk_there() {
    let (gateway_url, _mock) = spawn_mock_gateway().await;
    let (_dir, _app, token, _policy, state) = build_test_app(100);
    let app = app(with_gateway(state, gateway_url));
    let entered = call(app.clone(), &token, "enter_space", json!({})).await;
    assert_eq!(entered["result"]["isError"], false, "{entered}");

    let off_plot = content(
        &call(
            app.clone(),
            &token,
            "farm_water",
            json!({"action_id": "w0", "tile": [61, 90]}),
        )
        .await,
    );
    assert_eq!(off_plot["error"], "not_at_tile");
    let details = &off_plot["details"];
    assert_eq!(details["to_tile"], json!([61, 90]), "{off_plot}");
    assert_eq!(details["move_agent"], json!({"tile": [61, 90]}));
    assert!(details["walk_secs"].as_f64().is_some_and(|secs| secs > 0.0));

    let away = content(
        &call(
            app,
            &token,
            "farm_sell",
            json!({"action_id": "s0", "crop": "barley", "units": 1}),
        )
        .await,
    );
    assert_eq!(away["error"], "not_at_place");
    let details = &away["details"];
    assert_eq!(
        details["move_agent"],
        json!({"place_id": "the-willows-market-and-pharmacy"}),
        "{away}"
    );
    assert!(
        details["to_tile"]
            .as_array()
            .is_some_and(|tile| tile.len() == 2)
    );
    assert!(details["walk_secs"].as_f64().is_some());
}

#[tokio::test]
async fn a_lost_reply_holds_the_agent_as_long_as_the_gateway_says() {
    let (gateway_url, _mock) = spawn_mock_gateway().await;
    let (_dir, _app, token, _policy, state) = build_test_app(100);
    let app = app(with_gateway(state, gateway_url));
    let entered = call(app.clone(), &token, "enter_space", json!({})).await;
    assert_eq!(entered["result"]["isError"], false, "{entered}");
    let late = call(
        app.clone(),
        &token,
        "farm_harvest",
        json!({"action_id": "late", "tile": [60, 90]}),
    )
    .await;
    assert_eq!(late["result"]["isError"], true, "{late}");
    let late = content(&late);
    assert_eq!(late["error"], "outcome_unknown", "{late}");
    assert!(late["message"].as_str().unwrap().contains("late"), "{late}");
    let walk = content(&call(app, &token, "move_agent", json!({"tile": [60, 91]})).await);
    assert_eq!(walk["error"], "busy", "{walk}");
    assert!(
        walk["details"]["remaining_secs"].as_i64().unwrap() > 15,
        "held for the gateway's 20 s, not the node's assumed 6: {walk}"
    );
}

#[tokio::test]
async fn after_a_steal_the_node_walks_the_thief_off_the_field_once_the_steal_is_done() {
    let (gateway_url, mock) = spawn_mock_gateway().await;
    let (_dir, _app, token, _policy, state) = build_test_app(100);
    let app = app(with_gateway(state, gateway_url));
    let entered = call(app.clone(), &token, "enter_space", json!({})).await;
    assert_eq!(entered["result"]["isError"], false, "{entered}");
    let before = content(&call(app.clone(), &token, "get_agent_presence", json!({})).await);
    let standing = before["presence"]["record"]["from_tile"].clone();

    let stolen = call(
        app.clone(),
        &token,
        "farm_steal",
        json!({"action_id": "st1", "tile": [60, 90]}),
    )
    .await;
    assert_eq!(stolen["result"]["isError"], false, "{stolen}");
    let stolen = content(&stolen);
    let sent = mock.actions.lock().await.last().cloned().unwrap();
    assert_eq!(sent["payload"]["action"], "steal");
    assert_eq!(sent["payload"]["tile"], json!([60, 90]));

    // The walk leaves when the steal is done, from where the thief stands, to the escape tile.
    let busy_until = chrono::DateTime::parse_from_rfc3339(stolen["busy_until"].as_str().unwrap())
        .unwrap()
        .timestamp_millis();
    let after = content(&call(app.clone(), &token, "get_agent_presence", json!({})).await);
    let record = &after["presence"]["record"];
    assert_eq!(record["depart_at"], busy_until, "{after}");
    assert_eq!(record["from_tile"], standing, "{after}");
    assert!(record["seq"].as_u64() > before["presence"]["record"]["seq"].as_u64());
    let arrives =
        chrono::DateTime::parse_from_rfc3339(stolen["sneaks_off_until"].as_str().unwrap())
            .unwrap()
            .timestamp_millis();
    assert!(arrives >= busy_until, "{stolen}");

    // Held until it is off the field.
    let walk = content(&call(app, &token, "move_agent", json!({"tile": [60, 91]})).await);
    assert_eq!(walk["error"], "busy", "{walk}");
    assert!(
        walk["details"]["remaining_secs"].as_i64().unwrap() >= 19,
        "held through the steal and the walk: {walk}"
    );
}
