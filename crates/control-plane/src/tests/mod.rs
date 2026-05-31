use super::*;
use crate::social_host::{WattetheriaLocalIdentityProvider, WattetheriaTransportAdapter};
use axum::extract::{Path, Query};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use chrono::Utc;
use http_body_util::BodyExt;
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::fs;
use std::sync::Arc;
use tokio::sync::{Mutex, broadcast};
use tower::ServiceExt;
use watt_wallet::SignerPurpose;
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
use wattetheria_kernel::civilization::topics::HiveRegistry;
use wattetheria_kernel::event_log::EventLog;
use wattetheria_kernel::governance::{
    GovernanceEngine, PlanetConstitutionTemplate, PlanetCreationRequest,
};
use wattetheria_kernel::identity::{
    Identity, build_scoped_public_id, extract_public_id_fingerprint, fingerprint_from_did_key,
};
use wattetheria_kernel::mailbox::CrossSubnetMailbox;
use wattetheria_kernel::map::registry::GalaxyMapRegistry;
use wattetheria_kernel::policy_engine::{PolicyEngine, PolicyState};
use wattetheria_kernel::servicenet::ServiceNetClient;
use wattetheria_kernel::signing::verify_payload;
use wattetheria_kernel::swarm_bridge::{
    SwarmAgentEnvelope, SwarmAgentPaymentCommand, SwarmAgentView, SwarmBridge,
    SwarmDiagnosticsQuery, SwarmDiagnosticsSnapshot, SwarmDirectMessageCommand,
    SwarmNetworkStatusView, SwarmPeerDmMessageView, SwarmPeerDmThreadView,
    SwarmPeerRelationshipView, SwarmPeerView, SwarmRelationshipActionCommand,
    SwarmRunSubmitCommand, SwarmSourceAgentCard, SwarmTaskAnnounceCommand, SwarmTaskClaimCommand,
    SwarmTaskProposeCandidateCommand, SwarmTopicCursorView, SwarmTopicMessageView,
};
use wattetheria_kernel::swarm_sync::{
    SwarmRunEventsSnapshot, SwarmRunResultSnapshot, SwarmTopicActivitySnapshot,
};
use wattetheria_kernel::types::AgentStats;
use wattetheria_kernel::wallet_identity::open_local_wallet;
use wattetheria_social::application::{
    block_service, friend_request_service, friendship_service, message_service, receipt_service,
    thread_service,
};
use wattetheria_social::ports::local_identity_provider::LocalIdentityProvider;
use wattetheria_social::ports::transport_port::TransportPort;
use wattswarm_protocol::types::TaskContract;

#[allow(clippy::too_many_lines)]
fn build_test_app(
    rate_limit: usize,
) -> (
    tempfile::TempDir,
    Router,
    String,
    Arc<Mutex<PolicyEngine>>,
    ControlPlaneState,
) {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge: Arc<dyn SwarmBridge> =
        Arc::new(MockSwarmBridge::default_for(identity.agent_did.clone()));
    build_test_app_with_bridge(rate_limit, dir, identity, event_log, bridge)
}

#[allow(clippy::too_many_lines, clippy::needless_pass_by_value)]
fn build_test_app_with_bridge(
    rate_limit: usize,
    dir: tempfile::TempDir,
    identity: Identity,
    event_log: EventLog,
    swarm_bridge: Arc<dyn SwarmBridge>,
) -> (
    tempfile::TempDir,
    Router,
    String,
    Arc<Mutex<PolicyEngine>>,
    ControlPlaneState,
) {
    let (dir, state, token, policy_engine) =
        build_test_state_with_bridge(rate_limit, dir, identity, event_log, swarm_bridge);
    let router = app(state.clone());
    (dir, router, token, policy_engine, state)
}

