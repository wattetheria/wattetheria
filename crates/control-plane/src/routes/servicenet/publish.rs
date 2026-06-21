use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs;
use std::net::IpAddr;
use std::path::{Path as FsPath, PathBuf};
use uuid::Uuid;

use super::{servicenet_client, servicenet_error_response};
use crate::auth::{authorize, internal_error};
use crate::state::ControlPlaneState;
use wattetheria_kernel::audit::AuditEntry;
use wattetheria_kernel::servicenet::{
    ServiceNetClient, normalize_service_address, validate_servicenet_agent_name,
};

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
    agent_card: Value,
}

#[derive(Debug, Deserialize)]
pub(crate) struct UnpublishAgentBody {
    #[serde(default)]
    reason: Option<String>,
    #[serde(default)]
    ttl_minutes: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub(crate) struct ServiceNetPublisherState {
    #[serde(default)]
    pub(crate) registrations: Vec<ServiceNetPublisherRegistration>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct ServiceNetPublisherRegistration {
    pub(crate) provider_id: String,
    pub(crate) provider_did: String,
    pub(crate) agent_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) service_address: Option<String>,
    pub(crate) card_hash: String,
    pub(crate) version: String,
    pub(crate) updated_at: String,
    #[serde(default)]
    pub(crate) agent_card: Value,
    #[serde(default)]
    pub(crate) deployment: Value,
    #[serde(default)]
    pub(crate) review: Value,
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

fn default_agent_card() -> Value {
    json!({
        "name": "",
        "description": "",
        "url": "",
        "preferredTransport": "JSONRPC",
        "protocolVersion": "1.0",
        "scope": "real_world",
        "origin": "custom_built",
        "domain": "GENERAL",
        "cost": 0,
        "currency": "USDC",
        "supportsTask": false,
        "skills": [{"name": "", "description": ""}],
        "securitySchemes": {"none": {"type": "none"}},
        "security": [{"none": []}],
    })
}

fn normalize_agent_card_skills(mut card: Value) -> Value {
    if let Some(skills) = card.get_mut("skills").and_then(Value::as_array_mut) {
        for skill in skills {
            if let Some(skill_object) = skill.as_object_mut() {
                skill_object
                    .entry("description")
                    .or_insert_with(|| Value::String(String::new()));
            }
        }
    }
    card
}

fn real_world_domains() -> Vec<&'static str> {
    vec![
        "GENERAL",
        "TRANSPORTATION",
        "FOOD",
        "CLOTHING",
        "HOUSING",
        "PAYMENTS",
        "COMMERCE",
        "MEDIA",
        "HEALTH",
        "EDUCATION",
        "TRAVEL",
    ]
}

fn wattetheria_native_domains() -> Vec<&'static str> {
    vec![
        "GENERAL",
        "GOVERNANCE",
        "PRODUCTION",
        "TRADING",
        "AUTOMATION",
        "SECURITY",
        "EXPLORATION",
        "DISCOVERY",
        "SERVICENET",
    ]
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

fn validate_agent_card(card: &Value) -> Result<(), String> {
    let object = card
        .as_object()
        .ok_or_else(|| "agent card must be a JSON object".to_owned())?;
    validate_agent_card_required_fields(object)?;
    validate_agent_card_strings(object)?;
    validate_endpoint(
        object
            .get("url")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    )?;
    if object.get("preferredTransport").and_then(Value::as_str) != Some("JSONRPC") {
        return Err("agent card `preferredTransport` must be `JSONRPC`".to_owned());
    }
    if object.get("protocolVersion").and_then(Value::as_str) != Some("1.0") {
        return Err("agent card `protocolVersion` must be `1.0`".to_owned());
    }
    validate_scope_origin_domain(
        object
            .get("scope")
            .and_then(Value::as_str)
            .unwrap_or_default(),
        object
            .get("origin")
            .and_then(Value::as_str)
            .unwrap_or_default(),
        object
            .get("domain")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    )?;
    validate_agent_card_pricing(object)?;
    if object
        .get("supportsTask")
        .and_then(Value::as_bool)
        .is_none()
    {
        return Err("agent card `supportsTask` must be a boolean".to_owned());
    }
    validate_agent_card_skills(object)?;
    validate_agent_card_no_secrets(card)
}

fn validate_agent_card_required_fields(
    object: &serde_json::Map<String, Value>,
) -> Result<(), String> {
    for field in [
        "name",
        "description",
        "url",
        "preferredTransport",
        "protocolVersion",
        "scope",
        "origin",
        "domain",
        "cost",
        "currency",
        "supportsTask",
        "skills",
        "securitySchemes",
        "security",
    ] {
        if !object.contains_key(field) {
            return Err(format!("agent card is missing required field `{field}`"));
        }
    }
    Ok(())
}

fn validate_agent_card_strings(object: &serde_json::Map<String, Value>) -> Result<(), String> {
    for field in ["name", "description"] {
        if object
            .get(field)
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default()
            .is_empty()
        {
            return Err(format!("agent card `{field}` must not be empty"));
        }
    }
    validate_servicenet_agent_name(
        object
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    )
    .map_err(|error| error.to_string())?;
    Ok(())
}

fn validate_agent_card_pricing(object: &serde_json::Map<String, Value>) -> Result<(), String> {
    let cost = object
        .get("cost")
        .and_then(Value::as_u64)
        .ok_or_else(|| "agent card `cost` must be a non-negative integer".to_owned())?;
    if cost > u64::from(u32::MAX) {
        return Err(format!(
            "agent card `cost` must be a non-negative integer up to {}",
            u32::MAX
        ));
    }
    if !matches!(
        object.get("currency").and_then(Value::as_str),
        Some("USDC" | "USDT")
    ) {
        return Err("agent card `currency` must be `USDC` or `USDT`".to_owned());
    }
    Ok(())
}

fn validate_agent_card_skills(object: &serde_json::Map<String, Value>) -> Result<(), String> {
    let skills = object
        .get("skills")
        .and_then(Value::as_array)
        .ok_or_else(|| "agent card `skills` must be an array".to_owned())?;
    if skills.is_empty() {
        return Err("agent card `skills` must list at least one skill".to_owned());
    }
    for (index, skill) in skills.iter().enumerate() {
        if skill
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default()
            .is_empty()
        {
            return Err(format!("skill[{index}] is missing required field `name`"));
        }
    }
    Ok(())
}

fn validate_agent_card_no_secrets(card: &Value) -> Result<(), String> {
    let card_text = serde_json::to_string(card).unwrap_or_default();
    if card_text.contains("sk-") || card_text.contains("BEGIN PRIVATE KEY") {
        return Err(
            "agent card appears to contain a secret; remove it before publishing".to_owned(),
        );
    }
    Ok(())
}

fn validate_endpoint(endpoint: &str) -> Result<(), String> {
    let url = reqwest::Url::parse(endpoint)
        .map_err(|error| format!("endpoint is not a valid URL: {error}"))?;
    if url.scheme() != "https" {
        return Err(format!(
            "endpoint must use https:// (got scheme `{}`)",
            url.scheme()
        ));
    }
    let host = url
        .host_str()
        .ok_or_else(|| "endpoint must include a host".to_owned())?;
    if host.eq_ignore_ascii_case("localhost") {
        return Err("endpoint host must not be localhost".to_owned());
    }
    if host.parse::<IpAddr>().is_ok() {
        return Err(format!(
            "endpoint host is an IP literal ({host}); use a DNS hostname instead"
        ));
    }
    Ok(())
}

fn validate_scope_origin_domain(scope: &str, origin: &str, domain: &str) -> Result<(), String> {
    match scope {
        "real_world" if !matches!(origin, "established_service" | "custom_built") => {
            return Err(
                "agent card `origin` must be `established_service` or `custom_built` for `real_world` scope"
                    .to_owned(),
            );
        }
        "wattetheria_native" if origin != "native_published" => {
            return Err(
                "agent card `origin` must be `native_published` for `wattetheria_native` scope"
                    .to_owned(),
            );
        }
        "real_world" | "wattetheria_native" => {}
        _ => {
            return Err(
                "agent card `scope` must be `real_world` or `wattetheria_native`".to_owned(),
            );
        }
    }
    let allowed = if scope == "real_world" {
        real_world_domains()
    } else {
        wattetheria_native_domains()
    };
    if !allowed.contains(&domain) {
        return Err(format!(
            "agent card `domain` is not supported for `{scope}` scope"
        ));
    }
    Ok(())
}

fn publisher_state_path(data_dir: &FsPath) -> PathBuf {
    data_dir.join("servicenet").join("publisher-state.json")
}

pub(crate) fn load_publisher_state(data_dir: &FsPath) -> anyhow::Result<ServiceNetPublisherState> {
    let path = publisher_state_path(data_dir);
    if !path.exists() {
        return Ok(ServiceNetPublisherState::default());
    }
    let raw = fs::read_to_string(&path)?;
    Ok(serde_json::from_str(&raw)?)
}

pub(crate) fn save_publisher_state(
    data_dir: &FsPath,
    publisher_state: &ServiceNetPublisherState,
) -> anyhow::Result<()> {
    let path = publisher_state_path(data_dir);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, serde_json::to_vec_pretty(publisher_state)?)?;
    Ok(())
}

