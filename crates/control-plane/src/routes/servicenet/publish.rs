use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::Path as FsPath;
use uuid::Uuid;

use super::publish_validation::{
    default_agent_card, normalize_agent_card_skills, real_world_domains, validate_agent_card,
    wattetheria_native_domains,
};
use super::{servicenet_client, servicenet_error_response};
use crate::auth::{authorize, internal_error};
use crate::state::ControlPlaneState;
use wattetheria_kernel::agent_identity::service_agent::FileServiceAgentIdentityStore;
use wattetheria_kernel::audit::AuditEntry;
use wattetheria_kernel::servicenet::{
    CustomizedAgentProtocol, ServiceAgentExecution, ServiceAgentPublicationInput,
    ServiceAgentPublicationSubmitError, ServiceNetClient, ServiceNetConnectionMode,
    attach_service_agent_payment_binding, load_servicenet_publisher_state,
    normalize_service_address, prepare_service_agent_publication,
    remove_servicenet_publisher_registration, submit_service_agent_publication,
};
pub(crate) use wattetheria_kernel::servicenet::{
    ServiceNetPublisherRegistration, ServiceNetPublisherState,
};
use wattetheria_kernel::wallet_identity::active_payment_account_binding_proof;

const DEFAULT_SERVICENET_VERSION: &str = "0.1.0";
const DEFAULT_SERVICENET_TTL_MINUTES: u64 = 30;

#[derive(Debug, Deserialize)]
pub(crate) struct PublishAgentBody {
    #[serde(default)]
    agent_id: Option<String>,
    #[serde(default)]
    provider_id: Option<String>,
    #[serde(default)]
    service_address: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    risk_level: Option<String>,
    #[serde(default)]
    ttl_minutes: Option<u64>,
    #[serde(default)]
    execution_mode: PublishExecutionMode,
    #[serde(default)]
    connection_mode: ServiceNetConnectionMode,
    #[serde(default)]
    protocol: Option<CustomizedAgentProtocol>,
    #[serde(default)]
    customized_agent_url: Option<String>,
    agent_card: Value,
}

#[derive(Debug, Clone, Copy, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
enum PublishExecutionMode {
    #[default]
    WattetheriaRuntime,
    CustomizedAgent,
}

#[derive(Debug, Deserialize)]
pub(crate) struct UnpublishAgentBody {
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    ttl_minutes: Option<u64>,
}

fn bad_request(message: impl Into<String>) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error": message.into()})),
    )
        .into_response()
}

fn forbidden(message: impl Into<String>) -> Response {
    (
        StatusCode::FORBIDDEN,
        Json(json!({"error": message.into()})),
    )
        .into_response()
}

pub(crate) fn registration_matches_identity(
    registration: &ServiceNetPublisherRegistration,
    agent_did: &str,
) -> bool {
    registration.provider_did == agent_did
}

fn registration_matches_provider(
    registration: &ServiceNetPublisherRegistration,
    current_agent_did: &str,
    provider_id: &str,
    target_agent_id: Option<&str>,
) -> bool {
    registration_matches_identity(registration, current_agent_did)
        && registration.provider_id == provider_id
        && target_agent_id.is_none_or(|target_agent_id| registration.agent_id == target_agent_id)
}

pub(crate) fn agent_matches_identity(agent: &Value, agent_did: &str) -> bool {
    agent.get("provider_did").and_then(Value::as_str) == Some(agent_did)
        || agent.get("provider_attester_did").and_then(Value::as_str) == Some(agent_did)
        || agent
            .get("attestations")
            .and_then(|attestations| attestations.get("provider_attester_did"))
            .and_then(Value::as_str)
            == Some(agent_did)
}

async fn remote_agent_matches_provider(
    client: &ServiceNetClient,
    target_agent_id: &str,
    provider_id: &str,
    current_agent_did: &str,
) -> Result<bool, Response> {
    let agent = client
        .get_agent(target_agent_id)
        .await
        .map_err(|error| servicenet_error_response(&error))?;
    Ok(
        agent.get("provider_id").and_then(Value::as_str) == Some(provider_id)
            && agent_matches_identity(&agent, current_agent_did),
    )
}

