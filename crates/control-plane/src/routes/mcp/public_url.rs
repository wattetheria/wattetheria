mod secret;

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use axum::extract::{FromRequest, OriginalUri, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, Uri, header::AUTHORIZATION};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use serde_json::json;
use subtle::ConstantTimeEq;
use tokio::sync::{RwLock, watch};

use crate::auth::{authorize, internal_error};
use crate::state::ControlPlaneState;

pub struct PublicMcpUrl {
    path: PathBuf,
    secret: RwLock<String>,
    requests: watch::Sender<()>,
}

impl PublicMcpUrl {
    pub fn open(data_dir: &Path) -> Result<Self> {
        let path = data_dir.join("mcp_url_secret");
        let secret = secret::load_or_create(&path)?;
        Ok(Self {
            path,
            secret: RwLock::new(secret),
            requests: watch::channel(()).0,
        })
    }
}

pub fn public_mcp_app(state: ControlPlaneState) -> Router {
    Router::new()
        .route("/mcp/{secret}", post(public_mcp))
        .fallback(|| async { StatusCode::NOT_FOUND })
        .method_not_allowed_fallback(|| async { StatusCode::NOT_FOUND })
        .with_state(state)
}

pub async fn serve_public_mcp(state: ControlPlaneState, bind: SocketAddr) -> Result<()> {
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .context("bind public MCP listener")?;
    axum::serve(listener, public_mcp_app(state))
        .await
        .context("serve public MCP listener")
}

async fn public_mcp(State(state): State<ControlPlaneState>, mut request: Request) -> Response {
    let Some(public_url) = state.mcp_public_url.as_ref() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let supplied = request
        .uri()
        .path()
        .strip_prefix("/mcp/")
        .unwrap_or_default();
    let mut rotation = public_url.requests.subscribe();
    {
        let secret = public_url.secret.read().await;
        if !bool::from(secret.as_bytes().ct_eq(supplied.as_bytes())) {
            return StatusCode::NOT_FOUND.into_response();
        }
        rotation.borrow_and_update();
    }
    *request.uri_mut() = Uri::from_static("/mcp/***");
    request
        .extensions_mut()
        .insert(OriginalUri(Uri::from_static("/mcp/***")));
    request.headers_mut().remove(AUTHORIZATION);
    let Ok(authorization) = HeaderValue::from_str(&format!("Bearer {}", state.auth_token)) else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    request.headers_mut().insert(AUTHORIZATION, authorization);
    let headers = request.headers().clone();
    tracing::debug!(path = "/mcp/***", "public MCP request");
    let dispatch = async {
        let payload = match Json::<super::McpRequest>::from_request(request, &state).await {
            Ok(payload) => payload,
            Err(error) => return error.into_response(),
        };
        super::mcp(State(state.clone()), headers, payload).await
    };
    tokio::select! {
        biased;
        _ = rotation.changed() => StatusCode::NOT_FOUND.into_response(),
        response = dispatch => response,
    }
}

pub(crate) async fn rotate(State(state): State<ControlPlaneState>, headers: HeaderMap) -> Response {
    if let Err(response) = authorize(&state, &headers).await {
        return response;
    }
    let Some(public_url) = state.mcp_public_url.as_ref() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let Some(events) = state.mcp_events.as_ref() else {
        return internal_error(&anyhow::anyhow!(
            "MCP Events unavailable; cannot revoke subscriptions"
        ));
    };
    let mut current = public_url.secret.write().await;
    public_url.requests.send_replace(());
    let next = secret::generate();
    let revoked = match events.revoke_mcp_subscriptions().await {
        Ok(count) => count,
        Err(error) => return internal_error(&error),
    };
    if let Err(error) = secret::replace(&public_url.path, &next) {
        return internal_error(&error);
    }
    current.clone_from(&next);
    Json(json!({ "path": format!("/mcp/{next}"), "revokedSubscriptions": revoked })).into_response()
}
