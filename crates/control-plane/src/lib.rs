mod auth;
mod autonomy;
pub mod routes {
    pub(crate) mod core;
    pub(crate) mod governance;
    pub(crate) mod mailbox;
    pub(crate) mod policy;
}
mod state;

use anyhow::{Context, Result};
use axum::Router;
use axum::routing::{get, post};
use std::net::SocketAddr;

pub use autonomy::run_autonomy_tick_once;
pub use state::{ControlPlaneState, RateLimiter, StreamEvent};

pub fn app(state: ControlPlaneState) -> Router {
    Router::new()
        .route("/v1/health", get(routes::core::health))
        .route("/v1/state", get(routes::core::state_view))
        .route("/v1/events", get(routes::core::events))
        .route("/v1/events/export", get(routes::core::events_export))
        .route("/v1/night-shift", get(routes::core::night_shift))
        .route(
            "/v1/night-shift/humanized",
            get(routes::core::night_shift_humanized),
        )
        .route("/v1/actions", post(routes::core::actions))
        .route(
            "/v1/brain/propose-actions",
            get(routes::core::brain_propose_actions),
        )
        .route(
            "/v1/brain/plan-skill-calls",
            get(routes::core::brain_plan_skill_calls),
        )
        .route("/v1/autonomy/tick", post(routes::core::autonomy_tick))
        .route(
            "/v1/governance/planets",
            get(routes::governance::governance_planets),
        )
        .route(
            "/v1/governance/proposals",
            get(routes::governance::governance_proposals)
                .post(routes::governance::governance_create_proposal),
        )
        .route(
            "/v1/governance/proposals/vote",
            post(routes::governance::governance_vote_proposal),
        )
        .route(
            "/v1/governance/proposals/finalize",
            post(routes::governance::governance_finalize_proposal),
        )
        .route(
            "/v1/mailbox/messages",
            get(routes::mailbox::mailbox_fetch).post(routes::mailbox::mailbox_send),
        )
        .route("/v1/mailbox/ack", post(routes::mailbox::mailbox_ack))
        .route("/v1/policy/check", post(routes::policy::policy_check))
        .route("/v1/policy/pending", get(routes::policy::policy_pending))
        .route("/v1/policy/approve", post(routes::policy::policy_approve))
        .route("/v1/policy/revoke", post(routes::policy::policy_revoke))
        .route("/v1/policy/grants", get(routes::policy::policy_grants))
        .route("/v1/audit", get(routes::core::audit_recent))
        .route("/v1/stream", get(routes::core::stream))
        .with_state(state)
}