fn remove_registration(
    publisher_state: &mut ServiceNetPublisherState,
    agent_id: &str,
    owner_did: &str,
) {
    publisher_state.registrations.retain(|registration| {
        registration.agent_id != agent_id || !registration_matches_identity(registration, owner_did)
    });
}

fn upsert_registration(
    publisher_state: &mut ServiceNetPublisherState,
    registration: ServiceNetPublisherRegistration,
) {
    publisher_state
        .registrations
        .retain(|existing| existing.agent_id != registration.agent_id);
    publisher_state.registrations.push(registration);
}

fn hash_agent_card(card: &Value) -> String {
    let canonical = serde_jcs::to_string(card).unwrap_or_else(|_| card.to_string());
    format!("sha256:{:x}", Sha256::digest(canonical.as_bytes()))
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
            {"name": "name", "label": "Name", "kind": "text", "required": true, "note": "Public agent name used in ServiceNet listings."},
            {"name": "service_address", "label": "Service Address", "kind": "text", "required": false, "note": "Unique ServiceNet alias stored in DID alsoKnownAs. Use <name>@wattetheria."},
            {"name": "description", "label": "Description", "kind": "textarea", "required": true, "note": "Short summary shown before invocation."},
            {"name": "url", "label": "Endpoint URL", "kind": "url", "required": true, "note": "Public HTTPS A2A JSON-RPC endpoint. Do not use localhost or private IPs."},
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
    let agent_card = normalize_agent_card_skills(body.agent_card.clone());
    if let Err(message) = validate_agent_card(&agent_card) {
        return bad_request(message);
    }
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
    let endpoint = agent_card
        .get("url")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let deployment = json!({
        "runtime": "agent",
        "endpoint": {
            "url": endpoint,
            "protocol_binding": "JSONRPC",
            "protocol_version": "1.0",
            "interaction_protocol": "google_a2a",
        },
    });
    let review = json!({
        "risk_level": risk_level,
        "human_approval_required": false,
    });
    let artifacts = json!({});
    let issued_at_ms = now_ms();
    let expires_at_ms = issued_at_ms.saturating_add(
        body.ttl_minutes
            .unwrap_or(DEFAULT_SERVICENET_TTL_MINUTES)
            .saturating_mul(60_000),
    );
    let nonce = Uuid::new_v4().to_string();
    let attestation_payload = json!({
        "provider_id": provider_id,
        "agent_id": agent_id,
        "service_address": service_address.clone(),
        "version": version,
        "agent_card": agent_card,
        "deployment": deployment,
        "review": review,
        "artifacts": artifacts,
        "provider_attester_did": state.agent_did,
        "delegation_token": Value::Null,
        "source_commit": Value::Null,
        "build_digest": Value::Null,
        "payment_account_binding": Value::Null,
        "nonce": nonce,
        "issued_at_ms": issued_at_ms,
        "expires_at_ms": expires_at_ms,
    });
    let attestation_bytes = match serde_jcs::to_vec(&attestation_payload) {
        Ok(bytes) => bytes,
        Err(error) => return internal_error(&anyhow::anyhow!(error)),
    };
    let signature = match state.signer.sign_bytes(&attestation_bytes) {
        Ok(signature) => signature,
        Err(error) => return internal_error(&error),
    };
    let request = json!({
        "provider_id": provider_id,
        "agent_id": agent_id,
        "service_address": service_address.clone(),
        "version": version,
        "agent_card": agent_card,
        "deployment": deployment,
        "review": review,
        "artifacts": artifacts,
        "attestations": {
            "attestation_signature": signature,
            "provider_attester_did": state.agent_did,
            "nonce": nonce,
            "issued_at_ms": issued_at_ms,
            "expires_at_ms": expires_at_ms,
        },
    });
    let response = match client.submit_agent(&request).await {
        Ok(response) => response,
        Err(error) => return servicenet_error_response(&error),
    };
    upsert_registration(
        &mut publisher_state,
        ServiceNetPublisherRegistration {
            provider_id: request["provider_id"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            provider_did: state.agent_did.clone(),
            agent_id: request["agent_id"].as_str().unwrap_or_default().to_owned(),
            service_address: request["service_address"].as_str().map(ToOwned::to_owned),
            card_hash: hash_agent_card(&request["agent_card"]),
            version: request["version"].as_str().unwrap_or_default().to_owned(),
            updated_at: Utc::now().to_rfc3339(),
            agent_card: request["agent_card"].clone(),
            deployment: request["deployment"].clone(),
            review: request["review"].clone(),
        },
    );
    if let Err(error) = save_publisher_state(&state.data_dir, &publisher_state) {
        return internal_error(&error);
    }
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
        "service_address": request["service_address"],
        "provider_id": request["provider_id"],
        "provider_did": state.agent_did,
        "submission": response,
    }))
    .into_response()
}

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
    let mut publisher_state = match load_publisher_state(&state.data_dir) {
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
        Err(error) => return servicenet_error_response(&error),
    };
    remove_registration(&mut publisher_state, &agent_id, &state.agent_did);
    if let Err(error) = save_publisher_state(&state.data_dir, &publisher_state) {
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
