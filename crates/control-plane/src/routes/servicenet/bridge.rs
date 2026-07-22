use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode, header::AUTHORIZATION};
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use uuid::Uuid;

use super::wire;
use crate::state::ControlPlaneState;
use wattetheria_kernel::agent_identity::service_agent::{
    FileServiceAgentIdentityStore, ServiceAgentIdentity, ServiceAgentIdentityStore,
};
use wattetheria_kernel::servicenet::{
    ServiceNetPublisherRegistration, find_servicenet_publisher_registration,
    service_agent_card_requires_auth,
};
use wattetheria_kernel::signing::verify_payload;
use wattetheria_kernel::swarm_bridge::{SwarmAgentEnvelope, SwarmSourceAgentCard};

const DEFAULT_SERVICENET_NETWORK_ID: &str = "mainnet:watt-etheria";
const SERVICE_RESPONSE_SIGNATURE_PROTOCOL: &str = "wattetheria.servicenet.response.v1";
const INVOCATION_MAX_CLOCK_SKEW_MS: i64 = 5 * 60 * 1000;
const INVOCATION_MAX_TTL_MS: u64 = 5 * 60 * 1000;
const INVOCATION_REPLAY_CACHE_MAX_ENTRIES: usize = 262_144;
const AGENT_ENVELOPE_HEADER: &str = "x-wattetheria-agent-envelope";

static INVOCATION_REPLAY_CACHE: OnceLock<Mutex<HashMap<String, u64>>> = OnceLock::new();

#[derive(Debug, Serialize)]
struct SignedAgentEnvelopePayload<'a> {
    protocol: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    transport_profile: Option<&'a String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_agent_id: Option<&'a String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    target_agent_id: Option<&'a String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_node_id: Option<&'a String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    target_node_id: Option<&'a String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    capability: Option<&'a String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_agent_card_hash: Option<&'a String>,
    message_json: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    extensions_json: Option<&'a String>,
}

#[derive(Debug, Serialize)]
struct SignedSourceAgentCardPayload<'a> {
    agent_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    node_id: Option<&'a String>,
    card_hash: &'a str,
    issued_at: u64,
}

#[derive(Debug, Serialize)]
struct ServiceAgentResponseSignaturePayload<'a> {
    protocol: &'a str,
    service_did: &'a str,
    agent_id: &'a str,
    verification_method: &'a str,
    request_digest: &'a str,
    request_nonce: &'a str,
    result_digest: &'a str,
    nonce: &'a str,
    issued_at_ms: u64,
}

pub(crate) async fn a2a_root(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    handle_a2a(
        state,
        None,
        authorization_header(&headers),
        encoded_agent_envelope_header(&headers),
        body,
    )
    .await
}

pub(crate) async fn a2a_agent(
    State(state): State<ControlPlaneState>,
    Path(agent_id): Path<String>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    handle_a2a(
        state,
        Some(agent_id),
        authorization_header(&headers),
        encoded_agent_envelope_header(&headers),
        body,
    )
    .await
}

async fn handle_a2a(
    state: ControlPlaneState,
    path_agent_id: Option<String>,
    authorization: Option<String>,
    encoded_agent_envelope: Option<String>,
    body: Value,
) -> Response {
    let id = body.get("id").cloned().unwrap_or(Value::Null);
    let method = body
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match method {
        "SendMessage" | "message/send" => {
            send_message(state, path_agent_id, authorization.as_deref(), id, &body).await
        }
        "GetTask" | "tasks/get" | "ListTasks" | "tasks/list" | "CancelTask" | "tasks/cancel" => {
            super::bridge_tasks::task_operation(
                state,
                path_agent_id,
                authorization.as_deref(),
                encoded_agent_envelope.as_deref(),
                id,
                method,
                &body,
            )
            .await
        }
        "SubscribeToTask" | "tasks/subscribe" => {
            super::bridge_tasks::subscribe_to_task(
                state,
                path_agent_id,
                authorization.as_deref(),
                encoded_agent_envelope.as_deref(),
                id,
                &body,
            )
            .await
        }
        _ => jsonrpc_error(&id, -32601, "unsupported A2A method"),
    }
}

