use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use super::identity_support::{
    IDENTITY_EXPORT_FORMAT, IDENTITY_NETWORK_ID, append_identity_audit, private_export_response,
};
use crate::auth::{authorize, internal_error};
use crate::state::ControlPlaneState;
use wattetheria_kernel::agent_identity::service_agent::FileServiceAgentIdentityStore;
use wattetheria_kernel::identity::fingerprint_from_did_key;
use wattetheria_kernel::provider_identity::FileProviderIdentityStore;

#[derive(Debug, Serialize)]
struct ProviderIdentityResponse {
    provider_did: String,
    public_key: String,
    identity_uri: String,
    fingerprint: String,
    status: &'static str,
    managed_service_agents: usize,
}

#[derive(Debug, Serialize)]
struct ProviderIdentityExport {
    format: &'static str,
    version: u32,
    identity_kind: &'static str,
    provider_did: String,
    public_key: String,
    private_key: String,
}

pub(crate) async fn provider_identity(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, &headers).await {
        return response;
    }
    let identity = match FileProviderIdentityStore::new(&state.data_dir).load_or_create() {
        Ok(identity) => identity,
        Err(error) => return internal_error(&error),
    };
    if identity.agent_did != state.servicenet_provider.did {
        return internal_error(&anyhow::anyhow!(
            "stored Provider identity does not match the active Provider identity"
        ));
    }
    let fingerprint = match fingerprint_from_did_key(&identity.agent_did) {
        Ok(fingerprint) => fingerprint,
        Err(error) => return internal_error(&error),
    };
    let managed_service_agents = match FileServiceAgentIdentityStore::new(&state.data_dir).list() {
        Ok(identities) => identities.len(),
        Err(error) => return internal_error(&error),
    };
    Json(ProviderIdentityResponse {
        identity_uri: format!(
            "wattetheria://{IDENTITY_NETWORK_ID}/provider/{}",
            identity.agent_did
        ),
        provider_did: identity.agent_did,
        public_key: identity.public_key,
        fingerprint,
        status: "active",
        managed_service_agents,
    })
    .into_response()
}

pub(crate) async fn export_provider_identity(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
) -> Response {
    let actor = match authorize(&state, &headers).await {
        Ok(actor) => actor,
        Err(response) => return response,
    };
    let data_dir = state.data_dir.clone();
    let identity = match tokio::task::spawn_blocking(move || {
        FileProviderIdentityStore::new(data_dir).load_or_create()
    })
    .await
    {
        Ok(Ok(identity)) => identity,
        Ok(Err(error)) => return internal_error(&error),
        Err(error) => return internal_error(&anyhow::anyhow!(error)),
    };
    if identity.agent_did != state.servicenet_provider.did {
        return internal_error(&anyhow::anyhow!(
            "stored Provider identity does not match the active Provider identity"
        ));
    }
    let export = ProviderIdentityExport {
        format: IDENTITY_EXPORT_FORMAT,
        version: 1,
        identity_kind: "provider",
        provider_did: identity.agent_did,
        public_key: identity.public_key,
        private_key: identity.private_key,
    };
    append_identity_audit(
        &state,
        actor,
        "provider_identity.export",
        &export.provider_did,
    );
    private_export_response(export)
}
