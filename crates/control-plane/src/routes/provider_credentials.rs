use axum::Json;
use axum::extract::{Path as AxumPath, State};
use axum::http::HeaderMap;
use axum::response::Response;

use super::credentials::{
    ImportCredentialBody, delete_credential, import_credential, list_credentials,
};
use crate::auth::{authorize, internal_error};
use crate::state::ControlPlaneState;
use wattetheria_kernel::credentials::CredentialBinding;

pub(crate) async fn list_provider_credentials(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, &headers).await {
        return response;
    }
    let binding = match CredentialBinding::provider(&state.servicenet_provider.did) {
        Ok(binding) => binding,
        Err(error) => return internal_error(&error),
    };
    list_credentials(&state, &binding, "provider_did")
}

pub(crate) async fn import_provider_credential(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<ImportCredentialBody>,
) -> Response {
    let actor = match authorize(&state, &headers).await {
        Ok(actor) => actor,
        Err(response) => return response,
    };
    let binding = match CredentialBinding::provider(&state.servicenet_provider.did) {
        Ok(binding) => binding,
        Err(error) => return internal_error(&error),
    };
    import_credential(
        &state,
        actor,
        binding,
        body,
        "provider_identity.credentials.import",
    )
}

pub(crate) async fn delete_provider_credential(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    AxumPath(credential_id): AxumPath<String>,
) -> Response {
    let actor = match authorize(&state, &headers).await {
        Ok(actor) => actor,
        Err(response) => return response,
    };
    let binding = match CredentialBinding::provider(&state.servicenet_provider.did) {
        Ok(binding) => binding,
        Err(error) => return internal_error(&error),
    };
    delete_credential(
        &state,
        actor,
        &binding,
        &credential_id,
        "provider_identity.credentials.delete",
        "Provider credential not found",
    )
}