async fn send_message(
    state: ControlPlaneState,
    path_agent_id: Option<String>,
    authorization: Option<&str>,
    id: Value,
    body: &Value,
) -> Response {
    let params = value_at(body, &["params"]).unwrap_or(&Value::Null);
    let message = extract_message_text(params);
    if message.trim().is_empty() {
        return jsonrpc_error(&id, -32602, "A2A message text is required");
    }
    let envelope = match wire::agent_envelope(params) {
        Ok(Some(value)) => match verified_agent_envelope(&value) {
            Ok(envelope) => envelope,
            Err(message) => return jsonrpc_error(&id, -32602, &message),
        },
        Ok(None) => return jsonrpc_error(&id, -32602, "A2A agent_envelope is required"),
        Err(message) => return jsonrpc_error(&id, -32602, &message),
    };
    let (target, invocation_security) =
        match validate_target_invocation(&state, params, path_agent_id.as_deref(), &envelope) {
            Ok(validated) => validated,
            Err(message) => return jsonrpc_error(&id, -32602, &message),
        };
    if service_agent_card_requires_auth(&target.registration.agent_card) && authorization.is_none()
    {
        return jsonrpc_error(&id, -32001, "Service Agent authorization is required");
    }
    let result = match super::execution::execute_service_agent(
        &state,
        &target.registration,
        params,
        &message,
        &envelope,
        authorization,
    )
    .await
    {
        Ok(result) => result,
        Err(error) => return jsonrpc_error(&id, -32000, &error),
    };
    let request_digest = envelope
        .extensions
        .as_ref()
        .and_then(|extensions| extensions.get("request_digest"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    if request_digest.is_empty() {
        return jsonrpc_error(
            &id,
            -32602,
            "A2A agent_envelope.extensions.request_digest is required",
        );
    }
    let service_signature = match sign_service_agent_response(
        &target.identity,
        &target.registration.agent_id,
        request_digest,
        &invocation_security.request_nonce,
        &result,
    ) {
        Ok(signature) => signature,
        Err(error) => return jsonrpc_error(&id, -32000, &error.to_string()),
    };
    if wire::uses_standard_metadata(params) {
        let mut result = result;
        if let Err(error) = attach_service_signature_metadata(&mut result, &service_signature) {
            return jsonrpc_error(&id, -32000, &error);
        }
        Json(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result,
        }))
        .into_response()
    } else {
        Json(json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result,
            "extensions": {
                "service_agent_signature": service_signature,
            },
        }))
        .into_response()
    }
}

pub(super) fn attach_service_signature_metadata(
    result: &mut Value,
    signature: &Value,
) -> Result<(), String> {
    let payload_name = ["task", "message", "statusUpdate", "artifactUpdate"]
        .into_iter()
        .find(|name| result.get(*name).is_some());
    let payload = match payload_name {
        Some(payload_name) => result
            .get_mut(payload_name)
            .and_then(Value::as_object_mut)
            .ok_or_else(|| "A2A response payload must be an object".to_owned())?,
        None => result
            .as_object_mut()
            .ok_or_else(|| "A2A response result must be an object".to_owned())?,
    };
    let metadata = payload
        .entry("metadata")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| "A2A response metadata must be an object".to_owned())?;
    let signature = serde_json::to_string(&signature)
        .map_err(|error| format!("serialize Service Agent signature metadata: {error}"))?;
    metadata.insert(
        "wattetheriaServiceAgentSignature".to_owned(),
        Value::String(signature),
    );
    Ok(())
}

