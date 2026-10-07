use serde_json::{Map, Value, json};

use crate::routes::agent_events::AgentEventEnvelope;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum EventKind {
    FriendRequest,
    PaymentRequest,
    PaymentUpdate,
    ThirdPartyResult,
    TaskClaimReceived,
    TaskClaimDecisionReceived,
    TaskResultReceived,
    TaskCompletionDecisionReceived,
    TaskSettledReceived,
    TopicMessageRequiresReply,
}

impl EventKind {
    pub(crate) fn from_source(source: &str) -> Option<Self> {
        match source {
            "friend_request" => Some(Self::FriendRequest),
            "payment_request" => Some(Self::PaymentRequest),
            "payment_update" => Some(Self::PaymentUpdate),
            "third_party_result" => Some(Self::ThirdPartyResult),
            "task_claim_received" => Some(Self::TaskClaimReceived),
            "task_claim_decision_received" => Some(Self::TaskClaimDecisionReceived),
            "task_result_received" => Some(Self::TaskResultReceived),
            "task_completion_decision_received" => Some(Self::TaskCompletionDecisionReceived),
            "task_settled_received" => Some(Self::TaskSettledReceived),
            "topic_message_requires_reply" => Some(Self::TopicMessageRequiresReply),
            _ => None,
        }
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::FriendRequest => "wattetheria.agent.friend_request",
            Self::PaymentRequest => "wattetheria.agent.payment_request",
            Self::PaymentUpdate => "wattetheria.agent.payment_update",
            Self::ThirdPartyResult => "wattetheria.agent.third_party_result",
            Self::TaskClaimReceived => "wattetheria.agent.task_claim_received",
            Self::TaskClaimDecisionReceived => "wattetheria.agent.task_claim_decision_received",
            Self::TaskResultReceived => "wattetheria.agent.task_result_received",
            Self::TaskCompletionDecisionReceived => {
                "wattetheria.agent.task_completion_decision_received"
            }
            Self::TaskSettledReceived => "wattetheria.agent.task_settled_received",
            Self::TopicMessageRequiresReply => "wattetheria.agent.topic_message_requires_reply",
        }
    }

    #[cfg(test)]
    fn payload_fields(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Self::FriendRequest => &[
                ("request_id", "string"),
                ("source_public_id", "string"),
                ("target_public_id", "string"),
            ],
            Self::PaymentRequest | Self::PaymentUpdate => {
                &[("payment_id", "string"), ("payment_status", "string")]
            }
            Self::ThirdPartyResult => &[
                ("agent_id", "string"),
                ("task_id", "string"),
                ("receipt_id", "string"),
                ("result_status", "string"),
            ],
            Self::TaskClaimReceived => &[("task_id", "string"), ("claimer_node_id", "string")],
            Self::TaskClaimDecisionReceived => &[
                ("task_id", "string"),
                ("execution_id", "string"),
                ("approved", "boolean"),
            ],
            Self::TaskResultReceived => &[
                ("task_id", "string"),
                ("execution_id", "string"),
                ("result_kind", "string"),
            ],
            Self::TaskCompletionDecisionReceived => &[
                ("task_id", "string"),
                ("execution_id", "string"),
                ("approved", "boolean"),
                ("retry_requested", "boolean"),
            ],
            Self::TaskSettledReceived => &[
                ("task_id", "string"),
                ("execution_id", "string"),
                ("settlement_status", "string"),
            ],
            Self::TopicMessageRequiresReply => &[
                ("message_id", "string"),
                ("network_id", "string"),
                ("feed_key", "string"),
                ("contentRef", "string"),
                ("contentSummary", "string"),
            ],
        }
    }

    #[cfg(test)]
    fn payload_schema(self) -> Value {
        let mut properties = Map::new();
        for (name, field_type) in [
            ("event_id", "string"),
            ("source_kind", "string"),
            ("source_node_id", "string"),
            ("source_agent_id", "string"),
            ("target_agent_id", "string"),
            ("correlation_id", "string"),
            ("created_at", "integer"),
            ("requires_commit", "boolean"),
            ("decision_status", "string"),
            ("commit_status", "string"),
            ("chosen_action", "string"),
            ("route", "string"),
            ("requires_action", "boolean"),
        ]
        .into_iter()
        .chain(self.payload_fields().iter().copied())
        {
            properties.insert(name.to_owned(), json!({"type": field_type}));
        }
        json!({
            "type": "object",
            "properties": properties,
            "required": [
                "event_id", "source_kind", "created_at", "requires_commit",
                "decision_status", "commit_status", "requires_action"
            ],
            "additionalProperties": false
        })
    }
}

pub(crate) struct ProjectedEvent {
    pub(crate) name: &'static str,
    pub(crate) data: Value,
    pub(crate) full_content: Option<Value>,
}