pub async fn serve_control_plane(state: ControlPlaneState, bind: SocketAddr) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .with_context(|| format!("bind control plane on {bind}"))?;
    axum::serve(listener, app(state))
        .await
        .context("serve control plane")
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;
    use chrono::Utc;
    use http_body_util::BodyExt;
    use serde_json::{Value, json};
    use std::sync::Arc;
    use tokio::sync::{Mutex, broadcast};
    use tower::ServiceExt;
    use wattetheria_kernel::audit::AuditLog;
    use wattetheria_kernel::brain::{BrainEngine, BrainProviderConfig};
    use wattetheria_kernel::capabilities::CapabilityPolicy;
    use wattetheria_kernel::event_log::EventLog;
    use wattetheria_kernel::governance::{GovernanceEngine, PlanetCreationRequest};
    use wattetheria_kernel::identity::Identity;
    use wattetheria_kernel::mailbox::CrossSubnetMailbox;
    use wattetheria_kernel::policy_engine::PolicyEngine;
    use wattetheria_kernel::swarm_bridge::LegacyTaskEngineBridge;

    fn build_test_app(
        rate_limit: usize,
    ) -> (tempfile::TempDir, Router, String, Arc<Mutex<PolicyEngine>>) {
        let dir = tempfile::tempdir().unwrap();
        let identity = Identity::new_random();
        let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
        let ledger_path = dir.path().join("ledger.json");
        let governance_state_path = dir.path().join("governance/state.json");
        let mailbox_state_path = dir.path().join("mailbox/state.json");

        let policy_engine = Arc::new(Mutex::new(
            PolicyEngine::load_or_new(
                dir.path().join("policy.json"),
                "test-session",
                CapabilityPolicy::default(),
            )
            .unwrap(),
        ));

        let mut governance = GovernanceEngine::default();
        governance.issue_license(&identity.agent_id, &identity.agent_id, "proof", 7);
        governance.lock_bond(&identity.agent_id, 100, 30);
        let signer = Identity::new_random();
        let created_at = Utc::now().timestamp();
        let approvals = vec![
            GovernanceEngine::sign_genesis(
                "planet-test",
                "Planet Test",
                &identity.agent_id,
                created_at,
                &identity,
            )
            .unwrap(),
            GovernanceEngine::sign_genesis(
                "planet-test",
                "Planet Test",
                &identity.agent_id,
                created_at,
                &signer,
            )
            .unwrap(),
        ];
        let planet_request = PlanetCreationRequest {
            subnet_id: "planet-test".to_string(),
            name: "Planet Test".to_string(),
            creator: identity.agent_id.clone(),
            created_at,
            tax_rate: 0.05,
            min_bond: 50,
            min_approvals: 2,
        };
        governance
            .create_planet(&planet_request, &approvals)
            .unwrap();
        governance.persist(&governance_state_path).unwrap();
        let governance_engine = Arc::new(Mutex::new(governance));

        let audit_log = AuditLog::new(dir.path().join("audit/control_plane.jsonl")).unwrap();
        let mailbox = Arc::new(Mutex::new(CrossSubnetMailbox::default()));
        let (stream_tx, _) = broadcast::channel(32);
        let token = "test-token".to_string();
        let bridge_event_log = event_log.clone();
        let bridge_identity = identity.clone();

        let state = ControlPlaneState {
            agent_id: identity.agent_id.clone(),
            identity,
            started_at: Utc::now().timestamp(),
            auth_token: token.clone(),
            event_log,
            swarm_bridge: Arc::new(LegacyTaskEngineBridge::new(
                wattetheria_kernel::task_engine::TaskEngine::new(bridge_event_log, bridge_identity),
                ledger_path,
            )),
            governance_engine,
            governance_state_path,
            policy_engine: policy_engine.clone(),
            mailbox,
            mailbox_state_path,
            brain_engine: Arc::new(BrainEngine::from_config(&BrainProviderConfig::Rules)),
            autonomy_skill_planner_enabled: true,
            audit_log,
            rate_limiter: Arc::new(RateLimiter::new(rate_limit, 60)),
            stream_tx,
        };

        (dir, app(state), token, policy_engine)
    }

    #[tokio::test]
    async fn state_requires_auth() {
        let (_dir, app, _token, _) = build_test_app(10);
        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/v1/state")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn policy_flow_pending_then_approve_once() {
        let (_dir, app, token, _policy) = build_test_app(20);

        let check_body = json!({
            "subject": "skill:test@0.1.0",
            "trust": "verified",
            "capability": "p2p.publish",
            "reason": "integration-test"
        });

        let first = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/policy/check")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(check_body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(first.status(), StatusCode::ACCEPTED);

        let first_json: Value =
            serde_json::from_slice(&first.into_body().collect().await.unwrap().to_bytes()).unwrap();
        let request_id = first_json["request_id"].as_str().unwrap().to_string();

        let approve_body = json!({
            "request_id": request_id,
            "approved_by": "operator",
            "scope": "once"
        });

        let approve = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/policy/approve")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(approve_body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(approve.status(), StatusCode::OK);

        let second = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/policy/check")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(check_body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(second.status(), StatusCode::OK);

        let third = app
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/policy/check")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(check_body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(third.status(), StatusCode::ACCEPTED);
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn governance_proposal_flow_works() {
        let (dir, app, token, _) = build_test_app(30);

        let state_resp = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/v1/state")
                    .header("authorization", format!("Bearer {token}"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(state_resp.status(), StatusCode::OK);
        let state_json: Value =
            serde_json::from_slice(&state_resp.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        let agent_id = state_json["agent_id"].as_str().unwrap().to_string();

        let create_body = json!({
            "subnet_id": "planet-test",
            "kind": "update_tax_rate",
            "payload": {"tax_rate": 0.09},
            "created_by": agent_id,
        });
        let create_resp = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/governance/proposals")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(create_body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(create_resp.status(), StatusCode::CREATED);
        let create_json: Value =
            serde_json::from_slice(&create_resp.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        let proposal_id = create_json["proposal_id"].as_str().unwrap().to_string();

        let vote_body = json!({
            "proposal_id": proposal_id,
            "voter": state_json["agent_id"],
            "approve": true,
        });
        let vote_resp = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/governance/proposals/vote")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(vote_body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(vote_resp.status(), StatusCode::OK);

        let finalize_body = json!({
            "proposal_id": create_json["proposal_id"],
            "min_votes_for": 1,
        });
        let finalize_resp = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/governance/proposals/finalize")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(finalize_body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(finalize_resp.status(), StatusCode::OK);
        let finalize_json: Value = serde_json::from_slice(
            &finalize_resp
                .into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes(),
        )
        .unwrap();
        assert_eq!(finalize_json["status"], "accepted");

        let list_resp = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/v1/governance/proposals?subnet_id=planet-test")
                    .header("authorization", format!("Bearer {token}"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(list_resp.status(), StatusCode::OK);
        let list_json: Value =
            serde_json::from_slice(&list_resp.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(list_json.as_array().unwrap().len(), 1);
        let persisted =
            GovernanceEngine::load_or_new(dir.path().join("governance/state.json")).unwrap();
        assert_eq!(persisted.list_proposals(Some("planet-test")).len(), 1);
    }

    #[tokio::test]
    async fn demo_action_persists_ledger_to_disk() {
        let (dir, app, token, _) = build_test_app(20);

        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/actions")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(
                        json!({"action": "task.run_demo_market"}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let ledger = wattetheria_kernel::swarm_bridge::LegacyTaskEngineBridge::load_ledger(
            dir.path().join("ledger.json"),
        )
        .unwrap();
        assert!(!ledger.is_empty());
        assert!(ledger.values().any(|stats| stats.watt > 0));
    }

    #[tokio::test]
    async fn mailbox_send_fetch_ack_persists() {
        let (dir, app, token, _) = build_test_app(30);

        let send_resp = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/mailbox/messages")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(
                        json!({
                            "to_agent": "agent-receiver",
                            "from_subnet": "planet-a",
                            "to_subnet": "planet-b",
                            "payload": {"kind": "offer", "price": 42}
                        })
                        .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(send_resp.status(), StatusCode::CREATED);
        let send_json: Value =
            serde_json::from_slice(&send_resp.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        let message_id = send_json["message_id"].as_str().unwrap().to_string();

        let fetch_resp = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/v1/mailbox/messages?subnet_id=planet-b")
                    .header("authorization", format!("Bearer {token}"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(fetch_resp.status(), StatusCode::OK);
        let fetch_json: Value =
            serde_json::from_slice(&fetch_resp.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(fetch_json.as_array().unwrap().len(), 1);

        let ack_resp = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/mailbox/ack")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(
                        json!({"subnet_id": "planet-b", "message_id": message_id}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(ack_resp.status(), StatusCode::OK);

        let persisted =
            CrossSubnetMailbox::load_or_new(dir.path().join("mailbox/state.json")).unwrap();
        assert!(persisted.fetch_for_subnet("planet-b").is_empty());
    }

    #[tokio::test]
    async fn events_export_is_public_for_recovery() {
        let (_dir, app, _token, _) = build_test_app(10);

        let response = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/v1/events/export")
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
    }
}
