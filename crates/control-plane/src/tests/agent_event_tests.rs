use super::*;
use axum::http::Request;
use base64::engine::general_purpose::STANDARD;
use std::path::Path;
use watt_did::{Did, PaymentAccountCustody, VerifiedAgentContext};
use watt_wallet::{
    InMemoryKeyStore, KeyHandle, KeyStore, PaymentAccountBindingProofOptions, PaymentAccountSigner,
    build_payment_account_binding_proof,
};

use crate::routes::agent_events::VERIFIED_AGENT_CONTEXT_PAYLOAD_KEY;

fn assert_claim_brain_actions(data_dir: &Path, event_id: &str, expected_actions: &[&str]) {
    let entries = crate::diagnostics::list_diagnostics(
        data_dir,
        &crate::diagnostics::DiagnosticFilter {
            event_id: Some(event_id.to_owned()),
            ..Default::default()
        },
    )
    .unwrap();
    let received = entries
        .iter()
        .find(|entry| entry.phase == "callback.received")
        .expect("callback.received diagnostic");
    let actions = received.details["payload"]["brain_input"]["allowed_actions"]
        .as_array()
        .expect("brain allowed actions")
        .iter()
        .filter_map(serde_json::Value::as_str)
        .collect::<Vec<_>>();
    assert_eq!(actions, expected_actions);
}

fn signed_agent_event_envelope(
    source_identity: &Identity,
    source_node_id: &str,
    target_agent_id: Option<&str>,
    capability: &str,
    message: Value,
) -> SwarmAgentEnvelope {
    let protocol = "google_a2a".to_owned();
    let transport_profile = Some("wattswarm_mesh".to_owned());
    let source_agent_id = Some(source_identity.agent_did.clone());
    let source_node_id = Some(source_node_id.to_owned());
    let target_agent_id = target_agent_id.map(ToOwned::to_owned);
    let capability = Some(capability.to_owned());
    let message_json = serde_json::to_string(&message).expect("message serializes");
    let signed_payload = ExpectedSignedAgentEnvelopePayload {
        protocol: &protocol,
        transport_profile: transport_profile.as_ref(),
        source_agent_id: source_agent_id.as_ref(),
        target_agent_id: target_agent_id.as_ref(),
        source_node_id: source_node_id.as_ref(),
        target_node_id: None,
        capability: capability.as_ref(),
        source_agent_card_hash: None,
        message_json: &message_json,
        extensions_json: None,
    };
    let signature = sign_payload(&signed_payload, source_identity).expect("sign agent envelope");
    SwarmAgentEnvelope {
        protocol,
        transport_profile,
        source_agent_id,
        target_agent_id,
        source_node_id,
        target_node_id: None,
        capability,
        source_agent_card: None,
        message,
        extensions: None,
        signature: Some(signature),
    }
}

fn signed_agent_event_envelope_with_wallet_key(
    keystore: &InMemoryKeyStore,
    key_handle: &KeyHandle,
    agent_did: &str,
    source_node_id: &str,
    target_agent_id: Option<&str>,
    capability: &str,
    message: Value,
) -> SwarmAgentEnvelope {
    let protocol = "google_a2a".to_owned();
    let transport_profile = Some("wattswarm_mesh".to_owned());
    let source_agent_id = Some(agent_did.to_owned());
    let source_node_id = Some(source_node_id.to_owned());
    let target_agent_id = target_agent_id.map(ToOwned::to_owned);
    let capability = Some(capability.to_owned());
    let message_json = serde_json::to_string(&message).expect("message serializes");
    let signed_payload = ExpectedSignedAgentEnvelopePayload {
        protocol: &protocol,
        transport_profile: transport_profile.as_ref(),
        source_agent_id: source_agent_id.as_ref(),
        target_agent_id: target_agent_id.as_ref(),
        source_node_id: source_node_id.as_ref(),
        target_node_id: None,
        capability: capability.as_ref(),
        source_agent_card_hash: None,
        message_json: &message_json,
        extensions_json: None,
    };
    let signature_bytes = keystore
        .sign_bytes(
            key_handle,
            &canonical_bytes(&signed_payload).expect("canonical payload"),
        )
        .expect("wallet signs agent envelope");
    let signature = STANDARD.encode(signature_bytes.0);
    SwarmAgentEnvelope {
        protocol,
        transport_profile,
        source_agent_id,
        target_agent_id,
        source_node_id,
        target_node_id: None,
        capability,
        source_agent_card: None,
        message,
        extensions: None,
        signature: Some(signature),
    }
}