pub(crate) struct ProjectOutcome {
    pub(crate) decision_status: &'static str,
    pub(crate) commit_status: &'static str,
    pub(crate) chosen_action: Option<String>,
    pub(crate) route: Option<String>,
    pub(crate) requires_action: bool,
}

fn insert_str(data: &mut Map<String, Value>, key: &str, value: Option<&str>) {
    if let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) {
        data.insert(key.to_owned(), Value::String(value.to_owned()));
    }
}

fn insert_payload_str(data: &mut Map<String, Value>, key: &str, payload: &Value, paths: &[&str]) {
    let value = paths
        .iter()
        .find_map(|path| payload.pointer(path).and_then(Value::as_str));
    insert_str(data, key, value);
}

fn insert_payload_bool(data: &mut Map<String, Value>, key: &str, payload: &Value, path: &str) {
    if let Some(value) = payload.pointer(path).and_then(Value::as_bool) {
        data.insert(key.to_owned(), Value::Bool(value));
    }
}

fn project_event_fields(
    kind: EventKind,
    event: &AgentEventEnvelope,
    data: &mut Map<String, Value>,
) -> Option<Value> {
    let payload = &event.payload;
    let mut full_content = None;
    match kind {
        EventKind::FriendRequest => {
            if let Some(envelope) = event.agent_envelope.as_ref() {
                insert_payload_str(data, "request_id", &envelope.message, &["/request_id"]);
                insert_payload_str(
                    data,
                    "source_public_id",
                    &envelope.message,
                    &["/source_public_id"],
                );
                insert_payload_str(
                    data,
                    "target_public_id",
                    &envelope.message,
                    &["/target_public_id"],
                );
            }
        }
        EventKind::PaymentRequest | EventKind::PaymentUpdate => {
            let payment = event
                .agent_envelope
                .as_ref()
                .and_then(|envelope| envelope.message.get("payment"))
                .or_else(|| payload.get("payment"))
                .or_else(|| payload.pointer("/agent_envelope/message/payment"));
            if let Some(payment) = payment {
                insert_payload_str(data, "payment_id", payment, &["/payment_id"]);
                insert_payload_str(data, "payment_status", payment, &["/status"]);
            }
        }
        EventKind::ThirdPartyResult => {
            insert_payload_str(data, "agent_id", payload, &["/agent_id"]);
            insert_payload_str(data, "task_id", payload, &["/task_id"]);
            insert_payload_str(data, "receipt_id", payload, &["/response/receipt_id"]);
            insert_payload_str(data, "result_status", payload, &["/response/status"]);
        }
        EventKind::TaskClaimReceived => {
            insert_payload_str(data, "task_id", payload, &["/task_id"]);
            insert_payload_str(data, "claimer_node_id", payload, &["/claimer_node_id"]);
        }
        EventKind::TaskClaimDecisionReceived => {
            insert_payload_str(data, "task_id", payload, &["/task_id"]);
            insert_payload_str(data, "execution_id", payload, &["/execution_id"]);
            insert_payload_bool(data, "approved", payload, "/approved");
        }
        EventKind::TaskResultReceived => {
            insert_payload_str(data, "task_id", payload, &["/task_id"]);
            insert_payload_str(data, "execution_id", payload, &["/execution_id"]);
            insert_payload_str(data, "result_kind", payload, &["/event_kind"]);
        }
        EventKind::TaskCompletionDecisionReceived => {
            insert_payload_str(data, "task_id", payload, &["/task_id"]);
            insert_payload_str(data, "execution_id", payload, &["/execution_id"]);
            insert_payload_bool(data, "approved", payload, "/approved");
            insert_payload_bool(data, "retry_requested", payload, "/retry_requested");
        }
        EventKind::TaskSettledReceived => {
            insert_payload_str(data, "task_id", payload, &["/task_id"]);
            insert_payload_str(data, "execution_id", payload, &["/execution_id"]);
            insert_payload_str(data, "settlement_status", payload, &["/receipt/status"]);
        }
        EventKind::TopicMessageRequiresReply => {
            insert_payload_str(data, "message_id", payload, &["/message_id"]);
            insert_payload_str(data, "network_id", payload, &["/network_id"]);
            insert_payload_str(data, "feed_key", payload, &["/feed_key"]);
            full_content = event.agent_envelope.as_ref().and_then(|envelope| {
                envelope
                    .message
                    .pointer("/payload/content")
                    .or_else(|| envelope.message.get("content"))
                    .filter(|content| content.is_string())
                    .cloned()
            });
        }
    }

    full_content
}

