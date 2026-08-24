use anyhow::Context;
use axum::Json;
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use std::collections::{HashMap, hash_map::DefaultHasher};
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use tokio::time::sleep;
use uuid::Uuid;
use wattetheria_kernel::audit::AuditEntry;
use wattetheria_kernel::network_agent_registration::{
    PERMISSION_STATUS_ACTIVE, REGISTRATION_PROTOCOL_VERSION, RegistrationRequest,
    decode_credential, decode_request, load_network_permission_checkpoint,
    network_permission_is_active, network_permission_update, now_ms, request_signature_is_valid,
    set_membership_credential_status, store_membership_credential,
    update_network_permission_checkpoint,
};

use crate::auth::{authorize, internal_error};
use crate::social_host;
use crate::state::{ControlPlaneState, NetworkPeersQuery};

const REGISTRY_API_ROUTE: &str = "/v1";
const REGISTRY_AUTHORITY_ROUTE: &str = "/authority";
const REGISTRY_AUTO_REGISTRATION_ROUTE: &str = "/registrations/auto";
const REGISTRY_MANUAL_REGISTRATION_ROUTE: &str = "/registrations/manual";
const REGISTRY_REGISTRATION_ROUTE: &str = "/registrations";
const REGISTRY_NICKNAME_ROUTE: &str = "/agents/nickname";
const WATTSWARM_STATE_DIR_ENV: &str = "WATTSWARM_STATE_DIR";
const WATTSWARM_STATE_DIR_DEFAULT: &str = "/var/lib/wattswarm";
const DISCOVERY_BOOTNODE_URLS_FILE: &str = "discovery_bootnode_urls_v1.json";
const REGISTRY_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const NETWORK_PERMISSION_RETRY_INTERVAL: Duration = Duration::from_secs(5);
static NETWORK_PERMISSION_RETRY_REVISIONS: OnceLock<Mutex<HashMap<PathBuf, u64>>> = OnceLock::new();

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryRegistrationResult {
    pub request_id: String,
    pub status: String,
}

impl RegistryRegistrationResult {
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.status.eq_ignore_ascii_case("approved") || self.status.eq_ignore_ascii_case("active")
    }

    #[must_use]
    pub fn is_pending(&self) -> bool {
        self.status.eq_ignore_ascii_case("pending")
    }
}

pub(crate) async fn network_status(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let network = match state.swarm_bridge.network_status().await {
        Ok(network) => network,
        Err(error) => return internal_error(&error),
    };
    let peers = match state.swarm_bridge.peers().await {
        Ok(peers) => peers,
        Err(error) => return internal_error(&error),
    };
    let active_peers = if network.running { peers.len() } else { 0 };
    let active_nodes = 1 + active_peers;
    let total_nodes = 1 + peers.len();
    let health_percent = ((active_nodes * 100) / total_nodes.max(1)) as u64;
    let payload = json!({
        "running": network.running,
        "mode": network.mode,
        "total_nodes": total_nodes,
        "active_nodes": active_nodes,
        "health_percent": health_percent,
        "avg_latency_ms": 0,
        "peer_protocol_distribution": network.peer_protocol_distribution,
    });

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "network".to_string(),
        action: "network.status.query".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: None,
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(payload.clone()),
    });

    Json(payload).into_response()
}

pub(crate) async fn network_peers(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<NetworkPeersQuery>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let peers = match state.swarm_bridge.peers().await {
        Ok(peers) => peers,
        Err(error) => return internal_error(&error),
    };
    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    let payload = peers
        .into_iter()
        .take(limit)
        .map(|peer| {
            let (lat, lng) = derived_geo(&peer.node_id);
            json!({
                "id": peer.node_id,
                "status": "online",
                "distance_km": derived_distance_km(&peer.node_id),
                "latency_ms": 0,
                "lat": lat,
                "lng": lng,
                "coordinate_source": "derived",
            })
        })
        .collect::<Vec<_>>();

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "network".to_string(),
        action: "network.peers.query".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: None,
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({"count": payload.len()})),
    });

    Json(json!({"peers": payload})).into_response()
}

