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
    let actions = received.details["payload"]["brain_input"]["allowed_actions"]
        .as_array()
        .expect("brain allowed actions")
        .iter()
        .filter_map(serde_json::Value::as_str)
        .collect::<Vec<_>>();
    assert_eq!(actions, expected_actions);
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

fn signed_agent_event_envelope_with_wallet_key(
    keystore: &InMemoryKeyStore,
    key_handle: &KeyHandle,
    agent_did: &str,
    source_node_id: &str,
    target_agent_id: Option<&str>,
    capability: &str,
    message: Value,
) -> SwarmAgentEnvelope {
    let protocol = "google_a2a".to_owned();
    let transport_profile = Some("wattswarm_mesh".to_owned());
    let source_agent_id = Some(agent_did.to_owned());
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
    let signature_bytes = keystore
        .sign_bytes(
            key_handle,
            &canonical_bytes(&signed_payload).expect("canonical payload"),
        )
        .expect("wallet signs agent envelope");
    let signature = STANDARD.encode(signature_bytes.0);
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

fn set_signed_agent_envelope(event: &mut Value, envelope: &SwarmAgentEnvelope) {
    event["event"]["agent_envelope"] =
        serde_json::to_value(envelope).expect("agent envelope serializes");
}

fn sign_payment_event_with_identity(event: &mut Value, source_identity: &Identity) {
    let source_node_id = event["event"]["source_node_id"]
        .as_str()
        .expect("source_node_id")
        .to_owned();
    let target_agent_id = event["event"]["target_agent_id"]
        .as_str()
        .expect("target_agent_id")
        .to_owned();
    let message = event["event"]["payload"]["agent_envelope"]["message"].clone();
    let envelope = signed_agent_event_envelope(
        source_identity,
        &source_node_id,
        Some(&target_agent_id),
        "agent.payment",
        message,
    );
    set_signed_agent_envelope(event, &envelope);
    event["event"]["payload"]["agent_envelope"] =
        serde_json::to_value(envelope).expect("agent envelope serializes");
}

fn sign_payment_event_with_wallet_key(
    event: &mut Value,
    keystore: &InMemoryKeyStore,
    key_handle: &KeyHandle,
    source_agent_did: &str,
) {
    let source_node_id = event["event"]["source_node_id"]
        .as_str()
        .expect("source_node_id")
        .to_owned();
    let target_agent_id = event["event"]["target_agent_id"]
        .as_str()
        .expect("target_agent_id")
        .to_owned();
    let message = event["event"]["payload"]["agent_envelope"]["message"].clone();
    let envelope = signed_agent_event_envelope_with_wallet_key(
        keystore,
        key_handle,
        source_agent_did,
        &source_node_id,
        Some(&target_agent_id),
        "agent.payment",
        message,
    );
    set_signed_agent_envelope(event, &envelope);
    event["event"]["payload"]["agent_envelope"] =
        serde_json::to_value(envelope).expect("agent envelope serializes");
}

#[tokio::test]
async fn agent_events_sync_signed_payment_event_to_ledger_before_decision() {
    let (_dir, router, _token, _policy_engine, state) = build_test_app(20);
    let remote_identity = Identity::new_random();
    let payment_id = "payment-inbound-event-1";
    let local_agent_did = state.agent_did.clone();
    let remote_agent_did = remote_identity.agent_did.clone();
    let payment = json!({
        "payment_id": payment_id,
        "sender_did": remote_agent_did.clone(),
        "recipient_did": local_agent_did.clone(),
        "sender_public_id": "remote-public",
        "recipient_public_id": "local-public",
        "remote_node_id": "12D3KooRemotePeer",
        "amount": "1000",
        "currency": "USDC",
        "rail": "x402",
        "layer": "web3",
        "network": "base-sepolia",
        "sender_address": null,
        "recipient_address": "0x0000000000000000000000000000000000000001",
        "mission_id": null,
        "task_id": "task-7",
        "description": "inbound payment",
        "metadata": null,
        "status": "proposed",
        "authorization_signature": null,
        "authorization_public_key": null,
        "settlement_receipt": null,
        "reject_reason": null,
        "proposed_at": 10,
        "authorized_at": null,
        "settled_at": null,
        "expires_at": null
    });
    let agent_envelope = signed_agent_event_envelope(
        &remote_identity,
        "12D3KooRemotePeer",
        Some(&local_agent_did),
        "agent.payment",
        json!({
            "message_kind": "payment_request",
            "payment": payment
        }),
    );
    let event = json!({
        "event": {
            "event_id": "evt-payment-sync-1",
            "event_type": "payment_request",
            "source_kind": "payment_summary",
            "source_node_id": "12D3KooRemotePeer",
            "target_agent_id": local_agent_did.clone(),
            "target_executor": "core-agent",
            "agent_envelope": agent_envelope.clone(),
            "payload": {
                "agent_envelope": agent_envelope
            },
            "requires_commit": true,
            "allowed_actions": ["authorize", "reject"],
            "correlation_id": "payment-thread-1",
            "dedupe_key": "payment:payment-inbound-event-1",
            "created_at": 10
        }
    });

    let response = request_json(
        router,
        Request::post("/agent-events")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(event.to_string()))
            .expect("request"),
    )
    .await;

    assert_eq!(response["ok"].as_bool(), Some(true));
    let ledger = state.payment_ledger.lock().await;
    let payment = ledger.get(payment_id).expect("payment synced");
    assert_eq!(
        payment.status,
        wattetheria_kernel::payments::PaymentStatus::Proposed
    );
    assert_eq!(payment.sender_did, remote_identity.agent_did);
}

#[tokio::test]
async fn agent_events_defer_inbound_dm_until_friendship_is_active() {
    let (_dir, router, _token, _policy_engine, state) = build_test_app(20);
    let remote_identity = Identity::new_random();
    let local_agent_did = state.agent_did.clone();
    let agent_envelope = signed_agent_event_envelope(
        &remote_identity,
        "remote-node-1",
        Some(&local_agent_did),
        "social.dm.send",
        json!({
            "source_public_id": "agent-remote.123",
            "target_public_id": "agent-local.456",
            "message_id": "dm-message-1",
            "thread_id": "dm:agent-remote.123:agent-local.456",
            "content": {"text": "hello before friendship"},
            "sent_at": 10
        }),
    );
    let event = json!({
        "event": {
            "event_id": "evt-deferred-dm-1",
            "event_type": "topic_message_requires_reply",
            "source_kind": "topic_message",
            "source_node_id": "remote-node-1",
            "target_agent_id": local_agent_did,
            "target_executor": "core-agent",
            "agent_envelope": agent_envelope,
            "payload": {
                "feed_key": "wattswarm.dm",
                "scope_hint": "group:dm-1",
                "message_id": "dm-message-1",
                "topic_content": {
                    "kind": "direct_message",
                    "text": "hello before friendship"
                }
            },
            "requires_commit": true,
            "allowed_actions": ["reply", "ignore"],
            "created_at": 10
        }
    });

    let response = request_json(
        router,
        Request::post("/agent-events")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(event.to_string()))
            .expect("request"),
    )
    .await;

    assert_eq!(response["ok"].as_bool(), Some(true));
    assert_eq!(response["decision"], Value::Null);
    assert_eq!(
        response["detail"].as_str(),
        Some("deferred until friendship is active")
    );
    let deferred = state
        .social_store
        .get_deferred_agent_event("evt-deferred-dm-1")
        .expect("get deferred event")
        .expect("deferred event");
    assert_eq!(deferred.status, "waiting_for_friendship");
    assert_eq!(deferred.local_public_id, "agent-local.456");
    assert_eq!(deferred.remote_public_id, "agent-remote.123");

    friendship_service::upsert_friendship(
        &*state.social_store,
        &wattetheria_social::domain::friendships::Friendship {
            friendship_id: "friendship-deferred-dm".to_owned(),
            local_public_id: "agent-local.456".to_owned(),
            remote_public_id: "agent-remote.123".to_owned(),
            display_name: None,
            state: wattetheria_social::domain::friendships::FriendshipState::Active,
            established_from_request_id: None,
            thread_id: Some("dm:agent-remote.123:agent-local.456".to_owned()),
            created_at: 20,
            updated_at: 20,
        },
    )
    .expect("activate friendship");

    let replayed = crate::routes::agent_events::replay_deferred_dm_agent_events_for_friendship(
        &state,
        "agent-local.456",
        "agent-remote.123",
    )
    .await
    .expect("replay deferred dm events");
    assert_eq!(replayed, 1);
    let deferred = state
        .social_store
        .get_deferred_agent_event("evt-deferred-dm-1")
        .expect("get deferred event")
        .expect("deferred event");
    assert_eq!(deferred.status, "replayed");
    assert!(deferred.replayed_at.is_some());
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn agent_events_sync_mission_lifecycle_to_network_claims_before_decision() {
    let (_dir, router, _token, _policy_engine, state) = build_test_app(20);
    let local_agent_did = state.agent_did.clone();
    let mut claims = NetworkMissionClaimRegistry::default();
    claims.record(
        "mission-claim-sync-1",
        "mission-claim-sync-1",
        &local_agent_did,
        "exec-claim-sync-1",
        Some("network_claim_submitted".to_string()),
        NetworkMissionClaimMetadata {
            mission_feed_key: Some("wattetheria.missions".to_string()),
            mission_scope_hint: Some("group:mission-claim-sync-1".to_string()),
            ..NetworkMissionClaimMetadata::default()
        },
    );
    state
        .local_db
        .save_domain(
            wattetheria_kernel::local_db::domain::NETWORK_MISSION_CLAIMS,
            &claims,
        )
        .unwrap();

    let publisher_identity = Identity::new_random();
    let approved = json!({
        "kind": "mission_claim_approved",
        "mission_id": "mission-claim-sync-1",
        "task_id": "mission-claim-sync-1",
        "claimer_agent_did": local_agent_did,
        "status": "approved"
    });
    let approved_envelope = signed_agent_event_envelope(
        &publisher_identity,
        "publisher-node",
        Some(&local_agent_did),
        "mission.claim.approve",
        approved.clone(),
    );
    let approved_event = json!({
        "event": {
            "event_id": "evt-mission-claim-approved-sync",
            "event_type": "topic_message_requires_reply",
            "source_kind": "topic_message",
            "source_node_id": "publisher-node",
            "target_agent_id": local_agent_did,
            "target_executor": "core-agent",
            "agent_envelope": approved_envelope,
            "payload": {
                "feed_key": "wattetheria.missions",
                "scope_hint": "group:mission-claim-sync-1",
                "message_id": "msg-approved-sync",
                "content": approved
            },
            "requires_commit": true,
            "allowed_actions": ["complete_mission", "ignore"],
            "created_at": 10
        }
    });

    let response = request_json(
        router.clone(),
        Request::post("/agent-events")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(approved_event.to_string()))
            .expect("request"),
    )
    .await;
    assert_eq!(response["ok"].as_bool(), Some(true));
    let registry: NetworkMissionClaimRegistry = state
        .local_db
        .load_domain_or_default(wattetheria_kernel::local_db::domain::NETWORK_MISSION_CLAIMS)
        .unwrap();
    let record = registry
        .records()
        .into_iter()
        .find(|record| record.mission_id == "mission-claim-sync-1")
        .expect("claim record");
    assert_eq!(record.status.as_deref(), Some("claimed"));

    let settled = json!({
        "kind": "mission_settled",
        "mission_id": "mission-claim-sync-1",
        "task_id": "mission-claim-sync-1",
        "claimer_agent_did": local_agent_did,
        "status": "settled"
    });
    let settled_envelope = signed_agent_event_envelope(
        &publisher_identity,
        "publisher-node",
        Some(&local_agent_did),
        "mission.settle",
        settled.clone(),
    );
    let settled_event = json!({
        "event": {
            "event_id": "evt-mission-settled-sync",
            "event_type": "topic_message_requires_reply",
            "source_kind": "topic_message",
            "source_node_id": "publisher-node",
            "target_agent_id": local_agent_did,
            "target_executor": "core-agent",
            "agent_envelope": settled_envelope,
            "payload": {
                "feed_key": "wattetheria.missions",
                "scope_hint": "group:mission-claim-sync-1",
                "message_id": "msg-settled-sync",
                "content": settled
            },
            "requires_commit": false,
            "allowed_actions": ["ignore"],
            "created_at": 11
        }
    });
    let response = request_json(
        router,
        Request::post("/agent-events")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(settled_event.to_string()))
            .expect("request"),
    )
    .await;
    assert_eq!(response["ok"].as_bool(), Some(true));
    let registry: NetworkMissionClaimRegistry = state
        .local_db
        .load_domain_or_default(wattetheria_kernel::local_db::domain::NETWORK_MISSION_CLAIMS)
        .unwrap();
    let record = registry
        .records()
        .into_iter()
        .find(|record| record.mission_id == "mission-claim-sync-1")
        .expect("claim record");
    assert_eq!(record.status.as_deref(), Some("settled"));
}

