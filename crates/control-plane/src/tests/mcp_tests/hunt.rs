use super::*;
use axum::extract::{Path as AxumPath, Query as AxumQuery, State as AxumState};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Every signed hunt command and every log query the mock gateway received.
#[derive(Clone, Default)]
struct MockHunt {
    actions: Arc<Mutex<Vec<Value>>>,
    queries: Arc<Mutex<Vec<HashMap<String, String>>>>,
}

/// mossblob-1 and the four tiles beside it on the map the node walks.
const BLOB: [u32; 2] = [121, 102];
const AROUND: [[u32; 2]; 4] = [[121, 101], [122, 102], [121, 103], [120, 102]];

fn later(secs: i64) -> String {
    (chrono::Utc::now() + chrono::Duration::seconds(secs))
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

/// A gateway with the hunt manual, status, area, log, bag and action endpoints, plus the
/// presence recovery lookup `enter_space` needs.
async fn spawn_mock_gateway() -> (String, MockHunt) {
    let mock = MockHunt::default();
    let router = axum::Router::new()
        .route(
            "/api/spaces/presence/{did}/record",
            axum::routing::get(|| async { axum::http::StatusCode::NOT_FOUND }),
        )
        .route(
            "/api/games/{game_id}",
            axum::routing::get(|AxumPath(game_id): AxumPath<String>| async move {
                Json(json!({"game_id": game_id, "space_id": "the-ville", "rules": {}}))
            }),
        )
        .route(
            "/api/hunt/status",
            axum::routing::post(|Json(body): Json<Value>| async move {
                let did = own_read(&body);
                Json(json!({
                    "agent_did": did,
                    "hp": 150,
                    "smithy_place_id": "smithy",
                    "market_place_id": "the-willows-market-and-pharmacy",
                    "busy_until": later(20),
                }))
            }),
        )
        .route(
            "/api/hunt/area",
            axum::routing::post(|Json(body): Json<Value>| async move {
                own_read(&body);
                Json(json!({
                    "spawns": [
                        {"spawn_id": "mossblob-1", "status": "alive", "tile": BLOB, "around": AROUND},
                        {"spawn_id": "cliff_wraith-4", "status": "closed", "tile": [170, 120],
                         "around": [[170, 119]]},
                    ],
                }))
            }),
        )
        .route(
            "/api/hunt/fights",
            axum::routing::get(
                |AxumState(mock): AxumState<MockHunt>,
                 AxumQuery(query): AxumQuery<HashMap<String, String>>| async move {
                    mock.queries.lock().await.push(query);
                    Json(json!({"fights": []}))
                },
            ),
        )
        .route(
            "/api/agents/{agent_did}/bag",
            axum::routing::get(|AxumPath(did): AxumPath<String>| async move {
                Json(json!({"agent_did": did, "farm": [], "housing": [], "hunt": {"items": [], "weapons": []}}))
            }),
        )
        .route(
            "/api/hunt/actions",
            axum::routing::post(
                |AxumState(mock): AxumState<MockHunt>, Json(body): Json<Value>| async move {
                    mock.actions.lock().await.push(body.clone());
                    let payload = &body["payload"];
                    if payload["action_id"] == "far" {
                        return (
                            axum::http::StatusCode::CONFLICT,
                            Json(json!({"error": "not_beside", "message": "stand beside it",
                                        "details": {"tile": BLOB, "around": AROUND}})),
                        );
                    }
                    if payload["action"] == "smithy_buy" {
                        return (
                            axum::http::StatusCode::CONFLICT,
                            Json(json!({"error": "not_at_place", "message": "walk into smithy",
                                        "details": {"place_id": "smithy"}})),
                        );
                    }
                    (
                        axum::http::StatusCode::OK,
                        Json(json!({"action": payload["action"], "outcome": "win",
                                    "busy_until": later(10)})),
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

/// The agent a signed read is for; panics unless the node signed it with a fresh time.
fn own_read(body: &Value) -> Value {
    assert!(
        body["signature"].as_str().is_some_and(|s| !s.is_empty()),
        "{body}"
    );
    assert!(body["public_key"].as_str().is_some(), "{body}");
    assert!(body["payload"]["issued_at"].as_str().is_some(), "{body}");
    body["payload"]["agent_did"].clone()
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

#[tokio::test]
async fn mcp_lists_the_hunt_tools_with_precise_schemas() {
    let (_dir, app, token, _policy, _state) = build_test_app(100);
    let response = mcp_request(
        app,
        &token,
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
    )
    .await;
    let tools = response["result"]["tools"].as_array().unwrap();
    for read in ["hunt_status", "hunt_area", "hunt_log", "space_bag"] {
        assert_eq!(
            find_tool(tools, read)["_meta"]["wattetheria"]["readOnly"],
            true,
            "{read}"
        );
    }
    let fight = find_tool(tools, "hunt_fight");
    assert_schema_requires(fight, &["action_id", "spawn_id"]);
    assert_eq!(fight["inputSchema"]["additionalProperties"], false);
    // A percent, no more: the lowest and highest retreat line are the gateway's, in the manual.
    let retreat = &fight["inputSchema"]["properties"]["retreat_below"];
    assert_eq!(
        (&retreat["minimum"], &retreat["maximum"]),
        (&json!(1), &json!(100))
    );
    for smithy in ["smithy_buy", "smithy_forge", "smithy_enhance"] {
        assert_schema_requires(find_tool(tools, smithy), &["action_id"]);
    }
    let sell = find_tool(tools, "hunt_sell");
    assert_schema_requires(sell, &["action_id", "count", "gem"]);
    assert_eq!(
        sell["inputSchema"]["properties"]["gem"]["enum"],
        json!(["blessing", "soul", "chaos", "diamond"])
    );
    assert_schema_requires(find_tool(tools, "hunt_buy"), &["action_id", "count"]);
    let equip = find_tool(tools, "hunt_equip");
    assert_schema_requires(equip, &["action_id", "weapon"]);
    assert_eq!(
        equip["inputSchema"]["properties"]["weapon"]["enum"],
        json!([
            "stick",
            "training_sword",
            "copper_sword",
            "iron_sword",
            "steel_sword"
        ])
    );
    // No tool text gives a chance.
    for tool in tools.iter().filter(|tool| {
        tool["name"]
            .as_str()
            .is_some_and(|name| name.starts_with("hunt_"))
    }) {
        assert!(
            !tool["description"].as_str().unwrap().contains('%'),
            "{tool}"
        );
    }
    // A game tool says what it does, where to stand, what it takes and answers; how the game is
    // played is the gateway's to say (game_info), so that a rule changes without a node release.
    for tool in tools.iter().filter(|tool| {
        tool["name"].as_str().is_some_and(|name| {
            ["farm_", "housing_", "hunt_", "smithy_"]
                .iter()
                .any(|game| name.starts_with(game))
        })
    }) {
        assert!(
            tool["description"].as_str().unwrap().len() <= 900,
            "{} explains too much",
            tool["name"]
        );
    }
}

#[tokio::test]
async fn game_info_adds_the_hunt_tools() {
    let (gateway_url, _mock) = spawn_mock_gateway().await;
    let (_dir, _app, token, _policy, state) = build_test_app(100);
    let manual = content(
        &call(
            app(with_gateway(state, gateway_url)),
            &token,
            "game_info",
            json!({"game_id": "hunt"}),
        )
        .await,
    );
    assert_eq!(manual["tools"][0]["tool"], "hunt_status", "{manual}");
    assert!(
        manual["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["tool"] == "space_bag")
    );
}

/// A node in the Space, wired to the mock gateway.
async fn entered() -> (
    tempfile::TempDir,
    Router,
    String,
    ControlPlaneState,
    MockHunt,
) {
    let (gateway_url, mock) = spawn_mock_gateway().await;
    let (dir, _app, token, _policy, state) = build_test_app(100);
    let state = with_gateway(state, gateway_url);
    let app = app(state.clone());
    let entered = call(app.clone(), &token, "enter_space", json!({})).await;
    assert_eq!(entered["result"]["isError"], false, "{entered}");
    (dir, app, token, state, mock)
}

#[tokio::test]
async fn hunt_commands_are_signed_with_the_presence_and_hold_the_agent() {
    let (gateway_url, mock) = spawn_mock_gateway().await;
    let (_dir, _app, token, _policy, state) = build_test_app(100);
    let outside = call(
        app(with_gateway(state, gateway_url)),
        &token,
        "hunt_fight",
        json!({"action_id": "f0", "spawn_id": "mossblob-1"}),
    )
    .await;
    assert_eq!(content(&outside)["error"], "not_in_space", "{outside}");
    assert!(mock.actions.lock().await.is_empty());

    let (_dir, app, token, state, mock) = entered().await;

    // The area adds the nearest tile beside each standing monster and the walk there.
    let area = content(&call(app.clone(), &token, "hunt_area", json!({})).await);
    let blob = &area["spawns"][0];
    assert!(
        AROUND.iter().any(|tile| json!(tile) == blob["stand_at"]),
        "{area}"
    );
    assert_eq!(blob["move_agent"], json!({"tile": blob["stand_at"]}));
    assert!(area["spawns"][1].get("stand_at").is_none(), "{area}");

    for bad in [
        json!({"spawn_id": "mossblob-1"}),
        json!({"action_id": "has space", "spawn_id": "mossblob-1"}),
        json!({"action_id": "f1"}),
    ] {
        let refused = call(app.clone(), &token, "hunt_fight", bad).await;
        assert_eq!(content(&refused)["error"], "invalid", "{refused}");
    }

    let fought = call(
        app.clone(),
        &token,
        "hunt_fight",
        json!({"action_id": "f2", "spawn_id": "mossblob-1", "retreat_below": 40, "hp_potion_below": 50}),
    )
    .await;
    assert_eq!(fought["result"]["isError"], false, "{fought}");
    assert_eq!(content(&fought)["action_id"], "f2");
    let sent = mock.actions.lock().await.last().cloned().unwrap();
    assert!(sent["signature"].as_str().is_some_and(|s| !s.is_empty()));
    let payload = &sent["payload"];
    assert_eq!(payload["action"], "fight");
    assert_eq!(payload["spawn_id"], "mossblob-1");
    assert_eq!(payload["retreat_below"], 40);
    assert_eq!(payload["hp_potion_below"], 50);
    assert_eq!(payload["presence"]["record"]["agent_did"], state.agent_did);
    let walk = call(
        app.clone(),
        &token,
        "move_agent",
        json!({"tile": AROUND[0]}),
    )
    .await;
    assert_eq!(content(&walk)["error"], "busy", "{walk}");
}

#[tokio::test]
async fn hunt_refusals_say_where_to_stand_and_reads_fill_in_self() {
    let (_dir, app, token, state, mock) = entered().await;

    // Wrong spot: the refusal says where to stand and how to walk there.
    state.busy.set(&state.local_db, 0);
    let far = content(
        &call(
            app.clone(),
            &token,
            "hunt_fight",
            json!({"action_id": "far", "spawn_id": "mossblob-1"}),
        )
        .await,
    );
    assert_eq!(far["error"], "not_beside", "{far}");
    assert!(far["details"]["walk_secs"].as_f64().is_some(), "{far}");
    let away = content(
        &call(
            app.clone(),
            &token,
            "smithy_buy",
            json!({"action_id": "s1"}),
        )
        .await,
    );
    assert_eq!(away["error"], "not_at_place", "{away}");
    assert_eq!(away["details"]["move_agent"], json!({"place_id": "smithy"}));

    let sold = call(
        app.clone(),
        &token,
        "hunt_sell",
        json!({"action_id": "m1", "gem": "diamond", "count": 1}),
    )
    .await;
    assert_eq!(sold["result"]["isError"], false, "{sold}");
    let payload = mock.actions.lock().await.last().cloned().unwrap()["payload"].clone();
    assert_eq!(payload["action"], "sell");
    assert_eq!(payload["gem"], "diamond");

    // Taking a sword in hand is a signed command like the rest.
    state.busy.set(&state.local_db, 0);
    let equipped = call(
        app.clone(),
        &token,
        "hunt_equip",
        json!({"action_id": "w1", "weapon": "copper_sword"}),
    )
    .await;
    assert_eq!(equipped["result"]["isError"], false, "{equipped}");
    let payload = mock.actions.lock().await.last().cloned().unwrap()["payload"].clone();
    assert_eq!(payload["action"], "equip");
    assert_eq!(payload["weapon"], "copper_sword");

    // The log reads anyone's fights; self is this agent.
    state.busy.set(&state.local_db, 0);
    let log = call(
        app.clone(),
        &token,
        "hunt_log",
        json!({"agent_did": "self", "monster": "mossblob"}),
    )
    .await;
    assert_eq!(log["result"]["isError"], false, "{log}");
    let query = mock.queries.lock().await.last().cloned().unwrap();
    assert_eq!(query["agent_did"], state.agent_did);
    assert_eq!(query["monster"], "mossblob");

    let status = content(&call(app.clone(), &token, "hunt_status", json!({})).await);
    assert!(
        status["walk_to"]["smithy"]["walk_secs"].as_f64().is_some(),
        "{status}"
    );
    let bag = content(&call(app, &token, "space_bag", json!({})).await);
    assert_eq!(bag["agent_did"], state.agent_did);
}