#[allow(clippy::too_many_lines, clippy::needless_pass_by_value)]
fn build_test_state_with_bridge(
    rate_limit: usize,
    dir: tempfile::TempDir,
    identity: Identity,
    event_log: EventLog,
    swarm_bridge: Arc<dyn SwarmBridge>,
) -> (
    tempfile::TempDir,
    ControlPlaneState,
    String,
    Arc<Mutex<PolicyEngine>>,
) {
    let local_db =
        Arc::new(wattetheria_kernel::local_db::LocalDb::open_in_memory().expect("test local db"));
    let social_store =
        Arc::new(wattetheria_social::SocialStore::open_in_memory().expect("test social store"));

    let policy_engine = Arc::new(Mutex::new(PolicyEngine::new(
        "test-session",
        CapabilityPolicy::default(),
        PolicyState::default(),
    )));

    let mut governance = GovernanceEngine::default();
    governance.issue_license(&identity.agent_did, &identity.agent_did, "proof", 7);
    governance.lock_bond(&identity.agent_did, 100, 30);
    governance.issue_license("agent-challenger", &identity.agent_did, "proof", 7);
    governance.lock_bond("agent-challenger", 150, 30);
    let signer = Identity::new_random();
    let created_at = Utc::now().timestamp();
    let approvals = vec![
        GovernanceEngine::sign_genesis(
            "planet-test",
            "Planet Test",
            &identity.agent_did,
            created_at,
            &identity,
        )
        .unwrap(),
        GovernanceEngine::sign_genesis(
            "planet-test",
            "Planet Test",
            &identity.agent_did,
            created_at,
            &signer,
        )
        .unwrap(),
    ];
    let planet_request = PlanetCreationRequest {
        subnet_id: "planet-test".to_string(),
        name: "Planet Test".to_string(),
        creator: identity.agent_did.clone(),
        created_at,
        tax_rate: 0.05,
        min_bond: 50,
        min_approvals: 2,
        constitution_template: PlanetConstitutionTemplate::MigrantCouncil,
    };
    governance
        .create_planet(&planet_request, &approvals)
        .unwrap();
    local_db
        .save_domain(
            wattetheria_kernel::local_db::domain::GOVERNANCE,
            &governance,
        )
        .unwrap();
    let governance_engine = Arc::new(Mutex::new(governance));

    let audit_log = AuditLog::new(dir.path().join("audit/control_plane.jsonl")).unwrap();
    let mailbox = Arc::new(Mutex::new(CrossSubnetMailbox::default()));
    let mission_board = Arc::new(Mutex::new(MissionBoard::default()));
    let mut public_identity_registry = PublicIdentityRegistry::default();
    let default_identity = public_identity_registry
        .ensure_local_default_for_agent(&identity.agent_did, Some(&identity.agent_did))
        .unwrap();
    local_db
        .save_domain(
            wattetheria_kernel::local_db::domain::PUBLIC_IDENTITY_REGISTRY,
            &public_identity_registry,
        )
        .unwrap();
    let public_identity_registry = Arc::new(Mutex::new(public_identity_registry));
    let mut controller_binding_registry = ControllerBindingRegistry::default();
    controller_binding_registry
        .ensure_local_wattswarm(&default_identity.public_id, &identity.agent_did);
    local_db
        .save_domain(
            wattetheria_kernel::local_db::domain::CONTROLLER_BINDING_REGISTRY,
            &controller_binding_registry,
        )
        .unwrap();
    let controller_binding_registry = Arc::new(Mutex::new(controller_binding_registry));
    let citizen_registry = Arc::new(Mutex::new(CitizenRegistry::default()));
    let relationship_registry = Arc::new(Mutex::new(
        wattetheria_kernel::relationships::RelationshipRegistry::default(),
    ));
    let organization_registry = Arc::new(Mutex::new(OrganizationRegistry::default()));
    let hive_registry = Arc::new(Mutex::new(HiveRegistry::default()));
    let galaxy_state_loaded = GalaxyState::default_with_core_zones();
    let mut galaxy_map_registry_loaded = GalaxyMapRegistry::default();
    galaxy_map_registry_loaded
        .ensure_default_genesis_map(&galaxy_state_loaded.zones())
        .unwrap();
    local_db
        .save_domain(
            wattetheria_kernel::local_db::domain::GALAXY_MAP_REGISTRY,
            &galaxy_map_registry_loaded,
        )
        .unwrap();
    let default_map = galaxy_map_registry_loaded.get("genesis-base").unwrap();
    let galaxy_state = Arc::new(Mutex::new(galaxy_state_loaded));
    let galaxy_map_registry = Arc::new(Mutex::new(galaxy_map_registry_loaded));
    let mut travel_state_registry = wattetheria_kernel::map::state::TravelStateRegistry::default();
    let default_position =
        wattetheria_kernel::map::state::resolve_anchor_position(&default_map, None, None).unwrap();
    let _ = travel_state_registry.ensure_position(
        &default_identity.public_id,
        &identity.agent_did,
        default_position,
    );
    local_db
        .save_domain(
            wattetheria_kernel::local_db::domain::TRAVEL_STATE_REGISTRY,
            &travel_state_registry,
        )
        .unwrap();
    let travel_state_registry = Arc::new(Mutex::new(travel_state_registry));
    let payment_ledger = Arc::new(Mutex::new(
        wattetheria_kernel::payments::PaymentLedger::default(),
    ));
    let (stream_tx, _) = broadcast::channel(32);
    let token = "test-token".to_string();
    identity.save(dir.path().join("identity.json")).unwrap();
    {
        let mut wallet_state = open_local_wallet(dir.path()).unwrap();
        if wallet_state
            .wallet
            .active_identity(&wallet_state.profile)
            .is_err()
        {
            let seed_bytes = STANDARD.decode(&identity.private_key).unwrap();
            let seed: [u8; 32] = seed_bytes.try_into().unwrap();
            let wallet_identity = wallet_state
                .wallet
                .import_identity_ed25519_seed(
                    &mut wallet_state.profile,
                    seed,
                    Some("test-agent".to_string()),
                    vec![
                        SignerPurpose::General,
                        SignerPurpose::Authentication,
                        SignerPurpose::AssertionMethod,
                        SignerPurpose::CapabilityInvocation,
                    ],
                    1,
                )
                .unwrap();
            assert_eq!(wallet_identity.did.to_string(), identity.agent_did);
        }
    }

    let state = ControlPlaneState {
        data_dir: dir.path().to_path_buf(),
        agent_did: identity.agent_did.clone(),
        identity: identity.compat_view(),
        signer: Arc::new(identity.clone()),
        started_at: Utc::now().timestamp(),
        auth_token: token.clone(),
        event_log,
        swarm_bridge,
        governance_engine,
        policy_engine: policy_engine.clone(),
        mailbox,
        mission_board,
        public_identity_registry,
        controller_binding_registry,
        citizen_registry,
        relationship_registry,
        organization_registry,
        hive_registry,
        payment_ledger,
        galaxy_state,
        galaxy_map_registry,
        travel_state_registry,
        brain_engine: Arc::new(tokio::sync::RwLock::new(BrainEngine::from_config(
            &BrainProviderConfig::Rules,
        ))),
        brain_config: Arc::new(tokio::sync::RwLock::new(BrainProviderConfig::Rules)),
        brain_provider_label: "rules".to_string(),
        audit_log,
        local_db,
        social_store,
        servicenet_client: None,
        agent_executor_base_url: None,
        agent_event_callback_base_url: None,
        agent_topic_bridge_enabled: true,
        rate_limiter: Arc::new(RateLimiter::new(rate_limit, 60)),
        stream_tx,
        gateway_event_seq: GatewayEventSequence::load_or_seed(dir.path()),
        geo_location: Arc::new(NodeGeoLocation {
            lat: 0.0,
            lng: 0.0,
            source: crate::state::GeoSource::Derived,
        }),
    };

    (dir, state, token, policy_engine)
}

async fn request_json(app: Router, request: axum::http::Request<axum::body::Body>) -> Value {
    let response = app.oneshot(request).await.unwrap();
    serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap()
}

async fn request_text(
    app: Router,
    request: axum::http::Request<axum::body::Body>,
) -> (StatusCode, String) {
    let response = app.oneshot(request).await.unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8(body.to_vec()).unwrap())
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