#[tokio::test]
async fn agent_events_sync_task_claim_decision_to_network_claims_before_decision() {
    let (_dir, router, _token, _policy_engine, state) = build_test_app(20);
    let local_agent_did = state.agent_did.clone();
    let mut claims = NetworkMissionClaimRegistry::default();
    claims.record(
        "mission-claim-event-sync-1",
        "remote-task-claim-event-sync-1",
        &local_agent_did,
        "exec-claim-event-sync-1",
        Some("network_claim_submitted".to_string()),
        NetworkMissionClaimMetadata {
            mission_feed_key: Some("wattetheria.missions".to_string()),
            mission_scope_hint: Some("group:mission-claim-event-sync-1".to_string()),
            ..NetworkMissionClaimMetadata::default()
        },
    );
    state
        .local_db
        .save_domain(
            wattetheria_kernel::local_db::domain::NETWORK_MISSION_CLAIMS,
            &claims,
        )
        .unwrap();

    let publisher_identity = Identity::new_random();
    let claim_decision = json!({
        "approved": true,
        "task_id": "remote-task-claim-event-sync-1",
        "task_inputs": {
            "mission_id": "mission-claim-event-sync-1",
            "agent_did": local_agent_did,
        }
    });
    let claim_decision_envelope = signed_agent_event_envelope(
        &publisher_identity,
        "publisher-node",
        Some(&local_agent_did),
        "task.claim.decision",
        claim_decision.clone(),
    );
    let claim_decision_event = json!({
        "event": {
            "event_id": "evt-task-claim-decision-sync",
            "event_type": "task_claim_decision_received",
            "source_kind": "task_lifecycle",
            "source_node_id": "publisher-node",
            "target_agent_id": local_agent_did,
            "target_executor": "core-agent",
            "agent_envelope": claim_decision_envelope,
            "payload": claim_decision,
            "requires_commit": false,
            "allowed_actions": ["complete_mission", "ignore"],
            "correlation_id": "remote-task-claim-event-sync-1",
            "dedupe_key": "task_claim_decision:remote-task-claim-event-sync-1",
            "created_at": 10
        }
    });

    let response = request_json(
        router,
        Request::post("/agent-events")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(claim_decision_event.to_string()))
            .expect("request"),
    )
    .await;
    assert_eq!(response["ok"].as_bool(), Some(true));
    let registry: NetworkMissionClaimRegistry = state
        .local_db
        .load_domain_or_default(wattetheria_kernel::local_db::domain::NETWORK_MISSION_CLAIMS)
        .unwrap();
    let record = registry
        .records()
        .into_iter()
        .find(|record| record.mission_id == "mission-claim-event-sync-1")
        .expect("claim record");
    assert_eq!(record.status.as_deref(), Some("claimed"));
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn agent_events_sync_task_completion_and_settlement_to_network_claims_before_decision() {
    let (_dir, router, _token, _policy_engine, state) = build_test_app(20);
    let local_agent_did = state.agent_did.clone();
    let mission_id = "mission-claim-event-sync-2";
    let task_id = "remote-task-lifecycle-event-sync-2";
    let mut claims = NetworkMissionClaimRegistry::default();
    claims.record(
        mission_id,
        task_id,
        &local_agent_did,
        "exec-claim-event-sync-2",
        Some("claimed".to_string()),
        NetworkMissionClaimMetadata {
            mission_feed_key: Some("wattetheria.missions".to_string()),
            mission_scope_hint: Some("group:mission-claim-event-sync-2".to_string()),
            ..NetworkMissionClaimMetadata::default()
        },
    );
    state
        .local_db
        .save_domain(
            wattetheria_kernel::local_db::domain::NETWORK_MISSION_CLAIMS,
            &claims,
        )
        .unwrap();

    let publisher_identity = Identity::new_random();
    let completion_decision = json!({
        "approved": true,
        "retry_requested": false,
        "task_id": task_id,
        "execution_id": "exec-claim-event-sync-2",
        "task_inputs": {
            "kind": "wattetheria_mission",
            "mission_id": mission_id,
            "agent_did": local_agent_did,
        }
    });
    let completion_decision_envelope = signed_agent_event_envelope(
        &publisher_identity,
        "publisher-node",
        Some(&local_agent_did),
        "task.completion.decision",
        completion_decision.clone(),
    );
    let completion_decision_event = json!({
        "event": {
            "event_id": "evt-task-completion-decision-sync",
            "event_type": "task_completion_decision_received",
            "source_kind": "task_lifecycle",
            "source_node_id": "publisher-node",
            "target_agent_id": local_agent_did,
            "target_executor": "core-agent",
            "agent_envelope": completion_decision_envelope,
            "payload": completion_decision,
            "requires_commit": false,
            "allowed_actions": ["ignore"],
            "correlation_id": task_id,
            "dedupe_key": "task_completion_decision:remote-task-lifecycle-event-sync-2",
            "created_at": 11
        }
    });

    let response = request_json(
        router.clone(),
        Request::post("/agent-events")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(
                completion_decision_event.to_string(),
            ))
            .expect("request"),
    )
    .await;
    assert_eq!(response["ok"].as_bool(), Some(true));
    let registry: NetworkMissionClaimRegistry = state
        .local_db
        .load_domain_or_default(wattetheria_kernel::local_db::domain::NETWORK_MISSION_CLAIMS)
        .unwrap();
    let record = registry
        .records()
        .into_iter()
        .find(|record| record.mission_id == mission_id)
        .expect("claim record");
    assert_eq!(record.status.as_deref(), Some("completed"));

    let settlement = json!({
        "task_id": task_id,
        "execution_id": "exec-claim-event-sync-2",
        "receipt": {
            "status": "settled",
            "mission_id": mission_id,
        },
        "task_inputs": {
            "kind": "wattetheria_mission",
            "mission_id": mission_id,
            "agent_did": local_agent_did,
        }
    });
    let settlement_envelope = signed_agent_event_envelope(
        &publisher_identity,
        "publisher-node",
        Some(&local_agent_did),
        "task.settled",
        settlement.clone(),
    );
    let settlement_event = json!({
        "event": {
            "event_id": "evt-task-settled-sync",
            "event_type": "task_settled_received",
            "source_kind": "task_lifecycle",
            "source_node_id": "publisher-node",
            "target_agent_id": local_agent_did,
            "target_executor": "core-agent",
            "agent_envelope": settlement_envelope,
            "payload": settlement,
            "requires_commit": false,
            "allowed_actions": ["ignore"],
            "correlation_id": task_id,
            "dedupe_key": "task_settled:remote-task-lifecycle-event-sync-2",
            "created_at": 12
        }
    });

    let response = request_json(
        router,
        Request::post("/agent-events")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(settlement_event.to_string()))
            .expect("request"),
    )
    .await;
    assert_eq!(response["ok"].as_bool(), Some(true));
    let registry: NetworkMissionClaimRegistry = state
        .local_db
        .load_domain_or_default(wattetheria_kernel::local_db::domain::NETWORK_MISSION_CLAIMS)
        .unwrap();
    let record = registry
        .records()
        .into_iter()
        .find(|record| record.mission_id == mission_id)
        .expect("claim record");
    assert_eq!(record.status.as_deref(), Some("settled"));
}