pub(crate) async fn source_agent_card(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let card = match social_host::public_source_agent_card(&state).await {
        Ok(card) => card,
        Err(error) => return internal_error(&error),
    };

    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "network".to_string(),
        action: "network.source_agent_card.query".to_string(),
        status: "ok".to_string(),
        actor: Some(auth),
        subject: Some(card.agent_id.clone()),
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({
            "agent_id": card.agent_id.clone(),
            "node_id": card.node_id.clone(),
            "card_hash": card.card_hash.clone(),
            "issued_at": card.issued_at,
        })),
    });

    Json(card).into_response()
}

pub(crate) async fn registration_request(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let request = match build_registration_request(&state).await {
        Ok(request) => request,
        Err(error) => return internal_error(&error),
    };
    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "network".to_owned(),
        action: "network.registration.request.query".to_owned(),
        status: "ok".to_owned(),
        actor: Some(auth),
        subject: Some(state.agent_did.clone()),
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({"network_id": request.network_id, "request_id": request.request_id})),
    });
    Json(request).into_response()
}

pub async fn build_registration_request(
    state: &ControlPlaneState,
) -> anyhow::Result<RegistrationRequest> {
    let network_id = match state.swarm_bridge.current_network_id().await {
        Ok(network_id) if !network_id.trim().is_empty() => network_id,
        Ok(_) => anyhow::bail!("current network id is empty"),
        Err(error) => return Err(error),
    };
    let card = social_host::public_source_agent_card(state).await?;
    let nickname = card
        .card
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(&state.agent_did)
        .chars()
        .take(80)
        .collect::<String>();
    let request_id = new_registration_id("request");
    let nonce = new_registration_id("nonce");
    let mut request = RegistrationRequest {
        version: REGISTRATION_PROTOCOL_VERSION,
        request_id,
        network_id,
        agent_did: state.agent_did.clone(),
        nickname,
        agent_card: Some(card.card),
        agent_card_hash: Some(card.card_hash),
        tenant_instance_id: None,
        nonce,
        signature_b64: String::new(),
    };
    let signing_bytes = match request.signing_bytes() {
        Ok(bytes) => bytes,
        Err(error) => return Err(error),
    };
    request.signature_b64 = state.signer.sign_bytes(&signing_bytes)?;
    Ok(request)
}

pub(crate) async fn registration_credential(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(payload): Json<Value>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    match apply_registration_record(&state, &payload, auth).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => internal_error(&error),
    }
}

pub(crate) async fn network_permission(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, &headers).await {
        return response;
    }
    let checkpoint =
        match load_network_permission_checkpoint(&state.local_db, &state.agent_did, None) {
            Ok(checkpoint) => checkpoint,
            Err(error) => return internal_error(&error),
        };
    let checkpoint = match checkpoint
        .as_ref()
        .map(|checkpoint| network_permission_update(&state.local_db, checkpoint))
        .transpose()
    {
        Ok(checkpoint) => checkpoint,
        Err(error) => return internal_error(&error),
    };
    let active = match network_permission_is_active(&state.local_db, &state.agent_did) {
        Ok(active) => active,
        Err(error) => return internal_error(&error),
    };
    Json(json!({
        "ok": true,
        "active": active,
        "checkpoint": checkpoint,
    }))
    .into_response()
}

pub async fn apply_registry_registration_record(
    state: &ControlPlaneState,
    payload: &Value,
) -> anyhow::Result<RegistryRegistrationResult> {
    let value = apply_registration_record(state, payload, "registry").await?;
    let request_id = value
        .get("request_id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("registration response missing request_id"))?
        .to_owned();
    let status = value
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("pending")
        .to_owned();
    Ok(RegistryRegistrationResult { request_id, status })
}

async fn apply_registration_record(
    state: &ControlPlaneState,
    payload: &Value,
    auth: impl Into<String>,
) -> anyhow::Result<Value> {
    let request = decode_request(payload)?;
    if request.agent_did != state.agent_did {
        anyhow::bail!("registration credential agent_did does not match local Agent");
    }
    request_signature_is_valid(&request)?;
    let status = match payload
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("pending")
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "approved" | "active" => "approved",
        "pending" => "pending",
        "rejected" => "rejected",
        "disabled" => "disabled",
        other => anyhow::bail!("unsupported registration credential status: {other}"),
    };
    if status != "approved" {
        return persist_non_approved_registration(state, &request, status).await;
    }
    let raw_credential = payload
        .get("credential")
        .filter(|credential| !credential.is_null())
        .ok_or_else(|| anyhow::anyhow!("approved registration credential is missing"))?;
    let credential = decode_credential(raw_credential)?;
    persist_approved_registration(state, &request, credential, auth.into()).await
}