async fn public_get_json(app: Router, uri: &str) -> Value {
    request_json(
        app,
        axum::http::Request::builder()
            .uri(uri)
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

async fn authed_post_json_with_headers(
    app: Router,
    token: &str,
    uri: &str,
    body: Value,
    extra_headers: &[(&str, &str)],
) -> Value {
    let mut builder = axum::http::Request::builder()
        .method("POST")
        .uri(uri)
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json");
    for (name, value) in extra_headers {
        builder = builder.header(*name, *value);
    }
    request_json(
        app,
        builder
            .body(axum::body::Body::from(body.to_string()))
            .unwrap(),
    )
    .await
}

#[allow(clippy::too_many_lines)]
async fn spawn_mock_servicenet() -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    let app = Router::new()
        .route(
            "/v1/agents",
            get(|Query(query): Query<BTreeMap<String, String>>| async move {
                let limit = query
                    .get("limit")
                    .and_then(|value| value.parse::<usize>().ok())
                    .unwrap_or(50);
                let offset = query
                    .get("offset")
                    .and_then(|value| value.parse::<usize>().ok())
                    .unwrap_or(0);
                let agents = vec![
                    json!({
                        "agent_id": "agent-alpha",
                        "provider_id": "provider-one",
                        "version": "0.1.0",
                        "status": "approved",
                        "agent_card": {
                            "name": "Agent Alpha",
                            "description": "Alpha test agent",
                            "cost": 18,
                            "currency": "USDC",
                            "supportsTask": true,
                            "capabilities": {
                                "extensions": [
                                    {
                                        "uri": "https://github.com/google-a2a/a2a-x402/v0.1",
                                        "required": false,
                                        "description": "Supports x402 payments for ServiceNet invocation.",
                                        "params": {
                                            "accepts": [
                                                {
                                                    "scheme": "exact",
                                                    "network": "base",
                                                    "asset": "0x0000000000000000000000000000000000000000",
                                                    "payTo": "0x742d35Cc6634C0532925a3b844Bc454e4438f44e",
                                                    "maxAmountRequired": "180000",
                                                    "resource": "servicenet:agent:agent-alpha",
                                                    "description": "ServiceNet agent invocation",
                                                    "maxTimeoutSeconds": 600
                                                }
                                            ]
                                        }
                                    }
                                ]
                            },
                            "skills": [
                                {
                                    "id": "weather.lookup",
                                    "name": "Get weather",
                                    "description": "Returns current weather"
                                }
                            ]
                        },
                        "deployment": {
                            "runtime": "remote_http",
                            "endpoint": {
                                "url": "https://example.com/a2a",
                                "interaction_protocol": "google_a2a",
                                "protocol_binding": "JSONRPC"
                            }
                        },
                        "review": {"risk_level": "low"},
                    }),
                    json!({
                        "agent_id": "agent-beta",
                        "provider_id": "provider-two",
                        "version": "0.2.0",
                        "status": "approved",
                        "agent_card": {
                            "name": "Agent Beta",
                            "description": "Beta test agent",
                            "cost": 7,
                            "currency": "USDT",
                            "supportsTask": false,
                            "skills": [
                                {
                                    "id": "address.name",
                                    "name": "Name address"
                                }
                            ]
                        },
                        "deployment": {
                            "runtime": "remote_http",
                            "endpoint": {
                                "url": "https://example.net/a2a",
                                "interaction_protocol": "google_a2a",
                                "protocol_binding": "JSONRPC"
                            }
                        },
                        "review": {"risk_level": "medium"},
                    }),
                ];
                let known_count = agents.len();
                let items = agents
                    .into_iter()
                    .skip(offset)
                    .take(limit)
                    .collect::<Vec<_>>();
                let next_offset = offset.saturating_add(items.len());
                let has_more = next_offset < known_count;
                Json(json!({
                    "items": items,
                    "count": items.len(),
                    "limit": limit,
                    "offset": offset,
                    "next_offset": if has_more { Some(next_offset) } else { None },
                    "has_more": has_more,
                    "known_count": known_count
                }))
            }),
        )
        .route(
            "/v1/health/agents",
            get(|| async move {
                Json(json!({
                    "items": [
                        {
                            "agent_id": "agent-alpha",
                            "provider_id": "provider-one",
                            "status": "unknown",
                            "success_count": 0,
                            "failure_count": 0,
                            "success_rate": 1.0
                        },
                        {
                            "agent_id": "agent-beta",
                            "provider_id": "provider-two",
                            "status": "online",
                            "success_count": 3,
                            "failure_count": 0,
                            "success_rate": 1.0
                        }
                    ]
                }))
            }),
        )
        .route(
            "/v1/trust/agents",
            get(|| async move {
                Json(json!({
                    "items": [
                        {
                            "agent_id": "agent-alpha",
                            "reputation_score": 0.75,
                            "blocked": false
                        },
                        {
                            "agent_id": "agent-beta",
                            "reputation_score": 0.5,
                            "blocked": false
                        }
                    ]
                }))
            }),
        )
        .route(
            "/v1/agents/{agent_id}",
            get(|Path(agent_id): Path<String>| async move {
                if agent_id == "agent-oauth" {
                    return Json(json!({
                        "agent_id": agent_id,
                        "provider_id": "provider-oauth",
                        "version": "0.1.0",
                        "status": "approved",
                        "agent_card": {
                            "name": "OAuth Agent",
                            "description": "Agent requiring OAuth consent",
                            "cost": 21,
                            "currency": "USDC",
                            "supportsTask": false,
                            "securitySchemes": {
                                "oauth2": {
                                    "oauth2SecurityScheme": {
                                        "flows": {
                                            "authorizationCode": {
                                                "authorizationUrl": "https://auth.example.com/oauth/authorize",
                                                "tokenUrl": "https://auth.example.com/oauth/token",
                                                "refreshUrl": "https://auth.example.com/oauth/token",
                                                "scopes": {
                                                    "rides:request": "Request rides"
                                                },
                                                "pkceRequired": true
                                            }
                                        }
                                    }
                                }
                            },
                            "security": [
                                {
                                    "oauth2": ["rides:request"]
                                }
                            ],
                            "skills": [
                                {
                                    "id": "rides.request",
                                    "name": "Request ride",
                                    "description": "Requests a ride"
                                }
                            ]
                        },
                        "deployment": {
                            "runtime": "remote_http",
                            "endpoint": {
                                "url": "https://example.com/a2a",
                                "interaction_protocol": "google_a2a",
                                "protocol_binding": "JSONRPC"
                            }
                        },
                        "review": {"risk_level": "low"},
                    }));
                }
                Json(json!({
                    "agent_id": agent_id,
                    "provider_id": "provider-one",
                    "version": "0.1.0",
                    "status": "approved",
                    "agent_card": {
                        "name": "Agent Alpha",
                        "description": "Alpha test agent",
                        "cost": 18,
                        "currency": "USDC",
                        "supportsTask": true,
                        "capabilities": {
                            "extensions": [
                                {
                                    "uri": "https://github.com/google-a2a/a2a-x402/v0.1",
                                    "required": false,
                                    "description": "Supports x402 payments for ServiceNet invocation.",
                                    "params": {
                                        "accepts": [
                                            {
                                                "scheme": "exact",
                                                "network": "base",
                                                "asset": "0x0000000000000000000000000000000000000000",
                                                "payTo": "0x742d35Cc6634C0532925a3b844Bc454e4438f44e",
                                                "maxAmountRequired": "180000",
                                                "resource": "servicenet:agent:agent-alpha",
                                                "description": "ServiceNet agent invocation",
                                                "maxTimeoutSeconds": 600
                                            }
                                        ]
                                    }
                                }
                            ]
                        },
                        "skills": [
                            {
                                "id": "weather.lookup",
                                "name": "Get weather",
                                "description": "Returns current weather"
                            }
                        ]
                    },
                    "deployment": {
                        "runtime": "remote_http",
                        "endpoint": {
                            "url": "https://example.com/a2a",
                            "interaction_protocol": "google_a2a",
                            "protocol_binding": "JSONRPC"
                        }
                    },
                    "review": {"risk_level": "low"},
                }))
            }),
        )
        .route(
            "/v1/agents/{agent_id}/invoke",
            post(
                |Path(agent_id): Path<String>, Json(body): Json<Value>| async move {
                    Json(json!({
                        "agent_id": agent_id,
                        "status": "completed",
                        "receipt_id": "00000000-0000-0000-0000-000000000001",
                        "task_id": "task-42",
                        "context_id": "ctx-1",
                        "message": "ok",
                        "output": {
                            "echo": body["message"].clone(),
                            "agent_envelope_source": body["agent_envelope"]["source_agent_id"].clone(),
                        },
                        "settlement": body["settlement"].clone(),
                        "payment_receipt": {
                            "status": "submitted",
                            "rail": body["settlement"]["rail"].clone(),
                        },
                        "raw": {
                            "kind": "invoke",
                        },
                    }))
                },
            ),
        )
        .route(
            "/v1/agents/{agent_id}/invoke-async",
            post(|Path(agent_id): Path<String>, Json(_body): Json<Value>| async move {
                Json(json!({
                    "agent_id": agent_id,
                    "status": "running",
                    "receipt_id": "00000000-0000-0000-0000-000000000099",
                    "message": "ServiceNet invocation accepted",
                    "raw": {
                        "kind": "invoke_async",
                    },
                }))
            }),
        )
        .route(
            "/v1/receipts/{receipt_id}",
            get(|Path(receipt_id): Path<String>| async move {
                Json(json!({
                    "receipt": {
                        "receipt_id": receipt_id,
                        "agent_id": "agent-alpha",
                        "provider_id": "provider-one",
                        "status": "running",
                        "verification": "not_required",
                        "request_digest": "sha256:mock",
                        "started_at": "2026-05-24T00:00:00Z"
                    }
                }))
            }),
        )
        .route(
            "/v1/agents/{agent_id}/tasks/{task_id}/get",
            post(
                |Path((agent_id, task_id)): Path<(String, String)>, Json(body): Json<Value>| async move {
                    Json(json!({
                        "agent_id": agent_id,
                        "status": "completed",
                        "task_id": task_id,
                        "output": {
                            "history_length": body["history_length"].clone(),
                            "result": "done",
                        },
                        "raw": {
                            "kind": "task",
                        },
                    }))
                },
            ),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (addr, handle)
}

/// Build a fingerprinted `public_id` from a human slug and an agent's `did:key`.
fn scoped_id(slug: &str, agent_did: &str) -> String {
    let fp = fingerprint_from_did_key(agent_did).unwrap();
    build_scoped_public_id(slug, &fp)
}

type TopicSubscriptionRecord = (
    Option<String>,
    String,
    String,
    String,
    bool,
    Option<SwarmAgentEnvelope>,
);

struct MockSwarmBridge {
    local_node_id: String,
    agent_stats: BTreeMap<String, AgentStats>,
    network_status: SwarmNetworkStatusView,
    peers: Vec<SwarmPeerView>,
    subscriptions: Mutex<Vec<TopicSubscriptionRecord>>,
    messages: Mutex<Vec<SwarmTopicMessageView>>,
    relationship_views: Mutex<Vec<SwarmPeerRelationshipView>>,
    relationship_commands: Mutex<Vec<SwarmRelationshipActionCommand>>,
    dm_threads: Mutex<Vec<SwarmPeerDmThreadView>>,
    dm_messages: Mutex<BTreeMap<String, Vec<SwarmPeerDmMessageView>>>,
    dm_commands: Mutex<Vec<SwarmDirectMessageCommand>>,
    payment_commands: Mutex<Vec<SwarmAgentPaymentCommand>>,
    fail_accept_and_finalize: bool,
}

impl MockSwarmBridge {
    fn default_for(local_node_id: String) -> Self {
        Self {
            local_node_id,
            agent_stats: BTreeMap::new(),
            network_status: SwarmNetworkStatusView {
                running: false,
                mode: "local".to_owned(),
                peer_protocol_distribution: BTreeMap::new(),
            },
            peers: Vec::new(),
            subscriptions: Mutex::new(Vec::new()),
            messages: Mutex::new(Vec::new()),
            relationship_views: Mutex::new(Vec::new()),
            relationship_commands: Mutex::new(Vec::new()),
            dm_threads: Mutex::new(Vec::new()),
            dm_messages: Mutex::new(BTreeMap::new()),
            dm_commands: Mutex::new(Vec::new()),
            payment_commands: Mutex::new(Vec::new()),
            fail_accept_and_finalize: false,
        }
    }
}

#[async_trait::async_trait]
impl SwarmBridge for MockSwarmBridge {
    async fn agent_view(&self, agent_did: &str) -> anyhow::Result<SwarmAgentView> {
        Ok(SwarmAgentView {
            agent_did: agent_did.to_string(),
            stats: self.agent_stats.get(agent_did).cloned().unwrap_or_default(),
        })
    }

    async fn subscribe_topic(
        &self,
        network_id: Option<&str>,
        subscriber_id: &str,
        feed_key: &str,
        scope_hint: &str,
        active: bool,
        _agent_envelope: Option<SwarmAgentEnvelope>,
    ) -> anyhow::Result<()> {
        self.subscriptions.lock().await.push((
            network_id.map(ToOwned::to_owned),
            subscriber_id.to_string(),
            feed_key.to_string(),
            scope_hint.to_string(),
            active,
            _agent_envelope,
        ));
        Ok(())
    }

    async fn post_topic_message(
        &self,
        network_id: Option<&str>,
        feed_key: &str,
        scope_hint: &str,
        content: Value,
        reply_to_message_id: Option<String>,
        agent_envelope: Option<SwarmAgentEnvelope>,
    ) -> anyhow::Result<()> {
        let mut messages = self.messages.lock().await;
        let next_id = messages.len() + 1;
        messages.push(SwarmTopicMessageView {
            message_id: format!("msg-{next_id}"),
            network_id: network_id.map_or_else(
                || format!("local:{}", self.local_node_id),
                ToOwned::to_owned,
            ),
            feed_key: feed_key.to_string(),
            scope_hint: scope_hint.to_string(),
            author_node_id: self.local_node_id.clone(),
            agent_envelope,
            content,
            reply_to_message_id,
            created_at: Utc::now().timestamp_millis().max(0).cast_unsigned(),
        });
        Ok(())
    }

    async fn list_topic_messages(
        &self,
        network_id: Option<&str>,
        feed_key: &str,
        scope_hint: &str,
        limit: usize,
        _before_created_at: Option<u64>,
        _before_message_id: Option<String>,
    ) -> anyhow::Result<Vec<SwarmTopicMessageView>> {
        Ok(self
            .messages
            .lock()
            .await
            .iter()
            .filter(|message| {
                network_id.is_none_or(|network_id| message.network_id == network_id)
                    && message.feed_key == feed_key
                    && message.scope_hint == scope_hint
            })
            .take(limit)
            .cloned()
            .collect())
    }

    async fn topic_cursor(
        &self,
        _network_id: Option<&str>,
        feed_key: &str,
        subscriber_id: Option<&str>,
    ) -> anyhow::Result<Option<SwarmTopicCursorView>> {
        Ok(Some(SwarmTopicCursorView {
            subscriber_node_id: subscriber_id.unwrap_or(&self.local_node_id).to_string(),
            feed_key: feed_key.to_string(),
            scope_hint: "group:crew-7".to_string(),
            last_event_seq: self.messages.lock().await.len() as u64,
            updated_at: Utc::now().timestamp_millis().max(0).cast_unsigned(),
        }))
    }

    async fn topic_activity_snapshot(
        &self,
        network_id: Option<&str>,
        feed_key: &str,
        scope_hint: &str,
        limit: usize,
        subscriber_node_id: Option<&str>,
    ) -> anyhow::Result<SwarmTopicActivitySnapshot> {
        let messages = self
            .list_topic_messages(network_id, feed_key, scope_hint, limit, None, None)
            .await?;
        Ok(SwarmTopicActivitySnapshot {
            generated_at: Utc::now().timestamp_millis().max(0).cast_unsigned(),
            subscriber_node_id: subscriber_node_id
                .unwrap_or(&self.local_node_id)
                .to_string(),
            network_id: network_id.map_or_else(
                || format!("local:{}", self.local_node_id),
                ToOwned::to_owned,
            ),
            feed_key: feed_key.to_string(),
            scope_hint: scope_hint.to_string(),
            messages,
            cursor: None,
        })
    }

    async fn network_status(&self) -> anyhow::Result<SwarmNetworkStatusView> {
        Ok(self.network_status.clone())
    }

    async fn current_network_id(&self) -> anyhow::Result<String> {
        Ok(format!("local:{}", self.local_node_id))
    }

    async fn local_node_id(&self) -> anyhow::Result<String> {
        Ok(self.local_node_id.clone())
    }

    async fn peers(&self) -> anyhow::Result<Vec<SwarmPeerView>> {
        Ok(self.peers.clone())
    }

    async fn diagnostics(
        &self,
        _query: SwarmDiagnosticsQuery,
    ) -> anyhow::Result<SwarmDiagnosticsSnapshot> {
        Ok(SwarmDiagnosticsSnapshot {
            ok: true,
            generated_at: "1970-01-01T00:00:00Z".to_owned(),
            network_service_started: false,
            snapshot: None,
            diagnostics: Vec::new(),
        })
    }

    async fn list_peer_relationships(&self) -> anyhow::Result<Vec<SwarmPeerRelationshipView>> {
        Ok(self.relationship_views.lock().await.clone())
    }

    async fn send_peer_relationship_action(
        &self,
        command: SwarmRelationshipActionCommand,
    ) -> anyhow::Result<Value> {
        self.relationship_commands
            .lock()
            .await
            .push(command.clone());
        Ok(json!({
            "ok": true,
            "remote_node_id": command.remote_node_id,
            "action": command.action,
        }))
    }

    async fn list_peer_dm_threads(&self) -> anyhow::Result<Vec<SwarmPeerDmThreadView>> {
        Ok(self.dm_threads.lock().await.clone())
    }

    async fn list_peer_dm_messages(
        &self,
        thread_id: &str,
    ) -> anyhow::Result<Vec<SwarmPeerDmMessageView>> {
        Ok(self
            .dm_messages
            .lock()
            .await
            .get(thread_id)
            .cloned()
            .unwrap_or_default())
    }

    async fn send_peer_direct_message(
        &self,
        command: SwarmDirectMessageCommand,
    ) -> anyhow::Result<Value> {
        self.dm_commands.lock().await.push(command.clone());
        Ok(json!({
            "ok": true,
            "remote_node_id": command.remote_node_id,
            "message_kind": "direct",
        }))
    }

    async fn publish_agent_payment_message(
        &self,
        command: SwarmAgentPaymentCommand,
    ) -> anyhow::Result<Value> {
        self.payment_commands.lock().await.push(command.clone());
        Ok(json!({
            "ok": true,
            "remote_node_id": command.remote_node_id,
            "message_kind": command.message_kind,
        }))
    }

    async fn sample_task_contract(&self, task_id: &str) -> anyhow::Result<TaskContract> {
        Ok(TaskContract {
            protocol_version: "v0.1".to_owned(),
            task_id: task_id.to_owned(),
            task_type: "swarm".to_owned(),
            inputs: json!({"prompt":"hello"}),
            output_schema: json!({"type":"object"}),
            budget: wattswarm_protocol::types::Budget {
                time_ms: 30_000,
                max_steps: 10,
                cost_units: 1_000,
                mode: wattswarm_protocol::types::BudgetMode::Lifetime,
                explore_cost_units: 350,
                verify_cost_units: 450,
                finalize_cost_units: 200,
                reuse_verify_time_ms: 20_000,
                reuse_verify_cost_units: 200,
                reuse_max_attempts: 1,
            },
            assignment: wattswarm_protocol::types::Assignment {
                mode: "CLAIM".to_owned(),
                claim: wattswarm_protocol::types::ClaimPolicy {
                    lease_ms: 5_000,
                    max_concurrency: wattswarm_protocol::types::MaxConcurrency {
                        propose: 1,
                        verify: 1,
                    },
                },
                explore: wattswarm_protocol::types::ExploreAssignment {
                    max_proposers: 1,
                    topk: 3,
                    stop: wattswarm_protocol::types::ExploreStopPolicy {
                        no_new_evidence_rounds: 3,
                    },
                },
                verify: wattswarm_protocol::types::VerifyAssignment { max_verifiers: 1 },
                finalize: wattswarm_protocol::types::FinalizeAssignment { max_finalizers: 1 },
            },
            acceptance: wattswarm_protocol::types::Acceptance {
                quorum_threshold: 1,
                verifier_policy: wattswarm_protocol::types::PolicyBinding {
                    policy_id: "vp.schema_only.v1".to_owned(),
                    policy_version: "1".to_owned(),
                    policy_hash: "policy-hash".to_owned(),
                    policy_params: json!({}),
                },
                vote: wattswarm_protocol::types::VotePolicy {
                    commit_reveal: true,
                    reveal_deadline_ms: 10_000,
                },
                settlement: wattswarm_protocol::types::SettlementPolicy {
                    window_ms: 86_400_000,
                    implicit_weight: 0.1,
                    implicit_diminishing_returns:
                        wattswarm_protocol::types::SettlementDiminishingReturns { w: 10, k: 50 },
                    bad_penalty: wattswarm_protocol::types::SettlementBadPenalty { p: 3 },
                    feedback: wattswarm_protocol::types::FeedbackCapabilityPolicy {
                        mode: "CAPABILITY".to_owned(),
                        authority_pubkey: "ed25519:placeholder".to_owned(),
                    },
                },
                da_quorum_threshold: 1,
            },
            task_mode: wattswarm_protocol::types::TaskMode::OneShot,
            expiry_ms: chrono::Utc::now().timestamp_millis().max(0).cast_unsigned() + 86_400_000,
            evidence_policy: wattswarm_protocol::types::EvidencePolicy {
                max_inline_evidence_bytes: 65_536,
                max_inline_media_bytes: 0,
                inline_mime_allowlist: vec![
                    "application/json".to_owned(),
                    "text/plain".to_owned(),
                    "text/markdown".to_owned(),
                    "text/csv".to_owned(),
                ],
                max_snippet_bytes: 8_192,
                max_snippet_tokens: 2_048,
            },
        })
    }

    async fn submit_task(&self, contract: TaskContract) -> anyhow::Result<Value> {
        Ok(json!({
            "ok": true,
            "task_id": contract.task_id,
        }))
    }

    async fn submit_run(&self, command: SwarmRunSubmitCommand) -> anyhow::Result<Value> {
        let run_id = command
            .spec
            .get("run_id")
            .and_then(Value::as_str)
            .unwrap_or("mock-collective-run");
        Ok(json!({
            "ok": true,
            "run_id": run_id,
            "kicked_off": command.kickoff,
        }))
    }

    async fn import_task_contract(&self, contract: TaskContract) -> anyhow::Result<Value> {
        let scope_hint = contract
            .inputs
            .get("swarm_scope")
            .and_then(|value| {
                value.as_str().map(ToOwned::to_owned).or_else(|| {
                    let scope = value.as_object()?;
                    let kind = scope.get("kind")?.as_str()?;
                    let id = scope.get("id").and_then(Value::as_str).unwrap_or_default();
                    Some(if id.is_empty() {
                        kind.to_owned()
                    } else {
                        format!("{kind}:{id}")
                    })
                })
            })
            .map_or(Value::Null, Value::String);
        Ok(json!({
            "ok": true,
            "task_id": contract.task_id,
            "scope_hint": scope_hint,
        }))
    }

    async fn announce_task(&self, command: SwarmTaskAnnounceCommand) -> anyhow::Result<Value> {
        Ok(json!({
            "ok": true,
            "task_id": command.task_id,
            "feed_key": command.feed_key,
            "scope_hint": command.scope_hint,
        }))
    }

    async fn claim_task(&self, command: SwarmTaskClaimCommand) -> anyhow::Result<Value> {
        Ok(json!({
            "ok": true,
            "task_id": command.task_id,
            "execution_id": command.execution_id,
        }))
    }

    async fn propose_task_candidate(
        &self,
        command: SwarmTaskProposeCandidateCommand,
    ) -> anyhow::Result<Value> {
        Ok(json!({
            "ok": true,
            "task_id": command.task_id,
            "execution_id": command.execution_id,
            "candidate_id": command.candidate_id,
        }))
    }

    async fn accept_and_finalize_task(
        &self,
        task_id: &str,
        candidate_id: &str,
        _agent_envelope: Option<SwarmAgentEnvelope>,
    ) -> anyhow::Result<Value> {
        if self.fail_accept_and_finalize {
            return Err(anyhow::anyhow!("mock task finalize failure"));
        }
        Ok(json!({
            "ok": true,
            "status": "finalized",
            "task_id": task_id,
            "candidate_id": candidate_id,
        }))
    }

    async fn run_result_snapshot(&self, run_id: &str) -> anyhow::Result<SwarmRunResultSnapshot> {
        Ok(SwarmRunResultSnapshot {
            ok: true,
            result: json!({
                "run_id": run_id,
                "status": "finalized",
                "aggregation": {
                    "final_answer": "mock collective result"
                }
            }),
        })
    }

    async fn run_events_snapshot(
        &self,
        run_id: &str,
        _limit: usize,
    ) -> anyhow::Result<SwarmRunEventsSnapshot> {
        Ok(SwarmRunEventsSnapshot {
            ok: true,
            events: vec![json!({
                "run_id": run_id,
                "event_type": "RUN_KICKOFF"
            })],
        })
    }
}

#[derive(Debug, Serialize)]
struct ExpectedSignedAgentEnvelopePayload<'a> {
    protocol: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    transport_profile: Option<&'a String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_agent_id: Option<&'a String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    target_agent_id: Option<&'a String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_node_id: Option<&'a String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    target_node_id: Option<&'a String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    capability: Option<&'a String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_agent_card_hash: Option<&'a String>,
    message_json: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    extensions_json: Option<&'a String>,
}

#[derive(Debug, Serialize)]
struct ExpectedSignedSourceAgentCardPayload<'a> {
    agent_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    node_id: Option<&'a String>,
    card_hash: &'a str,
    issued_at: u64,
}

