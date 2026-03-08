mod auth;
mod autonomy;
pub mod routes {
    pub(crate) mod civilization;
    pub(crate) mod client;
    pub(crate) mod core;
    pub(crate) mod experience;
    pub(crate) mod game;
    pub(crate) mod governance;
    pub(crate) mod identity;
    pub(crate) mod mailbox;
    pub(crate) mod map;
    pub(crate) mod missions;
    pub(crate) mod organizations;
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
        .merge(client_router())
        .merge(game_router())
        .merge(map_router())
        .merge(civilization_router())
        .merge(governance_router())
        .merge(mailbox_router())
        .merge(policy_router())
        .with_state(state)
}

fn client_router() -> Router<ControlPlaneState> {
    Router::new()
        .route(
            "/v1/civilization/characters",
            get(routes::client::client_characters),
        )
        .route("/v1/dashboard/home", get(routes::client::dashboard_home))
        .route("/v1/missions/my", get(routes::client::my_missions))
        .route("/v1/governance/my", get(routes::client::my_governance))
        .route(
            "/v1/catalog/bootstrap",
            get(routes::client::bootstrap_catalog),
        )
        .route(
            "/v1/organizations/my",
            get(routes::organizations::my_organizations),
        )
}

fn game_router() -> Router<ControlPlaneState> {
    Router::new()
        .route("/v1/game/catalog", get(routes::game::game_catalog))
        .route("/v1/game/status", get(routes::game::game_status))
        .route("/v1/game/onboarding", get(routes::game::game_onboarding))
        .route(
            "/v1/game/mission-pack",
            get(routes::game::game_mission_pack),
        )
        .route(
            "/v1/game/starter-missions",
            get(routes::game::game_starter_missions),
        )
        .route(
            "/v1/game/starter-missions/bootstrap",
            post(routes::game::bootstrap_starter_missions_route),
        )
        .route(
            "/v1/game/mission-pack/bootstrap",
            post(routes::game::bootstrap_mission_pack_route),
        )
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

fn map_router() -> Router<ControlPlaneState> {
    Router::new()
        .route("/v1/galaxy/map", get(routes::map::galaxy_map))
        .route("/v1/galaxy/maps", get(routes::map::galaxy_maps))
        .route(
            "/v1/galaxy/travel/state",
            get(routes::map::galaxy_travel_state),
        )
        .route(
            "/v1/galaxy/travel/options",
            get(routes::map::galaxy_travel_options),
        )
        .route(
            "/v1/galaxy/travel/plan",
            get(routes::map::galaxy_travel_plan),
        )
        .route(
            "/v1/galaxy/travel/depart",
            post(routes::map::galaxy_travel_depart),
        )
        .route(
            "/v1/galaxy/travel/arrive",
            post(routes::map::galaxy_travel_arrive),
        )
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
        .merge(organization_civilization_router())
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
        .route("/v1/galaxy/zones", get(routes::civilization::galaxy_zones))
        .route("/v1/world/zones", get(routes::civilization::galaxy_zones))
        .route(
            "/v1/galaxy/events",
            get(routes::civilization::galaxy_events)
                .post(routes::civilization::galaxy_event_publish),
        )
        .route(
            "/v1/world/events",
            get(routes::civilization::galaxy_events)
                .post(routes::civilization::galaxy_event_publish),
        )
        .route(
            "/v1/galaxy/events/generate",
            post(routes::civilization::galaxy_event_generate),
        )
        .route(
            "/v1/world/events/generate",
            post(routes::civilization::galaxy_event_generate),
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

fn organization_civilization_router() -> Router<ControlPlaneState> {
    Router::new()
        .route(
            "/v1/civilization/organizations",
            get(routes::organizations::list_organizations)
                .post(routes::organizations::create_organization),
        )
        .route(
            "/v1/civilization/organizations/members",
            post(routes::organizations::upsert_organization_member),
        )
        .route(
            "/v1/civilization/organizations/proposals",
            get(routes::organizations::list_organization_governance)
                .post(routes::organizations::create_organization_proposal),
        )
        .route(
            "/v1/civilization/organizations/proposals/vote",
            post(routes::organizations::vote_organization_proposal),
        )
        .route(
            "/v1/civilization/organizations/proposals/finalize",
            post(routes::organizations::finalize_organization_proposal),
        )
        .route(
            "/v1/civilization/organizations/charters",
            post(routes::organizations::submit_subnet_charter_application),
        )
        .route(
            "/v1/civilization/organizations/missions",
            post(routes::organizations::publish_organization_mission),
        )
        .route(
            "/v1/civilization/organizations/treasury/fund",
            post(routes::organizations::fund_organization_treasury),
        )
        .route(
            "/v1/civilization/organizations/treasury/spend",
            post(routes::organizations::spend_organization_treasury),
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
    use wattetheria_kernel::civilization::galaxy::GalaxyState;
    use wattetheria_kernel::civilization::identities::{
        ControllerBindingRegistry, PublicIdentityRegistry,
    };
    use wattetheria_kernel::civilization::missions::MissionBoard;
    use wattetheria_kernel::civilization::organizations::OrganizationRegistry;
    use wattetheria_kernel::civilization::profiles::CitizenRegistry;
    use wattetheria_kernel::event_log::EventLog;
    use wattetheria_kernel::governance::{
        GovernanceEngine, PlanetConstitutionTemplate, PlanetCreationRequest,
    };
    use wattetheria_kernel::identity::Identity;
    use wattetheria_kernel::mailbox::CrossSubnetMailbox;
    use wattetheria_kernel::map::registry::GalaxyMapRegistry;
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
        let organization_registry_state_path = dir.path().join("civilization/organizations.json");
        let galaxy_state_path = dir.path().join("galaxy/state.json");
        let galaxy_map_registry_state_path = dir.path().join("galaxy/maps.json");
        let travel_state_registry_state_path = dir.path().join("galaxy/travel_state.json");

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
        let organization_registry = Arc::new(Mutex::new(
            OrganizationRegistry::load_or_new(&organization_registry_state_path).unwrap(),
        ));
        let galaxy_state_loaded = GalaxyState::load_or_new(&galaxy_state_path).unwrap();
        let mut galaxy_map_registry_loaded =
            GalaxyMapRegistry::load_or_new(&galaxy_map_registry_state_path).unwrap();
        galaxy_map_registry_loaded
            .ensure_default_genesis_map(&galaxy_state_loaded.zones())
            .unwrap();
        galaxy_map_registry_loaded
            .persist(&galaxy_map_registry_state_path)
            .unwrap();
        let default_map = galaxy_map_registry_loaded.get("genesis-base").unwrap();
        let galaxy_state = Arc::new(Mutex::new(galaxy_state_loaded));
        let galaxy_map_registry = Arc::new(Mutex::new(galaxy_map_registry_loaded));
        let mut travel_state_registry =
            wattetheria_kernel::map::state::TravelStateRegistry::load_or_new(
                &travel_state_registry_state_path,
            )
            .unwrap();
        let default_position =
            wattetheria_kernel::map::state::resolve_anchor_position(&default_map, None, None)
                .unwrap();
        let _ = travel_state_registry.ensure_position(
            &identity.agent_id,
            &identity.agent_id,
            default_position,
        );
        travel_state_registry
            .persist(&travel_state_registry_state_path)
            .unwrap();
        let travel_state_registry = Arc::new(Mutex::new(travel_state_registry));
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
            organization_registry,
            organization_registry_state_path,
            galaxy_state,
            galaxy_state_path,
            galaxy_map_registry,
            galaxy_map_registry_state_path,
            travel_state_registry,
            travel_state_registry_state_path,
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

    async fn bootstrap_broker_character(app: Router, token: &str, agent_id: &str) {
        authed_post_json(
            app,
            token,
            "/v1/civilization/bootstrap-character",
            json!({
                "public_id": "captain-aurora",
                "display_name": "Captain Aurora",
                "legacy_agent_id": agent_id,
                "faction": "freeport",
                "role": "broker",
                "strategy": "balanced",
                "home_subnet_id": "planet-test",
                "home_zone_id": "genesis-core"
            }),
        )
        .await;
    }

    async fn bootstrap_broker_game(app: Router, token: &str, agent_id: &str) -> Value {
        bootstrap_broker_character(app.clone(), token, agent_id).await;
        let starter_bootstrap = authed_post_json(
            app,
            token,
            "/v1/game/starter-missions/bootstrap",
            json!({"public_id": "captain-aurora"}),
        )
        .await;
        assert_eq!(starter_bootstrap["created"].as_array().unwrap().len(), 2);
        starter_bootstrap
    }

    struct TradeMissionSpec<'a> {
        title: &'a str,
        description: &'a str,
        reward_watt: u64,
        reward_reputation: i64,
        objective: &'a str,
        required_faction: Option<&'a str>,
        subnet_id: Option<&'a str>,
        zone_id: Option<&'a str>,
    }

    async fn publish_trade_mission(app: Router, token: &str, spec: TradeMissionSpec<'_>) -> Value {
        authed_post_json(
            app,
            token,
            "/v1/missions",
            json!({
                "title": spec.title,
                "description": spec.description,
                "publisher": "planet-test",
                "publisher_kind": "planetary_government",
                "domain": "trade",
                "subnet_id": spec.subnet_id,
                "zone_id": spec.zone_id,
                "required_role": "broker",
                "required_faction": spec.required_faction,
                "reward": {
                    "agent_watt": spec.reward_watt,
                    "reputation": spec.reward_reputation,
                    "capacity": 1,
                    "treasury_share_watt": 5
                },
                "payload": {"objective": spec.objective}
            }),
        )
        .await
    }

    async fn settle_trade_mission_for_agent(app: Router, token: &str, agent_id: &str) -> Value {
        let mission = publish_trade_mission(
            app.clone(),
            token,
            TradeMissionSpec {
                title: "Bootstrap exchange route",
                description: "Seed a frontier liquidity lane",
                reward_watt: 40,
                reward_reputation: 4,
                objective: "seed-route",
                required_faction: Some("freeport"),
                subnet_id: Some("planet-test"),
                zone_id: Some("genesis-core"),
            },
        )
        .await;
        let mission_id = mission["mission_id"].as_str().unwrap();
        let _ = authed_post_json(
            app.clone(),
            token,
            "/v1/missions/claim",
            json!({"mission_id": mission_id, "agent_id": agent_id}),
        )
        .await;
        let _ = authed_post_json(
            app.clone(),
            token,
            "/v1/missions/complete",
            json!({"mission_id": mission_id, "agent_id": agent_id}),
        )
        .await;
        let _ = authed_post_json(
            app,
            token,
            "/v1/missions/settle",
            json!({"mission_id": mission_id}),
        )
        .await;
        mission
    }

    async fn seed_client_view_missions(app: Router, token: &str, agent_id: &str) {
        let eligible_open = publish_trade_mission(
            app.clone(),
            token,
            TradeMissionSpec {
                title: "Route liquidity relay",
                description: "Rebalance frontier markets",
                reward_watt: 50,
                reward_reputation: 4,
                objective: "rebalance",
                required_faction: Some("freeport"),
                subnet_id: Some("planet-test"),
                zone_id: Some("genesis-core"),
            },
        )
        .await;
        assert_eq!(eligible_open["status"].as_str(), Some("open"));

        let travel_required = publish_trade_mission(
            app.clone(),
            token,
            TradeMissionSpec {
                title: "Deep watch exchange run",
                description: "Deliver market telemetry into deep space",
                reward_watt: 45,
                reward_reputation: 5,
                objective: "deep-route",
                required_faction: None,
                subnet_id: None,
                zone_id: Some("deep-space"),
            },
        )
        .await;
        assert_eq!(travel_required["status"].as_str(), Some("open"));

        let active = publish_trade_mission(
            app.clone(),
            token,
            TradeMissionSpec {
                title: "Escort exchange convoy",
                description: "Protect the settlement convoy",
                reward_watt: 30,
                reward_reputation: 3,
                objective: "escort",
                required_faction: None,
                subnet_id: Some("planet-test"),
                zone_id: Some("genesis-core"),
            },
        )
        .await;
        claim_mission(app.clone(), token, &active["mission_id"], agent_id).await;

        let history = publish_trade_mission(
            app.clone(),
            token,
            TradeMissionSpec {
                title: "Close market books",
                description: "Finalize settlement ledgers",
                reward_watt: 20,
                reward_reputation: 2,
                objective: "settle",
                required_faction: None,
                subnet_id: Some("planet-test"),
                zone_id: Some("genesis-core"),
            },
        )
        .await;
        complete_and_settle_mission(app, token, &history["mission_id"], agent_id).await;
    }

    fn assert_starter_templates_with_anchor(payload: &Value) {
        assert_eq!(
            payload["starter_missions"]["templates"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            payload["starter_missions"]["objective_chain"]["steps"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert!(
            payload["starter_missions"]["templates"][0]["anchor"]["map_id"]
                .as_str()
                .is_some()
        );
    }

    fn assert_game_status_payload(status_json: &Value) {
        assert_eq!(
            status_json["identity"]["public_identity"]["public_id"].as_str(),
            Some("captain-aurora")
        );
        assert!(status_json["onboarding"]["progress_pct"].as_u64().unwrap() > 0);
        assert_eq!(
            status_json["starter_missions"]["templates"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert!(
            status_json["starter_missions"]["objective_chain"]["progress_pct"]
                .as_u64()
                .is_some()
        );
        assert!(
            status_json["starter_missions"]["objective_chain"]["current_step_key"]
                .as_str()
                .is_some()
        );
        assert!(
            status_json["status"]["qualifications"]
                .as_array()
                .unwrap()
                .len()
                >= 3
        );
        assert!(
            status_json["status"]["qualifications"][0]["progress_pct"]
                .as_u64()
                .is_some()
        );
        assert!(
            status_json["status"]["qualifications"][0]["unlocks"]
                .as_array()
                .is_some()
        );
        assert!(
            status_json["starter_missions"]["templates"][0]["anchor"]["route_id"]
                .as_str()
                .is_some()
        );
        assert_eq!(
            status_json["status"]["governance_journey"]["next_gate"].as_str(),
            Some("influence_floor")
        );
        assert!(status_json["status"]["home_anchor"].is_object());
        assert!(status_json["status"]["total_influence"].as_i64().unwrap() > 0);
        assert!(
            status_json["status"]["recommended_actions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|action| action
                    .as_str()
                    .is_some_and(|action| action.contains("trade")))
        );
        assert!(
            status_json["onboarding_flow"]["action_cards"]
                .as_array()
                .unwrap()
                .len()
                >= 4
        );
        assert!(status_json["organizations"].as_array().is_some());
        assert!(
            status_json["experience"]["next_actions"]
                .as_array()
                .is_some_and(|actions| !actions.is_empty())
        );
        assert!(status_json["experience"]["alerts"].as_array().is_some());
        assert!(
            status_json["experience"]["priority_cards"]
                .as_array()
                .is_some_and(|cards| !cards.is_empty())
        );
    }

    fn assert_game_mission_pack_payload(payload: &Value) {
        assert_eq!(
            payload["identity"]["public_identity"]["public_id"].as_str(),
            Some("captain-aurora")
        );
        assert_eq!(
            payload["mission_pack"]["templates"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            payload["mission_pack"]["summary"]["current_template_count"].as_u64(),
            Some(2)
        );
        assert_eq!(
            payload["mission_pack"]["summary"]["role_template_count"].as_u64(),
            Some(1)
        );
        assert_eq!(
            payload["mission_pack"]["summary"]["civic_template_count"].as_u64(),
            Some(1)
        );
        assert!(
            payload["mission_pack"]["templates"][0]["payload_schema"]["fields"]
                .as_array()
                .unwrap()
                .iter()
                .any(|field| field["key"].as_str() == Some("map_anchor"))
        );
        assert!(
            payload["mission_pack"]["templates"][0]["anchor"]["system_id"]
                .as_str()
                .is_some()
        );
        assert!(
            payload["mission_pack"]["templates"][0]["suggested_payload"]["objective"]
                .as_str()
                .is_some()
        );
        assert_eq!(
            payload["mission_pack"]["upcoming_templates"]
                .as_array()
                .unwrap()
                .len(),
            usize::try_from(
                payload["mission_pack"]["summary"]["upcoming_template_count"]
                    .as_u64()
                    .unwrap()
            )
            .unwrap()
        );
    }

    fn assert_dashboard_game_block(dashboard_json: &Value) {
        assert_eq!(
            dashboard_json["identity"]["public_identity"]["public_id"].as_str(),
            Some("captain-aurora")
        );
        assert!(dashboard_json["mission_summary"]["eligible_open_count"].is_number());
        assert!(dashboard_json["mission_summary"]["local_open_count"].is_number());
        assert!(dashboard_json["mission_summary"]["travel_required_open_count"].is_number());
        assert!(dashboard_json["mission_summary"]["active_count"].is_number());
        assert_eq!(
            dashboard_json["home_planet"]["subnet_id"].as_str(),
            Some("planet-test")
        );
        assert_eq!(
            dashboard_json["game"]["status"]["stage"].as_str(),
            Some("expansion")
        );
        assert!(
            dashboard_json["game"]["starter_missions"]["templates"]
                .as_array()
                .unwrap()
                .len()
                >= 2
        );
        assert!(
            dashboard_json["game"]["starter_missions"]["objective_chain"]["steps"]
                .as_array()
                .unwrap()
                .len()
                >= 2
        );
        assert!(
            dashboard_json["game"]["mission_pack"]["templates"]
                .as_array()
                .unwrap()
                .len()
                >= 2
        );
        assert!(
            dashboard_json["game"]["mission_pack"]["upcoming_templates"]
                .as_array()
                .unwrap()
                .len()
                == usize::try_from(
                    dashboard_json["game"]["mission_pack"]["summary"]["upcoming_template_count"]
                        .as_u64()
                        .unwrap()
                )
                .unwrap()
        );
        assert!(
            dashboard_json["game"]["mission_pack"]["summary"]["upcoming_template_count"]
                .as_u64()
                .is_some()
        );
        assert!(
            dashboard_json["game"]["onboarding_flow"]["first_hour_plan"]
                .as_array()
                .unwrap()
                .len()
                >= 2
        );
        assert!(dashboard_json["organizations"].as_array().is_some());
        assert!(
            dashboard_json["experience"]["next_actions"]
                .as_array()
                .is_some_and(|actions| !actions.is_empty())
        );
        assert!(dashboard_json["experience"]["alerts"].as_array().is_some());
        assert!(
            dashboard_json["experience"]["priority_cards"]
                .as_array()
                .is_some_and(|cards| !cards.is_empty())
        );
    }

    fn assert_client_mission_travel_views(dashboard_json: &Value, my_missions_json: &Value) {
        assert_eq!(dashboard_json["mission_summary"]["eligible_open_count"], 2);
        assert_eq!(dashboard_json["mission_summary"]["local_open_count"], 1);
        assert_eq!(
            dashboard_json["mission_summary"]["travel_required_open_count"],
            1
        );
        assert_eq!(
            my_missions_json["eligible_open"].as_array().unwrap().len(),
            2
        );
        assert_eq!(my_missions_json["local_open"].as_array().unwrap().len(), 1);
        assert_eq!(
            my_missions_json["travel_required_open"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(my_missions_json["active"].as_array().unwrap().len(), 1);
        assert_eq!(my_missions_json["history"].as_array().unwrap().len(), 1);
        assert_eq!(
            my_missions_json["local_open"][0]["map_anchor"]["system_id"].as_str(),
            Some("frontier-gate")
        );
        assert_eq!(
            my_missions_json["local_open"][0]["travel"]["requires_travel"].as_bool(),
            Some(false)
        );
        assert_eq!(
            my_missions_json["travel_required_open"][0]["map_anchor"]["system_id"].as_str(),
            Some("abyss-watch")
        );
        assert_eq!(
            my_missions_json["travel_required_open"][0]["travel"]["requires_travel"].as_bool(),
            Some(true)
        );
    }

    fn assert_game_onboarding_payload(payload: &Value) {
        assert_eq!(
            payload["identity"]["public_identity"]["public_id"].as_str(),
            Some("captain-aurora")
        );
        assert!(
            payload["onboarding_flow"]["action_cards"]
                .as_array()
                .unwrap()
                .iter()
                .any(|card| card["key"].as_str() == Some("bootstrap_starter_missions"))
        );
        assert!(
            payload["onboarding_flow"]["first_hour_plan"]
                .as_array()
                .unwrap()
                .iter()
                .any(|step| step.as_str().is_some())
        );
        assert!(
            payload["briefing"]["human_report"].is_string()
                || payload["briefing"]["human_report"].is_object()
                || payload["briefing"]["human_report"].is_array()
        );
    }

    async fn claim_mission(app: Router, token: &str, mission_id: &Value, agent_id: &str) {
        assert_eq!(
            authed_post(
                app,
                token,
                "/v1/missions/claim",
                json!({"mission_id": mission_id, "agent_id": agent_id}),
            )
            .await,
            StatusCode::OK
        );
    }

    async fn complete_and_settle_mission(
        app: Router,
        token: &str,
        mission_id: &Value,
        agent_id: &str,
    ) {
        for uri in ["/v1/missions/claim", "/v1/missions/complete"] {
            assert_eq!(
                authed_post(
                    app.clone(),
                    token,
                    uri,
                    json!({"mission_id": mission_id, "agent_id": agent_id}),
                )
                .await,
                StatusCode::OK
            );
        }
        assert_eq!(
            authed_post(
                app,
                token,
                "/v1/missions/settle",
                json!({"mission_id": mission_id}),
            )
            .await,
            StatusCode::OK
        );
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
    async fn galaxy_event_publish_and_query_works() {
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
                    .uri("/v1/galaxy/events")
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
                    .uri("/v1/galaxy/zones")
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
                    .uri("/v1/galaxy/events?zone_id=genesis-core")
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
    async fn galaxy_map_endpoints_expose_official_genesis_map() {
        let (_dir, app, token, _) = build_test_app(20);

        let map_list_json = authed_get_json(app.clone(), &token, "/v1/galaxy/maps").await;
        let maps = map_list_json["maps"].as_array().unwrap();
        assert_eq!(maps.len(), 1);
        assert_eq!(maps[0]["map_id"].as_str(), Some("genesis-base"));
        assert_eq!(maps[0]["system_count"].as_u64(), Some(3));

        let selected_map_json = authed_get_json(app, &token, "/v1/galaxy/map").await;
        assert_eq!(selected_map_json["map_id"].as_str(), Some("genesis-base"));
        assert_eq!(selected_map_json["systems"].as_array().unwrap().len(), 3);
        assert_eq!(selected_map_json["routes"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn galaxy_travel_endpoints_expose_options_and_plans() {
        let (_dir, app, token, _) = build_test_app(21);
        let state_json = authed_get_json(app.clone(), &token, "/v1/state").await;
        let agent_id = state_json["agent_id"].as_str().unwrap();
        bootstrap_broker_character(app.clone(), &token, agent_id).await;

        let _event_json = authed_post_json(
            app.clone(),
            &token,
            "/v1/galaxy/events",
            json!({
                "category": "spatial",
                "zone_id": "frontier-belt",
                "title": "Frontier turbulence",
                "description": "Instability across the gate corridor.",
                "severity": 8,
                "tags": ["hazard"]
            }),
        )
        .await;

        let options_json = authed_get_json(
            app.clone(),
            &token,
            "/v1/galaxy/travel/options?public_id=captain-aurora",
        )
        .await;
        assert_eq!(
            options_json["from_system_id"].as_str(),
            Some("frontier-gate")
        );
        let options = options_json["options"].as_array().unwrap();
        assert_eq!(options.len(), 2);
        let abyss_option = options
            .iter()
            .find(|option| option["to_system_id"].as_str() == Some("abyss-watch"))
            .unwrap();
        assert_eq!(abyss_option["risk_level"].as_str(), Some("volatile"));
        assert!(
            abyss_option["warnings"]
                .as_array()
                .unwrap()
                .iter()
                .any(|warning| warning["code"].as_str() == Some("route_risk_high"))
        );

        let plan_json = authed_get_json(
            app,
            &token,
            "/v1/galaxy/travel/plan?public_id=captain-aurora&to_system_id=abyss-watch",
        )
        .await;
        assert_eq!(plan_json["map_id"].as_str(), Some("genesis-base"));
        assert_eq!(plan_json["total_travel_cost"].as_u64(), Some(5));
        assert_eq!(plan_json["legs"].as_array().unwrap().len(), 1);
        assert_eq!(
            plan_json["traversed_system_ids"].as_array().unwrap().len(),
            2
        );
    }

    #[tokio::test]
    async fn galaxy_travel_state_and_session_flow_work() {
        let (_dir, app, token, _) = build_test_app(21);
        let state_json = authed_get_json(app.clone(), &token, "/v1/state").await;
        let agent_id = state_json["agent_id"].as_str().unwrap();
        bootstrap_broker_character(app.clone(), &token, agent_id).await;
        let _ = publish_trade_mission(
            app.clone(),
            &token,
            TradeMissionSpec {
                title: "Deep watch market relay",
                description: "Unlock deep-space market visibility",
                reward_watt: 35,
                reward_reputation: 4,
                objective: "deep-watch",
                required_faction: None,
                subnet_id: None,
                zone_id: Some("deep-space"),
            },
        )
        .await;

        let initial_json = authed_get_json(
            app.clone(),
            &token,
            "/v1/galaxy/travel/state?public_id=captain-aurora",
        )
        .await;
        assert_eq!(
            initial_json["travel_state"]["current_position"]["system_id"].as_str(),
            Some("frontier-gate")
        );
        assert!(initial_json["travel_state"]["active_session"].is_null());

        let departed_json = authed_post_json(
            app.clone(),
            &token,
            "/v1/galaxy/travel/depart",
            json!({
                "public_id": "captain-aurora",
                "to_system_id": "abyss-watch"
            }),
        )
        .await;
        assert_eq!(
            departed_json["travel_state"]["active_session"]["to_system_id"].as_str(),
            Some("abyss-watch")
        );
        assert_eq!(
            departed_json["travel_state"]["active_session"]["status"].as_str(),
            Some("in_transit")
        );

        let arrived_json = authed_post_json(
            app,
            &token,
            "/v1/galaxy/travel/arrive",
            json!({
                "public_id": "captain-aurora"
            }),
        )
        .await;
        assert_eq!(
            arrived_json["travel_state"]["current_position"]["system_id"].as_str(),
            Some("abyss-watch")
        );
        assert!(arrived_json["travel_state"]["active_session"].is_null());
        assert_eq!(
            arrived_json["travel_state"]["current_position"]["zone_id"].as_str(),
            Some("deep-space")
        );
        assert_eq!(
            arrived_json["travel_state"]["last_consequence"]["mission_impact"]["eligible_local_count"]
                .as_u64(),
            Some(1)
        );
        assert_eq!(
            arrived_json["travel_state"]["last_consequence"]["route_risk_level"].as_str(),
            Some("volatile")
        );
        assert!(
            !arrived_json["travel_state"]["recent_consequences"]
                .as_array()
                .unwrap()
                .is_empty()
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
    async fn civilization_briefing_and_generated_galaxy_events_work() {
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
                "/v1/galaxy/events/generate",
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

    #[tokio::test]
    async fn client_characters_and_catalog_endpoints_work() {
        let (_dir, app, token, _) = build_test_app(20);

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

        let characters_json =
            authed_get_json(app.clone(), &token, "/v1/civilization/characters").await;
        let characters = characters_json["characters"].as_array().unwrap();
        assert!(characters.len() >= 2);
        assert!(characters.iter().any(|item| {
            item["identity"]["public_identity"]["public_id"].as_str() == Some("captain-aurora")
                && item["identity"]["profile"]["role"].as_str() == Some("broker")
                && item["travel_state"]["current_position"]["system_id"].as_str()
                    == Some("frontier-gate")
        }));

        let catalog_json = authed_get_json(app, &token, "/v1/catalog/bootstrap").await;
        assert!(
            catalog_json["roles"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item.as_str() == Some("broker"))
        );
        assert!(
            catalog_json["travel_risk_levels"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item.as_str() == Some("volatile"))
        );
        assert!(
            catalog_json["organization_kinds"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item.as_str() == Some("consortium"))
        );
        assert!(
            catalog_json["organization_permissions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item.as_str() == Some("publish_missions"))
        );
        assert!(
            catalog_json["organization_permissions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item.as_str() == Some("manage_governance"))
        );
        assert!(
            catalog_json["organization_proposal_kinds"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item.as_str() == Some("subnet_charter"))
        );
        assert_eq!(catalog_json["galaxy_zones"].as_array().unwrap().len(), 3);
    }

    #[tokio::test]
    async fn game_catalog_and_status_endpoints_work() {
        let (_dir, app, token, _) = build_test_app(20);
        let state_json = authed_get_json(app.clone(), &token, "/v1/state").await;
        let agent_id = state_json["agent_id"].as_str().unwrap();

        let _ = bootstrap_broker_game(app.clone(), &token, agent_id).await;
        let _ = settle_trade_mission_for_agent(app.clone(), &token, agent_id).await;

        let catalog_json = authed_get_json(app.clone(), &token, "/v1/game/catalog").await;
        assert_eq!(catalog_json["roles"].as_array().unwrap().len(), 4);
        assert_eq!(catalog_json["stages"].as_array().unwrap().len(), 4);
        let starter_list = authed_get_json(
            app.clone(),
            &token,
            "/v1/game/starter-missions?public_id=captain-aurora",
        )
        .await;
        assert_starter_templates_with_anchor(&starter_list);
        let pack_bootstrap = authed_post_json(
            app.clone(),
            &token,
            "/v1/game/mission-pack/bootstrap",
            json!({"public_id": "captain-aurora"}),
        )
        .await;
        assert_eq!(pack_bootstrap["created"].as_array().unwrap().len(), 2);
        let mission_pack_json = authed_get_json(
            app.clone(),
            &token,
            "/v1/game/mission-pack?public_id=captain-aurora",
        )
        .await;
        assert_game_mission_pack_payload(&mission_pack_json);
        let onboarding_json = authed_get_json(
            app.clone(),
            &token,
            "/v1/game/onboarding?public_id=captain-aurora",
        )
        .await;
        assert_game_onboarding_payload(&onboarding_json);

        let status_json =
            authed_get_json(app, &token, "/v1/game/status?public_id=captain-aurora").await;
        assert_game_status_payload(&status_json);
    }

    #[tokio::test]
    async fn client_dashboard_and_my_views_work() {
        let (_dir, app, token, _) = build_test_app(20);
        let state_json = authed_get_json(app.clone(), &token, "/v1/state").await;
        let agent_id = state_json["agent_id"].as_str().unwrap();

        bootstrap_broker_character(app.clone(), &token, agent_id).await;
        seed_client_view_missions(app.clone(), &token, agent_id).await;

        let dashboard_json = authed_get_json(
            app.clone(),
            &token,
            "/v1/dashboard/home?public_id=captain-aurora",
        )
        .await;
        assert_dashboard_game_block(&dashboard_json);

        let my_missions_json = authed_get_json(
            app.clone(),
            &token,
            "/v1/missions/my?public_id=captain-aurora",
        )
        .await;
        assert_client_mission_travel_views(&dashboard_json, &my_missions_json);

        let my_governance_json =
            authed_get_json(app, &token, "/v1/governance/my?public_id=captain-aurora").await;
        assert_eq!(
            my_governance_json["home_planet"]["subnet_id"].as_str(),
            Some("planet-test")
        );
        assert_eq!(
            my_governance_json["eligibility"]["has_valid_license"].as_bool(),
            Some(true)
        );
        assert_eq!(
            my_governance_json["governed_planets"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            my_governance_json["journey"]["next_gate"].as_str(),
            Some("influence_floor")
        );
        assert!(
            my_governance_json["qualification_tracks"]
                .as_array()
                .unwrap()
                .iter()
                .any(|track| track["key"].as_str() == Some("civic_governance"))
        );
        assert!(
            !my_governance_json["next_actions"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn organization_endpoints_and_views_work() {
        let (_dir, app, token, _) = build_test_app(80);
        let state_json = authed_get_json(app.clone(), &token, "/v1/state").await;
        let agent_id = state_json["agent_id"].as_str().unwrap();

        bootstrap_broker_character(app.clone(), &token, agent_id).await;
        let _ = authed_post_json(
            app.clone(),
            &token,
            "/v1/civilization/bootstrap-character",
            json!({
                "public_id": "quartermaster-echo",
                "display_name": "Quartermaster Echo",
                "legacy_agent_id": "agent-echo",
                "faction": "freeport",
                "role": "operator",
                "strategy": "balanced",
                "home_subnet_id": "planet-test",
                "home_zone_id": "frontier-belt",
                "controller_kind": "external_runtime",
                "controller_ref": "external-echo",
                "controller_node_id": "agent-echo",
                "ownership_scope": "external"
            }),
        )
        .await;
        let _ = authed_post_json(
            app.clone(),
            &token,
            "/v1/civilization/bootstrap-character",
            json!({
                "public_id": "scout-voss",
                "display_name": "Scout Voss",
                "legacy_agent_id": "agent-voss",
                "faction": "freeport",
                "role": "enforcer",
                "strategy": "balanced",
                "home_subnet_id": "planet-test",
                "home_zone_id": "frontier-belt",
                "controller_kind": "external_runtime",
                "controller_ref": "external-voss",
                "controller_node_id": "agent-voss",
                "ownership_scope": "external"
            }),
        )
        .await;
        let created_org = authed_post_json(
            app.clone(),
            &token,
            "/v1/civilization/organizations",
            json!({
                "public_id": "captain-aurora",
                "organization_id": "aurora-consortium",
                "name": "Aurora Consortium",
                "kind": "consortium",
                "summary": "Coordinates frontier logistics and trade corridors.",
                "faction_alignment": "freeport",
                "home_subnet_id": "planet-test",
                "home_zone_id": "frontier-belt"
            }),
        )
        .await;
        assert_eq!(
            created_org["organization"]["organization_id"].as_str(),
            Some("aurora-consortium")
        );

        let member_json = authed_post_json(
            app.clone(),
            &token,
            "/v1/civilization/organizations/members",
            json!({
                "organization_id": "aurora-consortium",
                "actor_public_id": "captain-aurora",
                "public_id": "quartermaster-echo",
                "role": "officer",
                "title": "Quartermaster"
            }),
        )
        .await;
        assert_eq!(
            member_json["membership"]["public_id"].as_str(),
            Some("quartermaster-echo")
        );

        let funded_json = authed_post_json(
            app.clone(),
            &token,
            "/v1/civilization/organizations/treasury/fund",
            json!({
                "organization_id": "aurora-consortium",
                "actor_public_id": "captain-aurora",
                "amount_watt": 60,
                "reason": "seed frontier treasury"
            }),
        )
        .await;
        assert_eq!(
            funded_json["organization"]["treasury_watt"].as_i64(),
            Some(60)
        );

        let spent_json = authed_post_json(
            app.clone(),
            &token,
            "/v1/civilization/organizations/treasury/spend",
            json!({
                "organization_id": "aurora-consortium",
                "actor_public_id": "captain-aurora",
                "amount_watt": 15,
                "reason": "fund escort contract"
            }),
        )
        .await;
        assert_eq!(
            spent_json["organization"]["treasury_watt"].as_i64(),
            Some(45)
        );

        let forbidden_member_json = authed_post_json(
            app.clone(),
            &token,
            "/v1/civilization/organizations/members",
            json!({
                "organization_id": "aurora-consortium",
                "actor_public_id": "quartermaster-echo",
                "public_id": "scout-voss",
                "role": "member",
                "title": "Scout"
            }),
        )
        .await;
        assert_eq!(
            forbidden_member_json["error"].as_str(),
            Some("officer role does not grant ManageMembers")
        );

        let scout_member_json = authed_post_json(
            app.clone(),
            &token,
            "/v1/civilization/organizations/members",
            json!({
                "organization_id": "aurora-consortium",
                "actor_public_id": "captain-aurora",
                "public_id": "scout-voss",
                "role": "member",
                "title": "Scout"
            }),
        )
        .await;
        assert_eq!(
            scout_member_json["membership"]["public_id"].as_str(),
            Some("scout-voss")
        );

        let published_mission = authed_post_json(
            app.clone(),
            &token,
            "/v1/civilization/organizations/missions",
            json!({
                "organization_id": "aurora-consortium",
                "actor_public_id": "quartermaster-echo",
                "title": "Staff the frontier exchange",
                "description": "Coordinate organization members around the exchange lane.",
                "domain": "trade",
                "subnet_id": "planet-test",
                "zone_id": "frontier-belt",
                "required_role": "broker",
                "required_faction": "freeport",
                "treasury_commit_watt": 5,
                "reward": {
                    "agent_watt": 30,
                    "reputation": 3,
                    "capacity": 2,
                    "treasury_share_watt": 4
                },
                "payload": {
                    "organization_id": "aurora-consortium"
                }
            }),
        )
        .await;
        assert_eq!(
            published_mission["mission"]["publisher_kind"].as_str(),
            Some("organization")
        );
        assert_eq!(
            published_mission["organization"]["treasury_watt"].as_i64(),
            Some(40)
        );
        complete_and_settle_mission(
            app.clone(),
            &token,
            &published_mission["mission"]["mission_id"],
            agent_id,
        )
        .await;

        let second_org_mission = authed_post_json(
            app.clone(),
            &token,
            "/v1/civilization/organizations/missions",
            json!({
                "organization_id": "aurora-consortium",
                "actor_public_id": "captain-aurora",
                "title": "Audit the frontier exchange",
                "description": "Verify the route books before expansion.",
                "domain": "power",
                "subnet_id": "planet-test",
                "zone_id": "frontier-belt",
                "required_role": "broker",
                "required_faction": "freeport",
                "treasury_commit_watt": 0,
                "reward": {
                    "agent_watt": 20,
                    "reputation": 2,
                    "capacity": 1,
                    "treasury_share_watt": 3
                },
                "payload": {
                    "organization_id": "aurora-consortium"
                }
            }),
        )
        .await;
        complete_and_settle_mission(
            app.clone(),
            &token,
            &second_org_mission["mission"]["mission_id"],
            agent_id,
        )
        .await;

        let proposal_json = authed_post_json(
            app.clone(),
            &token,
            "/v1/civilization/organizations/proposals",
            json!({
                "organization_id": "aurora-consortium",
                "actor_public_id": "captain-aurora",
                "kind": "subnet_charter",
                "title": "Charter Aurora Reach",
                "summary": "Request a dedicated subnet for consortium traffic and governance.",
                "proposed_subnet_id": "planet-aurora",
                "proposed_subnet_name": "Aurora Reach"
            }),
        )
        .await;
        let proposal_id = proposal_json["proposal"]["proposal_id"]
            .as_str()
            .unwrap()
            .to_string();

        let founder_vote = authed_post_json(
            app.clone(),
            &token,
            "/v1/civilization/organizations/proposals/vote",
            json!({
                "proposal_id": proposal_id.clone(),
                "actor_public_id": "captain-aurora",
                "approve": true
            }),
        )
        .await;
        assert_eq!(
            founder_vote["proposal"]["votes_for"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        let scout_vote = authed_post_json(
            app.clone(),
            &token,
            "/v1/civilization/organizations/proposals/vote",
            json!({
                "proposal_id": proposal_id.clone(),
                "actor_public_id": "scout-voss",
                "approve": true
            }),
        )
        .await;
        assert_eq!(
            scout_vote["proposal"]["votes_for"]
                .as_array()
                .unwrap()
                .len(),
            2
        );

        let finalized_proposal = authed_post_json(
            app.clone(),
            &token,
            "/v1/civilization/organizations/proposals/finalize",
            json!({
                "proposal_id": proposal_id.clone(),
                "actor_public_id": "quartermaster-echo",
                "min_votes_for": 2
            }),
        )
        .await;
        assert_eq!(
            finalized_proposal["proposal"]["status"].as_str(),
            Some("accepted")
        );

        let charter_json = authed_post_json(
            app.clone(),
            &token,
            "/v1/civilization/organizations/charters",
            json!({
                "proposal_id": proposal_json["proposal"]["proposal_id"],
                "actor_public_id": "captain-aurora"
            }),
        )
        .await;
        assert_eq!(
            charter_json["charter_application"]["status"].as_str(),
            Some("pending_governance")
        );
        assert_eq!(
            charter_json["charter_application"]["sponsor_controller_id"].as_str(),
            Some(agent_id)
        );

        let organizations_json = authed_get_json(
            app.clone(),
            &token,
            "/v1/civilization/organizations?public_id=captain-aurora",
        )
        .await;
        assert_eq!(
            organizations_json["organizations"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            organizations_json["organizations"][0]["active_member_count"].as_u64(),
            Some(3)
        );
        assert_eq!(
            organizations_json["organizations"][0]["organization"]["treasury_watt"].as_i64(),
            Some(40)
        );
        assert_eq!(
            organizations_json["organizations"][0]["open_mission_count"].as_u64(),
            Some(0)
        );
        assert_eq!(
            organizations_json["organizations"][0]["settled_mission_count"].as_u64(),
            Some(2)
        );
        assert_eq!(
            organizations_json["organizations"][0]["subnet_readiness"].as_str(),
            Some("subnet-ready")
        );
        assert_eq!(
            organizations_json["organizations"][0]["permissions"]
                .as_array()
                .unwrap()
                .len(),
            4
        );
        assert_eq!(
            organizations_json["organizations"][0]["autonomy_track"]["current_status"].as_str(),
            Some("subnet-ready")
        );
        assert_eq!(
            organizations_json["organizations"][0]["autonomy_track"]["eligible_for_subnet_charter"]
                .as_bool(),
            Some(true)
        );
        assert!(
            organizations_json["organizations"][0]["autonomy_track"]["gates"]
                .as_array()
                .unwrap()
                .len()
                >= 5
        );
        assert!(
            organizations_json["organizations"][0]["autonomy_track"]["next_action"]
                .as_str()
                .is_some()
        );
        assert_eq!(
            organizations_json["organizations"][0]["governance_summary"]["accepted_proposals_count"]
                .as_u64(),
            Some(1)
        );
        assert_eq!(
            organizations_json["organizations"][0]["governance_summary"]["charter_application_count"]
                .as_u64(),
            Some(1)
        );
        assert_eq!(
            organizations_json["organizations"][0]["governance_summary"]["latest_charter_application"]["proposed_subnet_id"]
                .as_str(),
            Some("planet-aurora")
        );

        let governance_json = authed_get_json(
            app.clone(),
            &token,
            "/v1/civilization/organizations/proposals?organization_id=aurora-consortium&public_id=captain-aurora",
        )
        .await;
        assert_eq!(governance_json["proposals"].as_array().unwrap().len(), 1);
        assert_eq!(
            governance_json["charter_applications"]
                .as_array()
                .unwrap()
                .len(),
            1
        );

        let my_organizations_json = authed_get_json(
            app.clone(),
            &token,
            "/v1/organizations/my?public_id=captain-aurora",
        )
        .await;
        assert_eq!(
            my_organizations_json["organizations"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        let officer_orgs_json = authed_get_json(
            app.clone(),
            &token,
            "/v1/organizations/my?public_id=quartermaster-echo",
        )
        .await;
        assert_eq!(
            officer_orgs_json["organizations"][0]["permissions"]
                .as_array()
                .unwrap()
                .len(),
            3
        );
        assert!(
            officer_orgs_json["organizations"][0]["permissions"]
                .as_array()
                .unwrap()
                .iter()
                .all(|permission| permission.as_str() != Some("manage_members"))
        );
        assert!(
            officer_orgs_json["organizations"][0]["permissions"]
                .as_array()
                .unwrap()
                .iter()
                .any(|permission| permission.as_str() == Some("manage_governance"))
        );

        let governance_my_json = authed_get_json(
            app.clone(),
            &token,
            "/v1/governance/my?public_id=captain-aurora",
        )
        .await;
        assert_eq!(
            governance_my_json["organizations"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            governance_my_json["charter_applications"]
                .as_array()
                .unwrap()
                .len(),
            1
        );

        let dashboard_json =
            authed_get_json(app, &token, "/v1/dashboard/home?public_id=captain-aurora").await;
        assert_eq!(dashboard_json["organizations"].as_array().unwrap().len(), 1);
        assert_eq!(
            dashboard_json["game"]["organizations"][0]["organization"]["organization_id"].as_str(),
            Some("aurora-consortium")
        );
        assert!(
            dashboard_json["game"]["organizations"][0]["autonomy_track"]["eligible_for_subnet_charter"]
                .as_bool()
                == Some(true)
        );
    }
}