async fn persist_non_approved_registration(
    state: &ControlPlaneState,
    request: &RegistrationRequest,
    status: &str,
) -> anyhow::Result<Value> {
    let stored = set_membership_credential_status(&state.local_db, request, status, now_ms())?;
    let (checkpoint, checkpoint_changed) =
        persist_network_permission_checkpoint(state, request, status, stored, None).await?;
    let callback_sent = if checkpoint_changed {
        push_network_permission_checkpoint(state, &checkpoint).await
    } else {
        true
    };
    Ok(json!({
        "ok": true,
        "status": status,
        "stored": stored,
        "request_id": request.request_id,
        "permission_checkpoint": checkpoint,
        "callback_sent": callback_sent,
    }))
}

async fn persist_approved_registration(
    state: &ControlPlaneState,
    request: &RegistrationRequest,
    credential: wattetheria_kernel::network_agent_registration::MembershipCredential,
    auth: String,
) -> anyhow::Result<Value> {
    let trust_anchor = state.swarm_bridge.network_credential_trust_anchor().await?;
    let credential_changed = store_membership_credential(
        state.local_db.as_ref(),
        request,
        &credential,
        &trust_anchor,
        now_ms(),
    )?;
    let (checkpoint, checkpoint_changed) = persist_network_permission_checkpoint(
        state,
        request,
        PERMISSION_STATUS_ACTIVE,
        credential_changed,
        None,
    )
    .await?;
    let callback_sent = if checkpoint_changed {
        push_network_permission_checkpoint(state, &checkpoint).await
    } else {
        true
    };
    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: 0,
        category: "network".to_owned(),
        action: "network.registration.credential.store".to_owned(),
        status: "ok".to_owned(),
        actor: Some(auth),
        subject: Some(request.agent_did.clone()),
        capability: None,
        reason: None,
        duration_ms: None,
        details: Some(json!({
            "network_id": request.network_id,
            "request_id": request.request_id,
            "credential_id": credential.unsigned.credential_id,
        })),
    });
    Ok(json!({
        "ok": true,
        "status": "approved",
        "stored": true,
        "request_id": request.request_id,
        "credential_id": credential.unsigned.credential_id,
        "permission_checkpoint": checkpoint,
        "callback_sent": callback_sent,
    }))
}

pub(crate) async fn persist_network_permission_checkpoint(
    state: &ControlPlaneState,
    request: &RegistrationRequest,
    status: &str,
    force_changed: bool,
    last_error: Option<String>,
) -> anyhow::Result<(
    wattetheria_kernel::local_db::NetworkPermissionCheckpoint,
    bool,
)> {
    let node_id = state
        .swarm_bridge
        .local_node_id()
        .await
        .ok()
        .filter(|node_id| !node_id.trim().is_empty())
        .unwrap_or_else(|| state.agent_did.clone());
    let network_status = if status == PERMISSION_STATUS_ACTIVE {
        "running"
    } else {
        "stopped"
    };
    if let Some(existing) = state.local_db.load_network_permission_checkpoint(
        &request.agent_did,
        Some(&request.network_id),
        Some(&node_id),
    )? && !force_changed
        && existing.permission_status == status
        && existing.network_status == network_status
        && existing.last_error == last_error
    {
        return Ok((existing, false));
    }
    let checkpoint = update_network_permission_checkpoint(
        &state.local_db,
        &request.network_id,
        &node_id,
        &request.agent_did,
        status,
        last_error,
        now_ms(),
    )?;
    Ok((checkpoint, true))
}

