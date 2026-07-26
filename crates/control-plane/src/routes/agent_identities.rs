use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::auth::{authorize, internal_error};
use crate::routes::identity_support::{
    IDENTITY_EXPORT_FORMAT, IDENTITY_NETWORK_ID, append_identity_audit, private_export_response,
};
use crate::state::ControlPlaneState;
use wattetheria_kernel::agent_identity::service_agent::{
    FileServiceAgentIdentityStore, ServiceAgentIdentity, ServiceAgentIdentityStore,
    ServiceAgentKeyOrigin,
};
use wattetheria_kernel::agent_identity::{
    AgentIdentityStore, FileAgentIdentityStore, RuntimeIdentityActivation,
};
use wattetheria_kernel::identity::{Identity, IdentityCompatView, fingerprint_from_did_key};
use wattetheria_kernel::servicenet::{
    ServiceNetPublisherRegistration, load_servicenet_publisher_state,
};

#[derive(Debug, Deserialize)]
pub(crate) struct ImportIdentityBody {
    #[serde(default)]
    agent_did: Option<String>,
    private_key: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct GenerateServiceAgentBody {}

#[derive(Debug, Deserialize)]
pub(crate) struct ImportServiceAgentBody {
    #[serde(default)]
    service_did: Option<String>,
    private_key: String,
}

#[derive(Debug, Serialize)]
struct RuntimeIdentityResponse {
    identity: IdentityCompatView,
    pending_import: Option<IdentityCompatView>,
    activation: &'static str,
    agent_card_refresh: &'static str,
}

#[derive(Debug, Serialize)]
struct RuntimeIdentityImportPreview {
    agent_did: String,
    identity_uri: String,
    fingerprint: String,
}

#[derive(Debug, Serialize)]
struct ServiceAgentIdentityImportPreview {
    service_did: String,
    identity_uri: String,
    fingerprint: String,
}

#[derive(Debug, Serialize)]
struct RuntimeIdentityExport {
    format: &'static str,
    version: u32,
    identity_kind: &'static str,
    agent_did: String,
    public_key: String,
    private_key: String,
}

#[derive(Debug, Serialize)]
struct ServiceAgentIdentityExport {
    format: &'static str,
    version: u32,
    identity_kind: &'static str,
    service_did: String,
    public_key: String,
    private_key: String,
}

#[derive(Debug, Serialize)]
struct ServiceAgentIdentityView {
    service_agent_identity_id: String,
    service_did: String,
    public_key: String,
    bound_agent_id: Option<String>,
    endpoint_url: Option<String>,
    key_origin: ServiceAgentKeyOrigin,
    agent_name: Option<String>,
    service_address: Option<String>,
    binding_status: &'static str,
    agent_card_status: &'static str,
}

enum ServiceAgentIdentityDeleteOutcome {
    Deleted(ServiceAgentIdentity),
    NotFound,
    Published(String),
}

impl ServiceAgentIdentityView {
    fn from_identity(
        identity: ServiceAgentIdentity,
        registration: Option<&ServiceNetPublisherRegistration>,
    ) -> Self {
        let published_agent_id = registration.map(|registration| registration.agent_id.as_str());
        let bound_agent_id = identity
            .bound_agent_id
            .or_else(|| published_agent_id.map(ToOwned::to_owned));
        let agent_name = registration
            .and_then(|registration| registration.agent_card.get("name"))
            .and_then(serde_json::Value::as_str)
            .map(ToOwned::to_owned);
        let service_address =
            registration.and_then(|registration| registration.service_address.clone());
        Self {
            service_agent_identity_id: identity.service_agent_identity_id,
            service_did: identity.service_did,
            public_key: identity.public_key,
            agent_name,
            service_address,
            binding_status: if bound_agent_id.is_some() {
                "bound"
            } else {
                "unbound"
            },
            bound_agent_id,
            endpoint_url: identity.endpoint_url,
            key_origin: identity.key_origin,
            agent_card_status: if published_agent_id.is_some() {
                "published"
            } else {
                "draft"
            },
        }
    }
}

pub(crate) async fn runtime_identity(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, &headers).await {
        return response;
    }
    let store = FileAgentIdentityStore::new(&state.data_dir);
    let pending_import = match store.pending_import() {
        Ok(pending) => pending,
        Err(error) => return internal_error(&error),
    };
    let policy = match store.transition_policy() {
        Ok(policy) => policy,
        Err(error) => return internal_error(&error),
    };
    let (activation, agent_card_refresh) = match policy.activation() {
        RuntimeIdentityActivation::Active => ("active", "current"),
        RuntimeIdentityActivation::RestartRequired => ("restart_required", "after_restart"),
    };
    Json(RuntimeIdentityResponse {
        identity: state.identity,
        activation,
        agent_card_refresh,
        pending_import,
    })
    .into_response()
}

