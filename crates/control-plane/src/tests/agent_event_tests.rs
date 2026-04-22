use super::*;
use axum::http::Request;

#[tokio::test]
async fn agent_events_route_translates_openai_compatible_reply_into_structured_decision() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind listener");
    let addr = listener.local_addr().expect("listener addr");
    let app_mock = Router::new().route(
        "/v1/chat/completions",
        post(|| async move {
            Json(json!({
                "choices": [{
                    "message": {
                        "content": "{\"action\":\"reply\",\"reason\":\"respond politely\",\"payload\":{\"content\":\"hello back\"}}"
                    }
                }]
            }))
        }),
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, app_mock).await.expect("serve mock");
    });

    let (_dir, _router, _token, _policy_engine, state) = build_test_app(20);
    let base_url = format!("http://{addr}/v1");
    let state = ControlPlaneState {
        brain_engine: Arc::new(BrainEngine::from_config(
            &BrainProviderConfig::OpenaiCompatible {
                base_url: base_url.clone(),
                model: "openclaw".to_owned(),
                api_key_env: None,
            },
        )),
        brain_provider_label: format!("openai-compatible model=openclaw url={base_url}"),
        ..state
    };
    let app = app(state);

    let response = request_json(
        app,
        Request::post("/agent-events")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(
                json!({
                    "event": {
                        "event_id": "evt-1",
                        "event_type": "dm_received",
                        "source_kind": "social",
                        "source_node_id": null,
                        "target_agent_id": null,
                        "target_executor": "core-agent",
                        "payload": {
                            "agent_envelope": {
                                "message": {
                                    "source_public_id": "peer-alpha",
                                    "target_public_id": "self-alpha",
                                    "content": "hello"
                                }
                            }
                        },
                        "requires_commit": true,
                        "allowed_actions": ["reply", "ignore"],
                        "correlation_id": "thread-1",
                        "dedupe_key": "dm:thread-1",
                        "created_at": 1
                    }
                })
                .to_string(),
            ))
            .expect("request"),
    )
    .await;

    assert_eq!(response["ok"].as_bool(), Some(true));
    assert_eq!(response["decision"]["action"].as_str(), Some("reply"));
    assert_eq!(
        response["decision"]["route"].as_str(),
        Some("wattetheria_commit")
    );
    assert_eq!(
        response["decision"]["payload"]["content"].as_str(),
        Some("hello back")
    );

    server.abort();
}