async fn push_network_permission_checkpoint(
    state: &ControlPlaneState,
    checkpoint: &wattetheria_kernel::local_db::NetworkPermissionCheckpoint,
) -> bool {
    let update = match network_permission_update(&state.local_db, checkpoint) {
        Ok(update) => update,
        Err(error) => {
            tracing::warn!(
                network_id = %checkpoint.network_id,
                agent_did = %checkpoint.agent_did,
                "build network permission callback failed: {error:#}"
            );
            schedule_network_permission_retry(state.clone(), checkpoint.revision);
            return false;
        }
    };
    match state.swarm_bridge.update_network_permission(update).await {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!(
                network_id = %checkpoint.network_id,
                agent_did = %checkpoint.agent_did,
                "network permission callback to Wattswarm failed: {error:#}"
            );
            schedule_network_permission_retry(state.clone(), checkpoint.revision);
            false
        }
    }
}

pub async fn sync_network_permission_checkpoint(
    state: &ControlPlaneState,
    checkpoint: &wattetheria_kernel::local_db::NetworkPermissionCheckpoint,
) -> bool {
    push_network_permission_checkpoint(state, checkpoint).await
}

fn schedule_network_permission_retry(state: ControlPlaneState, revision: u64) {
    let key = state.data_dir.clone();
    let retries = NETWORK_PERMISSION_RETRY_REVISIONS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut active = retries
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(pending_revision) = active.get_mut(&key) {
        *pending_revision = (*pending_revision).max(revision);
        return;
    }
    active.insert(key.clone(), revision);
    drop(active);

    tokio::spawn(async move {
        retry_network_permission_delivery_inner(
            &state,
            NETWORK_PERMISSION_RETRY_INTERVAL,
            Some(&key),
        )
        .await;
    });
}

#[cfg(test)]
pub(crate) async fn retry_network_permission_delivery(
    state: &ControlPlaneState,
    retry_interval: Duration,
) {
    retry_network_permission_delivery_inner(state, retry_interval, None).await;
}

async fn retry_network_permission_delivery_inner(
    state: &ControlPlaneState,
    retry_interval: Duration,
    retry_key: Option<&Path>,
) {
    loop {
        sleep(retry_interval).await;
        let checkpoint =
            match state
                .local_db
                .load_network_permission_checkpoint(&state.agent_did, None, None)
            {
                Ok(Some(checkpoint)) => checkpoint,
                Ok(None) => {
                    finish_network_permission_retry(retry_key, None);
                    return;
                }
                Err(error) => {
                    tracing::warn!(
                        "load network permission checkpoint for retry failed: {error:#}"
                    );
                    continue;
                }
            };
        let delivered_revision = checkpoint.revision;
        let update = match network_permission_update(&state.local_db, &checkpoint) {
            Ok(update) => update,
            Err(error) => {
                tracing::warn!("build network permission callback retry failed: {error:#}");
                continue;
            }
        };
        match state.swarm_bridge.update_network_permission(update).await {
            Ok(()) => {
                if !finish_network_permission_retry(retry_key, Some(delivered_revision)) {
                    return;
                }
            }
            Err(error) => {
                tracing::warn!("network permission callback retry failed: {error:#}");
            }
        }
    }
}

fn finish_network_permission_retry(
    retry_key: Option<&Path>,
    delivered_revision: Option<u64>,
) -> bool {
    let Some(retry_key) = retry_key else {
        return false;
    };
    let mut active = NETWORK_PERMISSION_RETRY_REVISIONS
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if delivered_revision.is_some_and(|delivered_revision| {
        active
            .get(retry_key)
            .is_some_and(|pending_revision| *pending_revision > delivered_revision)
    }) {
        return true;
    }
    active.remove(retry_key);
    false
}