fn authorization_header(headers: &HeaderMap) -> Option<String> {
    headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn encoded_agent_envelope_header(headers: &HeaderMap) -> Option<String> {
    headers
        .get(AGENT_ENVELOPE_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

pub(super) fn sign_service_agent_response(
    identity: &ServiceAgentIdentity,
    agent_id: &str,
    request_digest: &str,
    request_nonce: &str,
    result: &Value,
) -> anyhow::Result<Value> {
    let verification_method = identity.verification_method();
    let result_digest = format!("sha256:{:x}", Sha256::digest(serde_jcs::to_vec(result)?));
    let nonce = Uuid::new_v4().to_string();
    let issued_at_ms = chrono::Utc::now().timestamp_millis().max(0).cast_unsigned();
    let payload = ServiceAgentResponseSignaturePayload {
        protocol: SERVICE_RESPONSE_SIGNATURE_PROTOCOL,
        service_did: &identity.service_did,
        agent_id,
        verification_method: &verification_method,
        request_digest,
        request_nonce,
        result_digest: &result_digest,
        nonce: &nonce,
        issued_at_ms,
    };
    let signature = identity.sign(&serde_jcs::to_vec(&payload)?)?;
    Ok(json!({
        "protocol": SERVICE_RESPONSE_SIGNATURE_PROTOCOL,
        "service_did": identity.service_did,
        "agent_id": agent_id,
        "verification_method": verification_method,
        "request_digest": request_digest,
        "request_nonce": request_nonce,
        "result_digest": result_digest,
        "nonce": nonce,
        "issued_at_ms": issued_at_ms,
        "signature": signature,
    }))
}

pub(super) struct InvocationSecurity {
    pub(super) request_nonce: String,
    pub(super) request_digest: String,
}

pub(super) struct ValidatedInvocationTarget {
    pub(super) registration: ServiceNetPublisherRegistration,
    pub(super) identity: ServiceAgentIdentity,
}

fn validate_target_invocation(
    state: &ControlPlaneState,
    params: &Value,
    path_agent_id: Option<&str>,
    envelope: &SwarmAgentEnvelope,
) -> Result<(ValidatedInvocationTarget, InvocationSecurity), String> {
    let published_agent_id = path_agent_id
        .or(envelope.target_agent_id.as_deref())
        .ok_or_else(|| "target Service Agent id is required".to_owned())?;
    if envelope.target_agent_id.as_deref() != Some(published_agent_id) {
        return Err(
            "A2A target agent does not match signed agent_envelope.target_agent_id".to_owned(),
        );
    }
    let target = validate_local_service_agent(state, published_agent_id)?;
    let security = validate_invocation_security(state, params, envelope, published_agent_id)?;
    Ok((target, security))
}

pub(super) fn validate_local_service_agent(
    state: &ControlPlaneState,
    published_agent_id: &str,
) -> Result<ValidatedInvocationTarget, String> {
    let registration = find_servicenet_publisher_registration(&state.data_dir, published_agent_id)
        .map_err(|error| format!("load Service Agent publisher state failed: {error}"))?;
    let registration = registration.ok_or_else(|| {
        format!("Service Agent `{published_agent_id}` is not published by this Wattetheria Adapter")
    })?;
    let identity = FileServiceAgentIdentityStore::new(&state.data_dir)
        .load(published_agent_id)
        .map_err(|error| {
            format!(
                "load Service Agent identity failed; publish from this node data directory first: {error}"
            )
        })?;
    if identity.service_did != registration.service_did {
        return Err(format!(
            "local Service Agent identity does not match the published DID for `{published_agent_id}`"
        ));
    }
    Ok(ValidatedInvocationTarget {
        registration,
        identity,
    })
}

fn validate_invocation_security(
    state: &ControlPlaneState,
    params: &Value,
    envelope: &SwarmAgentEnvelope,
    published_agent_id: &str,
) -> Result<InvocationSecurity, String> {
    validate_a2a_params_match_signed_message(params, envelope, published_agent_id)?;
    validate_signed_envelope_security(state, envelope, published_agent_id)
}

pub(super) fn validate_signed_envelope_security(
    state: &ControlPlaneState,
    envelope: &SwarmAgentEnvelope,
    published_agent_id: &str,
) -> Result<InvocationSecurity, String> {
    let extensions = envelope
        .extensions
        .as_ref()
        .and_then(Value::as_object)
        .ok_or_else(|| "A2A agent_envelope.extensions is required".to_owned())?;
    let request_nonce = required_string(extensions.get("nonce"), "nonce")?;
    let request_digest = required_string(extensions.get("request_digest"), "request_digest")?;
    let issued_at_ms = extensions
        .get("issued_at_ms")
        .and_then(Value::as_u64)
        .ok_or_else(|| "A2A agent_envelope.extensions.issued_at_ms is required".to_owned())?;
    let expires_at_ms = extensions
        .get("expires_at_ms")
        .and_then(Value::as_u64)
        .ok_or_else(|| "A2A agent_envelope.extensions.expires_at_ms is required".to_owned())?;
    let computed_digest = format!(
        "sha256:{:x}",
        Sha256::digest(
            serde_jcs::to_vec(&envelope.message)
                .map_err(|error| format!("canonicalize signed invocation message: {error}"))?
        )
    );
    if request_digest != computed_digest {
        return Err(
            "A2A agent_envelope.extensions.request_digest does not match the signed message"
                .to_owned(),
        );
    }

    let now_ms = chrono::Utc::now().timestamp_millis();
    let issued_at = i64::try_from(issued_at_ms)
        .map_err(|_| "A2A agent_envelope issued_at_ms is invalid".to_owned())?;
    let expires_at = i64::try_from(expires_at_ms)
        .map_err(|_| "A2A agent_envelope expires_at_ms is invalid".to_owned())?;
    if issued_at - now_ms > INVOCATION_MAX_CLOCK_SKEW_MS {
        return Err("A2A agent_envelope issued_at_ms is too far in the future".to_owned());
    }
    if expires_at <= issued_at {
        return Err(
            "A2A agent_envelope expires_at_ms must be greater than issued_at_ms".to_owned(),
        );
    }
    if expires_at_ms.saturating_sub(issued_at_ms) > INVOCATION_MAX_TTL_MS {
        return Err("A2A agent_envelope validity window exceeds five minutes".to_owned());
    }
    if expires_at + INVOCATION_MAX_CLOCK_SKEW_MS < now_ms {
        return Err("A2A agent_envelope has expired".to_owned());
    }

    let source_agent_id = envelope
        .source_agent_id
        .as_deref()
        .expect("verified envelope should have source_agent_id");
    let cache_key = format!(
        "{}:{published_agent_id}:{source_agent_id}:{request_nonce}",
        state.data_dir.display()
    );
    let cache = INVOCATION_REPLAY_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut cache = cache
        .lock()
        .map_err(|_| "A2A invocation replay cache lock poisoned".to_owned())?;
    let cutoff = now_ms.max(0).cast_unsigned();
    cache.retain(|_, expires_at| *expires_at >= cutoff);
    if cache.contains_key(&cache_key) {
        return Err("A2A agent_envelope nonce has already been used; refusing replay".to_owned());
    }
    if cache.len() >= INVOCATION_REPLAY_CACHE_MAX_ENTRIES {
        return Err(
            "A2A invocation replay cache is at capacity; retry through another node instance"
                .to_owned(),
        );
    }
    cache.insert(
        cache_key,
        expires_at_ms.saturating_add(INVOCATION_MAX_CLOCK_SKEW_MS as u64),
    );
    Ok(InvocationSecurity {
        request_nonce,
        request_digest,
    })
}

fn validate_a2a_params_match_signed_message(
    params: &Value,
    envelope: &SwarmAgentEnvelope,
    published_agent_id: &str,
) -> Result<(), String> {
    let actual = json!({
        "taskId": wire::message_field(params, "taskId").cloned().unwrap_or(Value::Null),
        "contextId": wire::message_field(params, "contextId").cloned().unwrap_or(Value::Null),
        "skillId": wire::skill_id(params).cloned().unwrap_or(Value::Null),
        "returnImmediately": params
            .pointer("/configuration/returnImmediately")
            .cloned()
            .unwrap_or(Value::Null),
        "message": normalized_request_message(params.get("message")),
        "settlement": wire::settlement(params).cloned().unwrap_or(Value::Null),
    });
    let mut expected = expected_a2a_core_params(&envelope.message);
    if expected["contextId"].is_null()
        && let Some(source_agent_id) = envelope.source_agent_id.as_deref()
    {
        let derived = Value::String(format!(
            "wattetheria:servicenet:{source_agent_id}:{published_agent_id}:{DEFAULT_SERVICENET_NETWORK_ID}"
        ));
        if actual["contextId"] == derived {
            expected["contextId"] = derived;
        }
    }
    if expected == actual {
        return Ok(());
    }
    Err("A2A request params do not match the signed agent_envelope.message".to_owned())
}

fn normalized_request_message(message: Option<&Value>) -> Value {
    let Some(message) = message else {
        return Value::Null;
    };
    let role = message
        .get("role")
        .and_then(Value::as_str)
        .map(|role| match role {
            "ROLE_USER" | "user" => "user",
            "ROLE_AGENT" | "agent" => "agent",
            other => other,
        })
        .unwrap_or_default();
    let parts = message
        .get("parts")
        .and_then(Value::as_array)
        .map(|parts| {
            parts
                .iter()
                .map(|part| {
                    if let Some(text) = part.get("text") {
                        json!({"kind": "text", "text": text})
                    } else if let Some(data) = part.get("data") {
                        json!({"kind": "data", "data": data})
                    } else {
                        part.clone()
                    }
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    json!({"role": role, "parts": parts})
}

fn expected_a2a_core_params(signed_message: &Value) -> Value {
    let input = signed_message.get("input").cloned().unwrap_or(Value::Null);
    let message = signed_message
        .get("message")
        .and_then(Value::as_str)
        .and_then(non_empty_text)
        .map(ToOwned::to_owned)
        .or_else(|| message_text_from_value(&input));
    let mut parts = Vec::new();
    if let Some(message) = message {
        parts.push(json!({"kind": "text", "text": message}));
    }
    if !input.is_null() {
        parts.push(json!({"kind": "data", "data": input}));
    }
    if parts.is_empty() {
        parts.push(json!({"kind": "data", "data": Value::Null}));
    }
    json!({
        "taskId": signed_message.get("task_id").cloned().unwrap_or(Value::Null),
        "contextId": signed_message.get("context_id").cloned().unwrap_or(Value::Null),
        "skillId": signed_message.get("skill_id").cloned().unwrap_or(Value::Null),
        "returnImmediately": signed_message
            .get("return_immediately")
            .cloned()
            .unwrap_or(Value::Null),
        "message": {"role": "user", "parts": parts},
        "settlement": normalized_signed_settlement(signed_message),
    })
}

fn normalized_signed_settlement(signed_message: &Value) -> Value {
    let Some(mut settlement) = signed_message.get("settlement").cloned() else {
        return Value::Null;
    };
    if settlement.is_null() {
        return Value::Null;
    }
    if settlement.get("layer").is_none() {
        settlement["layer"] = Value::String("web3".to_owned());
    }
    if let Some(rail) = settlement
        .get("rail")
        .and_then(Value::as_str)
        .map(|rail| rail.trim().to_ascii_lowercase())
    {
        settlement["rail"] = Value::String(rail);
    }
    if settlement.get("rail").and_then(Value::as_str) == Some("x402") {
        let Some(settlement) = settlement.as_object_mut() else {
            return settlement;
        };
        match settlement.get_mut("request") {
            Some(Value::Object(request)) => {
                request
                    .entry("protocol")
                    .or_insert_with(|| Value::String("x402".to_owned()));
            }
            Some(request @ Value::Null) => {
                *request = json!({"protocol": "x402"});
            }
            None => {
                settlement.insert("request".to_owned(), json!({"protocol": "x402"}));
            }
            Some(_) => {}
        }
    }
    settlement
}

fn message_text_from_value(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => non_empty_text(text).map(ToOwned::to_owned),
        Value::Object(object) => {
            ["message", "text", "query", "prompt"]
                .into_iter()
                .find_map(|key| {
                    object
                        .get(key)
                        .and_then(Value::as_str)
                        .and_then(non_empty_text)
                        .map(ToOwned::to_owned)
                })
        }
        _ => None,
    }
}

fn non_empty_text(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()).then_some(value)
}

fn required_string(value: Option<&Value>, name: &str) -> Result<String, String> {
    value
        .and_then(Value::as_str)
        .and_then(non_empty_text)
        .map(ToOwned::to_owned)
        .ok_or_else(|| format!("A2A agent_envelope.extensions.{name} is required"))
}

pub(super) fn verified_agent_envelope(value: &Value) -> Result<SwarmAgentEnvelope, String> {
    let envelope: SwarmAgentEnvelope = serde_json::from_value(value.clone())
        .map_err(|error| format!("invalid A2A agent_envelope: {error}"))?;
    let signature = envelope
        .signature
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "A2A agent_envelope.signature is required".to_owned())?;
    let source_agent_id = envelope
        .source_agent_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "A2A agent_envelope.source_agent_id is required".to_owned())?;
    envelope
        .source_node_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "A2A agent_envelope.source_node_id is required".to_owned())?;
    if let Some(card) = envelope.source_agent_card.as_ref() {
        verify_source_agent_card(card, source_agent_id)?;
    }
    let message_json = serde_json::to_string(&envelope.message)
        .map_err(|error| format!("invalid A2A agent_envelope.message: {error}"))?;
    let extensions_json = envelope
        .extensions
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(|error| format!("invalid A2A agent_envelope.extensions: {error}"))?;
    let payload = SignedAgentEnvelopePayload {
        protocol: &envelope.protocol,
        transport_profile: envelope.transport_profile.as_ref(),
        source_agent_id: envelope.source_agent_id.as_ref(),
        target_agent_id: envelope.target_agent_id.as_ref(),
        source_node_id: envelope.source_node_id.as_ref(),
        target_node_id: envelope.target_node_id.as_ref(),
        capability: envelope.capability.as_ref(),
        source_agent_card_hash: envelope
            .source_agent_card
            .as_ref()
            .map(|card| &card.card_hash),
        message_json: &message_json,
        extensions_json: extensions_json.as_ref(),
    };
    let verified = verify_payload(&payload, signature, source_agent_id)
        .map_err(|error| format!("A2A agent_envelope signature verification failed: {error}"))?;
    if !verified {
        return Err("A2A agent_envelope signature verification failed".to_owned());
    }
    Ok(envelope)
}

fn verify_source_agent_card(
    card: &SwarmSourceAgentCard,
    source_agent_id: &str,
) -> Result<(), String> {
    if card.agent_id != source_agent_id {
        return Err("A2A source_agent_card.agent_id must match source_agent_id".to_owned());
    }
    let computed_hash = format!(
        "sha256:{}",
        hex::encode(Sha256::digest(
            serde_jcs::to_string(&card.card)
                .map_err(|error| format!("canonicalize A2A source_agent_card failed: {error}"))?
                .as_bytes()
        ))
    );
    if card.card_hash != computed_hash {
        return Err("A2A source_agent_card.card_hash does not match card".to_owned());
    }
    let signature = card
        .signature
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "A2A source_agent_card.signature is required".to_owned())?;
    let payload = SignedSourceAgentCardPayload {
        agent_id: &card.agent_id,
        node_id: card.node_id.as_ref(),
        card_hash: &card.card_hash,
        issued_at: card.issued_at,
    };
    let verified = verify_payload(&payload, signature, source_agent_id)
        .map_err(|error| format!("A2A source_agent_card signature verification failed: {error}"))?;
    if !verified {
        return Err("A2A source_agent_card signature verification failed".to_owned());
    }
    Ok(())
}

fn extract_message_text(params: &Value) -> String {
    let Some(parts) = value_at(params, &["message", "parts"]).and_then(Value::as_array) else {
        return string_at(params, &["message", "text"]).unwrap_or_default();
    };
    parts
        .iter()
        .filter_map(|part| string_at(part, &["text"]))
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn jsonrpc_error(id: &Value, code: i64, message: &str) -> Response {
    (StatusCode::OK, Json(jsonrpc_error_value(id, code, message))).into_response()
}

pub(super) fn jsonrpc_error_value(id: &Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message,
        }
    })
}

