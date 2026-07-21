use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::Path;
use uuid::Uuid;

use crate::signing::PayloadSigner;

use super::{
    SERVICENET_A2A_V1_PROTOCOL, ServiceAgentExecution, ServiceNetClient, ServiceNetClientError,
    ServiceNetConnectionMode, ServiceNetPublisherRegistration,
    rollback_servicenet_publisher_registration, stage_servicenet_publisher_registration,
};

pub struct ServiceAgentPublicationInput<'a> {
    pub provider_id: &'a str,
    pub agent_id: &'a str,
    pub service_did: &'a str,
    pub service_address: Option<&'a str>,
    pub version: &'a str,
    pub risk_level: &'a str,
    pub agent_card: Value,
    pub connection_mode: ServiceNetConnectionMode,
    pub execution: ServiceAgentExecution,
    pub provider_attester_did: &'a str,
    pub ttl_minutes: u64,
}

pub struct PreparedServiceAgentPublication {
    pub request: Value,
    pub registration: ServiceNetPublisherRegistration,
}

pub fn prepare_service_agent_publication(
    input: ServiceAgentPublicationInput<'_>,
    signer: &(impl PayloadSigner + ?Sized),
) -> Result<PreparedServiceAgentPublication> {
    if matches!(&input.execution, ServiceAgentExecution::WattetheriaRuntime)
        && service_agent_card_requires_auth(&input.agent_card)
    {
        bail!(
            "Wattetheria Runtime Service Agents must use public `none` security until the local Runtime has an authentication verifier"
        );
    }
    let endpoint = input
        .agent_card
        .get("url")
        .and_then(Value::as_str)
        .context("Service Agent Card is missing Adapter URL")?;
    let deployment = json!({
        "runtime": "wattetheria_adapter",
        "connection_mode": input.connection_mode,
        "endpoint": {
            "url": endpoint,
            "protocol_binding": "JSONRPC",
            "protocol_version": "1.0",
            "interaction_protocol": SERVICENET_A2A_V1_PROTOCOL,
        },
    });
    let review = json!({
        "risk_level": input.risk_level,
        "human_approval_required": false,
    });
    let artifacts = json!({});
    let issued_at_ms = chrono::Utc::now().timestamp_millis().max(0).cast_unsigned();
    let expires_at_ms = issued_at_ms.saturating_add(input.ttl_minutes.saturating_mul(60_000));
    let nonce = Uuid::new_v4().to_string();
    let attestation_payload = json!({
        "provider_id": input.provider_id,
        "agent_id": input.agent_id,
        "service_did": input.service_did,
        "service_address": input.service_address,
        "version": input.version,
        "agent_card": input.agent_card,
        "deployment": deployment,
        "review": review,
        "artifacts": artifacts,
        "provider_attester_did": input.provider_attester_did,
        "delegation_token": Value::Null,
        "source_commit": Value::Null,
        "build_digest": Value::Null,
        "nonce": nonce,
        "issued_at_ms": issued_at_ms,
        "expires_at_ms": expires_at_ms,
    });
    let signature = signer.sign_bytes(
        &serde_jcs::to_vec(&attestation_payload).context("canonicalize agent attestation")?,
    )?;
    let request = json!({
        "provider_id": input.provider_id,
        "agent_id": input.agent_id,
        "service_did": input.service_did,
        "service_address": input.service_address,
        "version": input.version,
        "agent_card": input.agent_card,
        "deployment": deployment,
        "review": review,
        "artifacts": artifacts,
        "attestations": {
            "attestation_signature": signature,
            "provider_attester_did": input.provider_attester_did,
            "nonce": nonce,
            "issued_at_ms": issued_at_ms,
            "expires_at_ms": expires_at_ms,
        },
    });
    let registration = ServiceNetPublisherRegistration {
        provider_id: input.provider_id.to_owned(),
        provider_did: input.provider_attester_did.to_owned(),
        agent_id: input.agent_id.to_owned(),
        service_did: input.service_did.to_owned(),
        service_address: input.service_address.map(ToOwned::to_owned),
        card_hash: canonical_agent_card_hash(&request["agent_card"])?,
        version: input.version.to_owned(),
        updated_at: chrono::Utc::now().to_rfc3339(),
        execution: input.execution,
        agent_card: request["agent_card"].clone(),
        deployment: request["deployment"].clone(),
        review: request["review"].clone(),
    };
    Ok(PreparedServiceAgentPublication {
        request,
        registration,
    })
}

