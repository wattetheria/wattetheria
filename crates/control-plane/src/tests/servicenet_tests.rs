use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn servicenet_a2a_bridge_reuses_runtime_adapter_with_servicenet_session() {
    let seen_session = Arc::new(std::sync::Mutex::new(None::<String>));
    let seen_session_clone = Arc::clone(&seen_session);
    let runtime_app = Router::new().route(
        "/v1/chat/completions",
        post(move |headers: axum::http::HeaderMap| {
            let seen_session = Arc::clone(&seen_session_clone);
            async move {
                *seen_session.lock().expect("session mutex poisoned") = headers
                    .get("x-hermes-session-id")
                    .and_then(|value| value.to_str().ok())
                    .map(ToOwned::to_owned);
                Json(json!({
                    "choices": [{
                        "message": {
                            "content": "{\"message\":\"bridge response\"}"
                        }
                    }]
                }))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("runtime listener should bind");
    let runtime_addr = listener.local_addr().expect("runtime addr");
    let runtime_server = tokio::spawn(async move {
        axum::serve(listener, runtime_app)
            .await
            .expect("runtime server should run");
    });

    let (_dir, _router, _token, _, state) = build_test_app(20);
    let brain_config = BrainProviderConfig::OpenaiCompatible {
        base_url: format!("http://{runtime_addr}/v1"),
        model: "hermes-agent".to_owned(),
        api_key_env: None,
        runtime_adapter: Some(wattetheria_kernel::brain::AgentRuntimeAdapter::Hermes {
            session_header_name: None,
        }),
    };
    let state = ControlPlaneState {
        brain_engine: Arc::new(tokio::sync::RwLock::new(BrainEngine::from_config(
            &brain_config,
        ))),
        brain_config: Arc::new(tokio::sync::RwLock::new(brain_config)),
        ..state
    };
    let caller_agent_id = state.agent_did.clone();
    let source_node_id = state.swarm_bridge.local_node_id().await.ok();
    let agent_envelope = crate::social_host::build_signed_agent_envelope_for_nodes(
        &state,
        crate::social_host::SignedAgentEnvelopeArgs {
            source_agent_id: caller_agent_id.clone(),
            source_public_id: Some("pub_caller".to_owned()),
            source_display_name: Some("Caller Agent".to_owned()),
            target_agent_id: Some("stripe-agent".to_owned()),
            source_node_id,
            target_node_id: None,
            capability: "servicenet.agents.invoke".to_owned(),
            message: json!({"message": "hello service"}),
            extensions: Some(json!({
                "caller_public_id": "pub_caller"
            })),
        },
    )
    .expect("agent envelope should sign");
    let app = app(state);

    let body = request_json(
        app,
        axum::http::Request::post("/a2a/stripe-agent")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(
                json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "SendMessage",
                    "params": {
                        "message": {
                            "role": "user",
                            "parts": [
                                {"kind": "text", "text": "hello service"}
                            ]
                        },
                        "extensions": {
                            "agent_envelope": agent_envelope
                        }
                    }
                })
                .to_string(),
            ))
            .expect("request should build"),
    )
    .await;
    assert_eq!(
        body["result"]["task"]["status"]["state"].as_str(),
        Some("TASK_STATE_COMPLETED")
    );
    assert_eq!(
        body["result"]["task"]["artifacts"][0]["parts"][0]["text"].as_str(),
        Some("bridge response")
    );
    let expected_session =
        format!("wattetheria:servicenet:{caller_agent_id}:stripe-agent:mainnet:watt-etheria");
    assert_eq!(
        seen_session
            .lock()
            .expect("session mutex poisoned")
            .as_deref(),
        Some(expected_session.as_str())
    );

    runtime_server.abort();
}

#[tokio::test]
async fn servicenet_a2a_bridge_rejects_missing_agent_envelope() {
    let (_dir, _router, _token, _, state) = build_test_app(20);
    let app = app(state);

    let body = request_json(
        app,
        axum::http::Request::post("/a2a/stripe-agent")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(
                json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "SendMessage",
                    "params": {
                        "message": {
                            "role": "user",
                            "parts": [
                                {"kind": "text", "text": "hello service"}
                            ]
                        }
                    }
                })
                .to_string(),
            ))
            .expect("request should build"),
    )
    .await;
    assert_eq!(body["error"]["code"].as_i64(), Some(-32602));
    assert_eq!(
        body["error"]["message"].as_str(),
        Some("A2A agent_envelope is required")
    );
}