pub(crate) async fn export_runtime_identity(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
) -> Response {
    let actor = match authorize(&state, &headers).await {
        Ok(actor) => actor,
        Err(response) => return response,
    };
    let store = FileAgentIdentityStore::new(&state.data_dir);
    let identity = match tokio::task::spawn_blocking(move || {
        let _transition_lock = store.lock_transition()?;
        store.load()
    })
    .await
    {
        Ok(Ok(identity)) => identity,
        Ok(Err(error)) => return internal_error(&error),
        Err(error) => return internal_error(&anyhow::anyhow!(error)),
    };
    if identity.agent_did != state.agent_did {
        return internal_error(&anyhow::anyhow!(
            "active Runtime Agent identity does not match the Control Plane Agent DID"
        ));
    }
    let export = RuntimeIdentityExport {
        format: IDENTITY_EXPORT_FORMAT,
        version: 1,
        identity_kind: "runtime_agent",
        agent_did: identity.agent_did,
        public_key: identity.public_key,
        private_key: identity.private_key,
    };
    append_identity_audit(
        &state,
        actor,
        "agent_identity.runtime.export",
        &export.agent_did,
    );
    private_export_response(export)
}

pub(crate) async fn import_runtime_identity(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<ImportIdentityBody>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let store = FileAgentIdentityStore::new(&state.data_dir);
    let (expected_did, private_key) = match normalize_runtime_import(&body) {
        Ok(import) => import,
        Err(message) => return bad_request(message),
    };
    let pending = match tokio::task::spawn_blocking(move || {
        let transition_lock = store.lock_transition()?;
        store.stage_import_locked(expected_did.as_deref(), &private_key, transition_lock)
    })
    .await
    {
        Ok(Ok(identity)) => identity,
        Ok(Err(error)) => return bad_request(error.to_string()),
        Err(error) => return internal_error(&anyhow::anyhow!(error)),
    };
    append_identity_audit(
        &state,
        auth,
        "agent_identity.runtime.import_staged",
        &pending.agent_did,
    );
    Json(json!({
        "status": "pending_restart",
        "pending_identity": pending,
        "activation": "restart_required",
        "agent_card_refresh": "after_restart",
    }))
    .into_response()
}

pub(crate) async fn preview_runtime_identity_import(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<ImportIdentityBody>,
) -> Response {
    if let Err(response) = authorize(&state, &headers).await {
        return response;
    }
    let (expected_did, private_key) = match normalize_runtime_import(&body) {
        Ok(import) => import,
        Err(message) => return bad_request(message),
    };
    let preview = match tokio::task::spawn_blocking(move || {
        let identity = Identity::import_ed25519_private_key(expected_did.as_deref(), &private_key)?;
        let fingerprint = fingerprint_from_did_key(&identity.agent_did)?;
        Ok::<_, anyhow::Error>(RuntimeIdentityImportPreview {
            identity_uri: format!(
                "wattetheria://{IDENTITY_NETWORK_ID}/identity/{}",
                identity.agent_did
            ),
            agent_did: identity.agent_did,
            fingerprint,
        })
    })
    .await
    {
        Ok(Ok(preview)) => preview,
        Ok(Err(error)) => return bad_request(error.to_string()),
        Err(error) => return internal_error(&anyhow::anyhow!(error)),
    };
    Json(preview).into_response()
}

