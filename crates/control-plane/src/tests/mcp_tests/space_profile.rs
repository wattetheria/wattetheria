use super::*;
use axum::extract::{Path, State as AxumState};
use std::sync::Arc;
use tokio::sync::Mutex;

/// A gateway answering `/api/agents/{did}/growth` with `reply`, recording each DID asked for.
async fn spawn_gateway(status: StatusCode, reply: Value) -> (String, Arc<Mutex<Vec<String>>>) {
    let asked = Arc::new(Mutex::new(Vec::new()));
    let router =
        axum::Router::new()
            .route(
                "/api/agents/{agent_did}/growth",
                axum::routing::get(
                    move |AxumState(asked): AxumState<Arc<Mutex<Vec<String>>>>,
                          Path(agent_did): Path<String>| {
                        let reply = reply.clone();
                        async move {
                            asked.lock().await.push(agent_did);
                            (status, Json(reply))
                        }
                    },
                ),
            )
            .route(
                "/api/agents/{agent_did}/rank",
                axum::routing::get(|| async {
                    Json(json!({"space": {"rank": 12, "xp": 650}, "farm": null,
                                "cards": {"rank": 4, "rating": 1032}}))
                }),
            )
            .route(
                "/api/growth/hints",
                axum::routing::post(
                    |AxumState(asked): AxumState<Arc<Mutex<Vec<String>>>>,
                     Json(body): Json<Value>| async move {
                        // The request names the agent and is signed by it.
                        assert!(body["signature"].is_string() && body["public_key"].is_string());
                        assert!(body["payload"]["issued_at"].is_string());
                        let agent_did = body["payload"]["agent_did"].as_str().unwrap().to_owned();
                        asked.lock().await.push(format!("hints:{agent_did}"));
                        if agent_did.is_empty() {
                            return (StatusCode::UNAUTHORIZED, Json(json!({"error": "no"})));
                        }
                        (
                            StatusCode::OK,
                            Json(json!({"farmer_rank": 3, "jackpot": []})),
                        )
                    },
                ),
            )
            .route(
                "/api/growth/titles",
                axum::routing::post(
                    |AxumState(asked): AxumState<Arc<Mutex<Vec<String>>>>,
                     Json(body): Json<Value>| async move {
                        assert!(body["signature"].is_string() && body["public_key"].is_string());
                        assert!(body["payload"]["issued_at"].is_string());
                        let titles = body["payload"]["titles"].clone();
                        asked.lock().await.push(format!(
                            "titles:{}:{titles}",
                            body["payload"]["agent_did"].as_str().unwrap()
                        ));
                        if titles == json!(["emperor"]) {
                            return (
                                StatusCode::BAD_REQUEST,
                                Json(json!({"error": "there is no title \"emperor\""})),
                            );
                        }
                        (StatusCode::OK, Json(json!({"slots": 1, "worn": titles})))
                    },
                ),
            )
            .with_state(asked.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (url, asked)
}

async fn call_space_profile(state: ControlPlaneState, token: &str, arguments: Value) -> Value {
    let response = mcp_request(
        app(state),
        token,
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": "space_profile", "arguments": arguments}}),
    )
    .await;
    response["result"].clone()
}

#[tokio::test]
async fn space_profile_tool_is_read_only_and_takes_only_an_agent_did() {
    let (_dir, app, token, _policy, _state) = build_test_app(100);
    let response = mcp_request(
        app,
        &token,
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
    )
    .await;
    let tools = response["result"]["tools"].as_array().unwrap();
    let tool = find_tool(tools, "space_profile");
    assert_eq!(tool["_meta"]["wattetheria"]["method"], "GET");
    assert_eq!(
        tool["_meta"]["wattetheria"]["path"],
        "/v1/wattetheria/space/profile"
    );
    assert_eq!(tool["_meta"]["wattetheria"]["readOnly"], true);
    let schema = &tool["inputSchema"];
    assert_eq!(schema["additionalProperties"], false);
    assert_eq!(schema["properties"].as_object().unwrap().len(), 1);
    assert_eq!(schema["properties"]["agent_did"]["type"], "string");
    assert_schema_requires(tool, &[]);
}

#[tokio::test]
async fn space_profile_tool_reads_its_own_profile_or_another_agents() {
    let growth = json!({
        "agent_did": "did:key:someone",
        "level": {"level": 4, "xp": 650},
        "proficiencies": [{"id": "farmer", "points": 700, "rank": 3, "max_rank": 10}],
    });
    let (url, asked) = spawn_gateway(StatusCode::OK, growth.clone()).await;
    let (_dir, _app, token, _policy, state) = build_test_app(100);
    let state = ControlPlaneState {
        gateway_urls: vec![url],
        ..state
    };

    let own = call_space_profile(state.clone(), &token, json!({})).await;
    assert_eq!(own["isError"], false, "{own}");
    assert_eq!(own["structuredContent"]["own"], true);
    assert_eq!(own["structuredContent"]["level"], growth["level"]);
    assert_eq!(
        own["structuredContent"]["proficiencies"],
        growth["proficiencies"]
    );

    // Where it stands on each board comes along, as ranks only.
    assert_eq!(
        own["structuredContent"]["ranking"],
        json!({"space": 12, "farm": null, "hunt": null, "cards": 4, "card_rating": 1032,
               "housing": null})
    );

    // Its own profile carries its season hints, asked for with a signed request.
    assert_eq!(
        own["structuredContent"]["farmer_hints"],
        json!({"farmer_rank": 3, "jackpot": []})
    );

    let other = call_space_profile(
        state.clone(),
        &token,
        json!({"agent_did": " did:key:other "}),
    )
    .await;
    assert_eq!(other["isError"], false, "{other}");
    assert_eq!(other["structuredContent"]["own"], false);
    assert_eq!(other["structuredContent"]["ranking"]["space"], 12);
    // Another agent's hints are never asked for.
    assert!(other["structuredContent"].get("farmer_hints").is_none());

    assert_eq!(
        *asked.lock().await,
        [
            state.agent_did.clone(),
            format!("hints:{}", state.agent_did),
            "did:key:other".to_string()
        ]
    );

    // Something that is not a DID never reaches the gateway.
    let bad = call_space_profile(state.clone(), &token, json!({"agent_did": "Top/../x"})).await;
    assert_eq!(bad["isError"], true, "{bad}");
    assert_eq!(asked.lock().await.len(), 3);
}

#[tokio::test]
async fn space_profile_tool_reports_a_gateway_refusal_and_a_missing_gateway() {
    let (url, _asked) = spawn_gateway(
        StatusCode::INTERNAL_SERVER_ERROR,
        json!({"error": "database is down"}),
    )
    .await;
    let (_dir, _app, token, _policy, state) = build_test_app(100);
    let refused = call_space_profile(
        ControlPlaneState {
            gateway_urls: vec![url],
            ..state.clone()
        },
        &token,
        json!({}),
    )
    .await;
    assert_eq!(refused["isError"], true, "{refused}");
    assert_eq!(refused["structuredContent"]["error"], "database is down");

    let missing = call_space_profile(
        ControlPlaneState {
            gateway_urls: vec![],
            ..state
        },
        &token,
        json!({}),
    )
    .await;
    assert_eq!(missing["isError"], true, "{missing}");
    assert_eq!(missing["structuredContent"]["error"], "gateway_unavailable");
}

async fn call_set_space_titles(state: ControlPlaneState, token: &str, arguments: Value) -> Value {
    let response = mcp_request(
        app(state),
        token,
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": "set_space_titles", "arguments": arguments}}),
    )
    .await;
    response["result"].clone()
}

#[tokio::test]
async fn set_space_titles_tool_sends_the_agents_signed_choice_to_the_gateway() {
    let (url, asked) = spawn_gateway(StatusCode::OK, json!({})).await;
    let (_dir, app, token, _policy, state) = build_test_app(100);
    let response = mcp_request(
        app,
        &token,
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
    )
    .await;
    let tools = response["result"]["tools"].as_array().unwrap();
    let tool = find_tool(tools, "set_space_titles");
    assert_eq!(tool["_meta"]["wattetheria"]["method"], "POST");
    assert_eq!(tool["_meta"]["wattetheria"]["readOnly"], false);
    assert_eq!(tool["inputSchema"]["properties"]["titles"]["type"], "array");
    assert_schema_requires(tool, &["titles"]);

    let state = ControlPlaneState {
        gateway_urls: vec![url],
        ..state
    };
    let worn =
        call_set_space_titles(state.clone(), &token, json!({"titles": ["green_thumb"]})).await;
    assert_eq!(worn["isError"], false, "{worn}");
    assert_eq!(worn["structuredContent"]["worn"], json!(["green_thumb"]));

    // The gateway's refusal comes back with its reason.
    let refused =
        call_set_space_titles(state.clone(), &token, json!({"titles": ["emperor"]})).await;
    assert_eq!(refused["isError"], true, "{refused}");
    assert_eq!(
        refused["structuredContent"]["error"],
        "there is no title \"emperor\""
    );

    assert_eq!(
        *asked.lock().await,
        [
            format!("titles:{}:[\"green_thumb\"]", state.agent_did),
            format!("titles:{}:[\"emperor\"]", state.agent_did),
        ]
    );

    let missing = call_set_space_titles(
        ControlPlaneState {
            gateway_urls: vec![],
            ..state
        },
        &token,
        json!({"titles": []}),
    )
    .await;
    assert_eq!(missing["isError"], true, "{missing}");
    assert_eq!(missing["structuredContent"]["error"], "gateway_unavailable");
}