#[tokio::test]
async fn agent_events_sync_mission_completed_to_publisher_board_before_decision() {
    let (_dir, router, token, _policy_engine, state) = build_test_app(20);
    let local_agent_did = state.agent_did.clone();
    let public_id = bootstrap_broker_identity(router.clone(), &token, &local_agent_did).await;
    let mission = authed_post_json(
        router.clone(),
        &token,
        "/v1/wattetheria/missions",
        json!({
            "title": "Publisher sync complete",
            "description": "Publisher receives completed lifecycle topic.",
            "publisher": public_id,
            "publisher_kind": "player",
            "domain": "trade",
            "reward": {
                "agent_watt": 2,
                "reputation": 1,
                "capacity": 0,
                "treasury_share_watt": 0
            },
            "payload": {"objective": "sync"}
        }),
    )
    .await;
    let mission_id = mission["mission_id"].as_str().expect("mission_id");
    let worker_identity = Identity::new_random();
    let _claimed = authed_post_json(
        router.clone(),
        &token,
        &format!("/v1/wattetheria/missions/{mission_id}/claim"),
        json!({
            "mission_id": mission_id,
            "agent_did": worker_identity.agent_did,
        }),
    )
    .await;

    let completed = json!({
        "kind": "mission_completed",
        "mission_id": mission_id,
        "task_id": mission_id,
        "publisher_agent_did": local_agent_did,
        "claimer_agent_did": worker_identity.agent_did,
        "result": {"ok": true, "summary": "done"},
        "status": "completed"
    });
    let completed_envelope = signed_agent_event_envelope(
        &worker_identity,
        "worker-node",
        Some(&local_agent_did),
        "mission.complete",
        completed.clone(),
    );
    let event = json!({
        "event": {
            "event_id": "evt-mission-completed-board-sync",
            "event_type": "topic_message_requires_reply",
            "source_kind": "topic_message",
            "source_node_id": "worker-node",
            "target_agent_id": local_agent_did,
            "target_executor": "core-agent",
            "agent_envelope": completed_envelope,
            "payload": {
                "feed_key": "wattetheria.missions",
                "scope_hint": format!("group:{mission_id}"),
                "message_id": "msg-completed-sync",
                "content": completed
            },
            "requires_commit": true,
            "allowed_actions": ["settle_mission", "ignore"],
            "created_at": 12
        }
    });

    let response = request_json(
        router,
        Request::post("/agent-events")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(event.to_string()))
            .expect("request"),
    )
    .await;
    assert_eq!(response["ok"].as_bool(), Some(true));
    let board = state.mission_board.lock().await;
    let synced = board.get(mission_id).expect("mission synced");
    assert_eq!(
        synced.status,
        wattetheria_kernel::civilization::missions::MissionStatus::Completed
    );
    assert_eq!(
        synced.completed_by.as_deref(),
        Some(worker_identity.agent_did.as_str())
    );
    assert_eq!(
        synced.completion_result,
        Some(json!({"ok": true, "summary": "done"}))
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn agent_events_route_translates_openai_compatible_reply_into_structured_decision() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind listener");
    let addr = listener.local_addr().expect("listener addr");
    let app_mock = Router::new().route(
        "/v1/chat/completions",
        post(|| async move {
            Json(json!({
                "choices": [{
                    "message": {
                        "content": "{\"action\":\"reply\",\"reason\":\"respond politely\",\"payload\":{\"content\":\"hello back\"}}"
                    }
                }]
            }))
        }),
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, app_mock).await.expect("serve mock");
    });

    let (_dir, _router, _token, _policy_engine, state) = build_test_app(20);
    let base_url = format!("http://{addr}/v1");
    let local_agent_did = state.agent_did.clone();
    let remote_identity = Identity::new_random();
    let dm_envelope = signed_agent_event_envelope(
        &remote_identity,
        "social-node",
        Some(&local_agent_did),
        "social.dm",
        json!({
            "source_public_id": "peer-alpha",
            "target_public_id": "self-alpha",
            "content": "hello"
        }),
    );
    let state = ControlPlaneState {
        brain_engine: Arc::new(tokio::sync::RwLock::new(BrainEngine::from_config(
            &BrainProviderConfig::OpenaiCompatible {
                base_url: base_url.clone(),
                model: "openclaw".to_owned(),
                api_key_env: None,
            },
        ))),
        brain_config: Arc::new(tokio::sync::RwLock::new(
            BrainProviderConfig::OpenaiCompatible {
                base_url: base_url.clone(),
                model: "openclaw".to_owned(),
                api_key_env: None,
            },
        )),
        brain_provider_label: format!("openai-compatible model=openclaw url={base_url}"),
        ..state
    };
    let data_dir = state.data_dir.clone();
    let app = app(state);

    let response = request_json(
        app,
        Request::post("/agent-events")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(
                json!({
                    "event": {
                        "event_id": "evt-1",
                        "event_type": "dm_received",
                        "source_kind": "social",
                        "source_node_id": "social-node",
                        "target_agent_id": local_agent_did,
                        "target_executor": "core-agent",
                        "agent_envelope": dm_envelope.clone(),
                        "payload": {
                            "agent_envelope": dm_envelope
                        },
                        "requires_commit": true,
                        "allowed_actions": ["reply", "ignore"],
                        "correlation_id": "thread-1",
                        "dedupe_key": "dm:thread-1",
                        "created_at": 1
                    }
                })
                .to_string(),
            ))
            .expect("request"),
    )
    .await;

    assert_eq!(response["ok"].as_bool(), Some(true));
    assert_eq!(response["decision"]["action"].as_str(), Some("reply"));
    assert_eq!(
        response["decision"]["route"].as_str(),
        Some("wattetheria_commit")
    );
    assert_eq!(
        response["decision"]["payload"]["content"].as_str(),
        Some("hello back")
    );

    let entries = crate::diagnostics::list_diagnostics(
        &data_dir,
        &crate::diagnostics::DiagnosticFilter {
            event_id: Some("evt-1".to_owned()),
            phase: Some("decision.brain_response".to_owned()),
            ..Default::default()
        },
    )
    .unwrap();
    let brain_response = entries.first().expect("decision.brain_response diagnostic");
    assert!(
        brain_response.details["payload"]["response_body"]
            .as_str()
            .expect("response body")
            .contains("\"choices\"")
    );
    assert!(
        brain_response.details["payload"]["completion_content"]
            .as_str()
            .expect("completion content")
            .contains("\"action\":\"reply\"")
    );
    assert_eq!(
        brain_response.details["payload"]["parse"]["status"].as_str(),
        Some("accepted")
    );

    server.abort();
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn agent_events_route_translates_topic_dm_reply_to_wattetheria_commit() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind listener");
    let addr = listener.local_addr().expect("listener addr");
    let app_mock = Router::new().route(
        "/v1/chat/completions",
        post(|| async move {
            Json(json!({
                "choices": [{
                    "message": {
                        "content": "{\"action\":\"reply\",\"reason\":\"respond to dm\",\"payload\":{\"content\":\"signed reply\"}}"
                    }
                }]
            }))
        }),
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, app_mock).await.expect("serve mock");
    });

    let (_dir, _router, _token, _policy_engine, state) = build_test_app(20);
    let base_url = format!("http://{addr}/v1");
    let local_agent_did = state.agent_did.clone();
    let remote_identity = Identity::new_random();
    let dm_message = json!({
        "source_public_id": "peer-alpha",
        "target_public_id": "self-alpha",
        "content": "hello",
        "thread_id": "dm:self-alpha:peer-alpha"
    });
    let dm_envelope = signed_agent_event_envelope(
        &remote_identity,
        "social-node",
        Some(&local_agent_did),
        "social.dm.send",
        dm_message.clone(),
    );
    let expected_source_agent_id = dm_envelope.source_agent_id.clone();
    friendship_service::upsert_friendship(
        &*state.social_store,
        &wattetheria_social::domain::friendships::Friendship {
            friendship_id: "friendship-topic-dm-reply".to_owned(),
            local_public_id: "self-alpha".to_owned(),
            remote_public_id: "peer-alpha".to_owned(),
            display_name: None,
            state: wattetheria_social::domain::friendships::FriendshipState::Active,
            established_from_request_id: None,
            thread_id: Some("dm:self-alpha:peer-alpha".to_owned()),
            created_at: 1,
            updated_at: 1,
        },
    )
    .expect("seed active friendship");
    let state = ControlPlaneState {
        brain_engine: Arc::new(tokio::sync::RwLock::new(BrainEngine::from_config(
            &BrainProviderConfig::OpenaiCompatible {
                base_url: base_url.clone(),
                model: "openclaw".to_owned(),
                api_key_env: None,
            },
        ))),
        brain_config: Arc::new(tokio::sync::RwLock::new(
            BrainProviderConfig::OpenaiCompatible {
                base_url: base_url.clone(),
                model: "openclaw".to_owned(),
                api_key_env: None,
            },
        )),
        brain_provider_label: format!("openai-compatible model=openclaw url={base_url}"),
        ..state
    };
    let data_dir = state.data_dir.clone();
    let app = app(state);

    let response = request_json(
        app,
        Request::post("/agent-events")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(
                json!({
                    "event": {
                        "event_id": "evt-topic-dm-1",
                        "event_type": "topic_message_requires_reply",
                        "source_kind": "topic_message",
                        "source_node_id": "social-node",
                        "target_agent_id": local_agent_did,
                        "target_executor": "core-agent",
                        "agent_envelope": dm_envelope.clone(),
                        "payload": {
                            "network_id": "mainnet:watt-etheria",
                            "feed_key": "wattswarm.dm",
                            "scope_hint": "group:dm-self-peer",
                            "message_id": "topic-msg-1",
                            "content": "hello",
                            "topic_content": {
                                "kind": "direct_message",
                                "agent_envelope": dm_envelope,
                                "content": "hello"
                            }
                        },
                        "requires_commit": false,
                        "allowed_actions": ["reply", "ignore"],
                        "correlation_id": "wattswarm.dm",
                        "dedupe_key": "topic_message:topic-msg-1",
                        "created_at": 1
                    }
                })
                .to_string(),
            ))
            .expect("request"),
    )
    .await;

    assert_eq!(response["ok"].as_bool(), Some(true));
    assert_eq!(response["decision"]["action"].as_str(), Some("reply"));
    assert_eq!(
        response["decision"]["route"].as_str(),
        Some("wattetheria_commit")
    );
    assert_eq!(
        response["decision"]["payload"]["content"].as_str(),
        Some("signed reply")
    );
    let entries = crate::diagnostics::list_diagnostics(
        &data_dir,
        &crate::diagnostics::DiagnosticFilter {
            event_id: Some("evt-topic-dm-1".to_owned()),
            phase: Some("callback.received".to_owned()),
            ..Default::default()
        },
    )
    .unwrap();
    let received = entries.first().expect("callback.received diagnostic");
    let brain_input = &received.details["payload"]["brain_input"];
    assert_eq!(
        brain_input["agent_envelope"]["source_agent_id"].as_str(),
        expected_source_agent_id.as_deref()
    );
    assert!(brain_input["payload"]["agent_envelope"].is_null());
    assert!(brain_input["payload"]["topic_content"]["agent_envelope"].is_null());
    assert_eq!(
        brain_input["payload"]["topic_content"]["content"].as_str(),
        Some("hello")
    );
    assert_eq!(brain_input["payload"]["content"].as_str(), Some("hello"));

    server.abort();
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn agent_events_route_reports_openai_compatible_missing_content_body() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind listener");
    let addr = listener.local_addr().expect("listener addr");
    let app_mock = Router::new().route(
        "/v1/chat/completions",
        post(|| async move {
            Json(json!({
                "choices": [{
                    "message": {}
                }]
            }))
        }),
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, app_mock).await.expect("serve mock");
    });

    let (_dir, _router, _token, _policy_engine, state) = build_test_app(20);
    let base_url = format!("http://{addr}/v1");
    let remote_identity = Identity::new_random();
    let task_claim_envelope = signed_agent_event_envelope(
        &remote_identity,
        "claimer-node",
        Some(&state.agent_did),
        "task.claim",
        json!({
            "task_id": "task-1",
            "event_kind": "task_claimed"
        }),
    );
    let state = ControlPlaneState {
        brain_engine: Arc::new(tokio::sync::RwLock::new(BrainEngine::from_config(
            &BrainProviderConfig::OpenaiCompatible {
                base_url: base_url.clone(),
                model: "openclaw".to_owned(),
                api_key_env: None,
            },
        ))),
        brain_config: Arc::new(tokio::sync::RwLock::new(
            BrainProviderConfig::OpenaiCompatible {
                base_url: base_url.clone(),
                model: "openclaw".to_owned(),
                api_key_env: None,
            },
        )),
        brain_provider_label: format!("openai-compatible model=openclaw url={base_url}"),
        ..state
    };
    let data_dir = state.data_dir.clone();
    let app = app(state);

    let response = request_json(
        app,
        Request::post("/agent-events")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(
                json!({
                    "event": {
                        "event_id": "evt-missing-content",
                        "event_type": "task_claim_received",
                        "source_kind": "task_lifecycle",
                        "source_node_id": "claimer-node",
                        "target_agent_id": null,
                        "target_executor": "core-agent",
                        "agent_envelope": task_claim_envelope.clone(),
                        "payload": {
                            "task_id": "task-1",
                            "event_kind": "task_claimed"
                        },
                        "requires_commit": false,
                        "allowed_actions": ["human_review", "decide_claim", "reject_claim"],
                        "correlation_id": "task-1",
                        "dedupe_key": "task_claim:task-1",
                        "created_at": 1
                    }
                })
                .to_string(),
            ))
            .expect("request"),
    )
    .await;

    assert_eq!(response["ok"].as_bool(), Some(false));
    let detail = response["detail"].as_str().expect("response detail");
    assert!(detail.contains("openai-compatible response missing content"));
    assert!(detail.contains("response_body="));
    assert!(detail.contains("\"choices\""));

    let entries = crate::diagnostics::list_diagnostics(
        &data_dir,
        &crate::diagnostics::DiagnosticFilter {
            event_id: Some("evt-missing-content".to_owned()),
            ..Default::default()
        },
    )
    .unwrap();
    let failed = entries
        .iter()
        .find(|entry| entry.phase == "decision.failed")
        .expect("decision.failed diagnostic");
    assert_eq!(
        failed.details["payload"]["callback_response"]["ok"].as_bool(),
        Some(false)
    );
    assert!(
        failed.details["payload"]["error"]
            .as_str()
            .expect("decision error")
            .contains("response_body=")
    );

    server.abort();
}

