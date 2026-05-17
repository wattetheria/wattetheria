use super::*;
use crate::diagnostics::{DiagnosticEvent, record_diagnostic};

struct DiagnosticsOnlyBridge {
    snapshot: SwarmDiagnosticsSnapshot,
}

#[async_trait::async_trait]
impl SwarmBridge for DiagnosticsOnlyBridge {
    async fn agent_view(&self, agent_did: &str) -> anyhow::Result<SwarmAgentView> {
        Ok(SwarmAgentView {
            agent_did: agent_did.to_owned(),
            stats: AgentStats::default(),
        })
    }

    async fn diagnostics(
        &self,
        _query: SwarmDiagnosticsQuery,
    ) -> anyhow::Result<SwarmDiagnosticsSnapshot> {
        Ok(self.snapshot.clone())
    }
}

#[tokio::test]
async fn client_diagnostics_lists_and_filters_local_node_logs() {
    let (_dir, app, token, _, state) = build_test_app(20);
    record_diagnostic(
        &state.data_dir,
        DiagnosticEvent::new(
            "warn",
            "wattswarm.network_bridge",
            "gossip",
            "event.ingest",
            "duplicate_ignored",
            "remote event already exists locally",
        )
        .event_id(Some("evt-duplicate".to_owned()))
        .source_node_id(Some("node-a".to_owned()))
        .object("task", Some("task-123".to_owned()))
        .details(json!({
            "scope_hint": "node:publisher",
            "event_kind": "TaskClaimed",
        })),
    );
    record_diagnostic(
        &state.data_dir,
        DiagnosticEvent::new(
            "info",
            "wattetheria.control_plane",
            "agent_event",
            "callback.received",
            "accepted",
            "agent event callback received",
        )
        .event_id(Some("evt-agent".to_owned()))
        .object("agent_event", Some("task-999".to_owned())),
    );

    let all = authed_get_json(app.clone(), &token, "/v1/client/diagnostics?limit=10").await;
    let entries = all["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0]["event_id"].as_str(), Some("evt-agent"));
    assert_eq!(entries[1]["object_id"].as_str(), Some("task-123"));

    let filtered = authed_get_json(
        app,
        &token,
        "/v1/client/diagnostics?level=warn&component=wattswarm.network_bridge&object_id=task-123",
    )
    .await;
    let entries = filtered["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["status"].as_str(), Some("duplicate_ignored"));
}

#[tokio::test]
async fn client_wattswarm_diagnostics_proxies_swarm_bridge_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(DiagnosticsOnlyBridge {
        snapshot: SwarmDiagnosticsSnapshot {
            ok: true,
            generated_at: "2026-04-30T00:00:00Z".to_owned(),
            network_service_started: true,
            snapshot: Some(json!({
                "local_iroh_endpoint_id": "iroh-local",
                "connected_peer_count": 1,
                "subscribed_scopes": ["node:iroh-local"],
            })),
            diagnostics: vec![json!({
                "id": "diag-1",
                "timestamp_ms": 123,
                "level": "info",
                "component": "wattswarm.network_bridge",
                "category": "gossip",
                "phase": "publish.event",
                "status": "ok",
                "message": "published local event",
                "event_id": "evt-1",
                "object_id": "task-1",
            })],
        },
    });
    let (_dir, app, token, _, _state) =
        build_test_app_with_bridge(20, dir, identity, event_log, bridge as Arc<dyn SwarmBridge>);

    let payload = authed_get_json(
        app,
        &token,
        "/v1/client/wattswarm-diagnostics?limit=10&search=task-1",
    )
    .await;
    assert_eq!(payload["ok"].as_bool(), Some(true));
    assert_eq!(payload["network_service_started"].as_bool(), Some(true));
    assert_eq!(
        payload["diagnostics"][0]["phase"].as_str(),
        Some("publish.event")
    );
    assert_eq!(
        payload["snapshot"]["local_iroh_endpoint_id"].as_str(),
        Some("iroh-local")
    );
}

#[tokio::test]
async fn agent_event_callback_writes_diagnostics() {
    let (_dir, app, _token, _, state) = build_test_app(20);
    let response = app
        .oneshot(
            axum::http::Request::post("/agent-events")
                .header("content-type", "application/json")
                .body(axum::body::Body::from(
                    json!({
                        "event": {
                            "event_id": "evt-task-claim",
                            "event_type": "task_claim_received",
                            "source_kind": "task_lifecycle",
                            "source_node_id": "claimer-node",
                            "payload": {
                                "task_id": "task-claim-1",
                                "event_kind": "task_claimed"
                            },
                            "requires_commit": false,
                            "allowed_actions": ["inspect_task", "decide_claim"],
                            "correlation_id": "task-claim-1",
                            "dedupe_key": "task_claim:task-claim-1:exec-1",
                            "created_at": 123
                        }
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let entries = crate::diagnostics::list_diagnostics(
        &state.data_dir,
        &crate::diagnostics::DiagnosticFilter {
            event_id: Some("evt-task-claim".to_owned()),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(
        entries
            .iter()
            .any(|entry| entry.phase == "callback.received")
    );
    assert!(
        entries
            .iter()
            .any(|entry| entry.phase == "callback.responded")
    );
    let received = entries
        .iter()
        .find(|entry| entry.phase == "callback.received")
        .expect("callback.received diagnostic");
    assert_eq!(
        received.details["payload"]["callback_request"]["event"]["event_id"].as_str(),
        Some("evt-task-claim")
    );
    assert_eq!(
        received.details["payload"]["brain_input"]["event_type"].as_str(),
        Some("task_claim_received")
    );
    let responded = entries
        .iter()
        .find(|entry| entry.phase == "callback.responded")
        .expect("callback.responded diagnostic");
    assert_eq!(
        responded.details["payload"]["callback_response"]["ok"].as_bool(),
        Some(true)
    );
    assert_eq!(
        responded.details["payload"]["callback_response"]["detail"].as_str(),
        Some("no decision for task_claim_received")
    );
    assert!(
        entries
            .iter()
            .any(|entry| entry.phase.starts_with("decision."))
    );
}

#[tokio::test]
async fn agent_action_commit_writes_event_bus_diagnostics() {
    let (_dir, app, token, _, state) = build_test_app(20);
    let response = authed_post_json(
        app,
        &token,
        "/v1/agent-actions/commit",
        json!({
            "event": {
                "event_id": "evt-unsupported-action",
                "event_type": "task_claim_received",
                "source_kind": "task_lifecycle",
                "source_node_id": "claimer-node",
                "target_agent_id": state.agent_did,
                "payload": {
                    "task_id": "task-unsupported-action",
                    "event_kind": "task_claimed"
                },
                "requires_commit": true
            },
            "decision": {
                "decision_id": "dec-unsupported-action",
                "action": "unsupported_action",
                "route": "wattetheria_commit",
                "payload": {}
            }
        }),
    )
    .await;
    assert_eq!(
        response["error"].as_str(),
        Some("unsupported agent action commit")
    );

    let entries = crate::diagnostics::list_diagnostics(
        &state.data_dir,
        &crate::diagnostics::DiagnosticFilter {
            event_id: Some("evt-unsupported-action".to_owned()),
            component: Some("wattetheria.event_bus".to_owned()),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(
        entries
            .iter()
            .any(|entry| entry.phase == "agent_action.commit.received")
    );
    assert!(
        entries
            .iter()
            .any(|entry| entry.phase == "agent_action.commit.routed")
    );
    assert!(
        entries
            .iter()
            .any(|entry| entry.phase == "agent_action.commit.failed")
    );
}
