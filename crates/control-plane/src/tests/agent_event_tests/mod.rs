use super::*;

use axum::http::Request;

use base64::engine::general_purpose::STANDARD;

use std::path::Path;

use watt_did::{Did, PaymentAccountCustody, VerifiedAgentContext};

use watt_wallet::{
    InMemoryKeyStore, KeyHandle, KeyStore, PaymentAccountBindingProofOptions, PaymentAccountSigner,
    build_payment_account_binding_proof,
};

use crate::routes::agent_events::VERIFIED_AGENT_CONTEXT_PAYLOAD_KEY;

fn assert_claim_brain_actions(data_dir: &Path, event_id: &str, expected_actions: &[&str]) {
    let received = callback_received_diagnostic(data_dir, event_id);
    let actions = received["brain_input"]["allowed_actions"]
        .as_array()
        .expect("brain allowed actions")
        .iter()
        .filter_map(serde_json::Value::as_str)
        .collect::<Vec<_>>();
    assert_eq!(actions, expected_actions);
}

fn callback_received_diagnostic(data_dir: &Path, event_id: &str) -> serde_json::Value {
    let entries = crate::diagnostics::list_diagnostics(
        data_dir,
        &crate::diagnostics::DiagnosticFilter {
            event_id: Some(event_id.to_owned()),
            ..Default::default()
        },
    )
    .unwrap();
    let received = entries
        .iter()
        .find(|entry| entry.phase == "callback.received")
        .expect("callback.received diagnostic");
    received.details["payload"].clone()
}

fn signed_agent_event_envelope(
    source_identity: &Identity,
    source_node_id: &str,
    target_agent_id: Option<&str>,
    capability: &str,
    message: Value,
) -> SwarmAgentEnvelope {
    let protocol = "google_a2a".to_owned();
    let transport_profile = Some("wattswarm_mesh".to_owned());
    let source_agent_id = Some(source_identity.agent_did.clone());
    let source_node_id = Some(source_node_id.to_owned());
    let target_agent_id = target_agent_id.map(ToOwned::to_owned);
    let capability = Some(capability.to_owned());
    let message_json = serde_json::to_string(&message).expect("message serializes");
    let signed_payload = ExpectedSignedAgentEnvelopePayload {
        protocol: &protocol,
        transport_profile: transport_profile.as_ref(),
        source_agent_id: source_agent_id.as_ref(),
        target_agent_id: target_agent_id.as_ref(),
        source_node_id: source_node_id.as_ref(),
        target_node_id: None,
        capability: capability.as_ref(),
        source_agent_card_hash: None,
        message_json: &message_json,
        extensions_json: None,
    };
    let signature = sign_payload(&signed_payload, source_identity).expect("sign agent envelope");
    SwarmAgentEnvelope {
        protocol,
        transport_profile,
        source_agent_id,
        target_agent_id,
        source_node_id,
        target_node_id: None,
        capability,
        source_agent_card: None,
        message,
        extensions: None,
        signature: Some(signature),
    }
}

mod mission_claims;
mod mission_settlement;
mod mission_sync;
mod payments;
mod replies;
mod runtime;
mod social;