/// Submit or poll the local Agent registration from Wattetheria itself.
///
/// Wattswarm only receives the resulting permission checkpoint. It never
/// submits an Agent registration to the Registry.
pub async fn run_registry_registration_once(
    state: &ControlPlaneState,
    pending_request_id: Option<&str>,
) -> anyhow::Result<RegistryRegistrationResult> {
    let request = if pending_request_id.is_none() {
        Some(build_registration_request(state).await?)
    } else {
        None
    };
    let expected_network_id = match request.as_ref() {
        Some(request) => request.network_id.clone(),
        None => state.swarm_bridge.current_network_id().await?,
    };
    let registry_urls = load_registry_urls(&state.data_dir)?;
    if registry_urls.is_empty() {
        anyhow::bail!("no Registry URL configured; provide {DISCOVERY_BOOTNODE_URLS_FILE}");
    }
    let client = reqwest::Client::builder()
        .timeout(REGISTRY_REQUEST_TIMEOUT)
        .build()
        .context("build Registry registration HTTP client")?;
    let mut last_error = None;
    for registry_base_url in registry_urls {
        let result = if let Some(request_id) = pending_request_id {
            poll_registry_registration(&client, &registry_base_url, request_id).await
        } else {
            submit_registry_registration(
                &client,
                &registry_base_url,
                request.as_ref().expect("new registration request"),
            )
            .await
        };
        let payload = match result {
            Ok(payload) => payload,
            Err(error) => {
                last_error = Some(error);
                continue;
            }
        };
        if let Err(error) =
            validate_registry_response_request(&payload, &expected_network_id, &state.agent_did)
        {
            last_error = Some(error);
            continue;
        }
        return apply_registry_registration_record(state, &payload).await;
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("Registry registration failed")))
}

async fn submit_registry_registration(
    client: &reqwest::Client,
    registry_base_url: &str,
    request: &RegistrationRequest,
) -> anyhow::Result<Value> {
    let authority = client
        .get(format!("{registry_base_url}{REGISTRY_AUTHORITY_ROUTE}"))
        .send()
        .await
        .context("fetch Registry registration mode")?
        .error_for_status()
        .context("Registry registration mode request failed")?
        .json::<Value>()
        .await
        .context("decode Registry registration mode")?;
    let mode = authority
        .get("registration_mode")
        .and_then(Value::as_str)
        .unwrap_or("manual");
    let route = match mode.to_ascii_lowercase().as_str() {
        "auto" => REGISTRY_AUTO_REGISTRATION_ROUTE,
        "manual" => REGISTRY_MANUAL_REGISTRATION_ROUTE,
        "disabled" => anyhow::bail!("Registry registration is disabled"),
        other => anyhow::bail!("Registry returned unsupported registration mode '{other}'"),
    };
    client
        .post(format!("{registry_base_url}{route}"))
        .json(request)
        .send()
        .await
        .context("submit Agent registration request to Registry")?
        .error_for_status()
        .context("Registry registration submission failed")?
        .json::<Value>()
        .await
        .context("decode Registry registration response")
}

async fn poll_registry_registration(
    client: &reqwest::Client,
    registry_base_url: &str,
    request_id: &str,
) -> anyhow::Result<Value> {
    client
        .get(format!(
            "{registry_base_url}{REGISTRY_REGISTRATION_ROUTE}/{request_id}"
        ))
        .send()
        .await
        .context("poll Registry registration")?
        .error_for_status()
        .context("Registry registration poll failed")?
        .json::<Value>()
        .await
        .context("decode Registry registration poll response")
}

fn validate_registry_response_request(
    payload: &Value,
    expected_network_id: &str,
    expected_agent_did: &str,
) -> anyhow::Result<()> {
    let response_request = decode_request(payload)?;
    if response_request.network_id != expected_network_id
        || response_request.agent_did != expected_agent_did
    {
        anyhow::bail!("Registry response does not match the local registration subject");
    }
    Ok(())
}

fn load_registry_urls(data_dir: &Path) -> anyhow::Result<Vec<String>> {
    let mut candidates = Vec::new();
    let state_dir = std::env::var(WATTSWARM_STATE_DIR_ENV).map_or_else(
        |_| PathBuf::from(WATTSWARM_STATE_DIR_DEFAULT),
        PathBuf::from,
    );
    let path = state_dir.join(DISCOVERY_BOOTNODE_URLS_FILE);
    if let Ok(bytes) = std::fs::read(&path) {
        candidates.extend(
            serde_json::from_slice::<Vec<String>>(&bytes)
                .with_context(|| format!("parse Registry URLs at {}", path.display()))?,
        );
    }
    if candidates.is_empty() {
        let local_state_dir = data_dir.join("..").join("wattswarm");
        let path = local_state_dir.join(DISCOVERY_BOOTNODE_URLS_FILE);
        if let Ok(bytes) = std::fs::read(&path) {
            candidates.extend(
                serde_json::from_slice::<Vec<String>>(&bytes)
                    .with_context(|| format!("parse Registry URLs at {}", path.display()))?,
            );
        }
    }
    let mut normalized = Vec::new();
    for candidate in candidates {
        if let Some(url) = registry_base_url(&candidate)
            && !normalized.iter().any(|existing| existing == &url)
        {
            normalized.push(url);
        }
    }
    Ok(normalized)
}