#[allow(clippy::too_many_lines)]
#[tokio::test]
async fn servicenet_routes_list_agents_and_return_invoke_feedback() {
    let (servicenet_addr, servicenet_server) = spawn_mock_servicenet().await;
    let callback_events = Arc::new(Mutex::new(Vec::<Value>::new()));
    let callback_app = axum::Router::new().route(
        "/agent-events",
        axum::routing::post({
            let callback_events = Arc::clone(&callback_events);
            move |Json(payload): Json<Value>| {
                let callback_events = Arc::clone(&callback_events);
                async move {
                    callback_events.lock().await.push(payload);
                    Json(json!({"ok": true, "acked_at": 1}))
                }
            }
        }),
    );
    let callback_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let callback_addr = callback_listener.local_addr().unwrap();
    let callback_server = tokio::spawn(async move {
        axum::serve(callback_listener, callback_app).await.unwrap();
    });
    let (_dir, _router, token, _, state) = build_test_app(20);
    let state = ControlPlaneState {
        servicenet_client: Some(Arc::new(
            ServiceNetClient::new(format!("http://{servicenet_addr}")).unwrap(),
        )),
        agent_executor_base_url: Some(format!("http://{callback_addr}")),
        agent_event_callback_base_url: Some(format!("http://{callback_addr}")),
        ..state
    };
    let expected_agent_did = state.agent_did.clone();
    let app = app(state);

    let list_json = authed_get_json(app.clone(), &token, "/v1/wattetheria/servicenet/agents").await;
    assert_eq!(list_json["count"].as_u64(), Some(3));
    assert_eq!(
        list_json["items"][0]["agent_id"].as_str(),
        Some("agent-alpha")
    );

    let agent_json = authed_get_json(
        app.clone(),
        &token,
        "/v1/wattetheria/servicenet/agents/agent-alpha",
    )
    .await;
    assert_eq!(agent_json["provider_id"].as_str(), Some("provider-one"));

    let invoke_json = authed_post_json(
        app.clone(),
        &token,
        "/v1/wattetheria/servicenet/agents/agent-alpha/invoke",
        json!({
            "message": "hello servicenet",
            "input": {"amount": 7},
            "settlement": {
                "layer": "web3",
                "rail": "x402",
                "request": {
                    "protocol": "x402",
                    "payment_account_ref": "payment-account-123"
                }
            }
        }),
    )
    .await;
    assert_eq!(invoke_json["status"].as_str(), Some("completed"));
    assert_eq!(
        invoke_json["output"]["echo"].as_str(),
        Some("hello servicenet")
    );
    assert_eq!(
        invoke_json["output"]["agent_envelope_source"].as_str(),
        Some(expected_agent_did.as_str())
    );
    assert!(
        invoke_json["output"]["caller_public_id"]
            .as_str()
            .is_some_and(|value| !value.trim().is_empty())
    );
    assert_eq!(invoke_json["task_id"].as_str(), Some("task-42"));
    assert_eq!(invoke_json["settlement"]["rail"].as_str(), Some("x402"));
    assert_eq!(
        invoke_json["settlement"]["request"]["payment_account_ref"].as_str(),
        Some("payment-account-123")
    );
    assert_eq!(
        invoke_json["payment_receipt"]["status"].as_str(),
        Some("submitted")
    );

    let async_json = authed_post_json(
        app.clone(),
        &token,
        "/v1/wattetheria/servicenet/agents/agent-alpha/invoke-async",
        json!({
            "message": "hello async servicenet"
        }),
    )
    .await;
    assert_eq!(async_json["status"].as_str(), Some("running"));
    assert_eq!(
        async_json["receipt_id"].as_str(),
        Some("00000000-0000-0000-0000-000000000099")
    );

    let receipt_json = authed_get_json(
        app.clone(),
        &token,
        "/v1/wattetheria/servicenet/receipts/00000000-0000-0000-0000-000000000099",
    )
    .await;
    assert_eq!(receipt_json["receipt"]["status"].as_str(), Some("running"));

    let task_json = authed_post_json(
        app,
        &token,
        "/v1/wattetheria/servicenet/agents/agent-alpha/tasks/task-42/get",
        json!({
            "history_length": 5
        }),
    )
    .await;
    assert_eq!(task_json["status"].as_str(), Some("completed"));
    assert_eq!(task_json["task_id"].as_str(), Some("task-42"));
    assert_eq!(task_json["output"]["result"].as_str(), Some("done"));
    assert_eq!(task_json["output"]["history_length"].as_u64(), Some(5));

    let callback_events = callback_events.lock().await;
    assert_eq!(callback_events.len(), 2);
    assert_eq!(
        callback_events[0]["event"]["event_type"].as_str(),
        Some("third_party_result")
    );
    assert_eq!(
        callback_events[0]["event"]["payload"]["operation"].as_str(),
        Some("invoke")
    );
    assert_eq!(
        callback_events[0]["event"]["agent_envelope"]["source_agent_id"].as_str(),
        Some(expected_agent_did.as_str())
    );
    assert_eq!(
        callback_events[0]["event"]["agent_envelope"]["target_agent_id"].as_str(),
        Some("agent-alpha")
    );
    assert!(
        callback_events[0]["event"]["agent_envelope"]["signature"]
            .as_str()
            .is_some_and(|value| !value.trim().is_empty())
    );
    assert_eq!(
        callback_events[1]["event"]["payload"]["operation"].as_str(),
        Some("task_get")
    );
    assert_eq!(
        callback_events[1]["event"]["payload"]["task_id"].as_str(),
        Some("task-42")
    );

    callback_server.abort();
    servicenet_server.abort();
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn servicenet_continue_decision_invokes_agent_again_with_context() {
    let invoke_bodies = Arc::new(Mutex::new(Vec::<Value>::new()));
    let invoke_bodies_for_route = Arc::clone(&invoke_bodies);
    let servicenet_app = Router::new().route(
        "/v1/agents/{agent_id}/invoke",
        post(move |Path(agent_id): Path<String>, Json(body): Json<Value>| {
            let invoke_bodies = Arc::clone(&invoke_bodies_for_route);
            async move {
                let mut bodies = invoke_bodies.lock().await;
                let call_index = bodies.len();
                bodies.push(body.clone());
                let message = if call_index == 0 {
                    "need more detail"
                } else {
                    "completed after follow-up"
                };
                Json(json!({
                    "agent_id": agent_id,
                    "status": "completed",
                    "task_id": "task-42",
                    "context_id": "ctx-1",
                    "message": message,
                    "output": {
                        "round": call_index + 1,
                        "echo": body["message"].clone(),
                        "agent_envelope_source": body["agent_envelope"]["source_agent_id"].clone(),
                    },
                    "raw": {"kind": "invoke", "round": call_index + 1},
                }))
            }
        }),
    );
    let servicenet_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let servicenet_addr = servicenet_listener.local_addr().unwrap();
    let servicenet_server = tokio::spawn(async move {
        axum::serve(servicenet_listener, servicenet_app)
            .await
            .unwrap();
    });

    let callback_events = Arc::new(Mutex::new(Vec::<Value>::new()));
    let callback_events_for_route = Arc::clone(&callback_events);
    let callback_app = Router::new().route(
        "/agent-events",
        post(move |Json(payload): Json<Value>| {
            let callback_events = Arc::clone(&callback_events_for_route);
            async move {
                let mut events = callback_events.lock().await;
                let event_index = events.len();
                events.push(payload);
                if event_index == 0 {
                    Json(json!({
                        "ok": true,
                        "acked_at": 1,
                        "decision": {
                            "decision_id": "decision-continue",
                            "action": "continue",
                            "route": "noop",
                            "reason": "ask the ServiceNet agent for one more detail",
                            "payload": {
                                "message": "follow up from caller",
                                "input": {"requested_detail": "shipping_eta"}
                            }
                        }
                    }))
                } else {
                    Json(json!({"ok": true, "acked_at": 2}))
                }
            }
        }),
    );
    let callback_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let callback_addr = callback_listener.local_addr().unwrap();
    let callback_server = tokio::spawn(async move {
        axum::serve(callback_listener, callback_app).await.unwrap();
    });

    let (_dir, _router, token, _, state) = build_test_app(20);
    let state = ControlPlaneState {
        servicenet_client: Some(Arc::new(
            ServiceNetClient::new(format!("http://{servicenet_addr}")).unwrap(),
        )),
        agent_event_callback_base_url: Some(format!("http://{callback_addr}")),
        ..state
    };
    let expected_agent_did = state.agent_did.clone();
    let app = app(state);

    let invoke_json = authed_post_json(
        app,
        &token,
        "/v1/wattetheria/servicenet/agents/agent-alpha/invoke",
        json!({
            "message": "hello servicenet",
            "input": {"topic": "order"}
        }),
    )
    .await;
    assert_eq!(invoke_json["output"]["round"].as_u64(), Some(1));

    let invoke_bodies = invoke_bodies.lock().await;
    assert_eq!(invoke_bodies.len(), 2);
    assert_eq!(
        invoke_bodies[1]["message"].as_str(),
        Some("follow up from caller")
    );
    assert_eq!(invoke_bodies[1]["task_id"].as_str(), Some("task-42"));
    assert_eq!(invoke_bodies[1]["context_id"].as_str(), Some("ctx-1"));
    assert_eq!(
        invoke_bodies[1]["input"]["requested_detail"].as_str(),
        Some("shipping_eta")
    );
    assert_eq!(
        invoke_bodies[1]["agent_envelope"]["source_agent_id"].as_str(),
        Some(expected_agent_did.as_str())
    );
    assert_eq!(
        invoke_bodies[1]["agent_envelope"]["target_agent_id"].as_str(),
        Some("agent-alpha")
    );
    assert!(
        invoke_bodies[1]["agent_envelope"]["signature"]
            .as_str()
            .is_some_and(|value| !value.trim().is_empty())
    );
    drop(invoke_bodies);

    let callback_events = callback_events.lock().await;
    assert_eq!(callback_events.len(), 2);
    assert_eq!(
        callback_events[0]["event"]["payload"]["operation"].as_str(),
        Some("invoke")
    );
    assert_eq!(
        callback_events[1]["event"]["payload"]["operation"].as_str(),
        Some("continue")
    );
    assert_eq!(
        callback_events[1]["event"]["payload"]["continue_hop"].as_u64(),
        Some(1)
    );

    callback_server.abort();
    servicenet_server.abort();
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn servicenet_async_invocation_is_polled_and_notifies_callback() {
    let task_poll_count = Arc::new(AtomicUsize::new(0));
    let task_poll_count_for_route = Arc::clone(&task_poll_count);
    let servicenet_app = Router::new()
        .route(
            "/v1/agents/{agent_id}/invoke-async",
            post(
                |Path(agent_id): Path<String>, Json(body): Json<Value>| async move {
                    Json(json!({
                        "agent_id": agent_id,
                        "status": "running",
                        "receipt_id": "00000000-0000-0000-0000-000000000077",
                        "task_id": "task-async",
                        "context_id": body["context_id"].as_str().unwrap_or("ctx-async"),
                        "message": "accepted",
                        "raw": {"kind": "invoke_async"},
                    }))
                },
            ),
        )
        .route(
            "/v1/agents/{agent_id}/tasks/{task_id}/get",
            post(
                move |Path((agent_id, task_id)): Path<(String, String)>,
                      Json(_body): Json<Value>| {
                    let task_poll_count = Arc::clone(&task_poll_count_for_route);
                    async move {
                        task_poll_count.fetch_add(1, Ordering::SeqCst);
                        Json(json!({
                            "agent_id": agent_id,
                            "status": "completed",
                            "task_id": task_id,
                            "context_id": "ctx-async",
                            "message": "async completed",
                            "output": {"result": "done async"},
                            "raw": {"kind": "task"},
                        }))
                    }
                },
            ),
        );
    let servicenet_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let servicenet_addr = servicenet_listener.local_addr().unwrap();
    let servicenet_server = tokio::spawn(async move {
        axum::serve(servicenet_listener, servicenet_app)
            .await
            .unwrap();
    });

    let callback_events = Arc::new(Mutex::new(Vec::<Value>::new()));
    let callback_app = Router::new().route(
        "/agent-events",
        post({
            let callback_events = Arc::clone(&callback_events);
            move |Json(payload): Json<Value>| {
                let callback_events = Arc::clone(&callback_events);
                async move {
                    callback_events.lock().await.push(payload);
                    Json(json!({"ok": true, "acked_at": 1}))
                }
            }
        }),
    );
    let callback_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let callback_addr = callback_listener.local_addr().unwrap();
    let callback_server = tokio::spawn(async move {
        axum::serve(callback_listener, callback_app).await.unwrap();
    });

    let (_dir, _router, token, _, state) = build_test_app(20);
    let state = ControlPlaneState {
        servicenet_client: Some(Arc::new(
            ServiceNetClient::new(format!("http://{servicenet_addr}")).unwrap(),
        )),
        agent_event_callback_base_url: Some(format!("http://{callback_addr}")),
        ..state
    };
    let expected_agent_did = state.agent_did.clone();
    let app = app(state.clone());

    let async_json = authed_post_json(
        app,
        &token,
        "/v1/wattetheria/servicenet/agents/agent-alpha/invoke-async",
        json!({
            "message": "start async",
            "context_id": "ctx-async"
        }),
    )
    .await;
    assert_eq!(async_json["status"].as_str(), Some("running"));
    assert_eq!(async_json["task_id"].as_str(), Some("task-async"));

    state
        .social_store
        .defer_reliability_task("servicenet_async_invocation", "task-async", 1, 1, None)
        .expect("defer async poll task to due time");
    let processed = run_reliability_maintenance_tick_once(&state, 10)
        .await
        .expect("run maintenance");
    assert_eq!(processed, 1);
    assert_eq!(task_poll_count.load(Ordering::SeqCst), 1);

    let callback_events = callback_events.lock().await;
    assert_eq!(callback_events.len(), 1);
    assert_eq!(
        callback_events[0]["event"]["payload"]["operation"].as_str(),
        Some("async_result")
    );
    assert_eq!(
        callback_events[0]["event"]["payload"]["task_id"].as_str(),
        Some("task-async")
    );
    assert_eq!(
        callback_events[0]["event"]["payload"]["response"]["output"]["result"].as_str(),
        Some("done async")
    );
    assert_eq!(
        callback_events[0]["event"]["agent_envelope"]["source_agent_id"].as_str(),
        Some(expected_agent_did.as_str())
    );
    assert_eq!(
        callback_events[0]["event"]["agent_envelope"]["target_agent_id"].as_str(),
        Some("agent-alpha")
    );

    callback_server.abort();
    servicenet_server.abort();
}

fn console_agent_publish_body(
    agent_id: Option<&str>,
    provider_id: Option<&str>,
    service_address: Option<&str>,
    version: &str,
    risk_level: &str,
    description: &str,
    supports_task: bool,
) -> Value {
    let mut body = json!({
        "version": version,
        "risk_level": risk_level,
        "agent_card": {
            "name": "Console Agent",
            "description": description,
            "url": "https://console-agent.example.com/a2a",
            "preferredTransport": "JSONRPC",
            "protocolVersion": "1.0",
            "scope": "real_world",
            "origin": "custom_built",
            "domain": "GENERAL",
            "cost": if supports_task { 20 } else { 18 },
            "currency": "USDC",
            "capabilities": {
                "extensions": [
                    {
                        "uri": "https://github.com/google-a2a/a2a-x402/v0.1",
                        "required": false,
                        "params": {
                            "accepts": [
                                {
                                    "scheme": "exact",
                                    "network": "base",
                                    "payTo": "0x0000000000000000000000000000000000000000",
                                    "maxAmountRequired": "0",
                                    "resource": "servicenet:agent:console-agent",
                                    "description": "ServiceNet agent invocation",
                                    "maxTimeoutSeconds": 600
                                }
                            ]
                        }
                    }
                ]
            },
            "supportsTask": supports_task,
            "skills": [{"name": "Lookup", "description": "Looks up records"}],
            "securitySchemes": {"none": {"type": "none"}},
            "security": [{"none": []}]
        }
    });
    if let Some(agent_id) = agent_id {
        body["agent_id"] = json!(agent_id);
    }
    if let Some(provider_id) = provider_id {
        body["provider_id"] = json!(provider_id);
    }
    if let Some(service_address) = service_address {
        body["service_address"] = json!(service_address);
    }
    body
}

fn assert_servicenet_template(template: &Value) {
    assert_eq!(
        template["defaults"]["preferredTransport"].as_str(),
        Some("JSONRPC")
    );
    let skills_field = template["fields"]
        .as_array()
        .unwrap()
        .iter()
        .find(|field| field["name"].as_str() == Some("skills"))
        .unwrap();
    assert_eq!(
        skills_field["optional_item_fields"].as_array().unwrap(),
        &[json!("description")]
    );
    assert!(
        template["defaults"]
            .get("payment_account_bindings")
            .is_none()
    );
    assert!(template["defaults"].get("didDocument").is_none());
}

fn assert_published_console_agent(published_json: &Value, target_agent_id: &str, owner_did: &str) {
    assert_eq!(published_json["count"].as_u64(), Some(1));
    assert_eq!(published_json["provider_did"].as_str(), Some(owner_did));
    assert_eq!(
        published_json["items"][0]["agent_id"].as_str(),
        Some(target_agent_id)
    );
    assert_eq!(
        published_json["items"][0]["agent_card"]["name"].as_str(),
        Some("Console Agent")
    );
    assert_eq!(
        published_json["items"][0]["service_address"].as_str(),
        Some("console@wattetheria")
    );
}

async fn spawn_list_counting_servicenet() -> (
    std::net::SocketAddr,
    tokio::task::JoinHandle<()>,
    Arc<AtomicUsize>,
) {
    let list_calls = Arc::new(AtomicUsize::new(0));
    let app = axum::Router::new().route(
        "/v1/agents",
        axum::routing::get({
            let list_calls = Arc::clone(&list_calls);
            move || {
                let list_calls = Arc::clone(&list_calls);
                async move {
                    list_calls.fetch_add(1, Ordering::SeqCst);
                    Json(json!({"items": [], "count": 0, "limit": 100, "offset": 0}))
                }
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (addr, server, list_calls)
}

#[tokio::test]
async fn servicenet_published_agents_uses_local_publisher_state_only() {
    let (servicenet_addr, servicenet_server, list_calls) = spawn_list_counting_servicenet().await;
    let (_dir, _router, token, _, state) = build_test_app(20);
    let state = ControlPlaneState {
        servicenet_client: Some(Arc::new(
            ServiceNetClient::new(format!("http://{servicenet_addr}")).unwrap(),
        )),
        ..state
    };
    let publisher_dir = state.data_dir.join("servicenet");
    fs::create_dir_all(&publisher_dir).unwrap();
    fs::write(
        publisher_dir.join("publisher-state.json"),
        serde_json::to_vec(&json!({
            "registrations": [{
                "provider_id": "provider-local",
                "provider_did": state.agent_did,
                "agent_id": "agent-local-only",
                "card_hash": "sha256:local",
                "version": "0.1.0",
                "updated_at": "2026-06-04T00:00:00Z",
                "agent_card": {
                    "name": "Local Only Agent",
                    "description": "Only this node published it",
                    "url": "https://local-only.example.com/a2a"
                },
                "deployment": {},
                "review": {"risk_level": "low"}
            }]
        }))
        .unwrap(),
    )
    .unwrap();
    let published = authed_get_json(
        app(state),
        &token,
        "/v1/wattetheria/servicenet/published-agents",
    )
    .await;
    assert_eq!(published["count"].as_u64(), Some(1));
    assert_eq!(
        published["items"][0]["agent_id"].as_str(),
        Some("agent-local-only")
    );
    assert_eq!(list_calls.load(Ordering::SeqCst), 0);
    servicenet_server.abort();
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn servicenet_template_and_publish_routes_support_console_flow() {
    let (servicenet_addr, servicenet_server) = spawn_mock_servicenet().await;
    let (_dir, _router, token, _, state) = build_test_app(20);
    let state = ControlPlaneState {
        servicenet_client: Some(Arc::new(
            ServiceNetClient::new(format!("http://{servicenet_addr}")).unwrap(),
        )),
        ..state
    };
    let app = app(state.clone());
    let created_payment = authed_post_json(
        app.clone(),
        &token,
        "/v1/wallet/payment-account/create",
        json!({
            "network": "base",
            "rail": "x402",
            "label": "servicenet-receiver"
        }),
    )
    .await;

    let template = authed_get_json(
        app.clone(),
        &token,
        "/v1/wattetheria/servicenet/agent-card-template",
    )
    .await;
    assert_servicenet_template(&template);

    let mut publish_body = console_agent_publish_body(
        None,
        None,
        Some("Console@Wattetheria"),
        "0.1.0",
        "low",
        "Published from the console",
        false,
    );
    publish_body["agent_card"]["skills"][0]
        .as_object_mut()
        .unwrap()
        .remove("description");

    let publish_json = authed_post_json(
        app.clone(),
        &token,
        "/v1/wattetheria/servicenet/publish",
        publish_body,
    )
    .await;
    assert_eq!(publish_json["status"].as_str(), Some("ok"));
    assert_eq!(publish_json["provider_id"].as_str(), Some("provider-ui"));
    assert_eq!(
        publish_json["service_address"].as_str(),
        Some("console@wattetheria")
    );
    assert_eq!(
        publish_json["provider_did"].as_str(),
        Some(state.agent_did.as_str())
    );
    let agent_id = publish_json["agent_id"].as_str().unwrap();
    assert!(agent_id.starts_with("console-agent-"));
    assert_eq!(
        publish_json["submission"]["attestations"]["provider_attester_did"].as_str(),
        Some(state.agent_did.as_str())
    );
    let payment_address = created_payment["active_payment_account"]["address"]
        .as_str()
        .unwrap();
    let submitted_card = &publish_json["submission"]["agent_card"];
    assert!(
        publish_json["submission"]
            .get("payment_account_binding")
            .is_none()
    );
    assert!(submitted_card.get("payment_account_bindings").is_none());
    let extension_binding =
        &submitted_card["capabilities"]["extensions"][0]["params"]["payment_account_bindings"][0];
    assert_eq!(
        extension_binding["payment_address"].as_str(),
        Some(payment_address)
    );
    assert_eq!(extension_binding["rail"].as_str(), Some("x402"));
    assert_eq!(extension_binding["network"].as_str(), Some("base"));
    let binding_agent_did = extension_binding["agent_did"].as_object().map(|agent_did| {
        format!(
            "did:{}:{}",
            agent_did["method"].as_str().unwrap(),
            agent_did["id"].as_str().unwrap()
        )
    });
    assert_eq!(
        submitted_card["didDocument"]["id"].as_str(),
        binding_agent_did.as_deref()
    );
    assert_eq!(
        submitted_card["didDocument"]["alsoKnownAs"]
            .as_array()
            .unwrap(),
        &[json!("console@wattetheria")]
    );
    assert_eq!(
        submitted_card["didDocument"]["service"][0]["serviceEndpoint"].as_str(),
        Some("wattetheria://servicenet/console@wattetheria")
    );
    assert_eq!(
        submitted_card["didDocument"]["payment_account_bindings"][0]["payment_address"].as_str(),
        Some(payment_address)
    );

    let published_json = authed_get_json(
        app.clone(),
        &token,
        "/v1/wattetheria/servicenet/published-agents",
    )
    .await;
    assert_published_console_agent(&published_json, agent_id, &state.agent_did);
    assert_eq!(
        published_json["items"][0]["agent_card"]["skills"][0]["description"].as_str(),
        Some("")
    );

    let update_json = authed_post_json(
        app.clone(),
        &token,
        "/v1/wattetheria/servicenet/publish",
        console_agent_publish_body(
            Some(agent_id),
            Some("provider-ui"),
            Some("console@wattetheria"),
            "0.1.1",
            "medium",
            "Updated from the console",
            true,
        ),
    )
    .await;
    assert_eq!(update_json["status"].as_str(), Some("ok"));
    assert_eq!(update_json["agent_id"].as_str(), Some(agent_id));
    assert_eq!(update_json["provider_id"].as_str(), Some("provider-ui"));
    assert_eq!(update_json["submission"]["version"].as_str(), Some("0.1.1"));

    let forbidden_update = authed_post(
        app.clone(),
        &token,
        "/v1/wattetheria/servicenet/publish",
        console_agent_publish_body(
            Some(agent_id),
            Some("provider-other"),
            Some("console@wattetheria"),
            "0.1.2",
            "medium",
            "Rejected update",
            true,
        ),
    )
    .await;
    assert_eq!(forbidden_update, StatusCode::FORBIDDEN);

    let unpublish_json = authed_post_json(
        app.clone(),
        &token,
        &format!("/v1/wattetheria/servicenet/agents/{agent_id}/unpublish"),
        json!({
            "reason": "console cleanup"
        }),
    )
    .await;
    assert_eq!(unpublish_json["status"].as_str(), Some("ok"));
    assert_eq!(unpublish_json["agent_id"].as_str(), Some(agent_id));
    assert_eq!(unpublish_json["provider_id"].as_str(), Some("provider-ui"));
    assert_eq!(
        unpublish_json["unpublished"]["status"].as_str(),
        Some("revoked")
    );
    assert_eq!(
        unpublish_json["unpublished"]["review"]["notes"].as_str(),
        Some("console cleanup")
    );

    let published_after_unpublish =
        authed_get_json(app, &token, "/v1/wattetheria/servicenet/published-agents").await;
    assert_eq!(published_after_unpublish["count"].as_u64(), Some(0));

    servicenet_server.abort();
}

#[tokio::test]
async fn servicenet_unpublish_cleans_local_state_when_remote_agent_is_missing() {
    let (servicenet_addr, servicenet_server) = spawn_mock_servicenet().await;
    let (_dir, _router, token, _, state) = build_test_app(20);
    let state = ControlPlaneState {
        servicenet_client: Some(Arc::new(
            ServiceNetClient::new(format!("http://{servicenet_addr}")).unwrap(),
        )),
        ..state
    };
    crate::routes::servicenet::publish::save_publisher_state(
        &state.data_dir,
        &crate::routes::servicenet::publish::ServiceNetPublisherState {
            registrations: vec![
                crate::routes::servicenet::publish::ServiceNetPublisherRegistration {
                    provider_id: "provider-ui".to_owned(),
                    provider_did: state.agent_did.clone(),
                    agent_id: "missing-remote-agent".to_owned(),
                    service_address: Some("missing@wattetheria".to_owned()),
                    card_hash: "sha256:missing-remote-agent".to_owned(),
                    version: "0.1.0".to_owned(),
                    updated_at: "2026-06-04T00:00:00Z".to_owned(),
                    agent_card: json!({
                        "name": "Missing Remote Agent",
                        "description": "Previously removed from ServiceNet",
                        "skills": [],
                    }),
                    deployment: json!({}),
                    review: json!({}),
                },
            ],
        },
    )
    .expect("save publisher state");
    let app = app(state.clone());

    let published_before_unpublish = authed_get_json(
        app.clone(),
        &token,
        "/v1/wattetheria/servicenet/published-agents",
    )
    .await;
    assert_eq!(published_before_unpublish["count"].as_u64(), Some(1));

    let unpublish_json = authed_post_json(
        app.clone(),
        &token,
        "/v1/wattetheria/servicenet/agents/missing-remote-agent/unpublish",
        json!({
            "reason": "remote already deleted"
        }),
    )
    .await;
    assert_eq!(unpublish_json["status"].as_str(), Some("ok"));
    assert_eq!(
        unpublish_json["agent_id"].as_str(),
        Some("missing-remote-agent")
    );
    assert_eq!(unpublish_json["provider_id"].as_str(), Some("provider-ui"));
    assert_eq!(
        unpublish_json["unpublished"]["status"].as_str(),
        Some("remote_missing")
    );
    assert_eq!(
        unpublish_json["unpublished"]["service_address"].as_str(),
        Some("missing@wattetheria")
    );

    let published_after_unpublish =
        authed_get_json(app, &token, "/v1/wattetheria/servicenet/published-agents").await;
    assert_eq!(published_after_unpublish["count"].as_u64(), Some(0));
    let publisher_state = crate::routes::servicenet::publish::load_publisher_state(&state.data_dir)
        .expect("load publisher state");
    assert!(publisher_state.registrations.is_empty());

    servicenet_server.abort();
}

#[tokio::test]
async fn servicenet_publish_rejects_unsafe_agent_name() {
    let (servicenet_addr, servicenet_server) = spawn_mock_servicenet().await;
    let (_dir, _router, token, _, state) = build_test_app(20);
    let state = ControlPlaneState {
        servicenet_client: Some(Arc::new(
            ServiceNetClient::new(format!("http://{servicenet_addr}")).unwrap(),
        )),
        ..state
    };
    let app = app(state);
    let mut publish_body = console_agent_publish_body(
        None,
        None,
        Some("unsafe-name@wattetheria"),
        "0.1.0",
        "low",
        "Published from the console",
        false,
    );
    publish_body["agent_card"]["name"] = json!("Bad\u{0007}Name");

    let status = authed_post(
        app,
        &token,
        "/v1/wattetheria/servicenet/publish",
        publish_body,
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    servicenet_server.abort();
}

#[tokio::test]
async fn servicenet_callback_decision_routes_into_mission_commit() {
    let (servicenet_addr, servicenet_server) = spawn_mock_servicenet().await;
    let (_dir, _router, token, _, state) = build_test_app(20);
    let mission_id = {
        let mut board = state.mission_board.lock().await;
        board
            .publish(
                "Inspect Result",
                "mission seeded for servicenet callback",
                &state.agent_did,
                wattetheria_kernel::civilization::missions::MissionPublisherKind::System,
                wattetheria_kernel::civilization::missions::MissionDomain::Trade,
                None,
                None,
                None,
                None,
                wattetheria_kernel::civilization::missions::MissionReward {
                    agent_watt: 0,
                    reputation: 0,
                    capacity: 0,
                    treasury_share_watt: 0,
                },
                Value::Null,
            )
            .mission_id
    };

    let callback_app = axum::Router::new().route(
        "/agent-events",
        axum::routing::post({
            let mission_id = mission_id.clone();
            move |Json(_payload): Json<Value>| {
                let mission_id = mission_id.clone();
                async move {
                    Json(json!({
                        "ok": true,
                        "acked_at": 1,
                        "decision": {
                            "decision_id": "dec-servicenet-1",
                            "action": "claim_mission",
                            "route": "wattetheria_commit",
                            "payload": {
                                "mission_id": mission_id,
                            }
                        }
                    }))
                }
            }
        }),
    );
    let callback_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let callback_addr = callback_listener.local_addr().unwrap();
    let callback_server = tokio::spawn(async move {
        axum::serve(callback_listener, callback_app).await.unwrap();
    });

    let state = ControlPlaneState {
        servicenet_client: Some(Arc::new(
            ServiceNetClient::new(format!("http://{servicenet_addr}")).unwrap(),
        )),
        agent_executor_base_url: Some(format!("http://{callback_addr}")),
        agent_event_callback_base_url: Some(format!("http://{callback_addr}")),
        ..state
    };
    let app = app(state.clone());

    let invoke_json = authed_post_json(
        app,
        &token,
        "/v1/wattetheria/servicenet/agents/agent-alpha/invoke",
        json!({
            "message": "claim the mission",
            "input": {"mode": "analysis"}
        }),
    )
    .await;
    assert_eq!(invoke_json["status"].as_str(), Some("completed"));

    let board = state.mission_board.lock().await;
    let claimed = board
        .list(None)
        .into_iter()
        .find(|mission| mission.mission_id == mission_id)
        .expect("mission present");
    assert_eq!(
        claimed.claimed_by.as_deref(),
        Some(state.agent_did.as_str())
    );
    assert_eq!(
        claimed.status,
        wattetheria_kernel::civilization::missions::MissionStatus::Claimed
    );

    callback_server.abort();
    servicenet_server.abort();
}
