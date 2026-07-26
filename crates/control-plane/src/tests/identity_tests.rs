use super::*;
use wattetheria_kernel::agent_identity::{AgentIdentityStore, FileAgentIdentityStore};
use wattetheria_kernel::provider_identity::FileProviderIdentityStore;

async fn authed_delete(app: Router, token: &str, uri: &str) -> StatusCode {
    app.oneshot(
        axum::http::Request::builder()
            .method("DELETE")
            .uri(uri)
            .header("authorization", format!("Bearer {token}"))
            .body(axum::body::Body::empty())
            .unwrap(),
    )
    .await
    .unwrap()
    .status()
}

async fn identity_export_request(
    app: Router,
    token: Option<&str>,
    uri: &str,
) -> (StatusCode, axum::http::HeaderMap, Value) {
    let mut builder = axum::http::Request::builder().method("POST").uri(uri);
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    let response = app
        .oneshot(builder.body(axum::body::Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let value = serde_json::from_slice(&body).unwrap();
    (status, headers, value)
}

fn save_published_service_agent(state: &ControlPlaneState, agent_id: &str, service_did: &str) {
    crate::routes::servicenet::publish::save_publisher_state(
        &state.data_dir,
        &crate::routes::servicenet::publish::ServiceNetPublisherState {
            registrations: vec![
                crate::routes::servicenet::publish::ServiceNetPublisherRegistration {
                    provider_id: "provider-test".to_owned(),
                    provider_did: state.servicenet_provider.did.clone(),
                    agent_id: agent_id.to_owned(),
                    service_did: service_did.to_owned(),
                    service_address: Some("published-agent@wattetheria".to_owned()),
                    card_hash: "sha256:test".to_owned(),
                    version: "0.1.0".to_owned(),
                    updated_at: Utc::now().to_rfc3339(),
                    execution: wattetheria_kernel::servicenet::ServiceAgentExecution::default(),
                    agent_card: json!({"name": "Published Agent"}),
                    deployment: json!({}),
                    review: json!({}),
                },
            ],
        },
    )
    .unwrap();
}

#[tokio::test]
async fn provider_identity_is_stable_distinct_and_manages_service_agent_dids() {
    let (_dir, router, token, _, state) = build_test_app(20);
    let service_identity = FileServiceAgentIdentityStore::new(&state.data_dir)
        .generate()
        .unwrap();

    let provider = authed_get_json(router, &token, "/v1/wattetheria/provider-identity").await;

    assert_eq!(provider["provider_did"], state.servicenet_provider.did);
    assert_ne!(provider["provider_did"], state.agent_did);
    assert!(provider.get("private_key").is_none());
    assert_eq!(provider["status"], "active");
    assert_eq!(provider["managed_service_agents"], 1);
    assert!(
        provider["identity_uri"]
            .as_str()
            .unwrap()
            .contains(provider["provider_did"].as_str().unwrap())
    );
    assert!(provider["fingerprint"].as_str().is_some());
    assert!(
        FileServiceAgentIdentityStore::new(&state.data_dir)
            .service_agent_identity_path(&service_identity.service_agent_identity_id)
            .starts_with(
                state
                    .data_dir
                    .join(".provider-identity")
                    .join("service-agents"),
            )
    );
}

#[tokio::test]
async fn provider_did_export_is_authenticated_and_reimportable() {
    let (_dir, router, token, _, state) = build_test_app(20);
    let (unauthorized, _, _) = identity_export_request(
        router.clone(),
        None,
        "/v1/wattetheria/provider-identity/export",
    )
    .await;
    let (status, headers, backup) = identity_export_request(
        router,
        Some(&token),
        "/v1/wattetheria/provider-identity/export",
    )
    .await;
    let restored = Identity::import_ed25519_private_key(
        backup["provider_did"].as_str(),
        backup["private_key"].as_str().unwrap(),
    )
    .unwrap();

    assert_eq!(unauthorized, StatusCode::UNAUTHORIZED);
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["cache-control"], "no-store");
    assert_eq!(backup["format"], "wattetheria-did-key-backup");
    assert_eq!(backup["version"], 1);
    assert_eq!(backup["identity_kind"], "provider");
    assert_eq!(backup["provider_did"], state.servicenet_provider.did);
    assert_eq!(restored.public_key, backup["public_key"]);
}

#[tokio::test]
async fn service_agent_identity_is_generated_before_publication_and_never_returns_private_key() {
    let (_dir, router, token, _, _state) = build_test_app(20);

    let generated = authed_post_json(
        router.clone(),
        &token,
        "/v1/wattetheria/agent-identities/service-agents/generate",
        json!({}),
    )
    .await;
    let listed = authed_get_json(
        router,
        &token,
        "/v1/wattetheria/agent-identities/service-agents",
    )
    .await;

    assert!(generated["service_agent_identity_id"].as_str().is_some());
    assert_eq!(generated["bound_agent_id"], Value::Null);
    assert_eq!(generated["binding_status"], "unbound");
    assert_eq!(generated["key_origin"], "generated");
    assert_eq!(generated["agent_card_status"], "draft");
    assert_eq!(generated["agent_name"], Value::Null);
    assert_eq!(generated["service_address"], Value::Null);
    assert!(generated.get("private_key").is_none());
    assert_eq!(listed["items"][0]["service_did"], generated["service_did"]);
    assert!(listed["items"][0].get("private_key").is_none());
}

#[tokio::test]
async fn runtime_and_service_agent_did_exports_are_authenticated_and_reimportable() {
    let (_dir, router, token, _, state) = build_test_app(20);
    let (unauthorized, _, _) = identity_export_request(
        router.clone(),
        None,
        "/v1/wattetheria/agent-identities/runtime/export",
    )
    .await;
    let (runtime_status, runtime_headers, runtime) = identity_export_request(
        router.clone(),
        Some(&token),
        "/v1/wattetheria/agent-identities/runtime/export",
    )
    .await;
    let restored_runtime = Identity::import_ed25519_private_key(
        runtime["agent_did"].as_str(),
        runtime["private_key"].as_str().unwrap(),
    )
    .unwrap();

    assert_eq!(unauthorized, StatusCode::UNAUTHORIZED);
    assert_eq!(runtime_status, StatusCode::OK);
    assert_eq!(runtime_headers["cache-control"], "no-store");
    assert_eq!(runtime["format"], "wattetheria-did-key-backup");
    assert_eq!(runtime["version"], 1);
    assert_eq!(runtime["identity_kind"], "runtime_agent");
    assert_eq!(runtime["agent_did"], state.agent_did);
    assert_eq!(restored_runtime.public_key, runtime["public_key"]);

    let generated = authed_post_json(
        router.clone(),
        &token,
        "/v1/wattetheria/agent-identities/service-agents/generate",
        json!({}),
    )
    .await;
    let service_agent_identity_id = generated["service_agent_identity_id"].as_str().unwrap();
    let (service_status, service_headers, service) = identity_export_request(
        router.clone(),
        Some(&token),
        &format!(
            "/v1/wattetheria/agent-identities/service-agents/{service_agent_identity_id}/export"
        ),
    )
    .await;
    let restored_service =
        wattetheria_kernel::agent_identity::service_agent::ServiceAgentIdentity::import(
            service["service_did"].as_str(),
            service["private_key"].as_str().unwrap(),
        )
        .unwrap();

    assert_eq!(service_status, StatusCode::OK);
    assert_eq!(service_headers["cache-control"], "no-store");
    assert_eq!(service["format"], "wattetheria-did-key-backup");
    assert_eq!(service["identity_kind"], "service_agent");
    assert_eq!(service["service_did"], generated["service_did"]);
    assert_eq!(restored_service.public_key, service["public_key"]);
    assert!(service.get("service_agent_identity_id").is_none());
    assert!(service.get("bound_agent_id").is_none());

    let (missing_status, _, _) = identity_export_request(
        router,
        Some(&token),
        "/v1/wattetheria/agent-identities/service-agents/sid-missing/export",
    )
    .await;
    assert_eq!(missing_status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn published_service_agent_identity_exposes_name_and_address_for_console_display() {
    let (_dir, router, token, _, state) = build_test_app(20);
    let store = FileServiceAgentIdentityStore::new(&state.data_dir);
    let identity = store.generate().unwrap();
    drop(
        store
            .provision(
                &identity.service_agent_identity_id,
                "published-agent",
                "https://agent.example.com/a2a",
            )
            .unwrap(),
    );
    save_published_service_agent(&state, "published-agent", &identity.service_did);

    let listed = authed_get_json(
        router,
        &token,
        "/v1/wattetheria/agent-identities/service-agents",
    )
    .await;

    assert_eq!(listed["items"][0]["agent_name"], "Published Agent");
    assert_eq!(
        listed["items"][0]["service_address"],
        "published-agent@wattetheria"
    );
}

#[tokio::test]
async fn importing_service_agent_private_key_creates_an_unbound_identity() {
    let (_dir, router, token, _, _state) = build_test_app(20);
    let portable =
        wattetheria_kernel::agent_identity::service_agent::ServiceAgentIdentity::generate()
            .unwrap();

    let imported = authed_post_json(
        router.clone(),
        &token,
        "/v1/wattetheria/agent-identities/service-agents/import",
        json!({
            "service_did": portable.service_did,
            "private_key": portable.private_key,
        }),
    )
    .await;
    let listed = authed_get_json(
        router,
        &token,
        "/v1/wattetheria/agent-identities/service-agents",
    )
    .await;

    assert_eq!(
        imported["service_agent_identity_id"],
        portable.service_agent_identity_id
    );
    assert_eq!(imported["bound_agent_id"], Value::Null);
    assert_eq!(imported["key_origin"], "imported");
    assert_eq!(listed["items"][0]["service_did"], imported["service_did"]);
}

#[tokio::test]
async fn unpublished_service_agent_identity_can_be_deleted() {
    let (_dir, router, token, _, state) = build_test_app(20);
    let store = FileServiceAgentIdentityStore::new(&state.data_dir);
    let identity = store.generate().unwrap();
    let service_agent_identity_path =
        store.service_agent_identity_path(&identity.service_agent_identity_id);
    let uri = format!(
        "/v1/wattetheria/agent-identities/service-agents/{}",
        identity.service_agent_identity_id
    );

    let deleted = authed_delete(router.clone(), &token, &uri).await;
    let listed = authed_get_json(
        router.clone(),
        &token,
        "/v1/wattetheria/agent-identities/service-agents",
    )
    .await;
    let missing = authed_delete(router, &token, &uri).await;

    assert_eq!(deleted, StatusCode::NO_CONTENT);
    assert_eq!(listed["items"], json!([]));
    assert!(!service_agent_identity_path.exists());
    assert_eq!(missing, StatusCode::NOT_FOUND);
    let audit_entries = state.audit_log.list_recent(10).unwrap();
    assert!(audit_entries.iter().any(|entry| {
        entry.action == "agent_identity.service_agent.delete"
            && entry.subject.as_deref() == Some(identity.service_did.as_str())
    }));
}

#[tokio::test]
async fn service_agent_identity_delete_requires_authentication() {
    let (_dir, router, _token, _, state) = build_test_app(20);
    let store = FileServiceAgentIdentityStore::new(&state.data_dir);
    let identity = store.generate().unwrap();

    let status = router
        .oneshot(
            axum::http::Request::builder()
                .method("DELETE")
                .uri(format!(
                    "/v1/wattetheria/agent-identities/service-agents/{}",
                    identity.service_agent_identity_id
                ))
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
        .status();

    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(store.load(&identity.service_agent_identity_id).is_ok());
}

#[tokio::test]
async fn published_service_agent_identity_must_be_unpublished_before_deletion() {
    let (_dir, router, token, _, state) = build_test_app(20);
    let store = FileServiceAgentIdentityStore::new(&state.data_dir);
    let identity = store.generate().unwrap();
    drop(
        store
            .provision(
                &identity.service_agent_identity_id,
                "published-agent",
                "https://agent.example.com/a2a",
            )
            .unwrap(),
    );
    save_published_service_agent(&state, "published-agent", &identity.service_did);

    let status = authed_delete(
        router,
        &token,
        &format!(
            "/v1/wattetheria/agent-identities/service-agents/{}",
            identity.service_agent_identity_id
        ),
    )
    .await;

    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        store
            .load(&identity.service_agent_identity_id)
            .unwrap()
            .service_did,
        identity.service_did
    );
}

#[tokio::test]
async fn runtime_import_preview_derives_identity_without_staging_it() {
    let (dir, router, token, _, state) = build_test_app(20);
    let imported = Identity::new_random();
    let preview = authed_post_json(
        router,
        &token,
        "/v1/wattetheria/agent-identities/runtime/import/preview",
        json!({
            "private_key": imported.private_key,
        }),
    )
    .await;

    assert_eq!(preview["agent_did"], imported.agent_did);
    assert_eq!(
        preview["identity_uri"],
        format!(
            "wattetheria://mainnet.watt-etheria/identity/{}",
            imported.agent_did
        )
    );
    assert_eq!(
        preview["fingerprint"],
        fingerprint_from_did_key(&imported.agent_did).unwrap()
    );
    assert!(preview.get("private_key").is_none());
    assert_eq!(state.agent_did, state.identity.agent_did);
    assert_ne!(state.agent_did, imported.agent_did);
    let store = FileAgentIdentityStore::new(dir.path());
    assert!(store.pending_import().unwrap().is_none());
    assert!(!store.pending_identity_path().exists());
    assert!(
        state
            .audit_log
            .list_recent(20)
            .unwrap()
            .iter()
            .all(|entry| entry.action != "agent_identity.runtime.import_staged")
    );
}

#[tokio::test]
async fn runtime_import_preview_requires_authentication() {
    let (dir, router, _token, _, state) = build_test_app(20);
    let imported = Identity::new_random();
    let response = router
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/v1/wattetheria/agent-identities/runtime/import/preview")
                .header("content-type", "application/json")
                .body(axum::body::Body::from(
                    json!({ "private_key": imported.private_key }).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let store = FileAgentIdentityStore::new(dir.path());
    assert!(store.pending_import().unwrap().is_none());
    assert!(
        state
            .audit_log
            .list_recent(20)
            .unwrap()
            .iter()
            .all(|entry| entry.action != "agent_identity.runtime.import_staged")
    );
}

#[tokio::test]
async fn runtime_import_confirmation_rejects_a_key_that_differs_from_preview() {
    let (dir, router, token, _, _) = build_test_app(20);
    let previewed = Identity::new_random();
    let changed = Identity::new_random();
    let response = router
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/v1/wattetheria/agent-identities/runtime/import")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(axum::body::Body::from(
                    json!({
                        "agent_did": previewed.agent_did,
                        "private_key": changed.private_key,
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let store = FileAgentIdentityStore::new(dir.path());
    assert!(store.pending_import().unwrap().is_none());
    assert!(!store.pending_identity_path().exists());
}

#[tokio::test]
async fn service_agent_import_preview_derives_identity_without_persisting_it() {
    let (_dir, router, token, _, state) = build_test_app(20);
    let portable =
        wattetheria_kernel::agent_identity::service_agent::ServiceAgentIdentity::generate()
            .unwrap();
    let store = FileServiceAgentIdentityStore::new(&state.data_dir);
    let initial_count = store.list().unwrap().len();

    let preview = authed_post_json(
        router,
        &token,
        "/v1/wattetheria/agent-identities/service-agents/import/preview",
        json!({
            "private_key": portable.private_key,
        }),
    )
    .await;

    assert_eq!(preview["service_did"], portable.service_did);
    assert_eq!(
        preview["identity_uri"],
        format!(
            "wattetheria://mainnet.watt-etheria/identity/{}",
            portable.service_did
        )
    );
    assert_eq!(
        preview["fingerprint"],
        fingerprint_from_did_key(&portable.service_did).unwrap()
    );
    assert!(preview.get("private_key").is_none());
    assert_eq!(store.list().unwrap().len(), initial_count);
    assert!(
        state
            .audit_log
            .list_recent(20)
            .unwrap()
            .iter()
            .all(|entry| entry.action != "agent_identity.service_agent.import")
    );
}

#[tokio::test]
async fn service_agent_import_confirmation_rejects_a_key_that_differs_from_preview() {
    let (_dir, router, token, _, state) = build_test_app(20);
    let previewed =
        wattetheria_kernel::agent_identity::service_agent::ServiceAgentIdentity::generate()
            .unwrap();
    let changed =
        wattetheria_kernel::agent_identity::service_agent::ServiceAgentIdentity::generate()
            .unwrap();
    let store = FileServiceAgentIdentityStore::new(&state.data_dir);
    let initial_count = store.list().unwrap().len();

    let response = router
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/v1/wattetheria/agent-identities/service-agents/import")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(axum::body::Body::from(
                    json!({
                        "service_did": previewed.service_did,
                        "private_key": changed.private_key,
                    })
                    .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(store.list().unwrap().len(), initial_count);
}

#[tokio::test]
async fn runtime_import_is_staged_until_restart_then_source_agent_card_uses_the_new_did() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileAgentIdentityStore::new(dir.path());
    let initial = store.load_or_create().unwrap();
    let imported = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge: Arc<dyn SwarmBridge> =
        Arc::new(MockSwarmBridge::default_for(initial.agent_did.clone()));
    let (dir, router, token, _, state) =
        build_test_app_with_bridge(20, dir, initial.clone(), event_log, bridge);

    let staged = authed_post_json(
        router,
        &token,
        "/v1/wattetheria/agent-identities/runtime/import",
        json!({
            "agent_did": imported.agent_did,
            "private_key": imported.private_key,
        }),
    )
    .await;

    assert_eq!(staged["status"], "pending_restart");
    assert_eq!(state.agent_did, initial.agent_did);
    let (activated, _) = FileAgentIdentityStore::new(dir.path())
        .load_or_create_runtime_identity()
        .unwrap();
    assert_eq!(activated.agent_did, imported.agent_did);

    let restarted_event_log = EventLog::new(dir.path().join("events-restarted.jsonl")).unwrap();
    let restarted_bridge: Arc<dyn SwarmBridge> =
        Arc::new(MockSwarmBridge::default_for(activated.agent_did.clone()));
    let (_dir, restarted_router, restarted_token, _, _) = build_test_app_with_bridge(
        20,
        dir,
        activated.clone(),
        restarted_event_log,
        restarted_bridge,
    );
    let card = authed_get_json(
        restarted_router,
        &restarted_token,
        "/v1/wattetheria/source-agent-card",
    )
    .await;

    assert_eq!(card["agent_id"], activated.agent_did);
    assert_eq!(card["card"]["metadata"]["public_key"], activated.public_key);
}

#[tokio::test]
async fn published_service_agents_do_not_block_runtime_or_service_did_imports() {
    let (dir, router, token, _, state) = build_test_app(20);
    let store = FileServiceAgentIdentityStore::new(&state.data_dir);
    let current = store.generate().unwrap();
    drop(
        store
            .provision(
                &current.service_agent_identity_id,
                "published-agent",
                "https://agent.example.com/a2a",
            )
            .unwrap(),
    );
    save_published_service_agent(&state, "published-agent", &current.service_did);
    let replacement = Identity::new_random();
    let portable =
        wattetheria_kernel::agent_identity::service_agent::ServiceAgentIdentity::generate()
            .unwrap();

    let runtime_import = authed_post_json(
        router.clone(),
        &token,
        "/v1/wattetheria/agent-identities/runtime/import",
        json!({
            "agent_did": replacement.agent_did,
            "private_key": replacement.private_key,
        }),
    )
    .await;
    let imported = authed_post_json(
        router,
        &token,
        "/v1/wattetheria/agent-identities/service-agents/import",
        json!({
            "service_did": portable.service_did,
            "private_key": portable.private_key,
        }),
    )
    .await;

    assert_eq!(runtime_import["status"], "pending_restart");
    assert_eq!(
        runtime_import["pending_identity"]["agent_did"],
        replacement.agent_did
    );
    assert_eq!(
        imported["service_agent_identity_id"],
        portable.service_agent_identity_id
    );
    assert_eq!(imported["binding_status"], "unbound");
    assert_eq!(
        store
            .load(&current.service_agent_identity_id)
            .unwrap()
            .service_did,
        current.service_did
    );
    let identity_store = FileAgentIdentityStore::new(dir.path());
    let provider = FileProviderIdentityStore::new(dir.path())
        .load_or_create()
        .unwrap();
    let (activated, activation) = identity_store.load_or_create_runtime_identity().unwrap();

    assert_eq!(
        activation,
        wattetheria_kernel::agent_identity::RuntimeIdentityActivation::RestartRequired
    );
    assert_eq!(activated.agent_did, replacement.agent_did);
    assert_eq!(provider.agent_did, state.servicenet_provider.did);
}

#[tokio::test]
async fn runtime_replacement_keeps_published_service_agent_visible_and_unpublishable() {
    let (servicenet_addr, servicenet_server) = spawn_mock_servicenet().await;
    let (_dir, _router, token, _, state) = build_test_app(20);
    let provider_did = state.servicenet_provider.did.clone();
    let replacement = Identity::new_random();
    let store = FileServiceAgentIdentityStore::new(&state.data_dir);
    let service_identity = store.generate().unwrap();
    drop(
        store
            .provision(
                &service_identity.service_agent_identity_id,
                "published-agent",
                "https://agent.example.com/a2a",
            )
            .unwrap(),
    );
    save_published_service_agent(&state, "published-agent", &service_identity.service_did);
    let state = ControlPlaneState {
        agent_did: replacement.agent_did.clone(),
        identity: replacement.compat_view(),
        signer: Arc::new(replacement),
        servicenet_client: Some(Arc::new(
            ServiceNetClient::new(format!("http://{servicenet_addr}")).unwrap(),
        )),
        ..state
    };
    let router = app(state.clone());

    let published = authed_get_json(
        router.clone(),
        &token,
        "/v1/wattetheria/servicenet/published-agents",
    )
    .await;
    let unpublished = authed_post_json(
        router.clone(),
        &token,
        "/v1/wattetheria/servicenet/agents/published-agent/unpublish",
        json!({"reason": "runtime identity replaced"}),
    )
    .await;
    let after = authed_get_json(
        router,
        &token,
        "/v1/wattetheria/servicenet/published-agents",
    )
    .await;

    assert_ne!(state.agent_did, provider_did);
    assert_eq!(published["count"], 1);
    assert_eq!(published["provider_did"], provider_did);
    assert_eq!(published["items"][0]["agent_id"], "published-agent");
    assert_eq!(unpublished["status"], "ok");
    assert_eq!(unpublished["provider_did"], provider_did);
    assert_eq!(after["count"], 0);
    servicenet_server.abort();
}