fn assert_envelope_signature_valid(envelope: &SwarmAgentEnvelope, public_key_b64: &str) {
    let message_json = serde_json::to_string(&envelope.message).unwrap();
    let extensions_json = envelope
        .extensions
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .unwrap();
    let payload = ExpectedSignedAgentEnvelopePayload {
        protocol: &envelope.protocol,
        transport_profile: envelope.transport_profile.as_ref(),
        source_agent_id: envelope.source_agent_id.as_ref(),
        target_agent_id: envelope.target_agent_id.as_ref(),
        source_node_id: envelope.source_node_id.as_ref(),
        target_node_id: envelope.target_node_id.as_ref(),
        capability: envelope.capability.as_ref(),
        source_agent_card_hash: envelope
            .source_agent_card
            .as_ref()
            .map(|card| &card.card_hash),
        message_json: &message_json,
        extensions_json: extensions_json.as_ref(),
    };
    let signature = envelope.signature.as_deref().expect("missing signature");
    assert!(
        verify_payload(&payload, signature, public_key_b64).unwrap(),
        "expected signed envelope to verify"
    );
    if let Some(card) = &envelope.source_agent_card {
        let card_payload = ExpectedSignedSourceAgentCardPayload {
            agent_id: &card.agent_id,
            node_id: card.node_id.as_ref(),
            card_hash: &card.card_hash,
            issued_at: card.issued_at,
        };
        let card_signature = card.signature.as_deref().expect("missing card signature");
        assert!(
            verify_payload(&card_payload, card_signature, public_key_b64).unwrap(),
            "expected signed source agent card to verify"
        );
    }
}