async fn resolve_provider_id(
    client: &ServiceNetClient,
    state: &ControlPlaneState,
    body: &PublishAgentBody,
    publisher_state: &mut ServiceNetPublisherState,
) -> Result<String, Response> {
    let body_agent_id = body
        .agent_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if let Some(provider_id) = body
        .provider_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if publisher_state.registrations.iter().any(|registration| {
            registration_matches_provider(
                registration,
                &state.agent_did,
                provider_id,
                body_agent_id,
            )
        }) {
            return Ok(provider_id.to_owned());
        }
        if let Some(agent_id) = body_agent_id
            && remote_agent_matches_provider(client, agent_id, provider_id, &state.agent_did)
                .await?
        {
            return Ok(provider_id.to_owned());
        }
        return Err(forbidden(
            "provider_id is not bound to the current Wattetheria identity",
        ));
    }
    if let Some(agent_id) = body_agent_id
        && let Some(registration) = publisher_state.registrations.iter().find(|record| {
            record.agent_id == agent_id && registration_matches_identity(record, &state.agent_did)
        })
    {
        return Ok(registration.provider_id.clone());
    }
    if let Some(registration) = publisher_state
        .registrations
        .iter()
        .find(|record| registration_matches_identity(record, &state.agent_did))
    {
        return Ok(registration.provider_id.clone());
    }
    let challenge = client
        .create_provider_ownership_challenge(&state.agent_did, "register")
        .await
        .map_err(|error| servicenet_error_response(&error))?;
    let signature = state
        .signer
        .sign_bytes(challenge.challenge.as_bytes())
        .map_err(|error| internal_error(&error))?;
    let display_name = body.agent_card.get("name").and_then(Value::as_str);
    let request = json!({
        "provider_id": challenge.provider_id,
        "provider_did": state.agent_did,
        "display_name": display_name,
        "ownership_challenge_id": challenge.challenge_id,
        "ownership_signature": signature,
    });
    let provider = client
        .register_provider(&request)
        .await
        .map_err(|error| servicenet_error_response(&error))?;
    Ok(provider["provider_id"]
        .as_str()
        .unwrap_or_else(|| request["provider_id"].as_str().unwrap_or_default())
        .to_owned())
}

fn service_agent_execution(body: &PublishAgentBody) -> Result<ServiceAgentExecution, String> {
    match body.execution_mode {
        PublishExecutionMode::WattetheriaRuntime => Ok(ServiceAgentExecution::WattetheriaRuntime),
        PublishExecutionMode::CustomizedAgent => {
            let protocol = body
                .protocol
                .ok_or_else(|| "protocol is required for Customized Agent execution".to_owned())?;
            let customized_agent_url = body
                .customized_agent_url
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| {
                    "customized_agent_url is required for Customized Agent execution".to_owned()
                })?;
            ServiceAgentExecution::customized(protocol, customized_agent_url)
                .map_err(|error| error.to_string())
        }
    }
}

pub(crate) fn load_publisher_state(data_dir: &FsPath) -> anyhow::Result<ServiceNetPublisherState> {
    load_servicenet_publisher_state(data_dir)
}

#[cfg(test)]
pub(crate) fn save_publisher_state(
    data_dir: &FsPath,
    publisher_state: &ServiceNetPublisherState,
) -> anyhow::Result<()> {
    wattetheria_kernel::servicenet::save_servicenet_publisher_state(data_dir, publisher_state)
}

fn derive_agent_id(agent_card: &Value, provider_id: &str) -> String {
    let name = agent_card
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or("agent");
    let url = agent_card.get("url").and_then(Value::as_str).unwrap_or("");
    let slug = slugify_agent_name(name);
    let digest = Sha256::digest(format!("{provider_id}:{url}").as_bytes());
    let suffix = format!("{digest:x}");
    format!("{slug}-{}", &suffix[..8])
}

fn slugify_agent_name(name: &str) -> String {
    let mut slug = String::new();
    let mut last_was_dash = false;
    for ch in name.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch);
            last_was_dash = false;
        } else if !last_was_dash && !slug.is_empty() {
            slug.push('-');
            last_was_dash = true;
        }
    }
    while slug.ends_with('-') {
        slug.pop();
    }
    if slug.is_empty() {
        "agent".to_owned()
    } else {
        slug
    }
}