fn set_signed_agent_envelope(event: &mut Value, envelope: &SwarmAgentEnvelope) {
    event["event"]["agent_envelope"] =
        serde_json::to_value(envelope).expect("agent envelope serializes");
}

fn sign_payment_event_with_identity(event: &mut Value, source_identity: &Identity) {
    let source_node_id = event["event"]["source_node_id"]
        .as_str()
        .expect("source_node_id")
        .to_owned();
    let target_agent_id = event["event"]["target_agent_id"]
        .as_str()
        .expect("target_agent_id")
        .to_owned();
    let message = event["event"]["payload"]["agent_envelope"]["message"].clone();
    let envelope = signed_agent_event_envelope(
        source_identity,
        &source_node_id,
        Some(&target_agent_id),
        "agent.payment",
        message,
    );
    set_signed_agent_envelope(event, &envelope);
    event["event"]["payload"]["agent_envelope"] =
        serde_json::to_value(envelope).expect("agent envelope serializes");
}

fn sign_payment_event_with_wallet_key(
    event: &mut Value,
    keystore: &InMemoryKeyStore,
    key_handle: &KeyHandle,
    source_agent_did: &str,
) {
    let source_node_id = event["event"]["source_node_id"]
        .as_str()
        .expect("source_node_id")
        .to_owned();
    let target_agent_id = event["event"]["target_agent_id"]
        .as_str()
        .expect("target_agent_id")
        .to_owned();
    let message = event["event"]["payload"]["agent_envelope"]["message"].clone();
    let envelope = signed_agent_event_envelope_with_wallet_key(
        keystore,
        key_handle,
        source_agent_did,
        &source_node_id,
        Some(&target_agent_id),
        "agent.payment",
        message,
    );
    set_signed_agent_envelope(event, &envelope);
    event["event"]["payload"]["agent_envelope"] =
        serde_json::to_value(envelope).expect("agent envelope serializes");
}