async fn bootstrap_broker_identity(app: Router, token: &str, agent_did: &str) -> String {
    let public_id = scoped_id("captain-aurora", agent_did);
    authed_post_json(
        app,
        token,
        "/v1/civilization/bootstrap-identity",
        json!({
            "public_id": public_id,
            "display_name": "Captain Aurora",
            "agent_did": agent_did,
            "faction": "freeport",
            "role": "broker",
            "strategy": "balanced",
            "home_subnet_id": "planet-test",
            "home_zone_id": "genesis-core"
        }),
    )
    .await;
    public_id
}

async fn bootstrap_broker_game(app: Router, token: &str, agent_did: &str) -> (Value, String) {
    let public_id = bootstrap_broker_identity(app.clone(), token, agent_did).await;
    let starter_bootstrap = authed_post_json(
        app,
        token,
        "/v1/game/starter-missions/bootstrap",
        json!({"public_id": public_id}),
    )
    .await;
    assert_eq!(starter_bootstrap["created"].as_array().unwrap().len(), 2);
    (starter_bootstrap, public_id)
}

fn seed_active_payment_account(state: &ControlPlaneState) -> String {
    let mut wallet_state = open_local_wallet(&state.data_dir).unwrap();
    let account = wallet_state
        .wallet
        .create_payment_account_web3_evm(
            &mut wallet_state.profile,
            Some("settlement".to_string()),
            Some("base-sepolia".to_string()),
            Some("x402".to_string()),
            1,
        )
        .unwrap();
    wallet_state
        .wallet
        .set_active_payment_account(&mut wallet_state.profile, &account.account_id, 2)
        .unwrap();
    wallet_state.save().unwrap();
    account.address.unwrap()
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
        "/v1/wattetheria/missions",
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

async fn settle_trade_mission_for_agent(app: Router, token: &str, agent_did: &str) -> Value {
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
        &format!("/v1/wattetheria/missions/{mission_id}/claim"),
        json!({"mission_id": mission_id, "agent_did": agent_did}),
    )
    .await;
    let _ = authed_post_json(
        app.clone(),
        token,
        &format!("/v1/wattetheria/missions/{mission_id}/complete"),
        json!({"mission_id": mission_id, "agent_did": agent_did}),
    )
    .await;
    let _ = authed_post_json(
        app,
        token,
        &format!("/v1/wattetheria/missions/{mission_id}/settle"),
        json!({"mission_id": mission_id}),
    )
    .await;
    mission
}