#[tokio::test]
async fn agent_events_route_allows_task_result_to_settle_mission_via_commit_plane() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind listener");
    let addr = listener.local_addr().expect("listener addr");
    let app_mock = Router::new().route(
        "/v1/chat/completions",
        post(|| async move {
            Json(json!({
                "choices": [{
                    "message": {
                        "content": "{\"action\":\"settle_mission\",\"reason\":\"publisher accepted result\",\"payload\":{\"mission_id\":\"mission-1\",\"agent_did\":\"agent-worker\"}}"
                    }
                }]
            }))
        }),
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, app_mock).await.expect("serve mock");
    });

    let (_dir, _router, _token, _policy_engine, state) = build_test_app(20);
    let base_url = format!("http://{addr}/v1");
    let remote_identity = Identity::new_random();
    let task_result_envelope = signed_agent_event_envelope(
        &remote_identity,
        "claimer-node",
        Some(&state.agent_did),
        "task.result",
        json!({
            "task_id": "mission-1",
            "mission_id": "mission-1",
            "candidate_output": {
                "mission_id": "mission-1",
                "agent_did": "agent-worker"
            }
        }),
    );
    let state = ControlPlaneState {
        brain_engine: Arc::new(tokio::sync::RwLock::new(BrainEngine::from_config(
            &BrainProviderConfig::OpenaiCompatible {
                base_url: base_url.clone(),
                model: "openclaw".to_owned(),
                api_key_env: None,
            },
        ))),
        brain_config: Arc::new(tokio::sync::RwLock::new(
            BrainProviderConfig::OpenaiCompatible {
                base_url: base_url.clone(),
                model: "openclaw".to_owned(),
                api_key_env: None,
            },
        )),
        brain_provider_label: format!("openai-compatible model=openclaw url={base_url}"),
        ..state
    };
    let app = app(state);

    let response = request_json(
        app,
        Request::post("/agent-events")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(
                json!({
                    "event": {
                        "event_id": "evt-task-result",
                        "event_type": "task_result_received",
                        "source_kind": "task_lifecycle",
                        "source_node_id": "claimer-node",
                        "target_agent_id": null,
                        "target_executor": "core-agent",
                        "agent_envelope": task_result_envelope,
                        "payload": {
                            "task_id": "mission-1",
                            "mission_id": "mission-1",
                            "candidate_output": {
                                "mission_id": "mission-1",
                                "agent_did": "agent-worker"
                            }
                        },
                        "requires_commit": true,
                        "allowed_actions": ["human_review", "settle_mission"],
                        "correlation_id": "mission-1",
                        "dedupe_key": "task_result:mission-1:cand-1",
                        "created_at": 1
                    }
                })
                .to_string(),
            ))
            .expect("request"),
    )
    .await;

    assert_eq!(response["ok"].as_bool(), Some(true));
    assert_eq!(
        response["decision"]["action"].as_str(),
        Some("settle_mission")
    );
    assert_eq!(
        response["decision"]["route"].as_str(),
        Some("wattetheria_commit")
    );

    server.abort();
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn agent_events_convert_approved_claim_decision_to_mission_commit() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind listener");
    let addr = listener.local_addr().expect("listener addr");
    let app_mock = Router::new().route(
        "/v1/chat/completions",
        post(|| async move {
            Json(json!({
                "choices": [{
                    "message": {
                        "content": "{\"ACTION\":\"DECIDE_CLAIM\",\"REASON\":\"claim is valid\",\"PAYLOAD\":{\"APPROVED\":true}}"
                    }
                }]
            }))
        }),
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, app_mock).await.expect("serve mock");
    });

    let (_dir, _router, _token, _policy_engine, state) = build_test_app(20);
    let base_url = format!("http://{addr}/v1");
    let remote_identity = Identity::new_random();
    let task_claim_envelope = signed_agent_event_envelope(
        &remote_identity,
        "claimer-node",
        Some(&state.agent_did),
        "task.claim",
        json!({
            "task_id": "mission-1",
            "claimer_node_id": "claimer-node",
            "task_inputs": {
                "kind": "wattetheria_mission",
                "mission_id": "mission-1"
            }
        }),
    );
    let state = ControlPlaneState {
        brain_engine: Arc::new(tokio::sync::RwLock::new(BrainEngine::from_config(
            &BrainProviderConfig::OpenaiCompatible {
                base_url: base_url.clone(),
                model: "openclaw".to_owned(),
                api_key_env: None,
            },
        ))),
        brain_config: Arc::new(tokio::sync::RwLock::new(
            BrainProviderConfig::OpenaiCompatible {
                base_url: base_url.clone(),
                model: "openclaw".to_owned(),
                api_key_env: None,
            },
        )),
        brain_provider_label: format!("openai-compatible model=openclaw url={base_url}"),
        ..state
    };
    let data_dir = state.data_dir.clone();
    let app = app(state);

    let response = request_json(
        app,
        Request::post("/agent-events")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(
                json!({
                    "event": {
                        "event_id": "evt-task-claim",
                        "event_type": "task_claim_received",
                        "source_kind": "task_lifecycle",
                        "source_node_id": "claimer-node",
                        "target_agent_id": null,
                        "target_executor": "core-agent",
                        "agent_envelope": task_claim_envelope.clone(),
                        "payload": {
                            "task_id": "mission-1",
                            "claimer_node_id": "claimer-node",
                            "task_inputs": {
                                "kind": "wattetheria_mission",
                                "mission_id": "mission-1"
                            }
                        },
                        "requires_commit": true,
                        "allowed_actions": ["human_review", "decide_claim", "reject_claim"],
                        "correlation_id": "mission-1",
                        "dedupe_key": "task_claim:mission-1",
                        "created_at": 1
                    }
                })
                .to_string(),
            ))
            .expect("request"),
    )
    .await;

    assert_eq!(response["ok"].as_bool(), Some(true));
    assert_eq!(
        response["decision"]["action"].as_str(),
        Some("claim_mission")
    );
    assert_eq!(
        response["decision"]["route"].as_str(),
        Some("wattetheria_commit")
    );
    assert_eq!(
        response["decision"]["payload"]["mission_id"].as_str(),
        Some("mission-1")
    );
    assert_eq!(
        response["decision"]["payload"]["agent_did"].as_str(),
        Some("claimer-node")
    );

    assert_claim_brain_actions(
        &data_dir,
        "evt-task-claim",
        &["decide_claim", "reject_claim", "human_review"],
    );

    server.abort();
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn agent_events_extract_json_prefixed_claim_decision_to_mission_commit() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind listener");
    let addr = listener.local_addr().expect("listener addr");
    let app_mock = Router::new().route(
        "/v1/chat/completions",
        post(|| async move {
            Json(json!({
                "choices": [{
                    "message": {
                        "content": "json\n{\n  \"action\": \"decide_claim\",\n  \"reason\": \"auto approved\",\n  \"payload\": {\n    \"approved\": true,\n    \"mission_id\": \"mission-prefixed\",\n    \"claimer_node_id\": \"claimer-node\",\n    \"agent_did\": \"did:key:claimer\"\n  }\n}"
                    }
                }]
            }))
        }),
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, app_mock).await.expect("serve mock");
    });

    let (_dir, _router, _token, _policy_engine, state) = build_test_app(20);
    let base_url = format!("http://{addr}/v1");
    let remote_identity = Identity::new_random();
    let task_claim_envelope = signed_agent_event_envelope(
        &remote_identity,
        "claimer-node",
        Some(&state.agent_did),
        "task.claim",
        json!({
            "task_id": "mission-prefixed",
            "claimer_node_id": "claimer-node",
            "task_inputs": {
                "kind": "wattetheria_mission",
                "mission_id": "mission-prefixed"
            }
        }),
    );
    let state = ControlPlaneState {
        brain_engine: Arc::new(tokio::sync::RwLock::new(BrainEngine::from_config(
            &BrainProviderConfig::OpenaiCompatible {
                base_url: base_url.clone(),
                model: "openclaw".to_owned(),
                api_key_env: None,
            },
        ))),
        brain_config: Arc::new(tokio::sync::RwLock::new(
            BrainProviderConfig::OpenaiCompatible {
                base_url: base_url.clone(),
                model: "openclaw".to_owned(),
                api_key_env: None,
            },
        )),
        brain_provider_label: format!("openai-compatible model=openclaw url={base_url}"),
        ..state
    };
    let data_dir = state.data_dir.clone();
    let app = app(state);

    let response = request_json(
        app,
        Request::post("/agent-events")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(
                json!({
                    "event": {
                        "event_id": "evt-task-claim-prefixed",
                        "event_type": "task_claim_received",
                        "source_kind": "task_lifecycle",
                        "source_node_id": "claimer-node",
                        "target_agent_id": null,
                        "target_executor": "core-agent",
                        "agent_envelope": task_claim_envelope.clone(),
                        "payload": {
                            "task_id": "mission-prefixed",
                            "claimer_node_id": "claimer-node",
                            "task_inputs": {
                                "kind": "wattetheria_mission",
                                "mission_id": "mission-prefixed"
                            }
                        },
                        "requires_commit": false,
                        "allowed_actions": ["decide_claim"],
                        "correlation_id": "mission-prefixed",
                        "dedupe_key": "task_claim:mission-prefixed",
                        "created_at": 1
                    }
                })
                .to_string(),
            ))
            .expect("request"),
    )
    .await;

    assert_eq!(response["ok"].as_bool(), Some(true));
    assert_eq!(
        response["decision"]["action"].as_str(),
        Some("claim_mission")
    );
    assert_eq!(
        response["decision"]["route"].as_str(),
        Some("wattetheria_commit")
    );

    let entries = crate::diagnostics::list_diagnostics(
        &data_dir,
        &crate::diagnostics::DiagnosticFilter {
            event_id: Some("evt-task-claim-prefixed".to_owned()),
            phase: Some("decision.brain_response".to_owned()),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        entries[0].details["payload"]["parse"]["status"].as_str(),
        Some("accepted")
    );

    server.abort();
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn agent_events_route_reject_claim_decision_to_wattetheria_commit() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind listener");
    let addr = listener.local_addr().expect("listener addr");
    let app_mock = Router::new().route(
        "/v1/chat/completions",
        post(|| async move {
            Json(json!({
                "choices": [{
                    "message": {
                        "content": "{\"action\":\"reject_claim\",\"reason\":\"reward requires more proof\",\"payload\":{\"mission_id\":\"mission-reject\",\"claimer_node_id\":\"claimer-node\"}}"
                    }
                }]
            }))
        }),
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, app_mock).await.expect("serve mock");
    });

    let (_dir, _router, token, _policy_engine, state) = build_test_app(20);
    let base_url = format!("http://{addr}/v1");
    let remote_identity = Identity::new_random();
    let task_claim_envelope = signed_agent_event_envelope(
        &remote_identity,
        "claimer-node",
        Some(&state.agent_did),
        "task.claim",
        json!({
            "task_id": "mission-reject",
            "claimer_node_id": "claimer-node",
            "task_inputs": {
                "kind": "wattetheria_mission",
                "mission_id": "mission-reject"
            }
        }),
    );
    let state = ControlPlaneState {
        brain_engine: Arc::new(tokio::sync::RwLock::new(BrainEngine::from_config(
            &BrainProviderConfig::OpenaiCompatible {
                base_url: base_url.clone(),
                model: "openclaw".to_owned(),
                api_key_env: None,
            },
        ))),
        brain_config: Arc::new(tokio::sync::RwLock::new(
            BrainProviderConfig::OpenaiCompatible {
                base_url: base_url.clone(),
                model: "openclaw".to_owned(),
                api_key_env: None,
            },
        )),
        brain_provider_label: format!("openai-compatible model=openclaw url={base_url}"),
        ..state
    };
    let app = app(state);

    let response = request_json(
        app.clone(),
        Request::post("/agent-events")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(
                json!({
                    "event": {
                        "event_id": "evt-task-claim-reject",
                        "event_type": "task_claim_received",
                        "source_kind": "task_lifecycle",
                        "source_node_id": "claimer-node",
                        "target_agent_id": null,
                        "target_executor": "core-agent",
                        "agent_envelope": task_claim_envelope.clone(),
                        "payload": {
                            "task_id": "mission-reject",
                            "claimer_node_id": "claimer-node",
                            "task_inputs": {
                                "kind": "wattetheria_mission",
                                "mission_id": "mission-reject"
                            }
                        },
                        "requires_commit": true,
                        "allowed_actions": ["human_review", "decide_claim", "reject_claim"],
                        "correlation_id": "mission-reject",
                        "dedupe_key": "task_claim:mission-reject",
                        "created_at": 1
                    }
                })
                .to_string(),
            ))
            .expect("request"),
    )
    .await;

    assert_eq!(response["ok"].as_bool(), Some(true));
    assert_eq!(
        response["decision"]["action"].as_str(),
        Some("reject_claim")
    );
    assert_eq!(
        response["decision"]["route"].as_str(),
        Some("wattetheria_commit")
    );
    assert_eq!(
        response["decision"]["payload"]["mission_id"].as_str(),
        Some("mission-reject")
    );
    assert_eq!(
        response["decision"]["payload"]["claimer_node_id"].as_str(),
        Some("claimer-node")
    );

    let committed = authed_post_json(
        app,
        &token,
        "/v1/agent-actions/commit",
        json!({
            "event": {
                "event_id": "evt-task-claim-reject",
                "event_type": "task_claim_received",
                "source_kind": "task_lifecycle",
                "source_node_id": "claimer-node",
                "target_agent_id": null,
                "target_executor": "core-agent",
                "agent_envelope": task_claim_envelope,
                "payload": {
                    "task_id": "mission-reject",
                    "claimer_node_id": "claimer-node",
                    "task_inputs": {
                        "kind": "wattetheria_mission",
                        "mission_id": "mission-reject"
                    }
                },
                "requires_commit": true,
                "allowed_actions": ["human_review", "decide_claim", "reject_claim"],
                "correlation_id": "mission-reject",
                "dedupe_key": "task_claim:mission-reject",
                "created_at": 1
            },
            "decision": response["decision"].clone(),
        }),
    )
    .await;
    assert_eq!(committed["status"].as_str(), Some("rejected"));
    assert_eq!(committed["mission_id"].as_str(), Some("mission-reject"));

    server.abort();
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn agent_event_approved_claim_commit_emits_gateway_claimed_event() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind listener");
    let addr = listener.local_addr().expect("listener addr");
    let app_mock = Router::new().route(
        "/v1/chat/completions",
        post(|| async move {
            Json(json!({
                "choices": [{
                    "message": {
                        "content": "{\"ACTION\":\"DECIDE_CLAIM\",\"REASON\":\"auto approved\",\"PAYLOAD\":{\"APPROVED\": TRUE,\"AGENT_DID\":\"did:key:claimer\",\"DISPLAY_NAME\":\"Agent-MX1111\",\"PUBLIC_ID\":\"agent-MX1111.public\"}}"
                    }
                }]
            }))
        }),
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, app_mock).await.expect("serve mock");
    });

    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge::default_for(identity.agent_did.clone()));
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, _router, token, _policy_engine, state) =
        build_test_app_with_bridge(20, dir, identity, event_log, bridge_handle);
    let base_url = format!("http://{addr}/v1");
    let state = ControlPlaneState {
        brain_engine: Arc::new(tokio::sync::RwLock::new(BrainEngine::from_config(
            &BrainProviderConfig::OpenaiCompatible {
                base_url: base_url.clone(),
                model: "openclaw".to_owned(),
                api_key_env: None,
            },
        ))),
        brain_config: Arc::new(tokio::sync::RwLock::new(
            BrainProviderConfig::OpenaiCompatible {
                base_url: base_url.clone(),
                model: "openclaw".to_owned(),
                api_key_env: None,
            },
        )),
        brain_provider_label: format!("openai-compatible model=openclaw url={base_url}"),
        ..state
    };
    let app = app(state.clone());
    let publisher_public_id =
        bootstrap_broker_identity(app.clone(), &token, &state.agent_did).await;
    let mut events = state.stream_tx.subscribe();
    let created = authed_post_json(
        app.clone(),
        &token,
        "/v1/wattetheria/missions",
        json!({
            "title": "Auto approve claim",
            "description": "Publisher agent approves a remote Wattswarm claim.",
            "publisher": publisher_public_id,
            "publisher_kind": "player",
            "domain": "trade",
            "reward": {
                "agent_watt": 2,
                "reputation": 1,
                "capacity": 0,
                "treasury_share_watt": 0
            },
            "payload": {"objective": "favorite local food"}
        }),
    )
    .await;
    let mission_id = created["mission_id"].as_str().expect("mission_id");
    let published = tokio::time::timeout(std::time::Duration::from_secs(2), events.recv())
        .await
        .expect("publish event timeout")
        .expect("publish event");
    assert_eq!(published.kind, "mission.published");

    let remote_identity = Identity::new_random();
    let task_claim_envelope = signed_agent_event_envelope(
        &remote_identity,
        "claimer-node",
        Some(&state.agent_did),
        "task.claim",
        json!({
            "task_id": mission_id,
            "claimer_node_id": "claimer-node",
            "task_inputs": {
                "kind": "wattetheria_mission",
                "mission_id": mission_id
            }
        }),
    );
    let event = json!({
        "event_id": "evt-task-claim-e2e",
        "event_type": "task_claim_received",
        "source_kind": "task_lifecycle",
        "source_node_id": "claimer-node",
        "target_agent_id": state.agent_did,
        "target_executor": "core-agent",
        "agent_envelope": task_claim_envelope,
        "payload": {
            "task_id": mission_id,
            "claimer_node_id": "claimer-node",
            "task_inputs": {
                "kind": "wattetheria_mission",
                "mission_id": mission_id
            }
        },
        "requires_commit": true,
        "allowed_actions": ["human_review", "decide_claim", "reject_claim"],
        "correlation_id": mission_id,
        "dedupe_key": format!("task_claim:{mission_id}"),
        "created_at": 1
    });

    let callback_response = request_json(
        app.clone(),
        Request::post("/agent-events")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(
                json!({"event": event.clone()}).to_string(),
            ))
            .expect("request"),
    )
    .await;
    assert_eq!(callback_response["ok"].as_bool(), Some(true));
    assert_eq!(
        callback_response["decision"]["action"].as_str(),
        Some("claim_mission")
    );
    assert_eq!(
        callback_response["decision"]["route"].as_str(),
        Some("wattetheria_commit")
    );

    let committed = authed_post_json(
        app,
        &token,
        "/v1/agent-actions/commit",
        json!({
            "event": event,
            "decision": callback_response["decision"].clone(),
        }),
    )
    .await;
    assert_eq!(committed["status"].as_str(), Some("claimed"));
    assert_eq!(committed["claimed_by"].as_str(), Some("did:key:claimer"));
    assert_eq!(
        committed["claimer_agent_did"].as_str(),
        Some("did:key:claimer")
    );
    assert_eq!(
        committed["claimer_agent_identity"].as_str(),
        Some("Agent-MX1111")
    );
    assert_eq!(
        committed["claimer_display_name"].as_str(),
        Some("Agent-MX1111")
    );
    assert_eq!(
        committed["claimer_public_id"].as_str(),
        Some("agent-MX1111.public")
    );
    assert_eq!(
        committed["mission_lifecycle_notice"]["kind"].as_str(),
        Some("mission_claim_approved")
    );
    assert_eq!(
        committed["mission_lifecycle_notice"]["target_agent_id"].as_str(),
        Some("did:key:claimer")
    );
    assert_eq!(
        committed["mission_lifecycle_notice"]["target_node_id"].as_str(),
        Some("claimer-node")
    );
    assert_eq!(
        committed["mission_lifecycle_notice"]["has_source_agent_card"].as_bool(),
        Some(true)
    );
    assert!(
        committed["updated_at"].as_i64().unwrap_or_default()
            >= committed["created_at"].as_i64().unwrap_or_default()
    );
    assert!(
        bridge.messages.lock().await.is_empty(),
        "ordinary mission claim approval is published through task lifecycle events, not topic messages"
    );

    let claimed = tokio::time::timeout(std::time::Duration::from_secs(2), events.recv())
        .await
        .expect("claim event timeout")
        .expect("claim event");
    assert_eq!(claimed.kind, "mission.claimed");
    assert_eq!(claimed.payload["mission_id"].as_str(), Some(mission_id));
    assert_eq!(claimed.payload["status"].as_str(), Some("claimed"));
    assert_eq!(
        claimed.payload["claimed_by"].as_str(),
        Some("did:key:claimer")
    );
    assert_eq!(
        claimed.payload["claimer_display_name"].as_str(),
        Some("Agent-MX1111")
    );
    assert!(
        claimed.payload["updated_at"].as_i64().unwrap_or_default()
            >= claimed.payload["created_at"].as_i64().unwrap_or_default()
    );
    let gateway_plan =
        crate::gateway_dispatch::plan_stream_event(&claimed).expect("gateway dispatch plan");
    assert_eq!(
        gateway_plan.data_kind,
        crate::gateway_dispatch::GatewayDataKind::MissionLifecycle
    );
    assert_eq!(gateway_plan.scope.task_id.as_deref(), Some(mission_id));

    let board = state.mission_board.lock().await;
    let claimed_mission = board.get(mission_id).expect("claimed mission");
    assert_eq!(
        claimed_mission.status,
        wattetheria_kernel::civilization::missions::MissionStatus::Claimed
    );
    assert_eq!(
        claimed_mission.claimed_by.as_deref(),
        Some("did:key:claimer")
    );

    server.abort();
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn agent_action_commit_settles_mission_completed_topic_without_candidate_finalize() {
    let dir = tempfile::tempdir().unwrap();
    let identity = Identity::new_random();
    let event_log = EventLog::new(dir.path().join("events.jsonl")).unwrap();
    let bridge = Arc::new(MockSwarmBridge::default_for(identity.agent_did.clone()));
    let bridge_handle: Arc<dyn SwarmBridge> = bridge.clone();
    let (_dir, app, token, _policy_engine, state) =
        build_test_app_with_bridge(20, dir, identity, event_log, bridge_handle);
    let publisher_public_id =
        bootstrap_broker_identity(app.clone(), &token, &state.agent_did).await;
    let worker_identity = Identity::new_random();
    let mission = authed_post_json(
        app.clone(),
        &token,
        "/v1/wattetheria/missions",
        json!({
            "title": "Ordinary mission completed topic",
            "description": "Publisher settles an ordinary mission lifecycle topic.",
            "publisher": publisher_public_id,
            "publisher_kind": "player",
            "domain": "trade",
            "reward": {
                "agent_watt": 2,
                "reputation": 1,
                "capacity": 0,
                "treasury_share_watt": 0
            },
            "payload": {"objective": "ordinary mission"}
        }),
    )
    .await;
    let mission_id = mission["mission_id"].as_str().expect("mission_id");
    let _claimed = authed_post_json(
        app.clone(),
        &token,
        &format!("/v1/wattetheria/missions/{mission_id}/claim"),
        json!({
            "mission_id": mission_id,
            "agent_did": worker_identity.agent_did,
        }),
    )
    .await;
    let _completed = authed_post_json(
        app.clone(),
        &token,
        &format!("/v1/wattetheria/missions/{mission_id}/complete"),
        json!({
            "mission_id": mission_id,
            "agent_did": worker_identity.agent_did,
            "result": {"ok": true, "summary": "done"}
        }),
    )
    .await;
    bridge.messages.lock().await.clear();

    let content = json!({
        "kind": "mission_completed",
        "mission_id": mission_id,
        "task_id": mission_id,
        "mission_feed_key": "wattetheria.missions",
        "mission_scope_hint": format!("group:{mission_id}"),
        "publisher_agent_did": state.agent_did,
        "claimer_agent_did": worker_identity.agent_did,
        "agent_did": worker_identity.agent_did,
        "result": {"ok": true, "summary": "done"},
        "status": "completed",
        "next_action": "settle_mission"
    });
    let agent_envelope = signed_agent_event_envelope(
        &worker_identity,
        "worker-node",
        Some(&state.agent_did),
        "mission.complete",
        content.clone(),
    );
    let committed = authed_post_json(
        app,
        &token,
        "/v1/agent-actions/commit",
        json!({
            "event": {
                "event_id": "evt-topic-mission-completed-commit",
                "event_type": "topic_message_requires_reply",
                "source_kind": "topic_message",
                "source_node_id": "worker-node",
                "target_agent_id": state.agent_did,
                "agent_envelope": agent_envelope,
                "payload": {
                    "feed_key": "wattetheria.missions",
                    "scope_hint": format!("group:{mission_id}"),
                    "message_id": "msg-completed-commit",
                    "content": content
                },
                "allowed_actions": ["settle_mission", "ignore"],
                "requires_commit": true
            },
            "decision": {
                "decision_id": "dec-topic-mission-completed-settle",
                "action": "settle_mission",
                "route": "wattetheria_commit",
                "payload": {}
            }
        }),
    )
    .await;

    assert_eq!(committed["status"].as_str(), Some("settled"));
    assert_eq!(
        committed["completed_by"].as_str(),
        Some(worker_identity.agent_did.as_str())
    );
    assert!(committed.get("swarm_finalize").is_none());
    assert!(committed.get("candidate_id").is_none());
    assert_eq!(
        committed["mission_lifecycle_notice"]["kind"].as_str(),
        Some("mission_settled")
    );

    assert!(
        bridge.messages.lock().await.is_empty(),
        "ordinary mission completion and settlement use task lifecycle events, not topic messages"
    );
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn agent_events_route_claim_approved_topic_to_complete_mission_commit() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind listener");
    let addr = listener.local_addr().expect("listener addr");
    let app_mock = Router::new().route(
        "/v1/chat/completions",
        post(|| async move {
            Json(json!({
                "choices": [{
                    "message": {
                        "content": "{\"action\":\"complete_mission\",\"reason\":\"work is ready\",\"payload\":{\"result\":{\"ok\":true,\"summary\":\"done\"}}}"
                    }
                }]
            }))
        }),
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, app_mock).await.expect("serve mock");
    });

    let (_dir, _router, _token, _policy_engine, state) = build_test_app(20);
    let base_url = format!("http://{addr}/v1");
    let publisher_identity = Identity::new_random();
    let content = json!({
        "kind": "mission_claim_approved",
        "mission_id": "mission-approved-1",
        "task_id": "mission-approved-1",
        "mission_feed_key": "wattetheria.missions",
        "mission_scope_hint": "group:mission-approved-1",
        "publisher_agent_did": publisher_identity.agent_did,
        "publisher_wattswarm_node_id": "publisher-node",
        "claimer_agent_did": state.agent_did,
        "status": "approved",
        "next_action": "complete_mission"
    });
    let agent_envelope = signed_agent_event_envelope(
        &publisher_identity,
        "publisher-node",
        Some(&state.agent_did),
        "mission.claim.approve",
        content.clone(),
    );
    let state = ControlPlaneState {
        brain_engine: Arc::new(tokio::sync::RwLock::new(BrainEngine::from_config(
            &BrainProviderConfig::OpenaiCompatible {
                base_url: base_url.clone(),
                model: "openclaw".to_owned(),
                api_key_env: None,
            },
        ))),
        brain_config: Arc::new(tokio::sync::RwLock::new(
            BrainProviderConfig::OpenaiCompatible {
                base_url: base_url.clone(),
                model: "openclaw".to_owned(),
                api_key_env: None,
            },
        )),
        brain_provider_label: format!("openai-compatible model=openclaw url={base_url}"),
        ..state
    };
    let app = app(state.clone());

    let response = request_json(
        app,
        Request::post("/agent-events")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(
                json!({
                    "event": {
                        "event_id": "evt-topic-claim-approved",
                        "event_type": "topic_message_requires_reply",
                        "source_kind": "topic_message",
                        "source_node_id": "publisher-node",
                        "target_agent_id": state.agent_did,
                        "target_executor": "core-agent",
                        "agent_envelope": agent_envelope,
                        "payload": {
                            "feed_key": "wattetheria.missions",
                            "scope_hint": "group:mission-approved-1",
                            "message_id": "msg-approved-1",
                            "content": content
                        },
                        "requires_commit": true,
                        "allowed_actions": ["reply"],
                        "correlation_id": "mission-approved-1",
                        "dedupe_key": "topic:mission-approved-1:approved",
                        "created_at": 1
                    }
                })
                .to_string(),
            ))
            .expect("request"),
    )
    .await;

    assert_eq!(response["ok"].as_bool(), Some(true));
    assert_eq!(
        response["decision"]["action"].as_str(),
        Some("complete_mission")
    );
    assert_eq!(
        response["decision"]["route"].as_str(),
        Some("wattetheria_commit")
    );
    assert_eq!(
        response["decision"]["payload"]["mission_id"].as_str(),
        Some("mission-approved-1")
    );
    assert_eq!(
        response["decision"]["payload"]["agent_did"].as_str(),
        Some(state.agent_did.as_str())
    );
    assert_eq!(
        response["decision"]["payload"]["task_id"].as_str(),
        Some("mission-approved-1")
    );
    assert_eq!(
        response["decision"]["payload"]["mission_scope_hint"].as_str(),
        Some("group:mission-approved-1")
    );
    assert_eq!(
        response["decision"]["payload"]["result"]["summary"].as_str(),
        Some("done")
    );

    server.abort();
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn agent_events_route_mission_completed_topic_to_settle_mission_commit() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind listener");
    let addr = listener.local_addr().expect("listener addr");
    let app_mock = Router::new().route(
        "/v1/chat/completions",
        post(|| async move {
            Json(json!({
                "choices": [{
                    "message": {
                        "content": "{\"action\":\"settle_mission\",\"reason\":\"ordinary mission result accepted\",\"payload\":{}}"
                    }
                }]
            }))
        }),
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, app_mock).await.expect("serve mock");
    });

    let (_dir, _router, _token, _policy_engine, state) = build_test_app(20);
    let base_url = format!("http://{addr}/v1");
    let claimer_identity = Identity::new_random();
    let content = json!({
        "kind": "mission_completed",
        "mission_id": "mission-completed-1",
        "task_id": "mission-completed-1",
        "mission_feed_key": "wattetheria.missions",
        "mission_scope_hint": "group:mission-completed-1",
        "publisher_agent_did": state.agent_did,
        "publisher_wattswarm_node_id": "publisher-node",
        "claimer_agent_did": claimer_identity.agent_did,
        "agent_did": claimer_identity.agent_did,
        "result": {"ok": true, "summary": "done"},
        "status": "completed",
        "next_action": "settle_mission"
    });
    let agent_envelope = signed_agent_event_envelope(
        &claimer_identity,
        "claimer-node",
        Some(&state.agent_did),
        "mission.complete",
        content.clone(),
    );
    let state = ControlPlaneState {
        brain_engine: Arc::new(tokio::sync::RwLock::new(BrainEngine::from_config(
            &BrainProviderConfig::OpenaiCompatible {
                base_url: base_url.clone(),
                model: "openclaw".to_owned(),
                api_key_env: None,
            },
        ))),
        brain_config: Arc::new(tokio::sync::RwLock::new(
            BrainProviderConfig::OpenaiCompatible {
                base_url: base_url.clone(),
                model: "openclaw".to_owned(),
                api_key_env: None,
            },
        )),
        brain_provider_label: format!("openai-compatible model=openclaw url={base_url}"),
        ..state
    };
    let app = app(state.clone());

    let response = request_json(
        app,
        Request::post("/agent-events")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(
                json!({
                    "event": {
                        "event_id": "evt-topic-mission-completed",
                        "event_type": "topic_message_requires_reply",
                        "source_kind": "topic_message",
                        "source_node_id": "claimer-node",
                        "target_agent_id": state.agent_did,
                        "target_executor": "core-agent",
                        "agent_envelope": agent_envelope,
                        "payload": {
                            "feed_key": "wattetheria.missions",
                            "scope_hint": "group:mission-completed-1",
                            "message_id": "msg-completed-1",
                            "content": content
                        },
                        "requires_commit": true,
                        "allowed_actions": ["reply"],
                        "correlation_id": "mission-completed-1",
                        "dedupe_key": "topic:mission-completed-1:completed",
                        "created_at": 1
                    }
                })
                .to_string(),
            ))
            .expect("request"),
    )
    .await;

    assert_eq!(response["ok"].as_bool(), Some(true));
    assert_eq!(
        response["decision"]["action"].as_str(),
        Some("settle_mission")
    );
    assert_eq!(
        response["decision"]["route"].as_str(),
        Some("wattetheria_commit")
    );
    assert_eq!(
        response["decision"]["payload"]["mission_id"].as_str(),
        Some("mission-completed-1")
    );
    assert_eq!(
        response["decision"]["payload"]["agent_did"].as_str(),
        Some(claimer_identity.agent_did.as_str())
    );
    assert_eq!(
        response["decision"]["payload"]["task_id"].as_str(),
        Some("mission-completed-1")
    );
    assert!(
        response["decision"]["payload"]
            .get("candidate_id")
            .is_none()
    );

    server.abort();
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn agent_events_convert_accept_result_to_settle_mission_commit() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind listener");
    let addr = listener.local_addr().expect("listener addr");
    let app_mock = Router::new().route(
        "/v1/chat/completions",
        post(|| async move {
            Json(json!({
                "choices": [{
                    "message": {
                        "content": "{\"action\":\"accept_result\",\"reason\":\"result is acceptable\",\"payload\":{}}"
                    }
                }]
            }))
        }),
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, app_mock).await.expect("serve mock");
    });

    let (_dir, _router, _token, _policy_engine, state) = build_test_app(20);
    let base_url = format!("http://{addr}/v1");
    let remote_identity = Identity::new_random();
    let task_result_envelope = signed_agent_event_envelope(
        &remote_identity,
        "claimer-node",
        Some(&state.agent_did),
        "task.result",
        json!({
            "task_id": "mission-1",
            "candidate_output": {
                "kind": "wattetheria_mission_result",
                "mission_id": "mission-1",
                "agent_did": "agent-worker"
            }
        }),
    );
    let state = ControlPlaneState {
        brain_engine: Arc::new(tokio::sync::RwLock::new(BrainEngine::from_config(
            &BrainProviderConfig::OpenaiCompatible {
                base_url: base_url.clone(),
                model: "openclaw".to_owned(),
                api_key_env: None,
            },
        ))),
        brain_config: Arc::new(tokio::sync::RwLock::new(
            BrainProviderConfig::OpenaiCompatible {
                base_url: base_url.clone(),
                model: "openclaw".to_owned(),
                api_key_env: None,
            },
        )),
        brain_provider_label: format!("openai-compatible model=openclaw url={base_url}"),
        ..state
    };
    let app = app(state);

    let response = request_json(
        app,
        Request::post("/agent-events")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(
                json!({
                    "event": {
                        "event_id": "evt-task-result-accept",
                        "event_type": "task_result_received",
                        "source_kind": "task_lifecycle",
                        "source_node_id": "claimer-node",
                        "target_agent_id": null,
                        "target_executor": "core-agent",
                        "agent_envelope": task_result_envelope,
                        "payload": {
                            "task_id": "mission-1",
                            "candidate_output": {
                                "kind": "wattetheria_mission_result",
                                "mission_id": "mission-1",
                                "agent_did": "agent-worker"
                            }
                        },
                        "requires_commit": true,
                        "allowed_actions": ["human_review", "accept_result"],
                        "correlation_id": "mission-1",
                        "dedupe_key": "task_result:mission-1:cand-1",
                        "created_at": 1
                    }
                })
                .to_string(),
            ))
            .expect("request"),
    )
    .await;

    assert_eq!(response["ok"].as_bool(), Some(true));
    assert_eq!(
        response["decision"]["action"].as_str(),
        Some("settle_mission")
    );
    assert_eq!(
        response["decision"]["route"].as_str(),
        Some("wattetheria_commit")
    );
    assert_eq!(
        response["decision"]["payload"]["mission_id"].as_str(),
        Some("mission-1")
    );
    assert_eq!(
        response["decision"]["payload"]["agent_did"].as_str(),
        Some("agent-worker")
    );

    server.abort();
}