fn now_ms() -> u64 {
    Utc::now().timestamp_millis().max(0).cast_unsigned()
}

pub(crate) async fn agent_card_template(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, &headers).await {
        return response;
    }
    Json(json!({
        "defaults": default_agent_card(),
        "fields": [
            {"name": "execution_mode", "label": "Execution Mode", "kind": "select", "required": true, "options": ["wattetheria_runtime", "customized_agent"]},
            {"name": "connection_mode", "label": "Connection Mode", "kind": "select", "required": true, "options": ["servicenet_relay", "wattetheria_direct"]},
            {"name": "protocol", "label": "Protocol", "kind": "select", "required": false, "options": ["a2a_v1"], "note": "Required only for Customized Agent execution."},
            {"name": "customized_agent_url", "label": "Customized Agent URL", "kind": "url", "required": false, "note": "Private upstream URL used only by the local Wattetheria Adapter."},
            {"name": "name", "label": "Name", "kind": "text", "required": true, "note": "Public agent name used in ServiceNet listings."},
            {"name": "service_address", "label": "Service Address", "kind": "text", "required": false, "note": "Unique ServiceNet alias stored in DID alsoKnownAs. Use <name>@wattetheria."},
            {"name": "description", "label": "Description", "kind": "textarea", "required": true, "note": "Short summary shown before invocation."},
            {"name": "url", "label": "Adapter URL", "kind": "url", "required": true, "note": "Public HTTPS URL mapped by the publisher to this Wattetheria Adapter. No path is added automatically."},
            {"name": "scope", "label": "Scope", "kind": "select", "required": true, "options": ["real_world", "wattetheria_native"], "note": "real_world is for public services; wattetheria_native is for agents published from a Wattetheria node."},
            {"name": "origin", "label": "Origin", "kind": "select", "required": true, "options_by_scope": {"real_world": ["established_service", "custom_built"], "wattetheria_native": ["native_published"]}},
            {"name": "domain", "label": "Domain", "kind": "select", "required": true, "options_by_scope": {"real_world": real_world_domains(), "wattetheria_native": wattetheria_native_domains()}},
            {"name": "cost", "label": "Cost", "kind": "number", "required": true, "note": "User-set amount charged for invoking this agent."},
            {"name": "currency", "label": "Currency", "kind": "select", "required": true, "options": ["USDC", "USDT"]},
            {"name": "supportsTask", "label": "Supports Task", "kind": "boolean", "required": true, "note": "True when SendMessage may return a Task that callers poll later."},
            {"name": "skills", "label": "Skills", "kind": "array", "required": true, "item_fields": ["name", "description"], "optional_item_fields": ["description"]},
            {"name": "x402", "label": "x402 Payment Discovery", "kind": "optional", "required": false, "note": "Optional static payment discovery. payTo is the callee settlement receiving address."}
        ],
    }))
    .into_response()
}