#[tokio::test]
async fn agent_events_sync_signed_payment_event_to_ledger_before_decision() {
    let (_dir, router, _token, _policy_engine, state) = build_test_app(20);
    let remote_identity = Identity::new_random();
    let payment_id = "payment-inbound-event-1";
    let local_agent_did = state.agent_did.clone();
    let remote_agent_did = remote_identity.agent_did.clone();
    let payment = json!({
        "payment_id": payment_id,
        "sender_did": remote_agent_did.clone(),
        "recipient_did": local_agent_did.clone(),
        "sender_public_id": "remote-public",
        "recipient_public_id": "local-public",
        "remote_node_id": "12D3KooRemotePeer",
        "amount": "1000",
        "currency": "USDC",
        "rail": "x402",
        "layer": "web3",
        "network": "base-sepolia",
        "sender_address": null,
        "recipient_address": "0x0000000000000000000000000000000000000001",
        "mission_id": null,
        "task_id": "task-7",
        "description": "inbound payment",
        "metadata": null,
        "status": "proposed",
        "authorization_signature": null,
        "authorization_public_key": null,
        "settlement_receipt": null,
        "reject_reason": null,
        "proposed_at": 10,
        "authorized_at": null,
        "settled_at": null,
        "expires_at": null
    });
    let agent_envelope = signed_agent_event_envelope(
        &remote_identity,
        "12D3KooRemotePeer",
        Some(&local_agent_did),
        "agent.payment",
        json!({
            "message_kind": "payment_request",
            "payment": payment
        }),
    );
    let event = json!({
        "event": {
            "event_id": "evt-payment-sync-1",
            "event_type": "payment_request",
            "source_kind": "payment_summary",
            "source_node_id": "12D3KooRemotePeer",
            "target_agent_id": local_agent_did.clone(),
            "target_executor": "core-agent",
            "agent_envelope": agent_envelope.clone(),
            "payload": {
                "agent_envelope": agent_envelope
            },
            "requires_commit": true,
            "allowed_actions": ["authorize", "reject"],
            "correlation_id": "payment-thread-1",
            "dedupe_key": "payment:payment-inbound-event-1",
            "created_at": 10
        }
    });

    let response = request_json(
        router,
        Request::post("/agent-events")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(event.to_string()))
            .expect("request"),
    )
    .await;

    assert_eq!(response["ok"].as_bool(), Some(true));
    let ledger = state.payment_ledger.lock().await;
    let payment = ledger.get(payment_id).expect("payment synced");
    assert_eq!(
        payment.status,
        wattetheria_kernel::payments::PaymentStatus::Proposed
    );
    assert_eq!(payment.sender_did, remote_identity.agent_did);
}

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
    let local_agent_did = state.agent_did.clone();
    let remote_identity = Identity::new_random();
    let dm_envelope = signed_agent_event_envelope(
        &remote_identity,
        "social-node",
        Some(&local_agent_did),
        "social.dm",
        json!({
            "source_public_id": "peer-alpha",
            "target_public_id": "self-alpha",
            "content": "hello"
        }),
    );
    let state = ControlPlaneState {
        brain_engine: Arc::new(tokio::sync::RwLock::new(BrainEngine::from_config(
            &BrainProviderConfig::OpenaiCompatible {
                base_url: base_url.clone(),
                model: "openclaw".to_owned(),
                api_key_env: None,
            },
        ))),
        brain_config: Arc::new(tokio::sync::RwLock::new(
            BrainProviderConfig::OpenaiCompatible {
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
                        "source_node_id": "social-node",
                        "target_agent_id": local_agent_did,
                        "target_executor": "core-agent",
                        "agent_envelope": dm_envelope.clone(),
                        "payload": {
                            "agent_envelope": dm_envelope
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

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn agent_events_route_reports_openai_compatible_missing_content_body() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind listener");
    let addr = listener.local_addr().expect("listener addr");
    let app_mock = Router::new().route(
        "/v1/chat/completions",
        post(|| async move {
            Json(json!({
                "choices": [{
                    "message": {}
                }]
            }))
        }),
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, app_mock).await.expect("serve mock");
    });

    let (_dir, _router, _token, _policy_engine, state) = build_test_app(20);
    let base_url = format!("http://{addr}/v1");
    let remote_identity = Identity::new_random();
    let task_claim_envelope = signed_agent_event_envelope(
        &remote_identity,
        "claimer-node",
        Some(&state.agent_did),
        "task.claim",
        json!({
            "task_id": "task-1",
            "event_kind": "task_claimed"
        }),
    );
    let state = ControlPlaneState {
        brain_engine: Arc::new(tokio::sync::RwLock::new(BrainEngine::from_config(
            &BrainProviderConfig::OpenaiCompatible {
                base_url: base_url.clone(),
                model: "openclaw".to_owned(),
                api_key_env: None,
            },
        ))),
        brain_config: Arc::new(tokio::sync::RwLock::new(
            BrainProviderConfig::OpenaiCompatible {
                base_url: base_url.clone(),
                model: "openclaw".to_owned(),
                api_key_env: None,
            },
        )),
        brain_provider_label: format!("openai-compatible model=openclaw url={base_url}"),
        ..state
    };
    let data_dir = state.data_dir.clone();
    let app = app(state);

    let response = request_json(
        app,
        Request::post("/agent-events")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(
                json!({
                    "event": {
                        "event_id": "evt-missing-content",
                        "event_type": "task_claim_received",
                        "source_kind": "task_lifecycle",
                        "source_node_id": "claimer-node",
                        "target_agent_id": null,
                        "target_executor": "core-agent",
                        "agent_envelope": task_claim_envelope,
                        "payload": {
                            "task_id": "task-1",
                            "event_kind": "task_claimed"
                        },
                        "requires_commit": false,
                        "allowed_actions": ["inspect_task", "decide_claim"],
                        "correlation_id": "task-1",
                        "dedupe_key": "task_claim:task-1",
                        "created_at": 1
                    }
                })
                .to_string(),
            ))
            .expect("request"),
    )
    .await;

    assert_eq!(response["ok"].as_bool(), Some(false));
    let detail = response["detail"].as_str().expect("response detail");
    assert!(detail.contains("openai-compatible response missing content"));
    assert!(detail.contains("response_body="));
    assert!(detail.contains("\"choices\""));

    let entries = crate::diagnostics::list_diagnostics(
        &data_dir,
        &crate::diagnostics::DiagnosticFilter {
            event_id: Some("evt-missing-content".to_owned()),
            ..Default::default()
        },
    )
    .unwrap();
    let failed = entries
        .iter()
        .find(|entry| entry.phase == "decision.failed")
        .expect("decision.failed diagnostic");
    assert_eq!(
        failed.details["payload"]["callback_response"]["ok"].as_bool(),
        Some(false)
    );
    assert!(
        failed.details["payload"]["error"]
            .as_str()
            .expect("decision error")
            .contains("response_body=")
    );

    server.abort();
}

#[tokio::test]
async fn agent_events_route_allows_task_result_to_settle_mission_via_commit_plane() {
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
                        "content": "{\"action\":\"settle_mission\",\"reason\":\"publisher accepted result\",\"payload\":{\"mission_id\":\"mission-1\",\"agent_did\":\"agent-worker\"}}"
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
    let remote_identity = Identity::new_random();
    let task_result_envelope = signed_agent_event_envelope(
        &remote_identity,
        "claimer-node",
        Some(&state.agent_did),
        "task.result",
        json!({
            "task_id": "mission-1",
            "mission_id": "mission-1",
            "candidate_output": {
                "mission_id": "mission-1",
                "agent_did": "agent-worker"
            }
        }),
    );
    let state = ControlPlaneState {
        brain_engine: Arc::new(tokio::sync::RwLock::new(BrainEngine::from_config(
            &BrainProviderConfig::OpenaiCompatible {
                base_url: base_url.clone(),
                model: "openclaw".to_owned(),
                api_key_env: None,
            },
        ))),
        brain_config: Arc::new(tokio::sync::RwLock::new(
            BrainProviderConfig::OpenaiCompatible {
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
                        "event_id": "evt-task-result",
                        "event_type": "task_result_received",
                        "source_kind": "task_lifecycle",
                        "source_node_id": "claimer-node",
                        "target_agent_id": null,
                        "target_executor": "core-agent",
                        "agent_envelope": task_result_envelope,
                        "payload": {
                            "task_id": "mission-1",
                            "mission_id": "mission-1",
                            "candidate_output": {
                                "mission_id": "mission-1",
                                "agent_did": "agent-worker"
                            }
                        },
                        "requires_commit": true,
                        "allowed_actions": ["inspect_task", "settle_mission"],
                        "correlation_id": "mission-1",
                        "dedupe_key": "task_result:mission-1:cand-1",
                        "created_at": 1
                    }
                })
                .to_string(),
            ))
            .expect("request"),
    )
    .await;

    assert_eq!(response["ok"].as_bool(), Some(true));
    assert_eq!(
        response["decision"]["action"].as_str(),
        Some("settle_mission")
    );
    assert_eq!(
        response["decision"]["route"].as_str(),
        Some("wattetheria_commit")
    );

    server.abort();
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn agent_events_convert_approved_claim_decision_to_mission_commit() {
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
                        "content": "{\"ACTION\":\"DECIDE_CLAIM\",\"REASON\":\"claim is valid\",\"PAYLOAD\":{\"APPROVED\":true}}"
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
    let remote_identity = Identity::new_random();
    let task_claim_envelope = signed_agent_event_envelope(
        &remote_identity,
        "claimer-node",
        Some(&state.agent_did),
        "task.claim",
        json!({
            "task_id": "mission-1",
            "claimer_node_id": "claimer-node",
            "task_inputs": {
                "kind": "wattetheria_mission",
                "mission_id": "mission-1"
            }
        }),
    );
    let state = ControlPlaneState {
        brain_engine: Arc::new(tokio::sync::RwLock::new(BrainEngine::from_config(
            &BrainProviderConfig::OpenaiCompatible {
                base_url: base_url.clone(),
                model: "openclaw".to_owned(),
                api_key_env: None,
            },
        ))),
        brain_config: Arc::new(tokio::sync::RwLock::new(
            BrainProviderConfig::OpenaiCompatible {
                base_url: base_url.clone(),
                model: "openclaw".to_owned(),
                api_key_env: None,
            },
        )),
        brain_provider_label: format!("openai-compatible model=openclaw url={base_url}"),
        ..state
    };
    let data_dir = state.data_dir.clone();
    let app = app(state);

    let response = request_json(
        app,
        Request::post("/agent-events")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(
                json!({
                    "event": {
                        "event_id": "evt-task-claim",
                        "event_type": "task_claim_received",
                        "source_kind": "task_lifecycle",
                        "source_node_id": "claimer-node",
                        "target_agent_id": null,
                        "target_executor": "core-agent",
                        "agent_envelope": task_claim_envelope,
                        "payload": {
                            "task_id": "mission-1",
                            "claimer_node_id": "claimer-node",
                            "task_inputs": {
                                "kind": "wattetheria_mission",
                                "mission_id": "mission-1"
                            }
                        },
                        "requires_commit": true,
                        "allowed_actions": ["inspect_task", "decide_claim"],
                        "correlation_id": "mission-1",
                        "dedupe_key": "task_claim:mission-1",
                        "created_at": 1
                    }
                })
                .to_string(),
            ))
            .expect("request"),
    )
    .await;

    assert_eq!(response["ok"].as_bool(), Some(true));
    assert_eq!(
        response["decision"]["action"].as_str(),
        Some("claim_mission")
    );
    assert_eq!(
        response["decision"]["route"].as_str(),
        Some("wattetheria_commit")
    );
    assert_eq!(
        response["decision"]["payload"]["mission_id"].as_str(),
        Some("mission-1")
    );
    assert_eq!(
        response["decision"]["payload"]["agent_did"].as_str(),
        Some("claimer-node")
    );

    assert_claim_brain_actions(&data_dir, "evt-task-claim", &["decide_claim"]);

    server.abort();
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn agent_events_convert_accept_result_to_settle_mission_commit() {
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
                        "content": "{\"action\":\"accept_result\",\"reason\":\"result is acceptable\",\"payload\":{}}"
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
    let remote_identity = Identity::new_random();
    let task_result_envelope = signed_agent_event_envelope(
        &remote_identity,
        "claimer-node",
        Some(&state.agent_did),
        "task.result",
        json!({
            "task_id": "mission-1",
            "candidate_output": {
                "kind": "wattetheria_mission_result",
                "mission_id": "mission-1",
                "agent_did": "agent-worker"
            }
        }),
    );
    let state = ControlPlaneState {
        brain_engine: Arc::new(tokio::sync::RwLock::new(BrainEngine::from_config(
            &BrainProviderConfig::OpenaiCompatible {
                base_url: base_url.clone(),
                model: "openclaw".to_owned(),
                api_key_env: None,
            },
        ))),
        brain_config: Arc::new(tokio::sync::RwLock::new(
            BrainProviderConfig::OpenaiCompatible {
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
                        "event_id": "evt-task-result-accept",
                        "event_type": "task_result_received",
                        "source_kind": "task_lifecycle",
                        "source_node_id": "claimer-node",
                        "target_agent_id": null,
                        "target_executor": "core-agent",
                        "agent_envelope": task_result_envelope,
                        "payload": {
                            "task_id": "mission-1",
                            "candidate_output": {
                                "kind": "wattetheria_mission_result",
                                "mission_id": "mission-1",
                                "agent_did": "agent-worker"
                            }
                        },
                        "requires_commit": true,
                        "allowed_actions": ["inspect_task", "accept_result"],
                        "correlation_id": "mission-1",
                        "dedupe_key": "task_result:mission-1:cand-1",
                        "created_at": 1
                    }
                })
                .to_string(),
            ))
            .expect("request"),
    )
    .await;

    assert_eq!(response["ok"].as_bool(), Some(true));
    assert_eq!(
        response["decision"]["action"].as_str(),
        Some("settle_mission")
    );
    assert_eq!(
        response["decision"]["route"].as_str(),
        Some("wattetheria_commit")
    );
    assert_eq!(
        response["decision"]["payload"]["mission_id"].as_str(),
        Some("mission-1")
    );
    assert_eq!(
        response["decision"]["payload"]["agent_did"].as_str(),
        Some("agent-worker")
    );

    server.abort();
}