#[tokio::test]
#[allow(clippy::too_many_lines)]
async fn agent_events_route_reject_result_decision_to_wattetheria_commit() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind listener");
    let addr = listener.local_addr().expect("listener addr");
    let app_mock = Router::new().route(
        "/v1/chat/completions",
        post(|| async move {
            Json(json!({
                "choices": [{
                    "message": {
                        "content": "{\"action\":\"reject_result\",\"reason\":\"result needs proof\",\"payload\":{\"candidate_id\":\"cand-reject\"}}"
                    }
                }]
            }))
        }),
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, app_mock).await.expect("serve mock");
    });

    let (_dir, _router, token, _policy_engine, state) = build_test_app(20);
    let base_url = format!("http://{addr}/v1");
    let remote_identity = Identity::new_random();
    let task_result_envelope = signed_agent_event_envelope(
        &remote_identity,
        "claimer-node",
        Some(&state.agent_did),
        "task.result",
        json!({
            "task_id": "mission-result-reject",
            "candidate_id": "cand-reject",
            "candidate_output": {
                "kind": "wattetheria_mission_result",
                "mission_id": "mission-result-reject",
                "agent_did": "agent-worker"
            }
        }),
    );
    let state = ControlPlaneState {
        brain_engine: Arc::new(tokio::sync::RwLock::new(BrainEngine::from_config(
            &BrainProviderConfig::OpenaiCompatible {
                base_url: base_url.clone(),
                model: "openclaw".to_owned(),
                api_key_env: None,
            },
        ))),
        brain_config: Arc::new(tokio::sync::RwLock::new(
            BrainProviderConfig::OpenaiCompatible {
                base_url: base_url.clone(),
                model: "openclaw".to_owned(),
                api_key_env: None,
            },
        )),
        brain_provider_label: format!("openai-compatible model=openclaw url={base_url}"),
        ..state
    };
    let app = app(state);
    let event = json!({
        "event_id": "evt-task-result-reject",
        "event_type": "task_result_received",
        "source_kind": "task_lifecycle",
        "source_node_id": "claimer-node",
        "target_agent_id": null,
        "target_executor": "core-agent",
        "agent_envelope": task_result_envelope,
        "payload": {
            "task_id": "mission-result-reject",
            "candidate_id": "cand-reject",
            "candidate_output": {
                "kind": "wattetheria_mission_result",
                "mission_id": "mission-result-reject",
                "agent_did": "agent-worker"
            }
        },
        "requires_commit": true,
        "allowed_actions": ["human_review", "accept_result", "reject_result", "request_retry"],
        "correlation_id": "mission-result-reject",
        "dedupe_key": "task_result:mission-result-reject:cand-reject",
        "created_at": 1
    });

    let response = request_json(
        app.clone(),
        Request::post("/agent-events")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(
                json!({"event": event.clone()}).to_string(),
            ))
            .expect("request"),
    )
    .await;

    assert_eq!(response["ok"].as_bool(), Some(true));
    assert_eq!(
        response["decision"]["action"].as_str(),
        Some("reject_result")
    );
    assert_eq!(
        response["decision"]["route"].as_str(),
        Some("wattetheria_commit")
    );

    let committed = authed_post_json(
        app,
        &token,
        "/v1/agent-actions/commit",
        json!({
            "event": event,
            "decision": response["decision"].clone(),
        }),
    )
    .await;
    assert_eq!(committed["status"].as_str(), Some("rejected"));
    assert_eq!(
        committed["mission_id"].as_str(),
        Some("mission-result-reject")
    );
    assert_eq!(committed["candidate_id"].as_str(), Some("cand-reject"));

    server.abort();
}