#[allow(clippy::too_many_lines)]
pub(crate) async fn publish_agent(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<PublishAgentBody>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let Some(client) = servicenet_client(&state) else {
        return super::servicenet_unavailable_response();
    };
    let mut agent_card = normalize_agent_card_skills(body.agent_card.clone());
    if let Err(message) = validate_agent_card(&agent_card) {
        return bad_request(message);
    }
    let execution = match service_agent_execution(&body) {
        Ok(execution) => execution,
        Err(message) => return bad_request(message),
    };
    let connection_mode = body.connection_mode;
    let mut publisher_state = match load_publisher_state(&state.data_dir) {
        Ok(state) => state,
        Err(error) => return internal_error(&error),
    };
    let provider_id = match resolve_provider_id(client, &state, &body, &mut publisher_state).await {
        Ok(provider_id) => provider_id,
        Err(response) => return response,
    };
    let agent_id = body
        .agent_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map_or_else(
            || derive_agent_id(&agent_card, &provider_id),
            ToOwned::to_owned,
        );
    let service_address = match body
        .service_address
        .as_deref()
        .map(normalize_service_address)
        .transpose()
    {
        Ok(value) => value.flatten(),
        Err(error) => return bad_request(error.to_string()),
    };
    let version = body
        .version
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(DEFAULT_SERVICENET_VERSION)
        .to_owned();
    let risk_level = body
        .risk_level
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("low")
        .to_owned();
    let payment_account_binding =
        match active_payment_account_binding_proof(&state.data_dir, state.signer.as_ref()) {
            Ok(Some(proof)) => match serde_json::to_value(proof) {
                Ok(value) => value,
                Err(error) => return internal_error(&anyhow::anyhow!(error)),
            },
            Ok(None) => Value::Null,
            Err(error) => return internal_error(&error),
        };
    let endpoint = agent_card
        .get("url")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let service_identity_store = FileServiceAgentIdentityStore::new(&state.data_dir);
    let identity_provision = {
        let store = service_identity_store.clone();
        let agent_id = agent_id.clone();
        let endpoint = endpoint.clone();
        match tokio::task::spawn_blocking(move || store.provision(&agent_id, &endpoint)).await {
            Ok(Ok(provision)) => provision,
            Ok(Err(error)) => return internal_error(&error),
            Err(error) => {
                return internal_error(&anyhow::anyhow!(
                    "Service Agent identity provisioning task failed: {error}"
                ));
            }
        }
    };
    let service_identity = identity_provision.identity();
    attach_service_agent_payment_binding(&mut agent_card, Some(&payment_account_binding));
    let publication = match prepare_service_agent_publication(
        ServiceAgentPublicationInput {
            provider_id: &provider_id,
            agent_id: &agent_id,
            service_did: &service_identity.service_did,
            service_address: service_address.as_deref(),
            version: &version,
            risk_level: &risk_level,
            agent_card,
            connection_mode,
            execution: execution.clone(),
            provider_attester_did: &state.agent_did,
            ttl_minutes: body.ttl_minutes.unwrap_or(DEFAULT_SERVICENET_TTL_MINUTES),
        },
        state.signer.as_ref(),
    ) {
        Ok(publication) => publication,
        Err(error) => {
            return match service_identity_store.rollback_provision(identity_provision) {
                Ok(()) => internal_error(&error),
                Err(rollback_error) => internal_error(&anyhow::anyhow!(
                    "prepare Service Agent publication failed: {error:#}; identity rollback failed: {rollback_error:#}"
                )),
            };
        }
    };
    let request = &publication.request;
    let response = match submit_service_agent_publication(client, &state.data_dir, &publication)
        .await
    {
        Ok(response) => response,
        Err(ServiceAgentPublicationSubmitError::BeforeSubmission(error)) => {
            return match service_identity_store.rollback_provision(identity_provision) {
                Ok(()) => internal_error(&error),
                Err(rollback_error) => internal_error(&anyhow::anyhow!(
                    "stage Service Agent publication failed: {error:#}; identity rollback failed: {rollback_error:#}"
                )),
            };
        }
        Err(ServiceAgentPublicationSubmitError::Remote(error)) => {
            if error.is_definitive_rejection() {
                return match service_identity_store.rollback_provision(identity_provision) {
                    Ok(()) => servicenet_error_response(&error),
                    Err(rollback_error) => internal_error(&anyhow::anyhow!(
                        "ServiceNet publication failed: {error}; identity rollback failed: {rollback_error:#}"
                    )),
                };
            }
            return servicenet_error_response(&error);
        }
        Err(ServiceAgentPublicationSubmitError::LocalRollback(error)) => {
            return internal_error(&error);
        }
    };
    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: Utc::now().timestamp(),
        category: "servicenet".to_string(),
        action: "servicenet.agents.publish".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(request["agent_id"].as_str().unwrap_or_default().to_owned()),
        capability: Some("net.outbound".to_string()),
        reason: Some("servicenet.publish".to_string()),
        duration_ms: None,
        details: Some(json!({
            "provider_id": request["provider_id"],
            "version": request["version"],
        })),
    });
    Json(json!({
        "status": "ok",
        "agent_id": request["agent_id"],
        "service_did": request["service_did"],
        "service_identity_path": service_identity_store
            .identity_path(request["agent_id"].as_str().unwrap_or_default()),
        "service_address": request["service_address"],
        "provider_id": request["provider_id"],
        "provider_did": state.agent_did,
        "connection_mode": connection_mode,
        "execution": execution,
        "submission": response,
    }))
    .into_response()
}