fn payment_event_with_optional_verified_context(
    local_agent_did: &str,
    remote_agent_did: &str,
    payment_id: &str,
    remote_node_id: &str,
    verified_context: Option<&serde_json::Value>,
) -> Value {
    payment_event_with_optional_extras(
        local_agent_did,
        remote_agent_did,
        payment_id,
        remote_node_id,
        verified_context,
        None,
    )
}

fn payment_event_with_optional_extras(
    local_agent_did: &str,
    remote_agent_did: &str,
    payment_id: &str,
    remote_node_id: &str,
    verified_context: Option<&serde_json::Value>,
    payment_account_binding: Option<&serde_json::Value>,
) -> Value {
    let payment = json!({
        "payment_id": payment_id,
        "sender_did": remote_agent_did,
        "recipient_did": local_agent_did,
        "sender_public_id": "remote-public",
        "recipient_public_id": "local-public",
        "remote_node_id": remote_node_id,
        "amount": "1000",
        "currency": "USDC",
        "rail": "x402",
        "layer": "web3",
        "network": "base-sepolia",
        "sender_address": null,
        "recipient_address": "0x0000000000000000000000000000000000000001",
        "mission_id": null,
        "task_id": "task-verified",
        "description": "inbound payment",
        "metadata": null,
        "status": "proposed",
        "authorization_signature": null,
        "authorization_public_key": null,
        "settlement_receipt": null,
        "reject_reason": null,
        "proposed_at": 10,
        "authorized_at": null,
        "settled_at": null,
        "expires_at": null
    });
    let mut message = json!({
        "message_kind": "payment_request",
        "payment": payment,
    });
    if let Some(binding) = payment_account_binding
        && let Some(map) = message.as_object_mut()
    {
        map.insert("payment_account_binding".to_owned(), binding.clone());
    }
    let agent_envelope = json!({
        "source_agent_id": remote_agent_did,
        "target_agent_id": local_agent_did,
        "source_node_id": remote_node_id,
        "message": message,
    });
    let mut payload = serde_json::Map::new();
    payload.insert("agent_envelope".to_owned(), agent_envelope);
    if let Some(context) = verified_context {
        payload.insert(
            VERIFIED_AGENT_CONTEXT_PAYLOAD_KEY.to_owned(),
            context.clone(),
        );
    }
    json!({
        "event": {
            "event_id": format!("evt-{payment_id}"),
            "event_type": "payment_request",
            "source_kind": "payment_summary",
            "source_node_id": remote_node_id,
            "target_agent_id": local_agent_did,
            "target_executor": "core-agent",
            "payload": Value::Object(payload),
            "requires_commit": true,
            "allowed_actions": ["authorize", "reject"],
            "correlation_id": format!("payment-thread-{payment_id}"),
            "dedupe_key": format!("payment:{payment_id}"),
            "created_at": 10
        }
    })
}

