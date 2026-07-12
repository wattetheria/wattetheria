use super::*;
#[tokio::test]
async fn state_requires_auth() {
    let (_dir, app, _token, _, _state) = build_test_app(10);
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
async fn brain_doctor_updates_attach_status() {
    let (dir, app, token, _, _state) = build_test_app(10);

    let status_before = authed_get_json(app.clone(), &token, "/v1/agent/attach/status").await;
    assert_eq!(status_before["status"].as_str(), Some("unknown"));
    assert_eq!(status_before["brain_provider"].as_str(), Some("rules"));

    let doctor_json = authed_post_json(app.clone(), &token, "/v1/brain/doctor", json!({})).await;
    assert_eq!(doctor_json["status"].as_str(), Some("connected"));
    assert_eq!(doctor_json["brain_connected"].as_bool(), Some(true));
    assert_eq!(doctor_json["control_plane_connected"].as_bool(), Some(true));

    let status_after = authed_get_json(app.clone(), &token, "/v1/agent/attach/status").await;
    assert_eq!(status_after["status"].as_str(), Some("connected"));
    assert_eq!(status_after["brain_connected"].as_bool(), Some(true));
    assert_eq!(
        status_after["control_plane_connected"].as_bool(),
        Some(true)
    );

    let persisted: Value = serde_json::from_str(
        &fs::read_to_string(dir.path().join(".agent-participation/status.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(persisted["status"].as_str(), Some("connected"));
}

#[tokio::test]
async fn brain_config_save_updates_deploy_env_and_runtime_label() {
    assert_brain_config_session_mode_roundtrip("stable_per_scope").await;
}

#[tokio::test]
async fn brain_config_preserves_new_per_interaction_session_mode() {
    assert_brain_config_session_mode_roundtrip("new_per_interaction").await;
}

async fn assert_brain_config_session_mode_roundtrip(runtime_session_mode: &str) {
    let (dir, app, token, _, _state) = build_test_app(10);

    let updated = request_json(
        app.clone(),
        axum::http::Request::builder()
            .method("PUT")
            .uri("/v1/brain/config")
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(axum::body::Body::from(
                json!({
                    "kind": "openai-compatible",
                    "adapter": "openclaw",
                    "session_header_name": "X-OpenClaw-Thread",
                    "runtime_session_mode": runtime_session_mode,
                    "base_url": "http://127.0.0.1:18789/v1",
                    "model": "openclaw",
                    "api_key": "secret-runtime-key"
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(updated["ok"].as_bool(), Some(true));
    assert_eq!(updated["restart_required"].as_bool(), Some(true));
    assert_eq!(
        updated["label"].as_str(),
        Some("adapter=openclaw model=openclaw url=http://127.0.0.1:18789/v1")
    );

    let env_path = dir.path().join("deploy/.env");
    let env_body = fs::read_to_string(&env_path).unwrap();
    assert!(env_body.contains("WATTETHERIA_BRAIN_PROVIDER_KIND=openai-compatible"));
    assert!(env_body.contains("WATTETHERIA_BRAIN_BASE_URL=http://127.0.0.1:18789/v1"));
    assert!(env_body.contains("WATTETHERIA_BRAIN_MODEL=openclaw"));
    assert!(env_body.contains("WATTETHERIA_BRAIN_API_KEY_ENV=WATTETHERIA_BRAIN_API_KEY"));
    assert!(env_body.contains("WATTETHERIA_BRAIN_RUNTIME_ADAPTER=openclaw"));
    assert!(env_body.contains("WATTETHERIA_BRAIN_SESSION_HEADER_NAME=X-OpenClaw-Thread"));
    assert!(env_body.contains(&format!(
        "WATTETHERIA_BRAIN_SESSION_MODE={runtime_session_mode}"
    )));
    assert!(env_body.contains("WATTETHERIA_BRAIN_API_KEY=secret-runtime-key"));
    assert!(!env_body.contains("WATTETHERIA_BRAIN_API_KEY_ENV=secret-runtime-key"));
    assert!(!env_body.lines().any(|line| line.starts_with("OPENCLAW_")));

    let config_body: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dir.path().join("config.json")).unwrap()).unwrap();
    assert_eq!(
        config_body["runtime_session_mode"].as_str(),
        Some(runtime_session_mode)
    );

    let loaded = authed_get_json(app.clone(), &token, "/v1/brain/config").await;
    assert_eq!(
        loaded["label"].as_str(),
        Some("adapter=openclaw model=openclaw url=http://127.0.0.1:18789/v1")
    );
    assert_eq!(loaded["runtime_adapter"].as_str(), Some("openclaw"));
    assert_eq!(
        loaded["session_header_name"].as_str(),
        Some("X-OpenClaw-Thread")
    );
    assert_eq!(
        loaded["runtime_session_mode"].as_str(),
        Some(runtime_session_mode)
    );
    assert_eq!(
        loaded["config"]["runtime_adapter"]["session_header_name"].as_str(),
        Some("X-OpenClaw-Thread")
    );
    assert_eq!(
        loaded["env_path"].as_str(),
        Some(env_path.to_string_lossy().as_ref())
    );
    assert_eq!(loaded["has_api_key"].as_bool(), Some(true));
    assert!(loaded["config"].get("api_key").is_none());

    let preserved = request_json(
        app.clone(),
        axum::http::Request::builder()
            .method("PUT")
            .uri("/v1/brain/config")
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json")
            .body(axum::body::Body::from(
                json!({
                    "kind": "openai-compatible",
                    "adapter": "openclaw",
                    "session_header_name": "X-OpenClaw-Thread",
                    "base_url": "http://127.0.0.1:18789/v1",
                    "model": "openclaw"
                })
                .to_string(),
            ))
            .unwrap(),
    )
    .await;
    assert_eq!(
        preserved["runtime_session_mode"].as_str(),
        Some(runtime_session_mode)
    );
}

#[tokio::test]
async fn night_shift_alias_endpoints_match_primary_routes() {
    let (_dir, app, token, _, _state) = build_test_app(20);
    let summary_json = authed_get_json(app.clone(), &token, "/v1/night-shift?hours=12").await;
    let summary_alias_json =
        authed_get_json(app.clone(), &token, "/v1/night-shift/summary?hours=12").await;
    assert_eq!(summary_alias_json, summary_json);

    let narrative_json = authed_get_json(app, &token, "/v1/night-shift/narrative?hours=12").await;
    assert_eq!(narrative_json["hours"].as_i64(), Some(12));
    assert!(narrative_json["report"].is_object());
}

#[tokio::test]
async fn policy_flow_pending_then_approve_once() {
    let (_dir, app, token, _policy, _state) = build_test_app(20);

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
    let (_dir, app, token, _, state) = build_test_app(30);

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
    let agent_did = state_json["agent_did"].as_str().unwrap().to_string();

    let create_body = json!({
        "subnet_id": "planet-test",
        "kind": "update_tax_rate",
        "payload": {"tax_rate": 0.09},
        "created_by": agent_did,
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
        "voter": state_json["agent_did"],
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
        serde_json::from_slice(&list_resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(list_json.as_array().unwrap().len(), 1);
    let persisted = state
        .local_db
        .load_domain::<GovernanceEngine>(wattetheria_kernel::local_db::domain::GOVERNANCE)
        .unwrap()
        .unwrap();
    assert_eq!(persisted.list_proposals(Some("planet-test")).len(), 1);
}

#[tokio::test]
async fn unsupported_action_is_rejected() {
    let (_dir, app, token, _, _state) = build_test_app(20);

    let response = app
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/v1/actions")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(axum::body::Body::from(
                    json!({"action": "task.unsupported"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn mailbox_send_fetch_ack_persists() {
    let (_dir, app, token, _, state) = build_test_app(30);

    let send_resp = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .method("POST")
                .uri("/v1/wattetheria/mailbox/messages")
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
        serde_json::from_slice(&send_resp.into_body().collect().await.unwrap().to_bytes()).unwrap();
    let message_id = send_json["message_id"].as_str().unwrap().to_string();

    let fetch_resp = app
        .clone()
        .oneshot(
            axum::http::Request::builder()
                .uri("/v1/wattetheria/mailbox/messages?subnet_id=planet-b")
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
                .uri("/v1/wattetheria/mailbox/ack")
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

    let persisted = state
        .local_db
        .load_domain::<CrossSubnetMailbox>(wattetheria_kernel::local_db::domain::MAILBOX)
        .unwrap()
        .unwrap();
    assert!(persisted.fetch_for_subnet("planet-b").is_empty());
}

#[tokio::test]
async fn events_export_is_public_for_recovery() {
    let (_dir, app, _token, _, _state) = build_test_app(10);

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
    let (_dir, app, token, _, _state) = build_test_app(20);

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
    let default_public_id = state_json["identity"]["public_identity"]["public_id"]
        .as_str()
        .unwrap();
    assert!(
        extract_public_id_fingerprint(default_public_id).is_some(),
        "default public_id should be fingerprinted: {default_public_id}"
    );
    let agent_did = state_json["agent_did"].as_str().unwrap();

    let profile_body = json!({
        "agent_did": agent_did,
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
        Some(agent_did)
    );
}

#[tokio::test]
async fn public_identity_and_controller_binding_flow_works() {
    let (_dir, app, token, _, state) = build_test_app(20);

    let state_json = authed_get_json(app.clone(), &token, "/v1/state").await;
    let agent_did = state_json["agent_did"].as_str().unwrap();

    let default_identity =
        authed_get_json(app.clone(), &token, "/v1/civilization/public-identity").await;
    let default_public_id = default_identity["public_identity"]["public_id"]
        .as_str()
        .unwrap();
    assert!(
        extract_public_id_fingerprint(default_public_id).is_some(),
        "default public_id should be fingerprinted: {default_public_id}"
    );
    assert_eq!(
        default_identity["public_memory_owner"]["controller_id"].as_str(),
        Some(agent_did)
    );

    let default_binding =
        authed_get_json(app.clone(), &token, "/v1/civilization/controller-binding").await;
    assert_eq!(
        default_binding["controller_binding"]["controller_kind"].as_str(),
        Some("local_wattswarm")
    );

    let agent_alpha = scoped_id("agent-alpha", agent_did);
    let public_identity_status = authed_post(
        app.clone(),
        &token,
        "/v1/civilization/public-identity",
        json!({
            "public_id": agent_alpha,
            "display_name": "Agent Alpha",
            "agent_did": agent_did,
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
            "public_id": agent_alpha,
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
        &format!("/v1/civilization/public-identity?public_id={agent_alpha}"),
    )
    .await;
    assert_eq!(
        fetched_identity["public_identity"]["display_name"].as_str(),
        Some("Agent Alpha")
    );

    let fetched_binding = authed_get_json(
        app,
        &token,
        &format!("/v1/civilization/controller-binding?public_id={agent_alpha}"),
    )
    .await;
    assert_eq!(
        fetched_binding["controller_binding"]["controller_ref"].as_str(),
        Some("openclaw://alpha")
    );
    assert_eq!(
        fetched_binding["public_memory_owner"]["public_id"].as_str(),
        Some(agent_alpha.as_str())
    );

    let persisted_identities: PublicIdentityRegistry = state
        .local_db
        .load_domain(wattetheria_kernel::local_db::domain::PUBLIC_IDENTITY_REGISTRY)
        .unwrap()
        .unwrap();
    assert!(persisted_identities.get(&agent_alpha).is_some());

    let persisted_bindings: ControllerBindingRegistry = state
        .local_db
        .load_domain(wattetheria_kernel::local_db::domain::CONTROLLER_BINDING_REGISTRY)
        .unwrap()
        .unwrap();
    assert!(persisted_bindings.get(&agent_alpha).is_some());
}

#[tokio::test]
async fn public_identity_display_name_patch_preserves_binding() {
    let (_dir, app, token, _, _state) = build_test_app(20);

    let state_json = authed_get_json(app.clone(), &token, "/v1/state").await;
    let agent_did = state_json["agent_did"].as_str().unwrap();
    let public_id = scoped_id("agent-alpha", agent_did);
    let created_status = authed_post(
        app.clone(),
        &token,
        "/v1/civilization/public-identity",
        json!({
            "public_id": public_id,
            "display_name": "Agent Alpha",
            "agent_did": agent_did,
            "active": true
        }),
    )
    .await;
    assert_eq!(created_status, StatusCode::OK);

    let patched_identity = authed_patch_json(
        app,
        &token,
        "/v1/civilization/public-identity",
        json!({
            "public_id": public_id,
            "display_name": "Agent Alpha Prime"
        }),
    )
    .await;

    assert_eq!(
        patched_identity["public_identity"]["display_name"].as_str(),
        Some("Agent Alpha Prime")
    );
    assert_eq!(
        patched_identity["public_identity"]["agent_did"].as_str(),
        Some(agent_did)
    );
    assert_eq!(
        patched_identity["public_identity"]["active"].as_bool(),
        Some(true)
    );
}
