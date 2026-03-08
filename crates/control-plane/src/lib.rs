mod auth;
mod autonomy;
pub mod routes {
    pub(crate) mod civilization;
    pub(crate) mod core;
    pub(crate) mod governance;
    pub(crate) mod mailbox;
    pub(crate) mod missions;
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
        .merge(core_router())
        .merge(civilization_router())
        .merge(governance_router())
        .merge(mailbox_router())
        .merge(policy_router())
        .with_state(state)
}

fn core_router() -> Router<ControlPlaneState> {
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
        .route("/v1/autonomy/tick", post(routes::core::autonomy_tick))
        .route("/v1/audit", get(routes::core::audit_recent))
        .route("/v1/stream", get(routes::core::stream))
}

fn civilization_router() -> Router<ControlPlaneState> {
    Router::new()
        .route(
            "/v1/civilization/bootstrap-character",
            post(routes::civilization::bootstrap_character),
        )
        .route(
            "/v1/civilization/public-identity",
            get(routes::civilization::public_identity)
                .post(routes::civilization::public_identity_upsert),
        )
        .route(
            "/v1/civilization/controller-binding",
            get(routes::civilization::controller_binding)
                .post(routes::civilization::controller_binding_upsert),
        )
        .route(
            "/v1/civilization/profile",
            get(routes::civilization::citizen_profile)
                .post(routes::civilization::citizen_profile_upsert),
        )
        .route(
            "/v1/civilization/metrics",
            get(routes::civilization::civilization_metrics),
        )
        .route(
            "/v1/civilization/emergencies",
            get(routes::civilization::civilization_emergencies),
        )
        .route(
            "/v1/civilization/briefing",
            get(routes::civilization::civilization_briefing),
        )
        .route("/v1/world/zones", get(routes::civilization::world_zones))
        .route(
            "/v1/world/events",
            get(routes::civilization::world_events).post(routes::civilization::world_event_publish),
        )
        .route(
            "/v1/world/events/generate",
            post(routes::civilization::world_event_generate),
        )
        .route(
            "/v1/missions",
            get(routes::missions::mission_list).post(routes::missions::mission_publish),
        )
        .route("/v1/missions/claim", post(routes::missions::mission_claim))
        .route(
            "/v1/missions/complete",
            post(routes::missions::mission_complete),
        )
        .route(
            "/v1/missions/settle",
            post(routes::missions::mission_settle),
        )
}

fn governance_router() -> Router<ControlPlaneState> {
    Router::new()
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
            "/v1/governance/treasury/fund",
            post(routes::governance::governance_fund_treasury),
        )
        .route(
            "/v1/governance/treasury/spend",
            post(routes::governance::governance_spend_treasury),
        )
        .route(
            "/v1/governance/stability",
            post(routes::governance::governance_adjust_stability),
        )
        .route(
            "/v1/governance/recall",
            post(routes::governance::governance_start_recall),
        )
        .route(
            "/v1/governance/recall/resolve",
            post(routes::governance::governance_resolve_recall),
        )
        .route(
            "/v1/governance/custody",
            post(routes::governance::governance_enter_custody),
        )
        .route(
            "/v1/governance/custody/release",
            post(routes::governance::governance_release_custody),
        )
        .route(
            "/v1/governance/takeover",
            post(routes::governance::governance_hostile_takeover),
        )
}

fn mailbox_router() -> Router<ControlPlaneState> {
    Router::new()
        .route(
            "/v1/mailbox/messages",
            get(routes::mailbox::mailbox_fetch).post(routes::mailbox::mailbox_send),
        )
        .route("/v1/mailbox/ack", post(routes::mailbox::mailbox_ack))
}