fn payment_event_with_optional_verified_context(
    local_agent_did: &str,
    remote_agent_did: &str,
    payment_id: &str,
    remote_node_id: &str,
    verified_context: Option<&serde_json::Value>,
) -> Value {
    payment_event_with_optional_extras(
        local_agent_did,
        remote_agent_did,
        payment_id,
        remote_node_id,
        verified_context,
        None,
    )
}

fn payment_event_with_optional_extras(
    local_agent_did: &str,
    remote_agent_did: &str,
    payment_id: &str,
    remote_node_id: &str,
    verified_context: Option<&serde_json::Value>,
    payment_account_binding: Option<&serde_json::Value>,
) -> Value {
    let payment = json!({
        "payment_id": payment_id,
        "sender_did": remote_agent_did,
        "recipient_did": local_agent_did,
        "sender_public_id": "remote-public",
        "recipient_public_id": "local-public",
        "remote_node_id": remote_node_id,
        "amount": "1000",
        "currency": "USDC",
        "rail": "x402",
        "layer": "web3",
        "network": "base-sepolia",
        "sender_address": null,
        "recipient_address": "0x0000000000000000000000000000000000000001",
        "mission_id": null,
        "task_id": "task-verified",
        "description": "inbound payment",
        "metadata": null,
        "status": "proposed",
        "authorization_signature": null,
        "authorization_public_key": null,
        "settlement_receipt": null,
        "reject_reason": null,
        "proposed_at": 10,
        "authorized_at": null,
        "settled_at": null,
        "expires_at": null
    });
    let mut message = json!({
        "message_kind": "payment_request",
        "payment": payment,
    });
    if let Some(binding) = payment_account_binding
        && let Some(map) = message.as_object_mut()
    {
        map.insert("payment_account_binding".to_owned(), binding.clone());
    }
    let agent_envelope = json!({
        "source_agent_id": remote_agent_did,
        "target_agent_id": local_agent_did,
        "source_node_id": remote_node_id,
        "message": message,
    });
    let mut payload = serde_json::Map::new();
    payload.insert("agent_envelope".to_owned(), agent_envelope);
    if let Some(context) = verified_context {
        payload.insert(
            VERIFIED_AGENT_CONTEXT_PAYLOAD_KEY.to_owned(),
            context.clone(),
        );
    }
    json!({
        "event": {
            "event_id": format!("evt-{payment_id}"),
            "event_type": "payment_request",
            "source_kind": "payment_summary",
            "source_node_id": remote_node_id,
            "target_agent_id": local_agent_did,
            "target_executor": "core-agent",
            "payload": Value::Object(payload),
            "requires_commit": true,
            "allowed_actions": ["authorize", "reject"],
            "correlation_id": format!("payment-thread-{payment_id}"),
            "dedupe_key": format!("payment:{payment_id}"),
            "created_at": 10
        }
    })
}

