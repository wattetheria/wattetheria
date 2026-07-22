use axum::Json;
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use futures_util::StreamExt;
use serde_json::{Value, json};
use std::convert::Infallible;

use super::bridge::{
    InvocationSecurity, ValidatedInvocationTarget, attach_service_signature_metadata,
    jsonrpc_error, jsonrpc_error_value, sign_service_agent_response, validate_local_service_agent,
    validate_signed_envelope_security, value_at, verified_agent_envelope,
};
use crate::state::ControlPlaneState;
use wattetheria_kernel::servicenet::service_agent_card_requires_auth;

const AGENT_ENVELOPE_HEADER: &str = "x-wattetheria-agent-envelope";

pub(super) async fn task_operation(
    state: ControlPlaneState,
    path_agent_id: Option<String>,
    authorization: Option<&str>,
    encoded_agent_envelope: Option<&str>,
    id: Value,
    method: &str,
    body: &Value,
) -> Response {
    let params = value_at(body, &["params"]).unwrap_or(&Value::Null);
    let (target, invocation_security) = match validate_task_invocation(
        &state,
        params,
        path_agent_id.as_deref(),
        encoded_agent_envelope,
        method,
    ) {
        Ok(validated) => validated,
        Err(message) => return jsonrpc_error(&id, -32602, &message),
    };
    if service_agent_card_requires_auth(&target.registration.agent_card) && authorization.is_none()
    {
        return jsonrpc_error(&id, -32001, "Service Agent authorization is required");
    }
    let result = match method {
        "GetTask" | "tasks/get" => {
            super::execution::get_customized_agent_task(&target.registration, params, authorization)
                .await
        }
        "ListTasks" | "tasks/list" => {
            super::execution::list_customized_agent_tasks(
                &target.registration,
                params,
                authorization,
            )
            .await
        }
        "CancelTask" | "tasks/cancel" => {
            super::execution::cancel_customized_agent_task(
                &target.registration,
                params,
                authorization,
            )
            .await
        }
        _ => Err("unsupported A2A Task operation".to_owned()),
    };
    let mut result = match result {
        Ok(result) => result,
        Err(error) => return jsonrpc_error(&id, -32000, &error),
    };
    let signature = match sign_service_agent_response(
        &target.identity,
        &target.registration.agent_id,
        &invocation_security.request_digest,
        &invocation_security.request_nonce,
        &result,
    ) {
        Ok(signature) => signature,
        Err(error) => return jsonrpc_error(&id, -32000, &error.to_string()),
    };
    if let Err(error) = attach_service_signature_metadata(&mut result, &signature) {
        return jsonrpc_error(&id, -32000, &error);
    }
    Json(json!({"jsonrpc": "2.0", "id": id, "result": result})).into_response()
}

pub(super) async fn subscribe_to_task(
    state: ControlPlaneState,
    path_agent_id: Option<String>,
    authorization: Option<&str>,
    encoded_agent_envelope: Option<&str>,
    id: Value,
    body: &Value,
) -> Response {
    let params = value_at(body, &["params"]).unwrap_or(&Value::Null);
    let (target, invocation_security) = match validate_task_invocation(
        &state,
        params,
        path_agent_id.as_deref(),
        encoded_agent_envelope,
        "SubscribeToTask",
    ) {
        Ok(validated) => validated,
        Err(message) => return jsonrpc_error(&id, -32602, &message),
    };
    if service_agent_card_requires_auth(&target.registration.agent_card) && authorization.is_none()
    {
        return jsonrpc_error(&id, -32001, "Service Agent authorization is required");
    }
    let stream = match super::execution::subscribe_customized_agent_task(
        &target.registration,
        params,
        authorization,
    )
    .await
    {
        Ok(stream) => stream,
        Err(error) => return jsonrpc_error(&id, -32000, &error),
    };
    let identity = target.identity;
    let agent_id = target.registration.agent_id;
    let request_digest = invocation_security.request_digest;
    let request_nonce = invocation_security.request_nonce;
    let event_id = id.clone();
    let events = stream.map(move |event| {
        let payload = match event {
            Ok(mut result) => match sign_service_agent_response(
                &identity,
                &agent_id,
                &request_digest,
                &request_nonce,
                &result,
            ) {
                Ok(signature) => match attach_service_signature_metadata(&mut result, &signature) {
                    Ok(()) => json!({"jsonrpc": "2.0", "id": event_id, "result": result}),
                    Err(error) => jsonrpc_error_value(&event_id, -32000, &error),
                },
                Err(error) => jsonrpc_error_value(&event_id, -32000, &error.to_string()),
            },
            Err(error) => jsonrpc_error_value(&event_id, -32000, &error),
        };
        Ok::<Event, Infallible>(
            Event::default()
                .json_data(payload)
                .expect("JSON-RPC subscription payload must serialize"),
        )
    });
    Sse::new(events).into_response()
}

