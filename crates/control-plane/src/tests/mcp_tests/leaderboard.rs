use super::*;
use axum::extract::{Query, State as AxumState};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

/// A gateway answering `/api/leaderboard` with `reply`, recording each `limit` it was asked for.
async fn spawn_gateway(
    status: StatusCode,
    reply: Value,
) -> (String, Arc<Mutex<Vec<Option<String>>>>) {
    let limits = Arc::new(Mutex::new(Vec::new()));
    let router = axum::Router::new()
        .route(
            "/api/leaderboard",
            axum::routing::get(
                move |AxumState(limits): AxumState<Arc<Mutex<Vec<Option<String>>>>>,
                      Query(query): Query<HashMap<String, String>>| {
                    let reply = reply.clone();
                    async move {
                        limits.lock().await.push(query.get("limit").cloned());
                        (status, Json(reply))
                    }
                },
            ),
        )
        .with_state(limits.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    (url, limits)
}

async fn call_leaderboard(state: ControlPlaneState, token: &str, arguments: Value) -> Value {
    let response = mcp_request(
        app(state),
        token,
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": "leaderboard", "arguments": arguments}}),
    )
    .await;
    response["result"].clone()
}

#[tokio::test]
async fn leaderboard_tool_is_read_only_and_takes_only_a_limit() {
    let (_dir, app, token, _policy, _state) = build_test_app(100);
    let response = mcp_request(
        app,
        &token,
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
    )
    .await;
    let tools = response["result"]["tools"].as_array().unwrap();
    let tool = find_tool(tools, "leaderboard");
    assert_eq!(tool["_meta"]["wattetheria"]["method"], "GET");
    assert_eq!(
        tool["_meta"]["wattetheria"]["path"],
        "/v1/wattetheria/leaderboard"
    );
    assert_eq!(tool["_meta"]["wattetheria"]["readOnly"], true);
    let schema = &tool["inputSchema"];
    assert_eq!(schema["additionalProperties"], false);
    assert_eq!(schema["properties"].as_object().unwrap().len(), 1);
    assert_eq!(schema["properties"]["limit"]["minimum"], 1);
    assert_eq!(schema["properties"]["limit"]["maximum"], 50);
    assert_schema_requires(tool, &[]);
}

#[tokio::test]
async fn leaderboard_tool_relays_the_gateways_rows() {
    let rows = json!([
        {"rank": 1, "agent_did": "did:key:top", "display_name": "Top", "xp": 500, "level": 3,
         "level_progress_pct": 20, "watt_balance": 7, "tasks_completed": 2},
        {"rank": 2, "agent_did": "did:key:next", "display_name": null, "xp": 0, "level": 1,
         "level_progress_pct": 0, "watt_balance": 0, "tasks_completed": 0},
    ]);
    let (url, limits) = spawn_gateway(StatusCode::OK, rows.clone()).await;
    let (_dir, _app, token, _policy, state) = build_test_app(100);
    let state = ControlPlaneState {
        gateway_urls: vec![url],
        ..state
    };

    let result = call_leaderboard(state.clone(), &token, json!({"limit": 2})).await;
    assert_eq!(result["isError"], false, "{result}");
    assert_eq!(result["structuredContent"]["items"], rows);

    // No limit takes the default; one out of range is brought back in.
    call_leaderboard(state.clone(), &token, json!({})).await;
    call_leaderboard(state.clone(), &token, json!({"limit": 500})).await;
    assert_eq!(
        *limits.lock().await,
        [
            Some("2".to_string()),
            Some("20".to_string()),
            Some("50".to_string())
        ]
    );
}

#[tokio::test]
async fn leaderboard_tool_reports_a_gateway_refusal_and_a_missing_gateway() {
    let (url, _limits) = spawn_gateway(
        StatusCode::INTERNAL_SERVER_ERROR,
        json!({"error": "database is down"}),
    )
    .await;
    let (_dir, _app, token, _policy, state) = build_test_app(100);
    let refused = call_leaderboard(
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

    let missing = call_leaderboard(
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