fn verified_context_value(agent_did: &str, source_node_id: &str) -> serde_json::Value {
    let context = VerifiedAgentContext {
        agent_did: Did::parse(agent_did).expect("agent did parses"),
        controller_node_id: source_node_id.to_owned(),
        source_node_id: Some(source_node_id.to_owned()),
        envelope_verified: true,
        source_node_verified: true,
        controller_binding_verified: false,
        controller_binding_proof: None,
        payment_account_binding: None,
        verified_at_ms: 1_716_120_000_000,
        expires_at_ms: None,
    };
    serde_json::to_value(&context).expect("serialize verified context")
}

#[tokio::test]
async fn agent_events_payment_sync_accepts_consistent_verified_agent_context() {
    let (_dir, router, _token, _policy_engine, state) = build_test_app(20);
    let remote_identity = Identity::new_random();
    let payment_id = "payment-context-ok-1";
    let remote_node_id = "12D3KooRemotePeer";
    let context = verified_context_value(&remote_identity.agent_did, remote_node_id);
    let mut event = payment_event_with_optional_verified_context(
        &state.agent_did,
        &remote_identity.agent_did,
        payment_id,
        remote_node_id,
        Some(&context),
    );
    sign_payment_event_with_identity(&mut event, &remote_identity);

    let response = request_json(
        router,
        Request::post("/agent-events")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(event.to_string()))
            .expect("request"),
    )
    .await;

    assert_eq!(response["ok"].as_bool(), Some(true));
    let ledger = state.payment_ledger.lock().await;
    let payment = ledger.get(payment_id).expect("payment synced");
    assert_eq!(
        payment.status,
        wattetheria_kernel::payments::PaymentStatus::Proposed
    );
}

