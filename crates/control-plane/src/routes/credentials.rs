use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::auth::{authorize, internal_error};
use crate::state::ControlPlaneState;
use wattetheria_kernel::audit::AuditEntry;
use wattetheria_kernel::credentials::{
    CredentialBinding, CredentialEnvelope, CredentialFormat, CredentialRecord, CredentialState,
    CredentialVerification, FileCredentialStore, ProofOutcome, TrustAnchor, TrustOutcome,
};

const DEFAULT_CREDENTIAL_FORMAT: &str = "w3c_vc_json";

#[derive(Debug, Deserialize)]
pub(crate) struct ImportCredentialBody {
    #[serde(default = "default_credential_format")]
    format: String,
    #[serde(default)]
    media_type: Option<String>,
    payload: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ReplaceTrustAnchorsBody {
    anchors: Vec<TrustAnchor>,
}

#[derive(Debug, Serialize)]
struct CredentialView {
    credential_id: String,
    format: CredentialFormat,
    media_type: Option<String>,
    sha256: String,
    imported_at: DateTime<Utc>,
    verification_status: &'static str,
    proof_outcome: Option<ProofOutcome>,
    credential_state: Option<CredentialState>,
    trust_outcome: Option<TrustOutcome>,
    issuer: Option<String>,
    credential_types: Vec<String>,
    subject_ids: Vec<String>,
}

impl From<CredentialRecord> for CredentialView {
    fn from(record: CredentialRecord) -> Self {
        let sha256 = record.sha256();
        let verification_status = record.verification_status();
        let (issuer, credential_types, subject_ids, proof_outcome, credential_state, trust_outcome) =
            match &record.verification {
                CredentialVerification::Pending { .. } => (None, vec![], vec![], None, None, None),
                CredentialVerification::Verified { context } => (
                    Some(context.credential.issuer.id.clone()),
                    context.credential.types.clone(),
                    context
                        .credential
                        .credential_subject
                        .iter()
                        .filter_map(|subject| subject.id.clone())
                        .collect(),
                    Some(context.evidence.proof.outcome),
                    Some(context.status.state),
                    Some(context.trust.outcome),
                ),
            };
        Self {
            credential_id: record.credential_id,
            format: record.envelope.format,
            media_type: record.envelope.media_type,
            sha256,
            imported_at: record.imported_at,
            verification_status,
            proof_outcome,
            credential_state,
            trust_outcome,
            issuer,
            credential_types,
            subject_ids,
        }
    }
}

pub(crate) async fn list_trust_anchors(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, &headers).await {
        return response;
    }
    match FileCredentialStore::new(&state.data_dir).load_trust_anchors() {
        Ok(anchors) => Json(json!({ "anchors": anchors })).into_response(),
        Err(error) => internal_error(&error),
    }
}

pub(crate) async fn replace_trust_anchors(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<ReplaceTrustAnchorsBody>,
) -> Response {
    let actor = match authorize(&state, &headers).await {
        Ok(actor) => actor,
        Err(response) => return response,
    };
    let store = FileCredentialStore::new(&state.data_dir);
    if let Err(error) = store.replace_trust_anchors(body.anchors) {
        return bad_request(format!("{error:#}"));
    }
    append_credential_audit(
        &state,
        actor,
        "credentials.trust_anchors.replace",
        "credential-trust-anchors",
    );
    StatusCode::NO_CONTENT.into_response()
}

pub(crate) fn list_credentials(
    state: &ControlPlaneState,
    binding: &CredentialBinding,
    owner_did_field: &'static str,
) -> Response {
    match FileCredentialStore::new(&state.data_dir).list(binding) {
        Ok(records) => Json(json!({
            (owner_did_field): binding.owner_did(),
            "items": records.into_iter().map(CredentialView::from).collect::<Vec<_>>(),
        }))
        .into_response(),
        Err(error) => internal_error(&error),
    }
}

pub(crate) fn import_credential(
    state: &ControlPlaneState,
    actor: String,
    binding: CredentialBinding,
    body: ImportCredentialBody,
    audit_action: &'static str,
) -> Response {
    let format = body.format.trim();
    if format.is_empty() || format.len() > 80 {
        return bad_request("format must contain 1 to 80 characters");
    }
    let media_type = body
        .media_type
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    let envelope = CredentialEnvelope {
        format: CredentialFormat::new(format),
        media_type,
        payload: body.payload.into_bytes(),
    };
    let record = match FileCredentialStore::new(&state.data_dir).import_pending(binding, envelope) {
        Ok(record) => record,
        Err(error) => return bad_request(error.to_string()),
    };
    append_credential_audit(state, actor, audit_action, &record.credential_id);
    (StatusCode::CREATED, Json(CredentialView::from(record))).into_response()
}

pub(crate) fn delete_credential(
    state: &ControlPlaneState,
    actor: String,
    binding: &CredentialBinding,
    credential_id: &str,
    audit_action: &'static str,
    not_found_message: &'static str,
) -> Response {
    match FileCredentialStore::new(&state.data_dir).delete(binding, credential_id) {
        Ok(true) => {
            append_credential_audit(state, actor, audit_action, credential_id);
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => not_found(not_found_message),
        Err(error) => bad_request(error.to_string()),
    }
}

fn default_credential_format() -> String {
    DEFAULT_CREDENTIAL_FORMAT.to_owned()
}

fn bad_request(message: impl Into<String>) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "error": message.into() })),
    )
        .into_response()
}

pub(crate) fn not_found(message: impl Into<String>) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(json!({ "error": message.into() })),
    )
        .into_response()
}

fn append_credential_audit(state: &ControlPlaneState, actor: String, action: &str, subject: &str) {
    let _ = state.audit_log.append(AuditEntry {
        id: String::new(),
        timestamp: Utc::now().timestamp(),
        category: "identity".to_owned(),
        action: action.to_owned(),
        status: "ok".to_owned(),
        actor: Some(actor),
        subject: Some(subject.to_owned()),
        capability: None,
        reason: None,
        duration_ms: None,
        details: None,
    });
}