fn verified_context_value(agent_did: &str, source_node_id: &str) -> serde_json::Value {
    let context = VerifiedAgentContext {
        agent_did: Did::parse(agent_did).expect("agent did parses"),
        controller_node_id: source_node_id.to_owned(),
        source_node_id: Some(source_node_id.to_owned()),
        envelope_verified: true,
        source_node_verified: true,
        controller_binding_verified: false,
        controller_binding_proof: None,
        payment_account_binding: None,
        verified_at_ms: 1_716_120_000_000,
        expires_at_ms: None,
    };
    serde_json::to_value(&context).expect("serialize verified context")
}

#[tokio::test]
async fn agent_events_payment_sync_accepts_consistent_verified_agent_context() {
    let (_dir, router, _token, _policy_engine, state) = build_test_app(20);
    let remote_identity = Identity::new_random();
    let payment_id = "payment-context-ok-1";
    let remote_node_id = "12D3KooRemotePeer";
    let context = verified_context_value(&remote_identity.agent_did, remote_node_id);
    let mut event = payment_event_with_optional_verified_context(
        &state.agent_did,
        &remote_identity.agent_did,
        payment_id,
        remote_node_id,
        Some(&context),
    );
    sign_payment_event_with_identity(&mut event, &remote_identity);

    let response = request_json(
        router,
        Request::post("/agent-events")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(event.to_string()))
            .expect("request"),
    )
    .await;

    assert_eq!(response["ok"].as_bool(), Some(true));
    let ledger = state.payment_ledger.lock().await;
    let payment = ledger.get(payment_id).expect("payment synced");
    assert_eq!(
        payment.status,
        wattetheria_kernel::payments::PaymentStatus::Proposed
    );
}