fn string_at(value: &Value, path: &[&str]) -> Option<String> {
    value_at(value, path)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

pub(super) fn value_at<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut current = value;
    for segment in path {
        current = current.get(segment)?;
    }
    Some(current)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a2a_params_must_match_signed_invocation_message() {
        let signed_message = json!({
            "task_id": "task-1",
            "context_id": "context-1",
            "message": "book a ride",
            "input": {"pickup": "airport"},
            "skill_id": "rides.book",
        });
        let params = json!({
            "taskId": "task-1",
            "contextId": "context-1",
            "skillId": "rides.book",
            "message": {
                "role": "user",
                "parts": [
                    {"kind": "text", "text": "book a ride"},
                    {"kind": "data", "data": {"pickup": "airport"}},
                ]
            }
        });
        let envelope = SwarmAgentEnvelope {
            protocol: "a2a_v1".to_owned(),
            transport_profile: None,
            source_agent_id: Some("did:key:caller".to_owned()),
            target_agent_id: Some("ride-agent".to_owned()),
            source_node_id: Some("caller-node".to_owned()),
            target_node_id: None,
            capability: None,
            source_agent_card: None,
            message: signed_message,
            extensions: None,
            signature: None,
        };
        assert!(validate_a2a_params_match_signed_message(&params, &envelope, "ride-agent").is_ok());

        let sdk_params = json!({
            "message": {
                "messageId": "message-1",
                "taskId": "task-1",
                "contextId": "context-1",
                "role": "ROLE_USER",
                "parts": [
                    {"text": "book a ride"},
                    {"data": {"pickup": "airport"}},
                ]
            },
            "metadata": {"skillId": "rides.book"}
        });
        assert!(
            validate_a2a_params_match_signed_message(&sdk_params, &envelope, "ride-agent").is_ok()
        );

        let mut tampered = params;
        tampered["message"]["parts"][0]["text"] = json!("send money instead");
        assert!(
            validate_a2a_params_match_signed_message(&tampered, &envelope, "ride-agent").is_err()
        );
    }

    #[test]
    fn a2a_settlement_must_match_signed_invocation() {
        let signed_message = json!({
            "message": "pay",
            "input": null,
            "settlement": {
                "layer": "web3",
                "rail": "x402",
                "request": {"payment": "signed"}
            }
        });
        let envelope = SwarmAgentEnvelope {
            protocol: "a2a_v1".to_owned(),
            transport_profile: None,
            source_agent_id: Some("did:key:caller".to_owned()),
            target_agent_id: Some("pay-agent".to_owned()),
            source_node_id: Some("caller-node".to_owned()),
            target_node_id: None,
            capability: None,
            source_agent_card: None,
            message: signed_message,
            extensions: None,
            signature: None,
        };
        let params = json!({
            "message": {
                "role": "user",
                "parts": [{"kind": "text", "text": "pay"}]
            },
            "extensions": {
                "settlement": {
                    "layer": "web3",
                    "rail": "x402",
                    "request": {"protocol": "x402", "payment": "tampered"}
                }
            }
        });

        assert!(validate_a2a_params_match_signed_message(&params, &envelope, "pay-agent").is_err());
    }

    #[test]
    fn adapter_requires_authorization_when_agent_card_declares_security() {
        assert!(service_agent_card_requires_auth(&json!({
            "securitySchemes": {"oauth2": {"type": "oauth2"}},
            "security": [{"oauth2": []}]
        })));
        assert!(!service_agent_card_requires_auth(&json!({
            "securitySchemes": {"none": {"type": "none"}},
            "security": [{"none": []}]
        })));
    }
}