#[tokio::test]
async fn agent_events_payment_sync_rejects_forged_verified_context_without_signed_envelope() {
    let (_dir, router, _token, _policy_engine, state) = build_test_app(20);
    let remote_identity = Identity::new_random();
    let payment_id = "payment-context-forged-1";
    let remote_node_id = "12D3KooRemotePeer";
    let context = verified_context_value(&remote_identity.agent_did, remote_node_id);
    let event = payment_event_with_optional_verified_context(
        &state.agent_did,
        &remote_identity.agent_did,
        payment_id,
        remote_node_id,
        Some(&context),
    );

    let response = request_json(
        router,
        Request::post("/agent-events")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(event.to_string()))
            .expect("request"),
    )
    .await;

    let error = response["error"].as_str().unwrap_or_default();
    assert!(
        error.contains("signed agent_envelope"),
        "expected signed agent_envelope error, got {response}"
    );
    let ledger = state.payment_ledger.lock().await;
    assert!(
        ledger.get(payment_id).is_none(),
        "forged context must not reach the ledger"
    );
}

#[tokio::test]
async fn agent_events_payment_sync_rejects_verified_agent_context_with_mismatched_did() {
    let (_dir, router, _token, _policy_engine, state) = build_test_app(20);
    let remote_identity = Identity::new_random();
    let unrelated_identity = Identity::new_random();
    let payment_id = "payment-context-mismatch-1";
    let remote_node_id = "12D3KooRemotePeer";
    let context = verified_context_value(&unrelated_identity.agent_did, remote_node_id);
    let mut event = payment_event_with_optional_verified_context(
        &state.agent_did,
        &remote_identity.agent_did,
        payment_id,
        remote_node_id,
        Some(&context),
    );
    sign_payment_event_with_identity(&mut event, &remote_identity);

    let response = request_json(
        router,
        Request::post("/agent-events")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(event.to_string()))
            .expect("request"),
    )
    .await;

    assert!(
        response["error"]
            .as_str()
            .unwrap_or_default()
            .contains("agent_did does not match"),
        "expected agent_did mismatch error, got {response}"
    );
    let ledger = state.payment_ledger.lock().await;
    assert!(
        ledger.get(payment_id).is_none(),
        "rejected event must not reach the ledger"
    );
}

struct RemoteBindingFixture {
    remote_agent_did: String,
    agent_key_handle: KeyHandle,
    keystore: InMemoryKeyStore,
    binding: serde_json::Value,
}

fn build_remote_binding_fixture() -> RemoteBindingFixture {
    let mut keystore = InMemoryKeyStore::new();
    let agent_info = keystore.generate_ed25519().expect("ed25519 key");
    let payment_info = keystore.generate_secp256k1().expect("secp256k1 key");
    let options = PaymentAccountBindingProofOptions {
        agent_did: agent_info.did.clone(),
        agent_key_handle: &agent_info.key_handle,
        agent_public_key_multibase: agent_info.public_key_multibase.clone(),
        rail: "x402".to_owned(),
        network: Some("base-sepolia".to_owned()),
        custody: PaymentAccountCustody::LocalGenerated,
        receive_only: false,
        can_sign: true,
        capabilities: vec!["payment.authorize".to_owned()],
        issued_at_ms: 1_716_120_000_000,
        expires_at_ms: None,
        nonce: None,
        payment_signer: Some(PaymentAccountSigner {
            key_handle: &payment_info.key_handle,
            public_key_multibase: payment_info.public_key_multibase.clone(),
        }),
        watch_only_payment_address: None,
    };
    let proof = build_payment_account_binding_proof(&keystore, options).expect("build proof");
    let binding = serde_json::to_value(&proof).expect("serialize binding");
    RemoteBindingFixture {
        remote_agent_did: agent_info.did.to_string(),
        agent_key_handle: agent_info.key_handle,
        keystore,
        binding,
    }
}

#[tokio::test]
async fn agent_events_payment_sync_accepts_valid_payment_account_binding() {
    let (_dir, router, _token, _policy_engine, state) = build_test_app(20);
    let fixture = build_remote_binding_fixture();
    let payment_id = "payment-binding-ok-1";
    let remote_node_id = "12D3KooRemotePeer";
    let mut event = payment_event_with_optional_extras(
        &state.agent_did,
        &fixture.remote_agent_did,
        payment_id,
        remote_node_id,
        None,
        Some(&fixture.binding),
    );
    sign_payment_event_with_wallet_key(
        &mut event,
        &fixture.keystore,
        &fixture.agent_key_handle,
        &fixture.remote_agent_did,
    );

    let response = request_json(
        router,
        Request::post("/agent-events")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(event.to_string()))
            .expect("request"),
    )
    .await;

    assert_eq!(response["ok"].as_bool(), Some(true));
    let ledger = state.payment_ledger.lock().await;
    assert!(ledger.get(payment_id).is_some(), "payment should be synced");
}

#[tokio::test]
async fn agent_events_payment_sync_rejects_tampered_payment_account_binding() {
    let (_dir, router, _token, _policy_engine, state) = build_test_app(20);
    let fixture = build_remote_binding_fixture();
    let mut tampered = fixture.binding.clone();
    if let Some(map) = tampered.as_object_mut() {
        map.insert(
            "payment_address".to_owned(),
            Value::String("0x0000000000000000000000000000000000000001".to_owned()),
        );
    }
    let payment_id = "payment-binding-tampered-1";
    let mut event = payment_event_with_optional_extras(
        &state.agent_did,
        &fixture.remote_agent_did,
        payment_id,
        "12D3KooRemotePeer",
        None,
        Some(&tampered),
    );
    sign_payment_event_with_wallet_key(
        &mut event,
        &fixture.keystore,
        &fixture.agent_key_handle,
        &fixture.remote_agent_did,
    );

    let response = request_json(
        router,
        Request::post("/agent-events")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(event.to_string()))
            .expect("request"),
    )
    .await;

    let error = response["error"].as_str().unwrap_or_default();
    assert!(
        error.contains("payment_account_binding"),
        "expected payment_account_binding error, got {response}"
    );
    let ledger = state.payment_ledger.lock().await;
    assert!(
        ledger.get(payment_id).is_none(),
        "rejected event must not reach the ledger"
    );
}

#[tokio::test]
async fn agent_events_payment_sync_rejects_binding_with_mismatched_agent_did() {
    let (_dir, router, _token, _policy_engine, state) = build_test_app(20);
    let fixture = build_remote_binding_fixture();
    let unrelated_identity = Identity::new_random();
    let payment_id = "payment-binding-wrong-did-1";
    let mut event = payment_event_with_optional_extras(
        &state.agent_did,
        &unrelated_identity.agent_did,
        payment_id,
        "12D3KooRemotePeer",
        None,
        Some(&fixture.binding),
    );
    sign_payment_event_with_identity(&mut event, &unrelated_identity);

    let response = request_json(
        router,
        Request::post("/agent-events")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(event.to_string()))
            .expect("request"),
    )
    .await;

    let error = response["error"].as_str().unwrap_or_default();
    assert!(
        error.contains("payment_account_binding agent_did does not match"),
        "expected agent_did mismatch error, got {response}"
    );
    let ledger = state.payment_ledger.lock().await;
    assert!(
        ledger.get(payment_id).is_none(),
        "rejected event must not reach the ledger"
    );
}

#[tokio::test]
async fn agent_events_payment_sync_rejects_sender_signed_state_without_payment_account_binding() {
    let (_dir, router, _token, _policy_engine, state) = build_test_app(20);
    let remote_identity = Identity::new_random();
    let payment_id = "payment-binding-required-1";
    let mut event = payment_event_with_optional_extras(
        &state.agent_did,
        &remote_identity.agent_did,
        payment_id,
        "12D3KooRemotePeer",
        None,
        None,
    );
    event["event"]["event_type"] = json!("payment_update");
    event["event"]["payload"]["agent_envelope"]["message"]["message_kind"] =
        json!("payment_authorized");
    event["event"]["payload"]["agent_envelope"]["message"]["payment"]["status"] =
        json!("authorized");
    event["event"]["payload"]["agent_envelope"]["message"]["payment"]["sender_address"] =
        json!("0x0000000000000000000000000000000000000002");
    sign_payment_event_with_identity(&mut event, &remote_identity);

    let response = request_json(
        router,
        Request::post("/agent-events")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(event.to_string()))
            .expect("request"),
    )
    .await;

    let error = response["error"].as_str().unwrap_or_default();
    assert!(
        error.contains("payment_account_binding is required"),
        "expected required payment_account_binding error, got {response}"
    );
    let ledger = state.payment_ledger.lock().await;
    assert!(
        ledger.get(payment_id).is_none(),
        "rejected event must not reach the ledger"
    );
}

#[tokio::test]
async fn agent_events_payment_sync_rejects_binding_with_mismatched_sender_address() {
    let (_dir, router, _token, _policy_engine, state) = build_test_app(20);
    let fixture = build_remote_binding_fixture();
    let payment_id = "payment-binding-address-mismatch-1";
    let mut event = payment_event_with_optional_extras(
        &state.agent_did,
        &fixture.remote_agent_did,
        payment_id,
        "12D3KooRemotePeer",
        None,
        Some(&fixture.binding),
    );
    event["event"]["event_type"] = json!("payment_update");
    event["event"]["payload"]["agent_envelope"]["message"]["message_kind"] =
        json!("payment_authorized");
    event["event"]["payload"]["agent_envelope"]["message"]["payment"]["status"] =
        json!("authorized");
    event["event"]["payload"]["agent_envelope"]["message"]["payment"]["sender_address"] =
        json!("0x0000000000000000000000000000000000000003");
    sign_payment_event_with_wallet_key(
        &mut event,
        &fixture.keystore,
        &fixture.agent_key_handle,
        &fixture.remote_agent_did,
    );

    let response = request_json(
        router,
        Request::post("/agent-events")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(event.to_string()))
            .expect("request"),
    )
    .await;

    let error = response["error"].as_str().unwrap_or_default();
    assert!(
        error.contains("payment_address does not match"),
        "expected sender address mismatch error, got {response}"
    );
    let ledger = state.payment_ledger.lock().await;
    assert!(
        ledger.get(payment_id).is_none(),
        "rejected event must not reach the ledger"
    );
}