#[tokio::test]
async fn agent_events_payment_sync_rejects_forged_verified_context_without_signed_envelope() {
    let (_dir, router, _token, _policy_engine, state) = build_test_app(20);
    let remote_identity = Identity::new_random();
    let payment_id = "payment-context-forged-1";
    let remote_node_id = "12D3KooRemotePeer";
    let context = verified_context_value(&remote_identity.agent_did, remote_node_id);
    let event = payment_event_with_optional_verified_context(
        &state.agent_did,
        &remote_identity.agent_did,
        payment_id,
        remote_node_id,
        Some(&context),
    );

    let response = request_json(
        router,
        Request::post("/agent-events")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(event.to_string()))
            .expect("request"),
    )
    .await;

    let error = response["error"].as_str().unwrap_or_default();
    assert!(
        error.contains("signed agent_envelope"),
        "expected signed agent_envelope error, got {response}"
    );
    let ledger = state.payment_ledger.lock().await;
    assert!(
        ledger.get(payment_id).is_none(),
        "forged context must not reach the ledger"
    );
}

#[tokio::test]
async fn agent_events_payment_sync_rejects_verified_agent_context_with_mismatched_did() {
    let (_dir, router, _token, _policy_engine, state) = build_test_app(20);
    let remote_identity = Identity::new_random();
    let unrelated_identity = Identity::new_random();
    let payment_id = "payment-context-mismatch-1";
    let remote_node_id = "12D3KooRemotePeer";
    let context = verified_context_value(&unrelated_identity.agent_did, remote_node_id);
    let mut event = payment_event_with_optional_verified_context(
        &state.agent_did,
        &remote_identity.agent_did,
        payment_id,
        remote_node_id,
        Some(&context),
    );
    sign_payment_event_with_identity(&mut event, &remote_identity);

    let response = request_json(
        router,
        Request::post("/agent-events")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(event.to_string()))
            .expect("request"),
    )
    .await;

    assert!(
        response["error"]
            .as_str()
            .unwrap_or_default()
            .contains("agent_did does not match"),
        "expected agent_did mismatch error, got {response}"
    );
    let ledger = state.payment_ledger.lock().await;
    assert!(
        ledger.get(payment_id).is_none(),
        "rejected event must not reach the ledger"
    );
}

