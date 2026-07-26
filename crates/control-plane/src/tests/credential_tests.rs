use super::*;

async fn authed_json_request(
    app: Router,
    token: &str,
    method: &str,
    uri: &str,
    body: Value,
) -> (StatusCode, Value) {
    let response = app
        .oneshot(
            axum::http::Request::builder()
                .method(method)
                .uri(uri)
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(axum::body::Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value = if bytes.is_empty() {
        json!({})
    } else {
        serde_json::from_slice(&bytes).unwrap()
    };
    (status, value)
}

#[tokio::test]
async fn runtime_credential_import_is_did_scoped_and_never_returns_raw_payload() {
    let (_dir, router, token, _, state) = build_test_app(20);
    let raw = json!({
        "@context": ["https://www.w3.org/ns/credentials/v2"],
        "type": ["VerifiableCredential", "RegionalIdentityCredential"],
        "issuer": "did:web:issuer.example",
        "credentialSubject": { "id": state.agent_did },
        "secret_claim": "must-not-be-returned"
    })
    .to_string();

    let imported = authed_post_json(
        router.clone(),
        &token,
        "/v1/wattetheria/agent-identities/runtime/credentials",
        json!({
            "format": "w3c_vc_json",
            "media_type": "application/vc+ld+json",
            "payload": raw,
        }),
    )
    .await;
    let listed = authed_get_json(
        router,
        &token,
        "/v1/wattetheria/agent-identities/runtime/credentials",
    )
    .await;

    assert_eq!(imported["verification_status"], "pending");
    assert!(imported["proof_outcome"].is_null());
    assert!(imported["credential_state"].is_null());
    assert!(imported["trust_outcome"].is_null());
    assert!(imported.get("payload").is_none());
    assert_eq!(listed["agent_did"], state.agent_did);
    assert_eq!(
        listed["items"][0]["credential_id"],
        imported["credential_id"]
    );
    assert!(listed["items"][0].get("payload").is_none());
    assert!(!listed.to_string().contains("must-not-be-returned"));
}

#[tokio::test]
async fn provider_credentials_are_did_scoped_isolated_and_deletable() {
    let (_dir, router, token, _, state) = build_test_app(20);
    let provider_did = state.servicenet_provider.did.clone();
    let raw = json!({
        "@context": ["https://www.w3.org/ns/credentials/v2"],
        "type": ["VerifiableCredential", "ProviderIdentityCredential"],
        "issuer": "did:web:issuer.example",
        "credentialSubject": { "id": provider_did },
        "secret_claim": "provider-private-claim"
    })
    .to_string();

    let imported = authed_post_json(
        router.clone(),
        &token,
        "/v1/wattetheria/provider-identity/credentials",
        json!({
            "format": "w3c_vc_json",
            "media_type": "application/vc+ld+json",
            "payload": raw,
        }),
    )
    .await;
    let listed = authed_get_json(
        router.clone(),
        &token,
        "/v1/wattetheria/provider-identity/credentials",
    )
    .await;
    let runtime = authed_get_json(
        router.clone(),
        &token,
        "/v1/wattetheria/agent-identities/runtime/credentials",
    )
    .await;

    assert_eq!(listed["provider_did"], state.servicenet_provider.did);
    assert_eq!(
        listed["items"][0]["credential_id"],
        imported["credential_id"]
    );
    assert!(listed["items"][0].get("payload").is_none());
    assert!(!listed.to_string().contains("provider-private-claim"));
    assert!(runtime["items"].as_array().unwrap().is_empty());

    let credential_id = imported["credential_id"].as_str().unwrap();
    let (status, _) = authed_json_request(
        router.clone(),
        &token,
        "DELETE",
        &format!("/v1/wattetheria/provider-identity/credentials/{credential_id}"),
        json!({}),
    )
    .await;
    let after_delete = authed_get_json(
        router,
        &token,
        "/v1/wattetheria/provider-identity/credentials",
    )
    .await;

    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(after_delete["items"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn credentials_are_addressed_by_the_generated_service_agent_identity_id() {
    let (_dir, router, token, _, _state) = build_test_app(20);
    let generated = authed_post_json(
        router.clone(),
        &token,
        "/v1/wattetheria/agent-identities/service-agents/generate",
        json!({}),
    )
    .await;

    let imported = authed_post_json(
        router,
        &token,
        &format!(
            "/v1/wattetheria/agent-identities/service-agents/{}/credentials",
            generated["service_agent_identity_id"].as_str().unwrap()
        ),
        json!({ "payload": "{\"type\":[\"VerifiableCredential\"]}" }),
    )
    .await;

    assert_eq!(imported["verification_status"], "pending");
}

#[tokio::test]
async fn service_agent_credentials_are_scoped_to_the_selected_service_did() {
    let (_dir, router, token, _, _state) = build_test_app(20);
    let generated = authed_post_json(
        router.clone(),
        &token,
        "/v1/wattetheria/agent-identities/service-agents/generate",
        json!({}),
    )
    .await;

    let imported = authed_post_json(
        router.clone(),
        &token,
        &format!(
            "/v1/wattetheria/agent-identities/service-agents/{}/credentials",
            generated["service_agent_identity_id"].as_str().unwrap()
        ),
        json!({
            "payload": json!({
                "@context": ["https://www.w3.org/ns/credentials/v2"],
                "type": ["VerifiableCredential"],
                "issuer": "did:web:issuer.example",
                "credentialSubject": { "id": generated["service_did"] },
            }).to_string(),
        }),
    )
    .await;
    let listed = authed_get_json(
        router,
        &token,
        &format!(
            "/v1/wattetheria/agent-identities/service-agents/{}/credentials",
            generated["service_agent_identity_id"].as_str().unwrap()
        ),
    )
    .await;

    assert_eq!(listed["service_did"], generated["service_did"]);
    assert_eq!(
        listed["items"][0]["credential_id"],
        imported["credential_id"]
    );
}

#[tokio::test]
async fn trust_anchor_configuration_validates_and_round_trips() {
    let (_dir, router, token, _, _state) = build_test_app(20);
    let anchor = json!({
        "id": "eu-qualified-issuer",
        "framework_id": "eu-eidas",
        "issuer": "did:web:issuer.example",
        "jurisdiction": "EU",
        "credential_types": ["RegionalIdentityCredential"],
    });

    let (status, _) = authed_json_request(
        router.clone(),
        &token,
        "PUT",
        "/v1/wattetheria/credentials/trust-anchors",
        json!({ "anchors": [anchor.clone()] }),
    )
    .await;
    let listed = authed_get_json(router, &token, "/v1/wattetheria/credentials/trust-anchors").await;

    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(listed["anchors"], json!([anchor]));
}

#[tokio::test]
async fn invalid_trust_anchor_configuration_is_rejected() {
    let (_dir, router, token, _, _state) = build_test_app(20);

    let (status, body) = authed_json_request(
        router,
        &token,
        "PUT",
        "/v1/wattetheria/credentials/trust-anchors",
        json!({
            "anchors": [{
                "id": "",
                "framework_id": "eu-eidas",
                "issuer": "did:web:issuer.example",
            }]
        }),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap().contains("id is required"));
}