fn validate_task_invocation(
    state: &ControlPlaneState,
    params: &Value,
    path_agent_id: Option<&str>,
    encoded_agent_envelope: Option<&str>,
    method: &str,
) -> Result<(ValidatedInvocationTarget, InvocationSecurity), String> {
    let envelope = decode_agent_envelope_header(encoded_agent_envelope)?;
    let envelope = verified_agent_envelope(&envelope)?;
    let published_agent_id = path_agent_id
        .or(envelope.target_agent_id.as_deref())
        .ok_or_else(|| "target Service Agent id is required".to_owned())?;
    if envelope.target_agent_id.as_deref() != Some(published_agent_id) {
        return Err(
            "A2A target agent does not match signed agent_envelope.target_agent_id".to_owned(),
        );
    }
    let target = validate_local_service_agent(state, published_agent_id)?;
    if !matches!(
        target.registration.execution,
        wattetheria_kernel::servicenet::ServiceAgentExecution::CustomizedAgent { .. }
    ) {
        return Err(
            "A2A Task operations require Customized Agent execution; Wattetheria Runtime uses the internal invocation flow"
                .to_owned(),
        );
    }
    let expected_message = task_operation_envelope_message(method, params)?;
    if envelope.message != expected_message {
        return Err("A2A Task request does not match agent_envelope.message".to_owned());
    }
    let security = validate_signed_envelope_security(state, &envelope, published_agent_id)?;
    Ok((target, security))
}

fn task_operation_envelope_message(method: &str, params: &Value) -> Result<Value, String> {
    let task_id = || {
        params
            .get("id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .ok_or_else(|| format!("A2A {method} id is required"))
    };
    match method {
        "GetTask" | "tasks/get" => Ok(json!({
            "operation": "GetTask",
            "task_id": task_id()?,
            "history_length": params.get("historyLength").cloned().unwrap_or(Value::Null),
        })),
        "ListTasks" | "tasks/list" => Ok(json!({
            "operation": "ListTasks",
            "context_id": params.get("contextId").cloned().unwrap_or(Value::Null),
            "status": params.get("status").cloned().unwrap_or(Value::Null),
            "page_size": params.get("pageSize").cloned().unwrap_or(Value::Null),
            "page_token": params.get("pageToken").cloned().unwrap_or(Value::Null),
            "history_length": params.get("historyLength").cloned().unwrap_or(Value::Null),
            "status_timestamp_after": params
                .get("statusTimestampAfter")
                .cloned()
                .unwrap_or(Value::Null),
            "include_artifacts": params
                .get("includeArtifacts")
                .cloned()
                .unwrap_or(Value::Null),
        })),
        "CancelTask" | "tasks/cancel" => Ok(json!({
            "operation": "CancelTask",
            "task_id": task_id()?,
        })),
        "SubscribeToTask" | "tasks/subscribe" => Ok(json!({
            "operation": "SubscribeToTask",
            "task_id": task_id()?,
        })),
        _ => Err("unsupported A2A Task operation".to_owned()),
    }
}

fn decode_agent_envelope_header(encoded: Option<&str>) -> Result<Value, String> {
    let encoded = encoded
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| format!("{AGENT_ENVELOPE_HEADER} header is required"))?;
    let bytes = STANDARD
        .decode(encoded)
        .map_err(|error| format!("decode {AGENT_ENVELOPE_HEADER}: {error}"))?;
    serde_json::from_slice(&bytes)
        .map_err(|error| format!("parse {AGENT_ENVELOPE_HEADER}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_tasks_uses_the_same_canonical_message_as_the_caller() {
        let message = task_operation_envelope_message(
            "ListTasks",
            &json!({
                "contextId": "ctx-1",
                "status": "TASK_STATE_WORKING",
                "pageSize": 25,
                "pageToken": "next",
                "historyLength": 3,
                "statusTimestampAfter": "2026-07-22T00:00:00Z",
                "includeArtifacts": true,
            }),
        )
        .expect("ListTasks message should canonicalize");

        assert_eq!(message["operation"], "ListTasks");
        assert_eq!(message["context_id"], "ctx-1");
        assert_eq!(message["page_size"], 25);
        assert_eq!(message["include_artifacts"], true);
    }

    #[test]
    fn agent_envelope_header_round_trips_without_json_number_coercion() {
        let envelope = json!({
            "source_agent_id": "did:key:zCaller",
            "extensions": {"issued_at_ms": 9_007_199_254_740_993_u64},
        });
        let encoded = STANDARD
            .encode(serde_json::to_vec(&envelope).expect("agent envelope should serialize"));

        assert_eq!(
            decode_agent_envelope_header(Some(&encoded))
                .expect("encoded agent envelope should parse"),
            envelope
        );
    }
}
