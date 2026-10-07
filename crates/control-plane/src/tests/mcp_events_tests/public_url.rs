use super::*;
use crate::{PublicMcpUrl, public_mcp_app};
use axum::body::Body;
use axum::http::Request;

async fn public_app() -> (tempfile::TempDir, ControlPlaneState, String) {
    let (dir, router, _token, mut state) = subscription_app().await;
    drop(router);
    state.mcp_token_auth_required = true;
    state.mcp_public_url = Some(Arc::new(PublicMcpUrl::open(dir.path()).unwrap()));
    let secret = std::fs::read_to_string(dir.path().join("mcp_url_secret")).unwrap();
    (dir, state, secret)
}

async fn public_call(
    state: &ControlPlaneState,
    secret: &str,
    method: &str,
    mut params: Value,
    modern: bool,
) -> (StatusCode, Value) {
    let version = if modern { "2026-07-28" } else { "2025-11-25" };
    if modern {
        params["_meta"] = json!({"io.modelcontextprotocol/protocolVersion": version,
            "io.modelcontextprotocol/clientCapabilities": {}});
    }
    let mut request = Request::builder()
        .method("POST")
        .uri(format!("/mcp/{secret}"))
        .header("content-type", "application/json")
        .header("mcp-protocol-version", version)
        .header("mcp-method", method)
        .header("authorization", "Bearer incorrect")
        .body(Body::from(
            json!({"jsonrpc": "2.0", "id": 7, "method": method, "params": params}).to_string(),
        ))
        .unwrap();
    request.headers_mut().append(
        "authorization",
        HeaderValue::from_static("Bearer also-incorrect"),
    );
    let response = public_mcp_app(state.clone())
        .oneshot(request)
        .await
        .unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    let value = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body).unwrap()
    };
    (status, value)
}

async fn rotate(state: &ControlPlaneState, token: Option<&str>) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method("POST")
        .uri("/v1/mcp/public-url/rotate");
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    let response = app(state.clone())
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        if body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&body).unwrap()
        },
    )
}

#[tokio::test]
async fn public_mcp_reuses_both_protocols_and_replaces_all_authorization_values() {
    let (_dir, mut state, secret) = public_app().await;
    for auth_required in [false, true] {
        state.mcp_token_auth_required = auth_required;
        for (method, params, modern) in [
            (
                "initialize",
                json!({"protocolVersion": "2025-11-25", "capabilities": {},
                "clientInfo": {"name": "remote-agent", "version": "1"}}),
                false,
            ),
            ("tools/list", json!({}), false),
            (
                "tools/call",
                json!({"name": "get_agent_identity", "arguments": {}}),
                false,
            ),
            ("server/discover", json!({}), true),
            ("events/list", json!({}), true),
        ] {
            let (status, body) = public_call(&state, &secret, method, params, modern).await;
            assert_eq!(status, StatusCode::OK, "{method}: {body}");
            assert!(body.get("error").is_none(), "{method}: {body}");
            assert_ne!(body["result"]["isError"], true, "{body}");
        }
    }
    let (_, tools) = public_call(&state, &secret, "tools/list", json!({}), false).await;
    assert!(
        tools["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .all(|tool| tool["name"] != "rotate")
    );
    let (status, _) = mcp_call(app(state.clone()), None, "tools/list", json!({}), |_| {}).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    state.mcp_token_auth_required = false;
    let (status, _) = mcp_call(app(state.clone()), None, "tools/list", json!({}), |_| {}).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn public_mcp_keeps_api_runtime_event_behavior() {
    let (_dir, mut state, secret) = public_app().await;
    state.agent_event_mode = AgentEventMode::ApiRuntime;
    let (status, discover) = public_call(&state, &secret, "server/discover", json!({}), true).await;
    assert_eq!(status, StatusCode::OK);
    assert!(discover["result"].get("capabilities").is_some());
    let (status, _) = public_call(&state, &secret, "events/list", json!({}), true).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn public_mcp_rejects_every_other_path_method_and_secret_before_body_parsing() {
    let (_dir, state, secret) = public_app().await;
    for path in [
        "/".to_owned(),
        "/mcp".into(),
        "/mcp/".into(),
        "/mcp/incorrect".into(),
        "/mcp/%ff".into(),
        "/v1/mcp/public-url/rotate".into(),
        "/v1/policy".into(),
        "/agent-events".into(),
        format!("/mcp/{secret}/extra"),
    ] {
        let response = public_mcp_app(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(&path)
                    .header("content-type", "application/json")
                    .body(Body::from("not-json"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
    }
    for method in [
        "GET", "HEAD", "PUT", "PATCH", "DELETE", "OPTIONS", "TRACE", "CONNECT",
    ] {
        let response = public_mcp_app(state.clone())
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(format!("/mcp/{secret}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{method}");
    }
}

#[tokio::test]
async fn public_mcp_secret_survives_restart_and_rotation_requires_local_bearer() {
    use base64::Engine;
    let (dir, mut state, secret) = public_app().await;
    assert_eq!(secret.len(), 43);
    assert_eq!(
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(&secret)
            .unwrap()
            .len(),
        32
    );
    assert_ne!(secret, state.auth_token);
    let (status, _) = rotate(&state, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _) = rotate(&state, Some("incorrect")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        std::fs::read_to_string(dir.path().join("mcp_url_secret")).unwrap(),
        secret
    );
    state.mcp_public_url = Some(Arc::new(PublicMcpUrl::open(dir.path()).unwrap()));
    assert_eq!(
        public_call(&state, &secret, "tools/list", json!({}), false)
            .await
            .0,
        StatusCode::OK
    );
    let (status, rotated) = rotate(&state, Some(&state.auth_token)).await;
    assert_eq!(status, StatusCode::OK);
    let next = rotated["path"]
        .as_str()
        .unwrap()
        .strip_prefix("/mcp/")
        .unwrap();
    assert_ne!(next, secret);
    assert_eq!(
        public_call(&state, &secret, "tools/list", json!({}), false)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        public_call(&state, next, "tools/list", json!({}), false)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("mcp_url_secret")).unwrap(),
        next
    );
    state.mcp_public_url = Some(Arc::new(PublicMcpUrl::open(dir.path()).unwrap()));
    assert_eq!(
        public_call(&state, next, "tools/list", json!({}), false)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        public_call(&state, &secret, "tools/list", json!({}), false)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.path().join("mcp_url_secret");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        for mode in [0o640, 0o604, 0o644] {
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
            assert!(PublicMcpUrl::open(dir.path()).is_err());
        }
    }
}

#[tokio::test]
async fn public_mcp_disabled_has_no_secret_or_public_routes() {
    let (dir, _router, _token, state) = subscription_app().await;
    assert!(!dir.path().join("mcp_url_secret").exists());
    let (status, _) = rotate(&state, Some(&state.auth_token)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(!dir.path().join("mcp_url_secret").exists());
    assert_eq!(
        public_call(&state, "anything", "tools/list", json!({}), false)
            .await
            .0,
        StatusCode::NOT_FOUND
    );
}

#[path = "public_url/logs.rs"]
mod logs;
#[path = "public_url/rotation.rs"]
mod rotation;