struct RemoteBindingFixture {
    remote_agent_did: String,
    agent_key_handle: KeyHandle,
    keystore: InMemoryKeyStore,
    binding: serde_json::Value,
}

fn build_remote_binding_fixture() -> RemoteBindingFixture {
    let mut keystore = InMemoryKeyStore::new();
    let agent_info = keystore.generate_ed25519().expect("ed25519 key");
    let payment_info = keystore.generate_secp256k1().expect("secp256k1 key");
    let options = PaymentAccountBindingProofOptions {
        agent_did: agent_info.did.clone(),
        agent_key_handle: &agent_info.key_handle,
        agent_public_key_multibase: agent_info.public_key_multibase.clone(),
        rail: "x402".to_owned(),
        network: Some("base-sepolia".to_owned()),
        custody: PaymentAccountCustody::LocalGenerated,
        receive_only: false,
        can_sign: true,
        capabilities: vec!["payment.authorize".to_owned()],
        issued_at_ms: 1_716_120_000_000,
        expires_at_ms: None,
        nonce: None,
        payment_signer: Some(PaymentAccountSigner {
            key_handle: &payment_info.key_handle,
            public_key_multibase: payment_info.public_key_multibase.clone(),
        }),
        watch_only_payment_address: None,
    };
    let proof = build_payment_account_binding_proof(&keystore, options).expect("build proof");
    let binding = serde_json::to_value(&proof).expect("serialize binding");
    RemoteBindingFixture {
        remote_agent_did: agent_info.did.to_string(),
        agent_key_handle: agent_info.key_handle,
        keystore,
        binding,
    }
}

#[tokio::test]
async fn agent_events_payment_sync_accepts_valid_payment_account_binding() {
    let (_dir, router, _token, _policy_engine, state) = build_test_app(20);
    let fixture = build_remote_binding_fixture();
    let payment_id = "payment-binding-ok-1";
    let remote_node_id = "12D3KooRemotePeer";
    let mut event = payment_event_with_optional_extras(
        &state.agent_did,
        &fixture.remote_agent_did,
        payment_id,
        remote_node_id,
        None,
        Some(&fixture.binding),
    );
    sign_payment_event_with_wallet_key(
        &mut event,
        &fixture.keystore,
        &fixture.agent_key_handle,
        &fixture.remote_agent_did,
    );

    let response = request_json(
        router,
        Request::post("/agent-events")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(event.to_string()))
            .expect("request"),
    )
    .await;

    assert_eq!(response["ok"].as_bool(), Some(true));
    let ledger = state.payment_ledger.lock().await;
    assert!(ledger.get(payment_id).is_some(), "payment should be synced");
}

#[tokio::test]
async fn agent_events_payment_sync_rejects_tampered_payment_account_binding() {
    let (_dir, router, _token, _policy_engine, state) = build_test_app(20);
    let fixture = build_remote_binding_fixture();
    let mut tampered = fixture.binding.clone();
    if let Some(map) = tampered.as_object_mut() {
        map.insert(
            "payment_address".to_owned(),
            Value::String("0x0000000000000000000000000000000000000001".to_owned()),
        );
    }
    let payment_id = "payment-binding-tampered-1";
    let mut event = payment_event_with_optional_extras(
        &state.agent_did,
        &fixture.remote_agent_did,
        payment_id,
        "12D3KooRemotePeer",
        None,
        Some(&tampered),
    );
    sign_payment_event_with_wallet_key(
        &mut event,
        &fixture.keystore,
        &fixture.agent_key_handle,
        &fixture.remote_agent_did,
    );

    let response = request_json(
        router,
        Request::post("/agent-events")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(event.to_string()))
            .expect("request"),
    )
    .await;

    let error = response["error"].as_str().unwrap_or_default();
    assert!(
        error.contains("payment_account_binding"),
        "expected payment_account_binding error, got {response}"
    );
    let ledger = state.payment_ledger.lock().await;
    assert!(
        ledger.get(payment_id).is_none(),
        "rejected event must not reach the ledger"
    );
}