pub(crate) fn project(
    event: &AgentEventEnvelope,
    outcome: &ProjectOutcome,
) -> Option<ProjectedEvent> {
    let kind = EventKind::from_source(&event.event_type)?;
    let mut data = Map::new();
    data.insert("event_id".to_owned(), Value::String(event.event_id.clone()));
    data.insert(
        "source_kind".to_owned(),
        Value::String(event.source_kind.clone()),
    );
    data.insert("created_at".to_owned(), json!(event.created_at));
    data.insert("requires_commit".to_owned(), json!(event.requires_commit));
    data.insert(
        "decision_status".to_owned(),
        Value::String(outcome.decision_status.to_owned()),
    );
    data.insert(
        "commit_status".to_owned(),
        Value::String(outcome.commit_status.to_owned()),
    );
    data.insert("requires_action".to_owned(), json!(outcome.requires_action));
    insert_str(&mut data, "chosen_action", outcome.chosen_action.as_deref());
    insert_str(&mut data, "route", outcome.route.as_deref());
    insert_str(&mut data, "source_node_id", event.source_node_id.as_deref());
    insert_str(
        &mut data,
        "source_agent_id",
        event
            .agent_envelope
            .as_ref()
            .and_then(|envelope| envelope.source_agent_id.as_deref()),
    );
    insert_str(
        &mut data,
        "target_agent_id",
        event.target_agent_id.as_deref(),
    );
    insert_str(&mut data, "correlation_id", event.correlation_id.as_deref());
    let full_content = project_event_fields(kind, event, &mut data);

    Some(ProjectedEvent {
        name: kind.name(),
        data: Value::Object(data),
        full_content,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use wattetheria_kernel::swarm_bridge::SwarmAgentEnvelope;

    fn test_event(kind: EventKind, payload: Value) -> AgentEventEnvelope {
        AgentEventEnvelope {
            event_id: "evt-1".to_owned(),
            event_type: kind
                .name()
                .trim_start_matches("wattetheria.agent.")
                .to_owned(),
            source_kind: "callback".to_owned(),
            source_node_id: Some("node-1".to_owned()),
            target_agent_id: Some("did:watt:target".to_owned()),
            target_executor: Some("never-export-target-executor".to_owned()),
            agent_envelope: None,
            payload,
            requires_commit: true,
            allowed_actions: vec!["never-export-allowed-action".to_owned()],
            correlation_id: Some("corr-1".to_owned()),
            dedupe_key: Some("never-export-dedupe-key".to_owned()),
            created_at: 42,
        }
    }

    fn test_envelope(message: Value) -> SwarmAgentEnvelope {
        SwarmAgentEnvelope {
            protocol: "google_a2a".to_owned(),
            transport_profile: None,
            source_agent_id: Some("did:watt:source".to_owned()),
            target_agent_id: None,
            source_node_id: None,
            target_node_id: None,
            capability: None,
            source_agent_card: None,
            message,
            extensions: Some(json!({"prompt": "never-export-extension"})),
            signature: Some("never-export-signature".to_owned()),
        }
    }

    fn test_outcome() -> ProjectOutcome {
        ProjectOutcome {
            decision_status: "decided",
            commit_status: "none",
            chosen_action: Some("human_review".to_owned()),
            route: Some("noop".to_owned()),
            requires_action: true,
        }
    }

    #[test]
    fn every_projection_uses_only_fields_declared_by_its_payload_schema() {
        let samples = [
            (EventKind::FriendRequest, json!({})),
            (
                EventKind::PaymentRequest,
                json!({"payment": {"payment_id": "pay-1", "status": "proposed"}}),
            ),
            (
                EventKind::PaymentUpdate,
                json!({"payment": {"payment_id": "pay-1", "status": "settled"}}),
            ),
            (
                EventKind::ThirdPartyResult,
                json!({"agent_id": "agent-1", "task_id": "task-1", "response": {"receipt_id": "receipt-1", "status": "done"}}),
            ),
            (
                EventKind::TaskClaimReceived,
                json!({"task_id": "task-1", "claimer_node_id": "node-2"}),
            ),
            (
                EventKind::TaskClaimDecisionReceived,
                json!({"task_id": "task-1", "execution_id": "exec-1", "approved": true}),
            ),
            (
                EventKind::TaskResultReceived,
                json!({"task_id": "task-1", "execution_id": "exec-1", "event_kind": "task_completed", "output": {"result": "never-export-result"}}),
            ),
            (
                EventKind::TaskCompletionDecisionReceived,
                json!({"task_id": "task-1", "execution_id": "exec-1", "approved": false, "retry_requested": true}),
            ),
            (
                EventKind::TaskSettledReceived,
                json!({"task_id": "task-1", "execution_id": "exec-1", "receipt": {"status": "settled"}}),
            ),
            (
                EventKind::TopicMessageRequiresReply,
                json!({"message_id": "msg-1", "network_id": "network-1", "feed_key": "wattswarm.dm"}),
            ),
        ];

        for (kind, mut payload) in samples {
            payload["secret"] = json!("never-export-payload-secret");
            payload["content"] = json!("never-export-raw-body");
            payload["agent_envelope"] = json!({"signature": "never-export-payload-envelope"});
            let event = test_event(kind, payload);
            let projected = project(&event, &test_outcome()).expect("known type");
            assert_eq!(projected.name, kind.name());
            assert!(projected.full_content.is_none());
            assert_eq!(projected.data["chosen_action"], "human_review");
            assert_eq!(projected.data["route"], "noop");
            assert_eq!(projected.data["requires_action"], true);
            let (field, expected) = match kind {
                EventKind::FriendRequest => ("event_id", json!("evt-1")),
                EventKind::PaymentRequest => ("payment_status", json!("proposed")),
                EventKind::PaymentUpdate => ("payment_status", json!("settled")),
                EventKind::ThirdPartyResult => ("receipt_id", json!("receipt-1")),
                EventKind::TaskClaimReceived => ("claimer_node_id", json!("node-2")),
                EventKind::TaskClaimDecisionReceived => ("approved", json!(true)),
                EventKind::TaskResultReceived => ("result_kind", json!("task_completed")),
                EventKind::TaskCompletionDecisionReceived => ("retry_requested", json!(true)),
                EventKind::TaskSettledReceived => ("settlement_status", json!("settled")),
                EventKind::TopicMessageRequiresReply => ("message_id", json!("msg-1")),
            };
            assert_eq!(projected.data[field], expected);
            let fields = projected.data.as_object().expect("object data");
            let schema = kind.payload_schema();
            let properties = schema["properties"].as_object().expect("properties");
            for (name, value) in fields {
                let field_type = properties[name]["type"].as_str().expect("declared type");
                assert!(match field_type {
                    "string" => value.is_string(),
                    "boolean" => value.is_boolean(),
                    "integer" => value.is_u64(),
                    _ => false,
                });
            }
            for name in schema["required"].as_array().expect("required fields") {
                assert!(fields.contains_key(name.as_str().expect("field name")));
            }
            let serialized = projected.data.to_string();
            for forbidden in [
                "never-export-payload-secret",
                "never-export-raw-body",
                "never-export-payload-envelope",
                "never-export-result",
                "never-export-target-executor",
                "never-export-allowed-action",
                "never-export-dedupe-key",
            ] {
                assert!(!serialized.contains(forbidden), "leaked {forbidden}");
            }
        }
    }

    #[test]
    fn topic_full_content_comes_only_from_signed_envelope_message() {
        let mut event = test_event(
            EventKind::TopicMessageRequiresReply,
            json!({"message_id": "msg-1", "content": "never-export-unsigned-body"}),
        );
        let unsigned = project(&event, &test_outcome()).expect("topic event");
        assert_eq!(unsigned.full_content, None);
        assert!(
            !unsigned
                .data
                .to_string()
                .contains("never-export-unsigned-body")
        );

        event.agent_envelope = Some(test_envelope(json!({
            "payload": {"content": "signed-topic-content"},
            "prompt": "never-export-prompt"
        })));
        let signed = project(&event, &test_outcome()).expect("topic event");
        assert_eq!(signed.full_content, Some(json!("signed-topic-content")));
        assert_eq!(signed.data["source_agent_id"], "did:watt:source");
        assert!(!signed.data.to_string().contains("signed-topic-content"));
        assert!(!signed.data.to_string().contains("never-export-prompt"));
        assert!(!signed.data.to_string().contains("never-export-signature"));
        assert!(!signed.data.to_string().contains("never-export-extension"));

        event.event_type = "unsupported".to_owned();
        assert!(project(&event, &test_outcome()).is_none());
    }

    #[test]
    fn friend_request_id_comes_from_signed_message_not_unsigned_payload() {
        let mut event = test_event(
            EventKind::FriendRequest,
            json!({"request_id": "never-export-unsigned-request"}),
        );
        let unsigned = project(&event, &test_outcome()).expect("friend event");
        assert!(unsigned.data.get("request_id").is_none());

        event.agent_envelope = Some(test_envelope(json!({
            "request_id": "request-1",
            "source_public_id": "public-source",
            "target_public_id": "public-target",
            "prompt": "never-export-prompt"
        })));
        let signed = project(&event, &test_outcome()).expect("friend event");
        assert_eq!(signed.data["request_id"], "request-1");
        assert_eq!(signed.data["source_public_id"], "public-source");
        assert_eq!(signed.data["target_public_id"], "public-target");
        assert!(
            !signed
                .data
                .to_string()
                .contains("never-export-unsigned-request")
        );
        assert!(!signed.data.to_string().contains("never-export-prompt"));
    }
}