pub(crate) async fn list_service_agent_identities(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, &headers).await {
        return response;
    }
    let store = FileServiceAgentIdentityStore::new(&state.data_dir);
    let identities = match store.list() {
        Ok(identities) => identities,
        Err(error) => return internal_error(&error),
    };
    let publisher_state = match load_servicenet_publisher_state(&state.data_dir) {
        Ok(publisher_state) => publisher_state,
        Err(error) => return internal_error(&error),
    };
    let items = identities
        .into_iter()
        .map(|identity| {
            let registration = publisher_state
                .registrations
                .iter()
                .find(|registration| registration.service_did == identity.service_did);
            ServiceAgentIdentityView::from_identity(identity, registration)
        })
        .collect::<Vec<_>>();
    Json(json!({ "items": items })).into_response()
}

pub(crate) async fn generate_service_agent_identity(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(_body): Json<GenerateServiceAgentBody>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let store = FileServiceAgentIdentityStore::new(&state.data_dir);
    let identity = match tokio::task::spawn_blocking(move || store.generate()).await {
        Ok(Ok(identity)) => identity,
        Ok(Err(error)) => return bad_request(error.to_string()),
        Err(error) => return internal_error(&anyhow::anyhow!(error)),
    };
    append_identity_audit(
        &state,
        auth,
        "agent_identity.service_agent.generate",
        &identity.service_did,
    );
    (
        StatusCode::CREATED,
        Json(ServiceAgentIdentityView::from_identity(identity, None)),
    )
        .into_response()
}

pub(crate) async fn import_service_agent_identity(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<ImportServiceAgentBody>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let (expected_did, private_key) = match normalize_service_agent_import(&body) {
        Ok(import) => import,
        Err(message) => return bad_request(message),
    };
    let store = FileServiceAgentIdentityStore::new(&state.data_dir);
    let identity = match tokio::task::spawn_blocking(move || {
        store.import(expected_did.as_deref(), &private_key)
    })
    .await
    {
        Ok(Ok(identity)) => identity,
        Ok(Err(error)) => return bad_request(error.to_string()),
        Err(error) => return internal_error(&anyhow::anyhow!(error)),
    };
    append_identity_audit(
        &state,
        auth,
        "agent_identity.service_agent.import",
        &identity.service_did,
    );
    Json(ServiceAgentIdentityView::from_identity(identity, None)).into_response()
}

pub(crate) async fn preview_service_agent_identity_import(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<ImportServiceAgentBody>,
) -> Response {
    if let Err(response) = authorize(&state, &headers).await {
        return response;
    }
    let (expected_did, private_key) = match normalize_service_agent_import(&body) {
        Ok(import) => import,
        Err(message) => return bad_request(message),
    };
    let preview = match tokio::task::spawn_blocking(move || {
        let identity = ServiceAgentIdentity::import(expected_did.as_deref(), &private_key)?;
        let fingerprint = fingerprint_from_did_key(&identity.service_did)?;
        Ok::<_, anyhow::Error>(ServiceAgentIdentityImportPreview {
            identity_uri: format!(
                "wattetheria://{IDENTITY_NETWORK_ID}/identity/{}",
                identity.service_did
            ),
            service_did: identity.service_did,
            fingerprint,
        })
    })
    .await
    {
        Ok(Ok(preview)) => preview,
        Ok(Err(error)) => return bad_request(error.to_string()),
        Err(error) => return internal_error(&anyhow::anyhow!(error)),
    };
    Json(preview).into_response()
}