#[tokio::test]
async fn agent_events_payment_sync_rejects_binding_with_mismatched_agent_did() {
    let (_dir, router, _token, _policy_engine, state) = build_test_app(20);
    let fixture = build_remote_binding_fixture();
    let unrelated_identity = Identity::new_random();
    let payment_id = "payment-binding-wrong-did-1";
    let mut event = payment_event_with_optional_extras(
        &state.agent_did,
        &unrelated_identity.agent_did,
        payment_id,
        "12D3KooRemotePeer",
        None,
        Some(&fixture.binding),
    );
    sign_payment_event_with_identity(&mut event, &unrelated_identity);

    let response = request_json(
        router,
        Request::post("/agent-events")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(event.to_string()))
            .expect("request"),
    )
    .await;

    let error = response["error"].as_str().unwrap_or_default();
    assert!(
        error.contains("payment_account_binding agent_did does not match"),
        "expected agent_did mismatch error, got {response}"
    );
    let ledger = state.payment_ledger.lock().await;
    assert!(
        ledger.get(payment_id).is_none(),
        "rejected event must not reach the ledger"
    );
}

#[tokio::test]
async fn agent_events_payment_sync_rejects_sender_signed_state_without_payment_account_binding() {
    let (_dir, router, _token, _policy_engine, state) = build_test_app(20);
    let remote_identity = Identity::new_random();
    let payment_id = "payment-binding-required-1";
    let mut event = payment_event_with_optional_extras(
        &state.agent_did,
        &remote_identity.agent_did,
        payment_id,
        "12D3KooRemotePeer",
        None,
        None,
    );
    event["event"]["event_type"] = json!("payment_update");
    event["event"]["payload"]["agent_envelope"]["message"]["message_kind"] =
        json!("payment_authorized");
    event["event"]["payload"]["agent_envelope"]["message"]["payment"]["status"] =
        json!("authorized");
    event["event"]["payload"]["agent_envelope"]["message"]["payment"]["sender_address"] =
        json!("0x0000000000000000000000000000000000000002");
    sign_payment_event_with_identity(&mut event, &remote_identity);

    let response = request_json(
        router,
        Request::post("/agent-events")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(event.to_string()))
            .expect("request"),
    )
    .await;

    let error = response["error"].as_str().unwrap_or_default();
    assert!(
        error.contains("payment_account_binding is required"),
        "expected required payment_account_binding error, got {response}"
    );
    let ledger = state.payment_ledger.lock().await;
    assert!(
        ledger.get(payment_id).is_none(),
        "rejected event must not reach the ledger"
    );
}

#[tokio::test]
async fn agent_events_payment_sync_rejects_binding_with_mismatched_sender_address() {
    let (_dir, router, _token, _policy_engine, state) = build_test_app(20);
    let fixture = build_remote_binding_fixture();
    let payment_id = "payment-binding-address-mismatch-1";
    let mut event = payment_event_with_optional_extras(
        &state.agent_did,
        &fixture.remote_agent_did,
        payment_id,
        "12D3KooRemotePeer",
        None,
        Some(&fixture.binding),
    );
    event["event"]["event_type"] = json!("payment_update");
    event["event"]["payload"]["agent_envelope"]["message"]["message_kind"] =
        json!("payment_authorized");
    event["event"]["payload"]["agent_envelope"]["message"]["payment"]["status"] =
        json!("authorized");
    event["event"]["payload"]["agent_envelope"]["message"]["payment"]["sender_address"] =
        json!("0x0000000000000000000000000000000000000003");
    sign_payment_event_with_wallet_key(
        &mut event,
        &fixture.keystore,
        &fixture.agent_key_handle,
        &fixture.remote_agent_did,
    );

    let response = request_json(
        router,
        Request::post("/agent-events")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(event.to_string()))
            .expect("request"),
    )
    .await;

    let error = response["error"].as_str().unwrap_or_default();
    assert!(
        error.contains("payment_address does not match"),
        "expected sender address mismatch error, got {response}"
    );
    let ledger = state.payment_ledger.lock().await;
    assert!(
        ledger.get(payment_id).is_none(),
        "rejected event must not reach the ledger"
    );
}