#[allow(clippy::too_many_lines)]
pub(crate) async fn unpublish_agent(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Path(agent_id): Path<String>,
    Json(body): Json<UnpublishAgentBody>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let Some(client) = servicenet_client(&state) else {
        return super::servicenet_unavailable_response();
    };
    let _agent_operation_lock = {
        let store = FileServiceAgentIdentityStore::new(&state.data_dir);
        let lock_agent_id = agent_id.clone();
        match tokio::task::spawn_blocking(move || store.lock_agent_operation(&lock_agent_id)).await
        {
            Ok(Ok(lock)) => lock,
            Ok(Err(error)) => return internal_error(&error),
            Err(error) => {
                return internal_error(&anyhow::anyhow!(
                    "Service Agent operation lock task failed: {error}"
                ));
            }
        }
    };
    let publisher_state = match load_publisher_state(&state.data_dir) {
        Ok(state) => state,
        Err(error) => return internal_error(&error),
    };
    let Some(registration) = publisher_state
        .registrations
        .iter()
        .find(|registration| {
            registration.agent_id == agent_id
                && registration_matches_identity(registration, &state.agent_did)
        })
        .cloned()
    else {
        return forbidden("agent is not published by the current Wattetheria identity");
    };
    let reason = body
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let issued_at_ms = now_ms();
    let expires_at_ms = issued_at_ms.saturating_add(
        body.ttl_minutes
            .unwrap_or(DEFAULT_SERVICENET_TTL_MINUTES)
            .saturating_mul(60_000),
    );
    let nonce = Uuid::new_v4().to_string();
    let unpublish_payload = json!({
        "action": "unpublish_agent",
        "provider_id": registration.provider_id.clone(),
        "provider_did": state.agent_did.clone(),
        "agent_id": agent_id.clone(),
        "nonce": nonce.clone(),
        "issued_at_ms": issued_at_ms,
        "expires_at_ms": expires_at_ms,
        "reason": reason.clone(),
    });
    let unpublish_bytes = match serde_jcs::to_vec(&unpublish_payload) {
        Ok(bytes) => bytes,
        Err(error) => return internal_error(&anyhow::anyhow!(error)),
    };
    let signature = match state.signer.sign_bytes(&unpublish_bytes) {
        Ok(signature) => signature,
        Err(error) => return internal_error(&error),
    };
    let request = json!({
        "provider_id": registration.provider_id,
        "provider_did": state.agent_did.clone(),
        "signature": signature,
        "nonce": nonce,
        "issued_at_ms": issued_at_ms,
        "expires_at_ms": expires_at_ms,
        "reason": reason,
    });
    let response = match client.unpublish_agent(&agent_id, &request).await {
        Ok(response) => response,
        Err(error)
            if error
                .status()
                .is_some_and(|status| status == StatusCode::NOT_FOUND) =>
        {
            json!({
                "status": "remote_missing",
                "agent_id": agent_id.clone(),
                "service_address": registration.service_address.clone(),
                "error": error.to_string(),
            })
        }
        Err(error) => return servicenet_error_response(&error),
    };
    if let Err(error) =
        remove_servicenet_publisher_registration(&state.data_dir, &agent_id, &state.agent_did)
    {
        return internal_error(&error);
    }
    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: Utc::now().timestamp(),
        category: "servicenet".to_string(),
        action: "servicenet.agents.unpublish".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(agent_id.clone()),
        capability: Some("net.outbound".to_string()),
        reason: Some("servicenet.unpublish".to_string()),
        duration_ms: None,
        details: Some(json!({
            "provider_id": request["provider_id"],
        })),
    });
    Json(json!({
        "status": "ok",
        "agent_id": agent_id,
        "provider_id": request["provider_id"],
        "provider_did": state.agent_did,
        "unpublished": response,
    }))
    .into_response()
}