pub(crate) async fn export_service_agent_identity(
    State(state): State<ControlPlaneState>,
    Path(service_agent_identity_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let actor = match authorize(&state, &headers).await {
        Ok(actor) => actor,
        Err(response) => return response,
    };
    let data_dir = state.data_dir.clone();
    let identity = match tokio::task::spawn_blocking(move || {
        let store = FileServiceAgentIdentityStore::new(&data_dir);
        let _lock = store.lock_service_agent_identity_operation(&service_agent_identity_id)?;
        if !store
            .service_agent_identity_path(&service_agent_identity_id)
            .exists()
        {
            return Ok(None);
        }
        store.load(&service_agent_identity_id).map(Some)
    })
    .await
    {
        Ok(Ok(Some(identity))) => identity,
        Ok(Ok(None)) => return not_found("Service Agent identity not found"),
        Ok(Err(error)) => return internal_error(&error),
        Err(error) => return internal_error(&anyhow::anyhow!(error)),
    };
    let export = ServiceAgentIdentityExport {
        format: IDENTITY_EXPORT_FORMAT,
        version: 1,
        identity_kind: "service_agent",
        service_did: identity.service_did,
        public_key: identity.public_key,
        private_key: identity.private_key,
    };
    append_identity_audit(
        &state,
        actor,
        "agent_identity.service_agent.export",
        &export.service_did,
    );
    private_export_response(export)
}

pub(crate) async fn delete_service_agent_identity(
    State(state): State<ControlPlaneState>,
    Path(service_agent_identity_id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let data_dir = state.data_dir.clone();
    let outcome = match tokio::task::spawn_blocking(move || {
        let store = FileServiceAgentIdentityStore::new(&data_dir);
        let lock = store.lock_service_agent_identity_operation(&service_agent_identity_id)?;
        if !store
            .service_agent_identity_path(&service_agent_identity_id)
            .exists()
        {
            return Ok(ServiceAgentIdentityDeleteOutcome::NotFound);
        }
        let identity = store.load(&service_agent_identity_id)?;
        let publisher_state = load_servicenet_publisher_state(&data_dir)?;
        if let Some(registration) = publisher_state
            .registrations
            .iter()
            .find(|registration| registration.service_did == identity.service_did)
        {
            return Ok(ServiceAgentIdentityDeleteOutcome::Published(
                registration.agent_id.clone(),
            ));
        }
        let deleted = store
            .delete_locked(&service_agent_identity_id, lock)?
            .ok_or_else(|| anyhow::anyhow!("Service Agent identity disappeared during deletion"))?;
        Ok(ServiceAgentIdentityDeleteOutcome::Deleted(deleted))
    })
    .await
    {
        Ok(Ok(outcome)) => outcome,
        Ok(Err(error)) => return internal_error(&error),
        Err(error) => return internal_error(&anyhow::anyhow!(error)),
    };
    match outcome {
        ServiceAgentIdentityDeleteOutcome::Deleted(identity) => {
            append_identity_audit(
                &state,
                auth,
                "agent_identity.service_agent.delete",
                &identity.service_did,
            );
            StatusCode::NO_CONTENT.into_response()
        }
        ServiceAgentIdentityDeleteOutcome::NotFound => {
            not_found("Service Agent identity not found")
        }
        ServiceAgentIdentityDeleteOutcome::Published(agent_id) => conflict(format!(
            "unpublish Service Agent '{agent_id}' before deleting its DID"
        )),
    }
}

fn normalized_optional(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn normalize_runtime_import(body: &ImportIdentityBody) -> Result<(Option<String>, String), String> {
    normalize_identity_import(body.agent_did.as_deref(), &body.private_key)
}

fn normalize_service_agent_import(
    body: &ImportServiceAgentBody,
) -> Result<(Option<String>, String), String> {
    normalize_identity_import(body.service_did.as_deref(), &body.private_key)
}

fn normalize_identity_import(
    expected_did: Option<&str>,
    private_key: &str,
) -> Result<(Option<String>, String), String> {
    let expected_did = normalized_optional(expected_did).map(ToOwned::to_owned);
    let private_key = private_key.trim().to_owned();
    if private_key.is_empty() {
        return Err("private_key is required".to_string());
    }
    Ok((expected_did, private_key))
}

fn bad_request(message: impl Into<String>) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "error": message.into() })),
    )
        .into_response()
}

fn conflict(message: impl Into<String>) -> Response {
    (
        StatusCode::CONFLICT,
        Json(json!({ "error": message.into() })),
    )
        .into_response()
}

fn not_found(message: impl Into<String>) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "error": message.into() })),
    )
        .into_response()
}