pub(crate) async fn reserve_registry_nickname(
    state: &ControlPlaneState,
    agent_did: &str,
    nickname: &str,
) -> anyhow::Result<()> {
    let network_id = state.swarm_bridge.current_network_id().await?;
    if network_id.starts_with("local:") {
        return Ok(());
    }
    let registry_urls = load_registry_urls(&state.data_dir)?;
    if registry_urls.is_empty() {
        anyhow::bail!("no Registry URL configured; cannot check display name uniqueness");
    }
    let client = reqwest::Client::builder()
        .timeout(REGISTRY_REQUEST_TIMEOUT)
        .build()
        .context("build Registry nickname client")?;
    let payload = json!({
        "network_id": network_id,
        "agent_did": agent_did,
        "nickname": nickname,
    });
    let mut last_error = None;
    for registry_base_url in registry_urls {
        let response = match client
            .post(format!("{registry_base_url}{REGISTRY_NICKNAME_ROUTE}"))
            .json(&payload)
            .send()
            .await
        {
            Ok(response) => response,
            Err(error) => {
                last_error = Some(anyhow::anyhow!(
                    "Registry nickname request failed: {error:#}"
                ));
                continue;
            }
        };
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if status.is_success() {
            return Ok(());
        }
        if status == reqwest::StatusCode::CONFLICT {
            anyhow::bail!("display name is already in use across this network");
        }
        last_error = Some(anyhow::anyhow!(
            "Registry nickname request returned HTTP {status}: {body}"
        ));
    }
    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("Registry nickname request failed")))
}

fn registry_base_url(base_url: &str) -> Option<String> {
    let base_url = base_url.trim().trim_end_matches('/');
    if base_url.is_empty() || base_url.contains("/api/network/discovery") {
        return None;
    }
    if let Some(base) = base_url.strip_suffix("/v1/nodes/discovery") {
        return Some(base.to_owned() + REGISTRY_API_ROUTE);
    }
    if let Some(base) = base_url.strip_suffix("/v1/nodes") {
        return Some(base.to_owned() + REGISTRY_API_ROUTE);
    }
    if base_url.ends_with(REGISTRY_API_ROUTE) {
        return Some(base_url.to_owned());
    }
    Some(format!("{base_url}{REGISTRY_API_ROUTE}"))
}

fn new_registration_id(kind: &str) -> String {
    format!("agent-{kind}-{}", Uuid::new_v4())
}

pub(crate) fn derived_geo(value: &str) -> (f64, f64) {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    let hash = hasher.finish();
    let lat_bucket = f64::from(u16::try_from(hash & 0xffff).unwrap_or(0)) / f64::from(u16::MAX);
    let lng_bucket =
        f64::from(u16::try_from((hash >> 16) & 0xffff).unwrap_or(0)) / f64::from(u16::MAX);
    let lat = -60.0 + lat_bucket * 120.0;
    let lng = -170.0 + lng_bucket * 340.0;
    (lat, lng)
}

pub(crate) fn derived_distance_km(value: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    25 + (hasher.finish() % 1800)
}

#[cfg(test)]
mod permission_retry_tests {
    use super::*;

    #[test]
    fn retry_finishes_only_after_latest_failed_revision_is_delivered() {
        let dir = tempfile::tempdir().expect("tempdir");
        let key = dir.path().to_path_buf();
        NETWORK_PERMISSION_RETRY_REVISIONS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(key.clone(), 2);

        assert!(finish_network_permission_retry(Some(&key), Some(1)));
        assert!(!finish_network_permission_retry(Some(&key), Some(2)));
        assert!(
            !NETWORK_PERMISSION_RETRY_REVISIONS
                .get_or_init(|| Mutex::new(HashMap::new()))
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .contains_key(&key)
        );
    }
}