#[must_use]
pub fn service_agent_card_requires_auth(agent_card: &Value) -> bool {
    match agent_card.get("security") {
        Some(Value::Array(items)) => {
            !items.is_empty()
                && !items.iter().any(|item| {
                    item.as_object()
                        .is_some_and(|object| object.contains_key("none"))
                })
        }
        Some(Value::Object(map)) => !map.is_empty() && !map.contains_key("none"),
        Some(Value::Null) | None => agent_card
            .get("securitySchemes")
            .and_then(Value::as_object)
            .is_some_and(|schemes| {
                !schemes.is_empty()
                    && !schemes.iter().all(|(name, scheme)| {
                        name == "none" || scheme.get("type").and_then(Value::as_str) == Some("none")
                    })
            }),
        Some(_) => true,
    }
}

pub async fn submit_service_agent_publication(
    client: &ServiceNetClient,
    data_dir: &Path,
    publication: &PreparedServiceAgentPublication,
) -> std::result::Result<Value, ServiceNetClientError> {
    let previous =
        stage_servicenet_publisher_registration(data_dir, publication.registration.clone())
            .map_err(ServiceNetClientError::local)?;
    match client.submit_agent(&publication.request).await {
        Ok(response) => Ok(response),
        Err(error) => {
            rollback_servicenet_publisher_registration(
                data_dir,
                &publication.registration.agent_id,
                previous,
            )
            .map_err(ServiceNetClientError::local)?;
            Err(error)
        }
    }
}

fn canonical_agent_card_hash(agent_card: &Value) -> Result<String> {
    Ok(format!(
        "sha256:{:x}",
        Sha256::digest(serde_jcs::to_vec(agent_card).context("canonicalize submitted Agent Card")?)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Identity;

    #[test]
    fn prepared_publication_keeps_modes_in_signed_request_and_local_registration() {
        let signer = Identity::new_random();
        let prepared = prepare_service_agent_publication(
            ServiceAgentPublicationInput {
                provider_id: "provider-1",
                agent_id: "ride",
                service_did: "did:key:z6Mkg5K92URgXhcuTfqt9jntq75JgPKgaQj36ougEQ3PrDXM",
                service_address: Some("ride@wattetheria"),
                version: "0.1.0",
                risk_level: "low",
                agent_card: json!({"url": "https://provider.example.com"}),
                connection_mode: ServiceNetConnectionMode::WattetheriaDirect,
                execution: ServiceAgentExecution::WattetheriaRuntime,
                provider_attester_did: &signer.agent_did,
                ttl_minutes: 30,
            },
            &signer,
        )
        .unwrap();

        assert_eq!(
            prepared.request["deployment"]["connection_mode"],
            "wattetheria_direct"
        );
        assert_eq!(
            prepared.registration.deployment["endpoint"]["url"],
            "https://provider.example.com"
        );
    }

    #[test]
    fn wattetheria_runtime_rejects_security_it_cannot_verify() {
        let signer = Identity::new_random();
        let result = prepare_service_agent_publication(
            ServiceAgentPublicationInput {
                provider_id: "provider-1",
                agent_id: "private-local",
                service_did: "did:key:z6Mkg5K92URgXhcuTfqt9jntq75JgPKgaQj36ougEQ3PrDXM",
                service_address: None,
                version: "0.1.0",
                risk_level: "low",
                agent_card: json!({
                    "url": "https://provider.example.com",
                    "securitySchemes": {"oauth2": {"type": "oauth2"}},
                    "security": [{"oauth2": []}]
                }),
                connection_mode: ServiceNetConnectionMode::WattetheriaDirect,
                execution: ServiceAgentExecution::WattetheriaRuntime,
                provider_attester_did: &signer.agent_did,
                ttl_minutes: 30,
            },
            &signer,
        );

        assert!(result.is_err());
    }
}