async fn seed_client_view_missions(app: Router, token: &str, agent_did: &str) {
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
    claim_mission(app.clone(), token, &active["mission_id"], agent_did).await;

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
    complete_and_settle_mission(app, token, &history["mission_id"], agent_did).await;
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

fn assert_game_status_payload(status_json: &Value, expected_public_id: &str) {
    assert_eq!(
        status_json["identity"]["public_identity"]["public_id"].as_str(),
        Some(expected_public_id)
    );
    assert!(status_json["bootstrap"]["progress_pct"].as_u64().unwrap() > 0);
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
        status_json["bootstrap_flow"]["action_cards"]
            .as_array()
            .unwrap()
            .len()
            >= 4
    );
    assert!(status_json["organizations"].as_array().is_some());
    assert!(
        status_json["supervision"]["next_actions"]
            .as_array()
            .is_some_and(|actions| !actions.is_empty())
    );
    assert!(status_json["supervision"]["alerts"].as_array().is_some());
    assert!(
        status_json["supervision"]["priority_cards"]
            .as_array()
            .is_some_and(|cards| !cards.is_empty())
    );
}

fn assert_game_mission_pack_payload(payload: &Value, expected_public_id: &str) {
    assert_eq!(
        payload["identity"]["public_identity"]["public_id"].as_str(),
        Some(expected_public_id)
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

fn assert_supervision_home_game_block(supervision_home_json: &Value, expected_public_id: &str) {
    assert_eq!(
        supervision_home_json["identity"]["public_identity"]["public_id"].as_str(),
        Some(expected_public_id)
    );
    assert!(supervision_home_json["mission_summary"]["eligible_open_count"].is_number());
    assert!(supervision_home_json["mission_summary"]["local_open_count"].is_number());
    assert!(supervision_home_json["mission_summary"]["travel_required_open_count"].is_number());
    assert!(supervision_home_json["mission_summary"]["active_count"].is_number());
    assert_eq!(
        supervision_home_json["home_planet"]["subnet_id"].as_str(),
        Some("planet-test")
    );
    assert_eq!(
        supervision_home_json["game"]["status"]["stage"].as_str(),
        Some("expansion")
    );
    assert!(
        supervision_home_json["game"]["starter_missions"]["templates"]
            .as_array()
            .unwrap()
            .len()
            >= 2
    );
    assert!(
        supervision_home_json["game"]["starter_missions"]["objective_chain"]["steps"]
            .as_array()
            .unwrap()
            .len()
            >= 2
    );
    assert!(
        supervision_home_json["game"]["mission_pack"]["templates"]
            .as_array()
            .unwrap()
            .len()
            >= 2
    );
    assert!(
        supervision_home_json["game"]["mission_pack"]["upcoming_templates"]
            .as_array()
            .unwrap()
            .len()
            == usize::try_from(
                supervision_home_json["game"]["mission_pack"]["summary"]["upcoming_template_count"]
                    .as_u64()
                    .unwrap()
            )
            .unwrap()
    );
    assert!(
        supervision_home_json["game"]["mission_pack"]["summary"]["upcoming_template_count"]
            .as_u64()
            .is_some()
    );
    assert!(
        supervision_home_json["game"]["bootstrap_flow"]["first_cycle_plan"]
            .as_array()
            .unwrap()
            .len()
            >= 2
    );
    assert!(supervision_home_json["organizations"].as_array().is_some());
    assert!(
        supervision_home_json["supervision"]["next_actions"]
            .as_array()
            .is_some_and(|actions| !actions.is_empty())
    );
    assert!(
        supervision_home_json["supervision"]["alerts"]
            .as_array()
            .is_some()
    );
    assert!(
        supervision_home_json["supervision"]["priority_cards"]
            .as_array()
            .is_some_and(|cards| !cards.is_empty())
    );
}

fn assert_client_mission_travel_views(supervision_home_json: &Value, my_missions_json: &Value) {
    assert_eq!(
        supervision_home_json["mission_summary"]["eligible_open_count"],
        2
    );
    assert_eq!(
        supervision_home_json["mission_summary"]["local_open_count"],
        1
    );
    assert_eq!(
        supervision_home_json["mission_summary"]["travel_required_open_count"],
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

fn assert_game_bootstrap_payload(payload: &Value, expected_public_id: &str) {
    assert_eq!(
        payload["identity"]["public_identity"]["public_id"].as_str(),
        Some(expected_public_id)
    );
    assert!(
        payload["bootstrap_flow"]["action_cards"]
            .as_array()
            .unwrap()
            .iter()
            .any(|card| card["key"].as_str() == Some("bootstrap_starter_missions"))
    );
    assert!(
        payload["bootstrap_flow"]["first_cycle_plan"]
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

async fn claim_mission(app: Router, token: &str, mission_id: &Value, agent_did: &str) {
    let mission_id = mission_id.as_str().expect("mission id");
    assert_eq!(
        authed_post(
            app,
            token,
            &format!("/v1/wattetheria/missions/{mission_id}/claim"),
            json!({"mission_id": mission_id, "agent_did": agent_did}),
        )
        .await,
        StatusCode::OK
    );
}

async fn complete_and_settle_mission(
    app: Router,
    token: &str,
    mission_id: &Value,
    agent_did: &str,
) {
    let mission_id = mission_id.as_str().expect("mission id");
    for action in ["claim", "complete"] {
        assert_eq!(
            authed_post(
                app.clone(),
                token,
                &format!("/v1/wattetheria/missions/{mission_id}/{action}"),
                json!({"mission_id": mission_id, "agent_did": agent_did}),
            )
            .await,
            StatusCode::OK
        );
    }
    assert_eq!(
        authed_post(
            app,
            token,
            &format!("/v1/wattetheria/missions/{mission_id}/settle"),
            json!({"mission_id": mission_id}),
        )
        .await,
        StatusCode::OK
    );
}

mod agent_event_tests;
mod civilization_tests;
mod client_tests;
mod diagnostics_tests;
mod galaxy_tests;
mod mcp_tests;
mod organization_tests;
mod servicenet_tests;
mod social_tests;
mod swarm_tests;
mod system_tests;