fn policy_router() -> Router<ControlPlaneState> {
    Router::new()
        .route("/v1/policy/check", post(routes::policy::policy_check))
        .route("/v1/policy/pending", get(routes::policy::policy_pending))
        .route("/v1/policy/approve", post(routes::policy::policy_approve))
        .route("/v1/policy/revoke", post(routes::policy::policy_revoke))
        .route("/v1/policy/grants", get(routes::policy::policy_grants))
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
    use wattetheria_kernel::civilization::identities::{
        ControllerBindingRegistry, PublicIdentityRegistry,
    };
    use wattetheria_kernel::civilization::missions::MissionBoard;
    use wattetheria_kernel::civilization::profiles::CitizenRegistry;
    use wattetheria_kernel::civilization::world::WorldState;
    use wattetheria_kernel::event_log::EventLog;
    use wattetheria_kernel::governance::{
        GovernanceEngine, PlanetConstitutionTemplate, PlanetCreationRequest,
    };
    use wattetheria_kernel::identity::Identity;
    use wattetheria_kernel::mailbox::CrossSubnetMailbox;
    use wattetheria_kernel::policy_engine::PolicyEngine;
    use wattetheria_kernel::swarm_bridge::LegacyTaskEngineBridge;

    #[allow(clippy::too_many_lines)]
    fn build_test_app(
        rate_limit: usize,
    ) -> (tempfile::TempDir, Router, String, Arc<Mutex<PolicyEngine>>) {
        let dir = tempfile::tempdir().unwrap();
        let identity = Identity::new_random();
        let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
        let ledger_path = dir.path().join("ledger.json");
        let governance_state_path = dir.path().join("governance/state.json");
        let mailbox_state_path = dir.path().join("mailbox/state.json");
        let mission_board_state_path = dir.path().join("missions/state.json");
        let public_identity_registry_state_path =
            dir.path().join("civilization/public_identities.json");
        let controller_binding_registry_state_path =
            dir.path().join("civilization/controller_bindings.json");
        let citizen_registry_state_path = dir.path().join("civilization/profiles.json");
        let world_state_path = dir.path().join("world/state.json");

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
        governance.issue_license("agent-challenger", &identity.agent_id, "proof", 7);
        governance.lock_bond("agent-challenger", 150, 30);
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
            constitution_template: PlanetConstitutionTemplate::MigrantCouncil,
        };
        governance
            .create_planet(&planet_request, &approvals)
            .unwrap();
        governance.persist(&governance_state_path).unwrap();
        let governance_engine = Arc::new(Mutex::new(governance));

        let audit_log = AuditLog::new(dir.path().join("audit/control_plane.jsonl")).unwrap();
        let mailbox = Arc::new(Mutex::new(CrossSubnetMailbox::default()));
        let mission_board = Arc::new(Mutex::new(
            MissionBoard::load_or_new(&mission_board_state_path).unwrap(),
        ));
        let mut public_identity_registry =
            PublicIdentityRegistry::load_or_new(&public_identity_registry_state_path).unwrap();
        public_identity_registry.ensure_local_default(&identity.agent_id);
        public_identity_registry
            .persist(&public_identity_registry_state_path)
            .unwrap();
        let public_identity_registry = Arc::new(Mutex::new(public_identity_registry));
        let mut controller_binding_registry =
            ControllerBindingRegistry::load_or_new(&controller_binding_registry_state_path)
                .unwrap();
        controller_binding_registry.ensure_local_wattswarm(&identity.agent_id, &identity.agent_id);
        controller_binding_registry
            .persist(&controller_binding_registry_state_path)
            .unwrap();
        let controller_binding_registry = Arc::new(Mutex::new(controller_binding_registry));
        let citizen_registry = Arc::new(Mutex::new(
            CitizenRegistry::load_or_new(&citizen_registry_state_path).unwrap(),
        ));
        let world_state = Arc::new(Mutex::new(
            WorldState::load_or_new(&world_state_path).unwrap(),
        ));
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
            mission_board,
            mission_board_state_path,
            public_identity_registry,
            public_identity_registry_state_path,
            controller_binding_registry,
            controller_binding_registry_state_path,
            citizen_registry,
            citizen_registry_state_path,
            world_state,
            world_state_path,
            brain_engine: Arc::new(BrainEngine::from_config(&BrainProviderConfig::Rules)),
            audit_log,
            rate_limiter: Arc::new(RateLimiter::new(rate_limit, 60)),
            stream_tx,
        };

        (dir, app(state), token, policy_engine)
    }

    async fn request_json(app: Router, request: axum::http::Request<axum::body::Body>) -> Value {
        let response = app.oneshot(request).await.unwrap();
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap()
    }

    async fn authed_get_json(app: Router, token: &str, uri: &str) -> Value {
        request_json(
            app,
            axum::http::Request::builder()
                .uri(uri)
                .header("authorization", format!("Bearer {token}"))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
    }

    async fn authed_post(app: Router, token: &str, uri: &str, body: Value) -> StatusCode {
        app.oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri(uri)
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(axum::body::Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap()
        .status()
    }

    async fn authed_post_json(app: Router, token: &str, uri: &str, body: Value) -> Value {
        request_json(
            app,
            axum::http::Request::builder()
                .method("POST")
                .uri(uri)
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(axum::body::Body::from(body.to_string()))
                .unwrap(),
        )
        .await
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
            "subject": "controller:test",
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

    #[tokio::test]
    async fn civilization_profile_and_metrics_flow_works() {
        let (_dir, app, token, _) = build_test_app(20);

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
        assert_eq!(
            state_json["identity"]["public_identity"]["public_id"].as_str(),
            state_json["agent_id"].as_str()
        );
        let agent_id = state_json["agent_id"].as_str().unwrap();

        let profile_body = json!({
            "agent_id": agent_id,
            "faction": "order",
            "role": "operator",
            "strategy": "balanced"
        });
        let upsert_resp = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/civilization/profile")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(profile_body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(upsert_resp.status(), StatusCode::OK);

        let profile_resp = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/v1/civilization/profile")
                    .header("authorization", format!("Bearer {token}"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(profile_resp.status(), StatusCode::OK);
        let profile_json: Value =
            serde_json::from_slice(&profile_resp.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(profile_json["profile"]["role"], "operator");

        let metrics_resp = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/v1/civilization/metrics")
                    .header("authorization", format!("Bearer {token}"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(metrics_resp.status(), StatusCode::OK);
        let metrics_json: Value =
            serde_json::from_slice(&metrics_resp.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert!(
            metrics_json["metrics"]["total_influence"].as_i64().unwrap() >= 3,
            "expected profile bonus to affect influence"
        );
        assert_eq!(
            metrics_json["public_memory_owner"]["controller_id"].as_str(),
            Some(agent_id)
        );
    }

    #[tokio::test]
    async fn public_identity_and_controller_binding_flow_works() {
        let (dir, app, token, _) = build_test_app(20);

        let state_json = authed_get_json(app.clone(), &token, "/v1/state").await;
        let agent_id = state_json["agent_id"].as_str().unwrap();

        let default_identity =
            authed_get_json(app.clone(), &token, "/v1/civilization/public-identity").await;
        assert_eq!(
            default_identity["public_identity"]["public_id"].as_str(),
            Some(agent_id)
        );
        assert_eq!(
            default_identity["public_memory_owner"]["controller_id"].as_str(),
            Some(agent_id)
        );

        let default_binding =
            authed_get_json(app.clone(), &token, "/v1/civilization/controller-binding").await;
        assert_eq!(
            default_binding["controller_binding"]["controller_kind"].as_str(),
            Some("local_wattswarm")
        );

        let public_identity_status = authed_post(
            app.clone(),
            &token,
            "/v1/civilization/public-identity",
            json!({
                "public_id": "citizen-alpha",
                "display_name": "Citizen Alpha",
                "legacy_agent_id": agent_id,
                "active": true
            }),
        )
        .await;
        assert_eq!(public_identity_status, StatusCode::OK);

        let binding_status = authed_post(
            app.clone(),
            &token,
            "/v1/civilization/controller-binding",
            json!({
                "public_id": "citizen-alpha",
                "controller_kind": "external_runtime",
                "controller_ref": "openclaw://alpha",
                "controller_node_id": null,
                "ownership_scope": "external",
                "active": true
            }),
        )
        .await;
        assert_eq!(binding_status, StatusCode::OK);

        let fetched_identity = authed_get_json(
            app.clone(),
            &token,
            "/v1/civilization/public-identity?public_id=citizen-alpha",
        )
        .await;
        assert_eq!(
            fetched_identity["public_identity"]["display_name"].as_str(),
            Some("Citizen Alpha")
        );

        let fetched_binding = authed_get_json(
            app,
            &token,
            "/v1/civilization/controller-binding?public_id=citizen-alpha",
        )
        .await;
        assert_eq!(
            fetched_binding["controller_binding"]["controller_ref"].as_str(),
            Some("openclaw://alpha")
        );
        assert_eq!(
            fetched_binding["public_memory_owner"]["public_id"].as_str(),
            Some("citizen-alpha")
        );

        let persisted_identities = PublicIdentityRegistry::load_or_new(
            dir.path().join("civilization/public_identities.json"),
        )
        .unwrap();
        assert!(persisted_identities.get("citizen-alpha").is_some());

        let persisted_bindings = ControllerBindingRegistry::load_or_new(
            dir.path().join("civilization/controller_bindings.json"),
        )
        .unwrap();
        assert!(persisted_bindings.get("citizen-alpha").is_some());
    }

    #[tokio::test]
    async fn world_event_publish_and_query_works() {
        let (_dir, app, token, _) = build_test_app(20);
        let state_json = authed_get_json(app.clone(), &token, "/v1/state").await;
        let agent_id = state_json["agent_id"].as_str().unwrap();

        let publish_body = json!({
            "category": "economic",
            "zone_id": "genesis-core",
            "title": "Power Shortage",
            "description": "Grid instability is driving up maintenance demand.",
            "severity": 4,
            "expires_at": null,
            "tags": ["market", "supply"]
        });
        let publish_resp = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/world/events")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(publish_body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(publish_resp.status(), StatusCode::OK);

        let zones_resp = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/v1/world/zones")
                    .header("authorization", format!("Bearer {token}"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(zones_resp.status(), StatusCode::OK);
        let zones_json: Value =
            serde_json::from_slice(&zones_resp.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert!(zones_json.as_array().unwrap().len() >= 3);

        let events_resp = app
            .oneshot(
                axum::http::Request::builder()
                    .uri("/v1/world/events?zone_id=genesis-core")
                    .header("authorization", format!("Bearer {token}"))
                    .body(axum::body::Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(events_resp.status(), StatusCode::OK);
        let events_json: Value =
            serde_json::from_slice(&events_resp.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        assert_eq!(events_json["events"].as_array().unwrap().len(), 1);
        assert_eq!(events_json["events"][0]["title"], "Power Shortage");
        assert_eq!(
            events_json["public_memory_owner"]["controller_id"].as_str(),
            Some(agent_id)
        );
    }

    #[tokio::test]
    async fn mission_lifecycle_settles_and_funds_treasury() {
        let (dir, app, token, _) = build_test_app(30);

        let publish_body = json!({
            "title": "Stabilize the relay",
            "description": "Restore uptime on the frontier relay.",
            "publisher": "planet-test",
            "publisher_kind": "planetary_government",
            "domain": "security",
            "subnet_id": "planet-test",
            "zone_id": "frontier-ring",
            "required_role": "enforcer",
            "required_faction": null,
            "reward": {
                "agent_watt": 120,
                "reputation": 8,
                "capacity": 2,
                "treasury_share_watt": 30
            },
            "payload": {"objective": "relay_repair"}
        });
        let publish_resp = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/missions")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(publish_body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(publish_resp.status(), StatusCode::CREATED);
        let publish_json: Value =
            serde_json::from_slice(&publish_resp.into_body().collect().await.unwrap().to_bytes())
                .unwrap();
        let mission_id = publish_json["mission_id"].as_str().unwrap().to_string();

        for (uri, agent_id) in [
            ("/v1/missions/claim", "agent-enforcer"),
            ("/v1/missions/complete", "agent-enforcer"),
        ] {
            let resp = app
                .clone()
                .oneshot(
                    axum::http::Request::builder()
                        .method("POST")
                        .uri(uri)
                        .header("authorization", format!("Bearer {token}"))
                        .header("content-type", "application/json")
                        .body(axum::body::Body::from(
                            json!({
                                "mission_id": mission_id,
                                "agent_id": agent_id
                            })
                            .to_string(),
                        ))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
        }

        let settle_resp = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/missions/settle")
                    .header("authorization", format!("Bearer {token}"))
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(
                        json!({"mission_id": mission_id}).to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(settle_resp.status(), StatusCode::OK);

        let list_resp = app
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .uri("/v1/missions?status=settled")
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
        assert_eq!(list_json[0]["status"], "settled");

        let persisted =
            GovernanceEngine::load_or_new(dir.path().join("governance/state.json")).unwrap();
        let planet = persisted.list_planets().remove(0);
        assert_eq!(planet.treasury_watt, 30);
    }

    #[tokio::test]
    async fn governance_lifecycle_endpoints_work() {
        let (dir, app, token, _) = build_test_app(40);
        let state_json = authed_get_json(app.clone(), &token, "/v1/state").await;
        let agent_id = state_json["agent_id"].as_str().unwrap();

        for (uri, body) in [
            (
                "/v1/governance/stability",
                json!({"subnet_id":"planet-test","delta":-80}),
            ),
            (
                "/v1/governance/recall",
                json!({
                    "subnet_id":"planet-test",
                    "initiated_by": agent_id,
                    "reason":"stability collapse",
                    "threshold":25
                }),
            ),
            (
                "/v1/governance/recall/resolve",
                json!({
                    "subnet_id":"planet-test",
                    "successor":"agent-challenger",
                    "min_bond":100
                }),
            ),
            (
                "/v1/governance/custody",
                json!({
                    "subnet_id":"planet-test",
                    "reason":"civil emergency",
                    "managed_by":"neutral-admin"
                }),
            ),
            (
                "/v1/governance/takeover",
                json!({
                    "subnet_id":"planet-test",
                    "challenger":"agent-challenger",
                    "reason":"secured orbit",
                    "min_bond":100
                }),
            ),
        ] {
            assert_eq!(
                authed_post(app.clone(), &token, uri, body).await,
                StatusCode::OK
            );
        }

        let persisted =
            GovernanceEngine::load_or_new(dir.path().join("governance/state.json")).unwrap();
        let planet = persisted.planet("planet-test").unwrap();
        assert_eq!(planet.creator, "agent-challenger");
    }

    #[tokio::test]
    async fn civilization_briefing_and_generated_world_events_work() {
        let (_dir, app, token, _) = build_test_app(40);
        let state_json = authed_get_json(app.clone(), &token, "/v1/state").await;
        let agent_id = state_json["agent_id"].as_str().unwrap();

        for (uri, body, expected) in [
            (
                "/v1/civilization/profile",
                json!({
                    "agent_id": agent_id,
                    "faction": "order",
                    "role": "operator",
                    "strategy": "conservative",
                    "home_subnet_id": "planet-test",
                    "home_zone_id": "genesis-core"
                }),
                StatusCode::OK,
            ),
            (
                "/v1/governance/stability",
                json!({"subnet_id":"planet-test","delta":-60}),
                StatusCode::OK,
            ),
            (
                "/v1/missions",
                json!({
                    "title": "Defend gate",
                    "description": "Interdict raiders",
                    "publisher": "planet-test",
                    "publisher_kind": "planetary_government",
                    "domain": "security",
                    "subnet_id": "planet-test",
                    "zone_id": "genesis-core",
                    "required_role": "enforcer",
                    "required_faction": null,
                    "reward": {"agent_watt": 20, "reputation": 3, "capacity": 1, "treasury_share_watt": 2},
                    "payload": {}
                }),
                StatusCode::CREATED,
            ),
            (
                "/v1/world/events/generate",
                json!({"max_events": 3}),
                StatusCode::OK,
            ),
        ] {
            assert_eq!(authed_post(app.clone(), &token, uri, body).await, expected);
        }

        let emergencies_json =
            authed_get_json(app.clone(), &token, "/v1/civilization/emergencies").await;
        assert!(
            !emergencies_json["emergencies"]
                .as_array()
                .unwrap()
                .is_empty()
        );

        let briefing_json =
            authed_get_json(app.clone(), &token, "/v1/civilization/briefing?hours=12").await;
        assert!(
            briefing_json["briefing"]["emergencies"]
                .as_array()
                .is_some()
        );
        assert_eq!(
            briefing_json["public_memory_owner"]["controller_id"].as_str(),
            Some(agent_id)
        );
    }

    #[tokio::test]
    async fn bootstrap_character_returns_unified_identity_bundle_and_public_memory_owner() {
        let (dir, app, token, _) = build_test_app(20);
        let state_json = authed_get_json(app.clone(), &token, "/v1/state").await;
        let agent_id = state_json["agent_id"].as_str().unwrap();

        let bootstrap_json = authed_post_json(
            app.clone(),
            &token,
            "/v1/civilization/bootstrap-character",
            json!({
                "public_id": "captain-aurora",
                "display_name": "Captain Aurora",
                "faction": "freeport",
                "role": "broker",
                "strategy": "balanced",
                "home_subnet_id": "planet-test",
                "home_zone_id": "genesis-core"
            }),
        )
        .await;

        assert_eq!(
            bootstrap_json["public_identity"]["public_id"].as_str(),
            Some("captain-aurora")
        );
        assert_eq!(
            bootstrap_json["controller_binding"]["controller_kind"].as_str(),
            Some("local_wattswarm")
        );
        assert_eq!(
            bootstrap_json["profile"]["agent_id"].as_str(),
            Some(agent_id)
        );
        assert_eq!(
            bootstrap_json["public_memory_owner"]["public_id"].as_str(),
            Some("captain-aurora")
        );

        let events = EventLog::new(dir.path().join("events.jsonl"))
            .unwrap()
            .get_all()
            .unwrap();
        let bootstrap_event = events
            .iter()
            .find(|event| event.event_type == "CIVILIZATION_CHARACTER_BOOTSTRAPPED")
            .unwrap();
        assert_eq!(
            bootstrap_event.payload["public_memory"]["public_id"].as_str(),
            Some("captain-aurora")
        );
        assert_eq!(
            bootstrap_event.payload["public_memory"]["controller_id"].as_str(),
            Some(agent_id)
        );
    }
}
