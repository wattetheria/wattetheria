use axum::Json;
use axum::extract::{Path as AxumPath, State};
use axum::http::HeaderMap;
use axum::response::Response;

use super::credentials::{
    ImportCredentialBody, delete_credential, import_credential, list_credentials, not_found,
};
use crate::auth::{authorize, internal_error};
use crate::state::ControlPlaneState;
use wattetheria_kernel::agent_identity::service_agent::{
    FileServiceAgentIdentityStore, ServiceAgentIdentityStore, ServiceAgentOperationLock,
};
use wattetheria_kernel::credentials::CredentialBinding;

pub(crate) async fn list_runtime_credentials(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, &headers).await {
        return response;
    }
    let binding = match CredentialBinding::runtime(&state.agent_did) {
        Ok(binding) => binding,
        Err(error) => return internal_error(&error),
    };
    list_credentials(&state, &binding, "agent_did")
}

pub(crate) async fn import_runtime_credential(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<ImportCredentialBody>,
) -> Response {
    let actor = match authorize(&state, &headers).await {
        Ok(actor) => actor,
        Err(response) => return response,
    };
    let binding = match CredentialBinding::runtime(&state.agent_did) {
        Ok(binding) => binding,
        Err(error) => return internal_error(&error),
    };
    import_credential(
        &state,
        actor,
        binding,
        body,
        "agent_identity.runtime.credentials.import",
    )
}

pub(crate) async fn delete_runtime_credential(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    AxumPath(credential_id): AxumPath<String>,
) -> Response {
    let actor = match authorize(&state, &headers).await {
        Ok(actor) => actor,
        Err(response) => return response,
    };
    let binding = match CredentialBinding::runtime(&state.agent_did) {
        Ok(binding) => binding,
        Err(error) => return internal_error(&error),
    };
    delete_credential(
        &state,
        actor,
        &binding,
        &credential_id,
        "agent_identity.runtime.credentials.delete",
        "Runtime Agent credential not found",
    )
}

pub(crate) async fn list_service_agent_credentials(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    AxumPath(service_agent_identity_id): AxumPath<String>,
) -> Response {
    if let Err(response) = authorize(&state, &headers).await {
        return response;
    }
    let (binding, _operation_lock) =
        match locked_service_agent_binding(&state, &service_agent_identity_id) {
            Ok(Some(binding)) => binding,
            Ok(None) => return not_found("Service Agent identity not found"),
            Err(error) => return internal_error(&error),
        };
    list_credentials(&state, &binding, "service_did")
}

pub(crate) async fn import_service_agent_credential(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    AxumPath(service_agent_identity_id): AxumPath<String>,
    Json(body): Json<ImportCredentialBody>,
) -> Response {
    let actor = match authorize(&state, &headers).await {
        Ok(actor) => actor,
        Err(response) => return response,
    };
    let (binding, _operation_lock) =
        match locked_service_agent_binding(&state, &service_agent_identity_id) {
            Ok(Some(binding)) => binding,
            Ok(None) => return not_found("Service Agent identity not found"),
            Err(error) => return internal_error(&error),
        };
    import_credential(
        &state,
        actor,
        binding,
        body,
        "agent_identity.service_agent.credentials.import",
    )
}

pub(crate) async fn delete_service_agent_credential(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    AxumPath((service_agent_identity_id, credential_id)): AxumPath<(String, String)>,
) -> Response {
    let actor = match authorize(&state, &headers).await {
        Ok(actor) => actor,
        Err(response) => return response,
    };
    let (binding, _operation_lock) =
        match locked_service_agent_binding(&state, &service_agent_identity_id) {
            Ok(Some(binding)) => binding,
            Ok(None) => return not_found("Service Agent identity not found"),
            Err(error) => return internal_error(&error),
        };
    delete_credential(
        &state,
        actor,
        &binding,
        &credential_id,
        "agent_identity.service_agent.credentials.delete",
        "Service Agent credential not found",
    )
}

fn locked_service_agent_binding(
    state: &ControlPlaneState,
    service_agent_identity_id: &str,
) -> anyhow::Result<Option<(CredentialBinding, ServiceAgentOperationLock)>> {
    let store = FileServiceAgentIdentityStore::new(&state.data_dir);
    let operation_lock = store.lock_service_agent_identity_operation(service_agent_identity_id)?;
    if !store
        .service_agent_identity_path(service_agent_identity_id)
        .exists()
    {
        return Ok(None);
    }
    let identity = store.load(service_agent_identity_id)?;
    let binding =
        CredentialBinding::service_agent(service_agent_identity_id, identity.service_did)?;
    Ok(Some((binding, operation_lock)))
}
