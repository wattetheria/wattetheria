use super::*;
use axum::extract::{Path as AxumPath, State as AxumState};
use std::sync::Arc;
use tokio::sync::Mutex;

/// Every signed housing command the mock gateway received.
#[derive(Clone, Default)]
struct MockHousing {
    actions: Arc<Mutex<Vec<Value>>>,
}

/// Lot 7's gate on the map the node walks.
const GATE: [u32; 2] = [146, 72];

fn later(secs: i64) -> String {
    (chrono::Utc::now() + chrono::Duration::seconds(secs))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// A gateway with the housing manual, one agent's status and the action endpoint, plus the
/// presence recovery lookup `enter_space` needs.
async fn spawn_mock_gateway() -> (String, MockHousing) {
    let mock = MockHousing::default();
    let router = axum::Router::new()
        .route(
            "/api/spaces/presence/{did}/record",
            axum::routing::get(|| async { axum::http::StatusCode::NOT_FOUND }),
        )
        .route(
            "/api/games/{game_id}",
            axum::routing::get(|AxumPath(game_id): AxumPath<String>| async move {
                Json(json!({
                    "game_id": game_id,
                    "space_id": "the-ville",
                    "district_place_id": "residential-district",
                    "availability": {"lots": 9, "free_lots": 9},
                }))
            }),
        )
        .route(
            "/api/housing/agents/{did}",
            axum::routing::get(|AxumPath(did): AxumPath<String>| async move {
                Json(json!({
                    "agent_did": did,
                    "space_id": "the-ville",
                    "district_place_id": "residential-district",
                    "lots": [],
                    "free_lots": [{"lot": 7, "tier": "small", "gate": GATE}],
                    "next": {"do": "buy_lot"},
                    "busy_until": later(20),
                }))
            }),
        )
        .route(
            "/api/housing/actions",
            axum::routing::post(
                |AxumState(mock): AxumState<MockHousing>, Json(body): Json<Value>| async move {
                    mock.actions.lock().await.push(body.clone());
                    let payload = &body["payload"];
                    if payload["action_id"] == "lost" {
                        return (axum::http::StatusCode::OK, Json(Value::Null));
                    }
                    if payload["action"] == "buy_lot" {
                        return (
                            axum::http::StatusCode::CONFLICT,
                            Json(
                                json!({"error": "not_at_gate", "message": "stand at the gate",
                                        "details": {"gate": GATE, "standing": [0, 0]}}),
                            ),
                        );
                    }
                    if payload["action"] == "enter" {
                        return (
                            axum::http::StatusCode::OK,
                            Json(json!({"action": "enter", "lot": payload["lot"],
                                        "cell": [6, 2], "busy_until": later(0)})),
                        );
                    }
                    if payload["action"] == "remove" {
                        return (
                            axum::http::StatusCode::CONFLICT,
                            Json(
                                json!({"error": "not_at_gate", "message": "stand at your gate",
                                        "details": {"gates": [GATE], "standing": [0, 0]}}),
                            ),
                        );
                    }
                    (
                        axum::http::StatusCode::OK,
                        Json(json!({"action": payload["action"], "lot": 7,
                                    "busy_until": later(6)})),
                    )
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

/// The map before the residential district was added: every district place removed, re-hashed
/// so it validates as a distinct version.
fn map_without_the_district() -> wattetheria_kernel::space::SpaceCatalog {
    use sha2::Digest as _;
    let mut package = test_space_package();
    package
        .places
        .retain(|place| !place.place_id.starts_with("residential-district"));
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
async fn mcp_lists_the_housing_tools_with_precise_schemas() {
    let (_dir, app, token, _policy, _state) = build_test_app(100);
    let response = mcp_request(
        app,
        &token,
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
    )
    .await;
    let tools = response["result"]["tools"].as_array().unwrap();
    assert_eq!(
        find_tool(tools, "housing_status")["_meta"]["wattetheria"]["readOnly"],
        true
    );
    assert_schema_requires(find_tool(tools, "housing_buy_lot"), &["action_id", "lot"]);
    let place = find_tool(tools, "housing_place");
    assert_schema_requires(place, &["action_id", "cell", "element"]);
    assert_eq!(place["inputSchema"]["additionalProperties"], false);
    assert_eq!(place["inputSchema"]["properties"]["cell"]["minItems"], 2);
    assert_eq!(
        place["inputSchema"]["properties"]["facing"]["enum"],
        json!(["down", "left", "right", "up"])
    );
    assert_schema_requires(find_tool(tools, "housing_remove"), &["action_id", "cell"]);
    assert_schema_requires(find_tool(tools, "housing_name"), &["action_id", "name"]);
    let enter = find_tool(tools, "housing_enter");
    assert_schema_requires(enter, &["action_id", "lot"]);
    assert_eq!(enter["inputSchema"]["properties"]["cell"]["maxItems"], 2);
    assert_eq!(enter["inputSchema"]["additionalProperties"], false);
}

#[tokio::test]
async fn game_info_adds_the_housing_tools_and_marks_a_map_without_the_district_as_stale() {
    let (gateway_url, _mock) = spawn_mock_gateway().await;
    let (_dir, _app, token, _policy, state) = build_test_app(100);
    let ready = with_gateway(state.clone(), gateway_url.clone());
    let manual = content(
        &call(
            app(ready),
            &token,
            "game_info",
            json!({"game_id": "housing"}),
        )
        .await,
    );
    assert_eq!(manual["tools"][0]["tool"], "housing_status", "{manual}");
    assert!(manual["availability"].get("reason").is_none(), "{manual}");

    let old_map = ControlPlaneState {
        gateway_urls: vec![gateway_url],
        space_catalog: Arc::new(crate::SpaceMaps::pinned(Some(map_without_the_district()))),
        ..state
    };
    let manual = content(
        &call(
            app(old_map),
            &token,
            "game_info",
            json!({"game_id": "housing"}),
        )
        .await,
    );
    assert_eq!(manual["availability"]["reason"], "stale_client", "{manual}");
}

#[tokio::test]
async fn housing_commands_are_signed_with_the_presence_and_hold_the_agent() {
    let (gateway_url, mock) = spawn_mock_gateway().await;
    let (_dir, _app, token, _policy, state) = build_test_app(100);
    let state = with_gateway(state, gateway_url);
    let app = app(state.clone());

    let outside = call(
        app.clone(),
        &token,
        "housing_place",
        json!({"action_id": "p0", "element": "bed", "cell": [1, 2]}),
    )
    .await;
    assert_eq!(content(&outside)["error"], "not_in_space", "{outside}");
    assert!(mock.actions.lock().await.is_empty());

    let entered = call(app.clone(), &token, "enter_space", json!({})).await;
    assert_eq!(entered["result"]["isError"], false, "{entered}");

    // The status says whether the map has the district and how far each free gate is.
    let status = content(&call(app.clone(), &token, "housing_status", json!({})).await);
    assert_eq!(status["map_ready"], true, "{status}");
    let free = &status["free_lots"][0];
    assert!(
        free["walk_secs"].as_f64().is_some_and(|secs| secs > 0.0),
        "{status}"
    );
    assert_eq!(free["move_agent"], json!({"tile": GATE}));

    for bad in [
        json!({"element": "bed", "cell": [1, 2]}),
        json!({"action_id": "has space", "element": "bed", "cell": [1, 2]}),
        json!({"action_id": "p1", "element": "bed"}),
        json!({"action_id": "p1", "element": "chair", "cell": [1, 2], "facing": "north"}),
    ] {
        let refused = call(app.clone(), &token, "housing_place", bad).await;
        assert_eq!(content(&refused)["error"], "invalid", "{refused}");
    }

    let placed = call(
        app.clone(),
        &token,
        "housing_place",
        json!({"action_id": "p2", "element": "chair", "cell": [1, 2], "facing": "up"}),
    )
    .await;
    assert_eq!(placed["result"]["isError"], false, "{placed}");
    assert_eq!(content(&placed)["action_id"], "p2");
    let sent = mock.actions.lock().await.last().cloned().unwrap();
    assert!(sent["signature"].as_str().is_some_and(|s| !s.is_empty()));
    assert_eq!(sent["public_key"], state.identity.public_key);
    let payload = &sent["payload"];
    assert_eq!(payload["action"], "place");
    assert_eq!(payload["element"], "chair");
    assert_eq!(payload["cell"], json!([1, 2]));
    assert_eq!(payload["facing"], "up");
    assert_eq!(payload["agent_did"], state.agent_did);
    assert_eq!(payload["presence"]["record"]["agent_did"], state.agent_did);
    let walk = call(app.clone(), &token, "move_agent", json!({"tile": GATE})).await;
    assert_eq!(content(&walk)["error"], "busy", "{walk}");

    // Away from the gate: the refusal says how to walk to it, whichever form the gateway sent.
    state.busy.set(&state.local_db, 0);
    for (tool, arguments) in [
        ("housing_buy_lot", json!({"action_id": "b1", "lot": 7})),
        ("housing_remove", json!({"action_id": "r1", "cell": [1, 2]})),
    ] {
        let refused = content(&call(app.clone(), &token, tool, arguments).await);
        assert_eq!(refused["error"], "not_at_gate", "{refused}");
        assert_eq!(refused["details"]["move_agent"], json!({"tile": GATE}));
        assert!(
            refused["details"]["walk_secs"].as_f64().is_some(),
            "{refused}"
        );
    }
    let free_again = call(app.clone(), &token, "move_agent", json!({"tile": GATE})).await;
    assert_eq!(free_again["result"]["isError"], false, "{free_again}");

    // A lost reply holds the agent for what the gateway's status says.
    state.busy.set(&state.local_db, 0);
    let lost = call(
        app.clone(),
        &token,
        "housing_name",
        json!({"action_id": "lost", "name": "Home"}),
    )
    .await;
    assert_eq!(content(&lost)["error"], "outcome_unknown", "{lost}");
    let held = content(&call(app, &token, "move_agent", json!({"tile": GATE})).await);
    assert_eq!(held["error"], "busy", "{held}");
    assert!(
        held["details"]["remaining_secs"].as_i64().unwrap() > 6,
        "held for the gateway's 20 s, not the node's 6: {held}"
    );
}

#[tokio::test]
async fn walking_into_a_lot_names_it_and_does_not_hold_the_agent() {
    let (gateway_url, mock) = spawn_mock_gateway().await;
    let (_dir, _app, token, _policy, state) = build_test_app(100);
    let state = with_gateway(state, gateway_url);
    let app = app(state.clone());
    let entered = call(app.clone(), &token, "enter_space", json!({})).await;
    assert_eq!(entered["result"]["isError"], false, "{entered}");

    // With or without a cell; the gateway picks one when it is left out.
    for (id, arguments, cell) in [
        ("e1", json!({"action_id": "e1", "lot": 7}), Value::Null),
        (
            "e2",
            json!({"action_id": "e2", "lot": 7, "cell": [3, 4]}),
            json!([3, 4]),
        ),
    ] {
        let inside = call(app.clone(), &token, "housing_enter", arguments).await;
        assert_eq!(inside["result"]["isError"], false, "{inside}");
        assert_eq!(content(&inside)["cell"], json!([6, 2]));
        let sent = mock.actions.lock().await.last().cloned().unwrap();
        let payload = &sent["payload"];
        assert_eq!(payload["action_id"], id);
        assert_eq!(payload["action"], "enter");
        assert_eq!(payload["lot"], 7);
        assert_eq!(payload.get("cell").cloned().unwrap_or(Value::Null), cell);
        assert_eq!(payload["presence"]["record"]["agent_did"], state.agent_did);
    }
    let no_lot = call(
        app.clone(),
        &token,
        "housing_enter",
        json!({"action_id": "e3"}),
    )
    .await;
    assert_eq!(content(&no_lot)["error"], "invalid", "{no_lot}");
    let leaves = call(app, &token, "move_agent", json!({"tile": GATE})).await;
    assert_eq!(leaves["result"]["isError"], false, "{leaves}");
}
