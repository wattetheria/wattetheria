use crate::routes::client_api::{SignedPublicClientSnapshot, build_signed_public_client_snapshot};
use crate::state::{ClientExportQuery, ControlPlaneState, StreamEvent};
use anyhow::{Context, Result};
use reqwest::Client;
use serde_json::Value;
pub use wattetheria_gateway_contract::{
    DataKind as GatewayDataKind, EventScope as GatewayEventScope,
    NodeEventPayload as GatewayNodeEventPayload,
    ProvisionalExportPolicy as GatewayProvisionalExportPolicy,
    SignedNodeEvent as SignedGatewayNodeEvent, Visibility as GatewayVisibility,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayDispatchPlan {
    pub data_kind: GatewayDataKind,
    pub visibility: GatewayVisibility,
    pub provisional_policy: GatewayProvisionalExportPolicy,
    pub scope: GatewayEventScope,
    pub identity_key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayDispatchDecision {
    pub data_kind: Option<GatewayDataKind>,
    pub mechanism_path: GatewayMechanismPath,
    pub push_disposition: GatewayPushDisposition,
    pub confirmation_requirement: GatewayConfirmationRequirement,
    pub visibility: GatewayVisibility,
    pub provisional_policy: GatewayProvisionalExportPolicy,
    pub scope: GatewayEventScope,
    pub identity_key: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GatewayMechanismPath {
    DirectProjection,
    WattswarmFirst,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GatewayPushDisposition {
    PushEligible,
    PullOnlyFallback,
    NotPubliclyExported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GatewayConfirmationRequirement {
    Required,
    NotRequired,
}

pub fn plan_stream_event(event: &StreamEvent) -> Option<GatewayDispatchPlan> {
    let decision = describe_stream_event(event)?;
    if decision.push_disposition != GatewayPushDisposition::PushEligible
        || decision.mechanism_path != GatewayMechanismPath::DirectProjection
    {
        return None;
    }
    Some(GatewayDispatchPlan {
        data_kind: decision.data_kind?,
        visibility: decision.visibility,
        provisional_policy: decision.provisional_policy,
        scope: decision.scope,
        identity_key: decision.identity_key,
    })
}

pub fn describe_stream_event(event: &StreamEvent) -> Option<GatewayDispatchDecision> {
    let kind = event.kind.as_str();
    let payload = &event.payload;
    if let Some(plan) = plan_mission_event(kind, payload)
        .or_else(|| plan_identity_event(kind, payload))
        .or_else(|| plan_organization_event(kind, payload))
        .or_else(|| plan_governance_event(kind, payload))
        .or_else(|| plan_topic_event(kind, payload))
        .or_else(|| plan_public_block_event(kind, payload))
        .or_else(|| plan_galaxy_event(kind, payload))
    {
        return Some(GatewayDispatchDecision {
            data_kind: Some(plan.data_kind),
            mechanism_path: GatewayMechanismPath::DirectProjection,
            push_disposition: GatewayPushDisposition::PushEligible,
            confirmation_requirement: if matches!(
                plan.provisional_policy,
                GatewayProvisionalExportPolicy::NeverBeforeConfirmation
            ) {
                GatewayConfirmationRequirement::Required
            } else {
                GatewayConfirmationRequirement::NotRequired
            },
            visibility: plan.visibility,
            provisional_policy: plan.provisional_policy,
            scope: plan.scope,
            identity_key: plan.identity_key,
        });
    }

    non_push_stream_decision(kind, payload)
}

fn non_push_stream_decision(kind: &str, payload: &Value) -> Option<GatewayDispatchDecision> {
    match kind {
        "topic.subscription.updated" => Some(topic_non_push_decision(
            payload,
            GatewayDataKind::HiveMetadata,
            GatewayPushDisposition::PullOnlyFallback,
            GatewayProvisionalExportPolicy::NeverBeforeConfirmation,
        )),
        "topic.message.posted" => Some(topic_non_push_decision(
            payload,
            GatewayDataKind::HiveMessagePosted,
            GatewayPushDisposition::NotPubliclyExported,
            GatewayProvisionalExportPolicy::EphemeralOnly,
        )),
        "civilization.agent_relationship.command" => {
            Some(private_non_push_decision(payload, "remote_node_id"))
        }
        "civilization.agent_dm.command" => Some(private_non_push_decision(payload, "thread_id")),
        _ => None,
    }
}

fn topic_non_push_decision(
    payload: &Value,
    data_kind: GatewayDataKind,
    push_disposition: GatewayPushDisposition,
    provisional_policy: GatewayProvisionalExportPolicy,
) -> GatewayDispatchDecision {
    GatewayDispatchDecision {
        data_kind: Some(data_kind),
        mechanism_path: GatewayMechanismPath::WattswarmFirst,
        push_disposition,
        confirmation_requirement: GatewayConfirmationRequirement::Required,
        visibility: GatewayVisibility::Public,
        provisional_policy,
        scope: GatewayEventScope {
            node_id: None,
            topic_id: payload
                .get("topic_id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            organization_id: payload
                .get("organization_id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            task_id: None,
        },
        identity_key: payload
            .get(match data_kind {
                GatewayDataKind::HiveMetadata => "topic_id",
                _ => "message_id",
            })
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
    }
}

fn private_non_push_decision(payload: &Value, identity_field: &str) -> GatewayDispatchDecision {
    GatewayDispatchDecision {
        data_kind: None,
        mechanism_path: GatewayMechanismPath::WattswarmFirst,
        push_disposition: GatewayPushDisposition::NotPubliclyExported,
        confirmation_requirement: GatewayConfirmationRequirement::Required,
        visibility: GatewayVisibility::Protected,
        provisional_policy: GatewayProvisionalExportPolicy::EphemeralOnly,
        scope: GatewayEventScope::default(),
        identity_key: payload
            .get(identity_field)
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
    }
}

pub fn build_signed_node_event(
    state: &ControlPlaneState,
    event: &StreamEvent,
) -> Result<Option<SignedGatewayNodeEvent>> {
    let Some(plan) = plan_stream_event(event) else {
        return Ok(None);
    };
    let seq = state.next_gateway_event_seq();
    let payload = GatewayNodeEventPayload {
        event_id: format!("{}:{seq}", state.agent_did),
        node_id: state.agent_did.clone(),
        public_key: state.identity.public_key.clone(),
        signer_agent_did: state.identity.agent_did.clone(),
        seq,
        timestamp: event.timestamp,
        data_kind: plan.data_kind,
        event_kind: event.kind.clone(),
        visibility: plan.visibility,
        provisional_policy: plan.provisional_policy,
        scope: plan.scope,
        identity_key: plan.identity_key,
        payload: event.payload.clone(),
    };
    let signature = state.sign_payload(&payload)?;
    Ok(Some(SignedGatewayNodeEvent { payload, signature }))
}

pub async fn push_signed_node_event(
    client: &Client,
    gateway_url: &str,
    event: &SignedGatewayNodeEvent,
) -> Result<()> {
    client
        .post(normalized_gateway_event_ingest_url(gateway_url))
        .json(event)
        .send()
        .await
        .context("push gateway node event")?
        .error_for_status()
        .context("gateway node event ingest returned error status")?;
    Ok(())
}

pub async fn push_signed_snapshot(
    client: &Client,
    gateway_url: &str,
    state: &ControlPlaneState,
    query: &ClientExportQuery,
) -> Result<SignedPublicClientSnapshot> {
    let snapshot = build_signed_public_client_snapshot(state, query).await?;
    client
        .post(normalized_gateway_snapshot_ingest_url(gateway_url))
        .json(&snapshot)
        .send()
        .await
        .context("push gateway snapshot")?
        .error_for_status()
        .context("gateway snapshot ingest returned error status")?;
    Ok(snapshot)
}

pub fn normalized_gateway_snapshot_ingest_url(gateway_url: &str) -> String {
    normalized_gateway_ingest_url(gateway_url, "/api/ingest/snapshot")
}

pub fn normalized_gateway_event_ingest_url(gateway_url: &str) -> String {
    normalized_gateway_ingest_url(gateway_url, "/api/ingest/event")
}

fn normalized_gateway_ingest_url(gateway_url: &str, suffix: &str) -> String {
    let trimmed = gateway_url.trim_end_matches('/');
    if trimmed.ends_with(suffix) {
        trimmed.to_string()
    } else {
        format!("{trimmed}{suffix}")
    }
}

fn public_subject_key(payload: &Value) -> Option<String> {
    payload
        .pointer("/memory_owner/subject")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .or_else(|| {
            payload
                .pointer("/public_memory_owner/public")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
}

fn organization_id_from_payload(payload: &Value) -> Option<String> {
    payload
        .pointer("/data/organization/organization_id")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .or_else(|| {
            payload
                .get("organization_id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
}

fn proposal_id_from_payload(payload: &Value) -> Option<String> {
    payload
        .get("proposal_id")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .or_else(|| {
            payload
                .pointer("/proposal/proposal_id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
        })
}

fn plan_mission_event(kind: &str, payload: &Value) -> Option<GatewayDispatchPlan> {
    match kind {
        "mission.published" | "mission.claimed" | "mission.completed" | "mission.settled" => {
            Some(GatewayDispatchPlan {
                data_kind: GatewayDataKind::MissionLifecycle,
                visibility: GatewayVisibility::Public,
                provisional_policy: GatewayProvisionalExportPolicy::NeverBeforeConfirmation,
                scope: GatewayEventScope {
                    node_id: None,
                    topic_id: None,
                    organization_id: organization_id_from_payload(payload),
                    task_id: payload
                        .get("mission_id")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned),
                },
                identity_key: payload
                    .get("mission_id")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
            })
        }
        "organization.mission.published" => Some(GatewayDispatchPlan {
            data_kind: GatewayDataKind::MissionLifecycle,
            visibility: GatewayVisibility::Public,
            provisional_policy: GatewayProvisionalExportPolicy::NeverBeforeConfirmation,
            scope: GatewayEventScope {
                node_id: None,
                topic_id: None,
                organization_id: payload
                    .pointer("/memory_owner/subject")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
                    .or_else(|| organization_id_from_payload(payload)),
                task_id: payload
                    .pointer("/data/mission/mission_id")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
            },
            identity_key: payload
                .pointer("/data/mission/mission_id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
        }),
        _ => None,
    }
}

fn plan_identity_event(kind: &str, payload: &Value) -> Option<GatewayDispatchPlan> {
    match kind {
        "civilization.public_identity.updated"
        | "civilization.controller_binding.updated"
        | "civilization.identity.bootstrapped" => Some(GatewayDispatchPlan {
            data_kind: GatewayDataKind::Identity,
            visibility: GatewayVisibility::Public,
            provisional_policy: GatewayProvisionalExportPolicy::NeverBeforeConfirmation,
            scope: GatewayEventScope::default(),
            identity_key: public_subject_key(payload),
        }),
        "civilization.profile.updated" => Some(GatewayDispatchPlan {
            data_kind: GatewayDataKind::OperatorProfile,
            visibility: GatewayVisibility::Public,
            provisional_policy: GatewayProvisionalExportPolicy::NeverBeforeConfirmation,
            scope: GatewayEventScope::default(),
            identity_key: public_subject_key(payload),
        }),
        _ => None,
    }
}

fn plan_organization_event(kind: &str, payload: &Value) -> Option<GatewayDispatchPlan> {
    match kind {
        "organization.created"
        | "organization.member.updated"
        | "organization.treasury.funded"
        | "organization.treasury.spent"
        | "organization.policy.updated"
        | "organization.subnet.updated" => Some(GatewayDispatchPlan {
            data_kind: GatewayDataKind::OrganizationSummary,
            visibility: GatewayVisibility::Public,
            provisional_policy: GatewayProvisionalExportPolicy::NeverBeforeConfirmation,
            scope: GatewayEventScope {
                node_id: None,
                topic_id: None,
                organization_id: organization_id_from_payload(payload),
                task_id: None,
            },
            identity_key: organization_id_from_payload(payload),
        }),
        _ => None,
    }
}

fn plan_governance_event(kind: &str, payload: &Value) -> Option<GatewayDispatchPlan> {
    let data_kind = match kind {
        "governance.proposal.created" => GatewayDataKind::GovernanceProposal,
        "governance.proposal.voted" => GatewayDataKind::GovernanceVote,
        "governance.proposal.finalized" => GatewayDataKind::GovernanceDecision,
        _ => return None,
    };
    Some(GatewayDispatchPlan {
        data_kind,
        visibility: GatewayVisibility::Public,
        provisional_policy: GatewayProvisionalExportPolicy::NeverBeforeConfirmation,
        scope: GatewayEventScope::default(),
        identity_key: proposal_id_from_payload(payload),
    })
}

fn plan_topic_event(kind: &str, payload: &Value) -> Option<GatewayDispatchPlan> {
    match kind {
        "topic.created" => Some(GatewayDispatchPlan {
            data_kind: GatewayDataKind::HiveMetadata,
            visibility: GatewayVisibility::Public,
            provisional_policy: GatewayProvisionalExportPolicy::NeverBeforeConfirmation,
            scope: GatewayEventScope {
                node_id: None,
                topic_id: payload
                    .pointer("/topic/topic_id")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
                organization_id: payload
                    .pointer("/topic/organization_id")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
                task_id: payload
                    .pointer("/topic/mission_id")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
            },
            identity_key: payload
                .pointer("/topic/topic_id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
        }),
        _ => None,
    }
}

fn plan_public_block_event(kind: &str, payload: &Value) -> Option<GatewayDispatchPlan> {
    if kind != "civilization.relationship.updated"
        || payload.get("relationship_state").and_then(Value::as_str) != Some("blocked")
    {
        return None;
    }
    Some(GatewayDispatchPlan {
        data_kind: GatewayDataKind::PublicBlock,
        visibility: GatewayVisibility::Public,
        provisional_policy: GatewayProvisionalExportPolicy::NeverBeforeConfirmation,
        scope: GatewayEventScope::default(),
        identity_key: payload
            .get("counterpart_public_id")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned),
    })
}

fn plan_galaxy_event(kind: &str, payload: &Value) -> Option<GatewayDispatchPlan> {
    let (data_kind, identity_key) = match kind {
        "galaxy.event.published" | "galaxy.events.generated" => (
            GatewayDataKind::GalaxyEvent,
            payload
                .get("event_id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
        ),
        "galaxy.travel.departed" | "galaxy.travel.arrived" => (
            GatewayDataKind::TravelState,
            payload
                .pointer("/memory_owner/subject")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
                .or_else(|| {
                    payload
                        .pointer("/data/travel_state/public_id")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned)
                }),
        ),
        _ => return None,
    };
    Some(GatewayDispatchPlan {
        data_kind,
        visibility: GatewayVisibility::Public,
        provisional_policy: GatewayProvisionalExportPolicy::NeverBeforeConfirmation,
        scope: GatewayEventScope::default(),
        identity_key,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn mission_events_become_public_gateway_events() {
        let event = StreamEvent {
            kind: "mission.published".to_string(),
            timestamp: 1_710_000_000,
            payload: json!({
                "mission_id": "mission-1",
                "organization_id": "org-1",
            }),
        };
        let plan = plan_stream_event(&event).expect("mission plan");
        assert_eq!(plan.data_kind, GatewayDataKind::MissionLifecycle);
        assert_eq!(plan.scope.organization_id.as_deref(), Some("org-1"));
        assert_eq!(plan.scope.task_id.as_deref(), Some("mission-1"));
    }

    #[test]
    fn citizen_published_missions_do_not_use_publisher_as_organization_scope() {
        let event = StreamEvent {
            kind: "mission.published".to_string(),
            timestamp: 1_710_000_000,
            payload: json!({
                "mission_id": "mission-1",
                "publisher": "Citizen-citizen-b2HM",
            }),
        };
        let plan = plan_stream_event(&event).expect("mission plan");
        assert_eq!(plan.scope.organization_id, None);
        assert_eq!(plan.scope.task_id.as_deref(), Some("mission-1"));
    }

    #[test]
    fn topic_message_posts_are_not_gateway_pushed_directly() {
        let event = StreamEvent {
            kind: "topic.message.posted".to_string(),
            timestamp: 1_710_000_000,
            payload: json!({"feed_key":"topic"}),
        };
        assert!(plan_stream_event(&event).is_none());
        let decision = describe_stream_event(&event).expect("topic decision");
        assert_eq!(
            decision.mechanism_path,
            GatewayMechanismPath::WattswarmFirst
        );
        assert_eq!(
            decision.push_disposition,
            GatewayPushDisposition::NotPubliclyExported
        );
        assert_eq!(
            decision.confirmation_requirement,
            GatewayConfirmationRequirement::Required
        );
    }

    #[test]
    fn topic_subscription_updates_are_pull_only_fallback() {
        let event = StreamEvent {
            kind: "topic.subscription.updated".to_string(),
            timestamp: 1,
            payload: json!({"topic_id":"topic-1"}),
        };
        assert!(plan_stream_event(&event).is_none());
        let decision = describe_stream_event(&event).expect("topic subscription decision");
        assert_eq!(
            decision.push_disposition,
            GatewayPushDisposition::PullOnlyFallback
        );
    }

    #[test]
    fn only_public_blocks_are_public_relationship_exports() {
        let block = StreamEvent {
            kind: "civilization.relationship.updated".to_string(),
            timestamp: 1,
            payload: json!({
                "relationship_state": "blocked",
                "counterpart_public_id": "peer-b",
            }),
        };
        let pending = StreamEvent {
            kind: "civilization.relationship.updated".to_string(),
            timestamp: 1,
            payload: json!({
                "relationship_state": "pending_inbound",
                "counterpart_public_id": "peer-c",
            }),
        };
        assert_eq!(
            plan_stream_event(&block).expect("block plan").data_kind,
            GatewayDataKind::PublicBlock
        );
        assert!(plan_stream_event(&pending).is_none());
        assert!(describe_stream_event(&pending).is_none());
    }

    #[test]
    fn normalized_gateway_urls_append_missing_suffix() {
        assert_eq!(
            normalized_gateway_event_ingest_url("https://gw.example"),
            "https://gw.example/api/ingest/event"
        );
        assert_eq!(
            normalized_gateway_snapshot_ingest_url("https://gw.example/api/ingest/snapshot"),
            "https://gw.example/api/ingest/snapshot"
        );
    }

    #[test]
    fn mission_events_are_router_approved_for_direct_gateway_push() {
        let event = StreamEvent {
            kind: "mission.settled".to_string(),
            timestamp: 1,
            payload: json!({
                "mission_id": "mission-2",
                "publisher": "org-2",
            }),
        };
        let decision = describe_stream_event(&event).expect("mission decision");
        assert_eq!(
            decision.mechanism_path,
            GatewayMechanismPath::DirectProjection
        );
        assert_eq!(
            decision.push_disposition,
            GatewayPushDisposition::PushEligible
        );
        assert_eq!(
            decision.confirmation_requirement,
            GatewayConfirmationRequirement::Required
        );
    }
}
