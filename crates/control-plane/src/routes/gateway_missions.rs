//! Missions as the gateway keeps them.
//!
//! The gateway holds every mission and its status; this node keeps none. Reads build a
//! transient view from the gateway's records, and every change is a signed gateway command.
//! A gateway that cannot be asked is an error, never an empty answer: a node must never take
//! an outage for "no missions".

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, TimeZone, Utc};
use serde::de::DeserializeOwned;
use serde_json::{Map, Value, json};
use wattetheria_kernel::civilization::missions::{
    CivilMission, CompletedMissions, CompletedTotals, MissionBoard, MissionPublisherKind,
    MissionReward, MissionStatus,
};
use wattswarm_protocol::types::TaskContract;

use crate::ledger_client::{LedgerClient, MissionAction};
use crate::routes::mission_trade::scope_name;
use crate::state::ControlPlaneState;

pub(crate) const KIND_STANDARD: &str = "standard";
pub(crate) const KIND_DELEGATED: &str = "delegated";
pub(crate) const KIND_ORGANIZATION: &str = "organization";

const DEFAULT_TASK_TYPE: &str = "wattetheria.mission";
const INVOLVING_LIMIT: &str = "500";
const OPEN_LIMIT: &str = "200";

/// Fields of the gateway `payload` that tell another node how to reach the mission on
/// wattswarm, kept at the payload's top level.
pub(crate) const ROUTE_FIELDS: [&str; 8] = [
    "task_id",
    "task_type",
    "task_contract",
    "publisher_wattswarm_node_id",
    "mission_feed_key",
    "mission_scope_hint",
    "swarm_scope",
    "settlement_delegation",
];

/// Why a mission read or command did not go through.
#[derive(Debug)]
pub(crate) enum GatewayMissionError {
    /// This node has no gateway configured.
    NoGateway,
    /// The gateway could not be reached, or its reply could not be read.
    Unreachable(String),
    /// The gateway answered with a refusal; its status and body are kept as they are.
    Refused(StatusCode, Value),
}

impl std::fmt::Display for GatewayMissionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoGateway => f.write_str("missions need a gateway and this node has none"),
            Self::Unreachable(detail) => write!(f, "the gateway could not be asked: {detail}"),
            Self::Refused(status, body) => write!(f, "the gateway refused ({status}): {body}"),
        }
    }
}

impl std::error::Error for GatewayMissionError {}

impl IntoResponse for GatewayMissionError {
    fn into_response(self) -> Response {
        match self {
            Self::NoGateway => (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({
                    "error": "missions need a gateway and this node has none configured",
                    "code": "gateway_unavailable",
                })),
            )
                .into_response(),
            Self::Unreachable(detail) => (
                StatusCode::BAD_GATEWAY,
                Json(json!({
                    "error": format!("the gateway could not be asked ({detail}); try again"),
                    "code": "gateway_unavailable",
                })),
            )
                .into_response(),
            Self::Refused(status, mut body) => {
                // The ledger refuses a bounty above the balance with 402 and no code.
                if status == StatusCode::PAYMENT_REQUIRED
                    && body.is_object()
                    && body.get("code").is_none()
                {
                    body["code"] = json!("insufficient_watt");
                }
                (status, Json(body)).into_response()
            }
        }
    }
}

pub(crate) type MissionResult<T> = Result<T, GatewayMissionError>;

fn gateway(state: &ControlPlaneState) -> MissionResult<LedgerClient> {
    LedgerClient::from_state(state).ok_or(GatewayMissionError::NoGateway)
}

fn answer((status, body): (StatusCode, Value)) -> MissionResult<Value> {
    if status.is_success() {
        Ok(body)
    } else {
        Err(GatewayMissionError::Refused(status, body))
    }
}

/// Sends one signed mission command; the answer is the mission as it stands afterwards.
pub(crate) async fn command(
    state: &ControlPlaneState,
    mission_id: &str,
    action: MissionAction,
    fields: Value,
) -> MissionResult<Value> {
    let reply = gateway(state)?
        .mission_command(state, mission_id, action, fields)
        .await
        .map_err(|error| GatewayMissionError::Unreachable(format!("{error:#}")))?;
    answer(reply)
}

/// One mission as the gateway records it; `None` when the gateway does not know it.
pub(crate) async fn fetch(
    state: &ControlPlaneState,
    mission_id: &str,
) -> MissionResult<Option<Value>> {
    let reply = gateway(state)?
        .mission(mission_id)
        .await
        .map_err(|error| GatewayMissionError::Unreachable(format!("{error:#}")))?;
    if reply.0 == StatusCode::NOT_FOUND {
        return Ok(None);
    }
    answer(reply).map(Some)
}

/// Missions matching `query`, newest first.
pub(crate) async fn list(
    state: &ControlPlaneState,
    query: &[(&str, &str)],
) -> MissionResult<Vec<Value>> {
    let reply = gateway(state)?
        .missions(query)
        .await
        .map_err(|error| GatewayMissionError::Unreachable(format!("{error:#}")))?;
    let body = answer(reply)?;
    body.get("missions")
        .and_then(Value::as_array)
        .cloned()
        .ok_or_else(|| {
            GatewayMissionError::Unreachable("the mission list had no `missions`".to_owned())
        })
}

/// The records of the missions this node's agent takes part in, with every open mission, once
/// each.
pub(crate) async fn view_records(state: &ControlPlaneState) -> MissionResult<Vec<Value>> {
    let agent_did = state.identity.agent_did.clone();
    let mut records = list(
        state,
        &[
            ("involving", agent_did.as_str()),
            ("limit", INVOLVING_LIMIT),
        ],
    )
    .await?;
    records.extend(list(state, &[("status", "published"), ("limit", OPEN_LIMIT)]).await?);
    let mut seen = std::collections::BTreeSet::new();
    records.retain(|record| {
        record
            .get("mission_id")
            .and_then(Value::as_str)
            .is_some_and(|mission_id| seen.insert(mission_id.to_owned()))
    });
    Ok(records)
}

/// What the settled missions this node's agent carried out add up to, per domain. The gateway
/// adds them up over every mission, so none is lost to the row limit of a list read.
async fn completed(state: &ControlPlaneState) -> MissionResult<CompletedMissions> {
    let reply = gateway(state)?
        .missions_completed(&state.identity.agent_did)
        .await
        .map_err(|error| GatewayMissionError::Unreachable(format!("{error:#}")))?;
    let body = answer(reply)?;
    let rows = body
        .get("completed")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            GatewayMissionError::Unreachable("the completed totals had no `completed`".to_owned())
        })?;
    let mut completed = CompletedMissions::default();
    for row in rows {
        // A domain this node does not know is left out, as its missions are left out of a view.
        let Some(domain) = parsed(row, "domain") else {
            continue;
        };
        let number = |field: &str| row.get(field).and_then(Value::as_i64).unwrap_or(0);
        completed.add(
            domain,
            CompletedTotals {
                missions: usize::try_from(number("missions")).unwrap_or(0),
                watt: number("watt"),
                reputation: number("reputation"),
                capacity: number("capacity"),
            },
        );
    }
    Ok(completed)
}

/// The missions this node's agent takes part in, with every open mission, as one view. What
/// the agent completed comes from the gateway's totals, not from the listed missions.
pub(crate) async fn board_view(state: &ControlPlaneState) -> MissionResult<MissionBoard> {
    let (records, completed) = tokio::try_join!(view_records(state), completed(state))?;
    Ok(
        MissionBoard::from_missions(records.iter().filter_map(civil_mission))
            .with_completed(&state.identity.agent_did, completed),
    )
}

/// Whether this node's agent published the mission.
pub(crate) fn is_own(state: &ControlPlaneState, record: &Value) -> bool {
    record.get("publisher").and_then(Value::as_str) == Some(state.identity.agent_did.as_str())
}

/// The gateway status of a mission record.
pub(crate) fn status(record: &Value) -> &str {
    record
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or_default()
}

fn text(value: &Value, field: &str) -> Option<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(ToOwned::to_owned)
}

fn parsed<T: DeserializeOwned>(value: &Value, field: &str) -> Option<T> {
    value
        .get(field)
        .filter(|value| !value.is_null())
        .and_then(|value| serde_json::from_value(value.clone()).ok())
}

fn unix_secs(value: &Value, field: &str) -> Option<i64> {
    value
        .get(field)
        .and_then(Value::as_str)
        .and_then(|text| DateTime::parse_from_rfc3339(text).ok())
        .map(|time| time.timestamp())
}

fn view_status(status: &str) -> MissionStatus {
    match status {
        "published" => MissionStatus::Open,
        "claimed" => MissionStatus::Claimed,
        "completed" => MissionStatus::Completed,
        "settled" => MissionStatus::Settled,
        _ => MissionStatus::Cancelled,
    }
}

fn publisher_kind_for(kind: &str) -> MissionPublisherKind {
    match kind {
        KIND_ORGANIZATION => MissionPublisherKind::Organization,
        _ => MissionPublisherKind::Player,
    }
}

/// The gateway record seen as a `CivilMission`. `None` for a record the node cannot read
/// (no status, or a domain it does not know).
pub(crate) fn civil_mission(record: &Value) -> Option<CivilMission> {
    let gateway_status = record.get("status").and_then(Value::as_str)?;
    let payload = record.get("payload").cloned().unwrap_or(Value::Null);
    let civil = payload.get("civil").cloned().unwrap_or(Value::Null);
    let kind = record
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or(KIND_STANDARD);
    let reward = record.get("reward").cloned().unwrap_or(Value::Null);
    let place = record.get("place").cloned().unwrap_or(Value::Null);
    let claimer = text(record, "claimer");
    let handed_in = matches!(gateway_status, "completed" | "settled");
    let agent_watt = if reward.get("kind").and_then(Value::as_str) == Some("watt") {
        reward.get("amount").and_then(Value::as_i64).unwrap_or(0)
    } else {
        0
    };
    // A mission published with nothing to give has no reward, as it was drafted.
    let reward = Some(MissionReward {
        agent_watt,
        reputation: reward
            .get("reputation")
            .and_then(Value::as_i64)
            .unwrap_or(0),
        capacity: reward.get("capacity").and_then(Value::as_i64).unwrap_or(0),
    })
    .filter(|reward| reward.agent_watt != 0 || reward.reputation != 0 || reward.capacity != 0);
    Some(CivilMission {
        mission_id: text(record, "mission_id")?,
        title: text(record, "title").unwrap_or_default(),
        description: text(record, "description").unwrap_or_default(),
        publisher: text(&civil, "publisher").or_else(|| text(record, "publisher"))?,
        publisher_kind: parsed(&civil, "publisher_kind")
            .unwrap_or_else(|| publisher_kind_for(kind)),
        domain: parsed(record, "domain")?,
        scope: parsed(record, "scope").unwrap_or_default(),
        subnet_id: text(&civil, "subnet_id"),
        zone_id: text(&civil, "zone_id"),
        required_role: parsed(&civil, "required_role"),
        required_faction: parsed(&civil, "required_faction"),
        reward,
        payload: payload.get("body").cloned().unwrap_or(Value::Null),
        lat: place.get("lat").and_then(Value::as_f64),
        lng: place.get("lng").and_then(Value::as_f64),
        coordinate_source: text(&place, "source"),
        city: text(&place, "city"),
        country: text(&place, "country"),
        created_at: unix_secs(record, "created_at").unwrap_or_default(),
        updated_at: unix_secs(record, "updated_at").unwrap_or_default(),
        claimed_by: claimer.clone(),
        claimed_by_display_name: text(record, "claimer_label"),
        completed_by: claimer.filter(|_| handed_in),
        completion_result: record
            .get("result")
            .filter(|value| !value.is_null())
            .cloned(),
        settled_at: unix_secs(record, "settled_at"),
        status: view_status(gateway_status),
        expires_at: unix_secs(record, "expires_at"),
        assignee_did: text(record, "assignee"),
        assignee_display_name: text(record, "assignee_label"),
    })
}

/// What a node shows of a mission: the `CivilMission` view, with what only the gateway knows
/// (its own status, the reward terms, what it holds, the route to the publisher).
pub(crate) fn mission_json(record: &Value) -> Option<Value> {
    let mission = civil_mission(record)?;
    let mut view = serde_json::to_value(&mission).ok()?;
    let object = view.as_object_mut()?;
    object.insert("gateway_status".to_owned(), json!(status(record)));
    object.insert(
        "reward_terms".to_owned(),
        record.get("reward").cloned().unwrap_or(Value::Null),
    );
    for field in [
        "kind",
        "publisher_label",
        "organization_id",
        "claimed_at",
        "pledge_deadline",
        "delivered_at",
        "held",
        "ended_at",
        "takers",
    ] {
        if let Some(value) = record.get(field).filter(|value| !value.is_null()) {
            object.insert(field.to_owned(), value.clone());
        }
    }
    if let Some(publisher) = record.get("publisher") {
        object.insert("publisher_agent_did".to_owned(), publisher.clone());
    }
    if let Some(payload) = record.get("payload") {
        for field in ROUTE_FIELDS {
            if let Some(value) = payload.get(field).filter(|value| !value.is_null()) {
                object.insert(field.to_owned(), value.clone());
            }
        }
    }
    Some(view)
}

/// One record as the node shows it; a record it cannot read is shown as the gateway sent it.
pub(crate) fn mission_json_or_record(record: &Value) -> Value {
    mission_json(record).unwrap_or_else(|| record.clone())
}

/// How a mission is put on the gateway, besides the mission itself.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PublishTerms<'a> {
    pub(crate) kind: &'a str,
    pub(crate) organization_id: Option<&'a str>,
    pub(crate) publisher_label: Option<&'a str>,
}

impl<'a> PublishTerms<'a> {
    pub(crate) const fn kind(kind: &'a str) -> Self {
        Self {
            kind,
            organization_id: None,
            publisher_label: None,
        }
    }
}

fn rfc3339(unix_secs: i64) -> Option<String> {
    Utc.timestamp_opt(unix_secs, 0)
        .single()
        .map(|time| time.to_rfc3339())
}

/// The fields of a gateway Publish command for `mission`. The node-only fields travel in the
/// gateway `payload`: `civil` (who published it and where it belongs), `body` (the mission's
/// own payload) and the route fields, taken from `contract` when the mission is on wattswarm.
/// None of them names the mission: the gateway makes its id and writes it in.
pub(crate) fn publish_fields(
    mission: &CivilMission,
    contract: Option<&TaskContract>,
    terms: &PublishTerms<'_>,
) -> Value {
    let mut payload = Map::new();
    payload.insert(
        "civil".to_owned(),
        json!({
            "publisher": mission.publisher,
            "publisher_kind": mission.publisher_kind,
            "subnet_id": mission.subnet_id,
            "zone_id": mission.zone_id,
            "required_role": mission.required_role,
            "required_faction": mission.required_faction,
        }),
    );
    payload.insert("body".to_owned(), mission.payload.clone());
    payload.insert(
        "task_type".to_owned(),
        json!(
            mission
                .payload
                .get("task_type")
                .and_then(Value::as_str)
                .unwrap_or(DEFAULT_TASK_TYPE)
        ),
    );
    if let Some(delegation) = mission.payload.get("settlement_delegation") {
        payload.insert("settlement_delegation".to_owned(), delegation.clone());
    }
    if let Some(contract) = contract {
        payload.insert("task_contract".to_owned(), json!(contract));
        for field in ROUTE_FIELDS {
            if let Some(value) = contract.inputs.get(field) {
                payload.insert(field.to_owned(), value.clone());
            }
        }
    }
    let reward = mission.reward.clone().unwrap_or(MissionReward {
        agent_watt: 0,
        reputation: 0,
        capacity: 0,
    });
    let reward = json!({
        "kind": if reward.agent_watt > 0 { "watt" } else { "none" },
        "amount": reward.agent_watt,
        "reputation": reward.reputation,
        "capacity": reward.capacity,
    });
    json!({
        "kind": terms.kind,
        "scope": scope_name(&mission.scope),
        "publisher_label": terms.publisher_label,
        "title": mission.title,
        "description": mission.description,
        "domain": mission.domain,
        "payload": Value::Object(payload),
        "reward": reward,
        "assignee": mission.assignee_did,
        "assignee_label": mission.assignee_display_name,
        "organization_id": terms.organization_id,
        "place": {
            "city": mission.city,
            "country": mission.country,
            "lat": mission.lat,
            "lng": mission.lng,
            "source": mission.coordinate_source,
        },
        "expires_at": mission.expires_at.and_then(rfc3339),
    })
}

/// Publishes `mission` on the gateway and returns the record it keeps. The gateway makes the
/// mission's id and writes it into the record and into the contract the record carries;
/// `request_id` names this request, so sending it again never makes a second mission.
pub(crate) async fn publish(
    state: &ControlPlaneState,
    request_id: &str,
    mission: &CivilMission,
    contract: Option<&TaskContract>,
    terms: &PublishTerms<'_>,
) -> MissionResult<Value> {
    let reply = gateway(state)?
        .publish_mission(state, request_id, publish_fields(mission, contract, terms))
        .await
        .map_err(|error| GatewayMissionError::Unreachable(format!("{error:#}")))?;
    answer(reply)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wattetheria_kernel::civilization::missions::{MissionDomain, MissionScope};

    fn draft() -> CivilMission {
        let mut mission = CivilMission::draft(
            "Route cargo",
            "Move supplies",
            "captain-alpha",
            MissionPublisherKind::Organization,
            MissionDomain::Trade,
            MissionScope::InWorld,
            Some("subnet-1".to_owned()),
            None,
            None,
            None,
            Some(MissionReward {
                agent_watt: 12,
                reputation: 2,
                capacity: 1,
            }),
            json!({"cargo": "water"}),
            None,
        );
        mission.expires_at = Some(1_900_000_000);
        mission
    }

    /// What the gateway keeps after a Publish carrying these fields.
    fn recorded(fields: &Value, mission_id: &str, publisher: &str, status: &str) -> Value {
        json!({
            "mission_id": mission_id,
            "kind": fields["kind"],
            "scope": fields["scope"],
            "publisher": publisher,
            "title": fields["title"],
            "description": fields["description"],
            "domain": fields["domain"],
            "payload": fields["payload"],
            "reward": fields["reward"],
            "assignee": fields["assignee"],
            "place": fields["place"],
            "status": status,
            "claimer": "did:key:claimer",
            "claimer_label": "Claimer",
            "result": {"done": true},
            "expires_at": fields["expires_at"],
            "created_at": "2026-11-26T10:00:00Z",
            "updated_at": "2026-11-26T11:00:00Z",
        })
    }

    #[test]
    fn a_published_mission_reads_back_as_the_same_mission() {
        let mission = draft();
        let fields = publish_fields(&mission, None, &PublishTerms::kind(KIND_ORGANIZATION));
        assert_eq!(fields["reward"]["kind"], "watt");
        assert_eq!(fields["reward"]["amount"], 12);
        assert_eq!(fields["scope"], "in_world");
        assert_eq!(fields["payload"]["civil"]["subnet_id"], "subnet-1");
        assert_eq!(fields["payload"]["body"], json!({"cargo": "water"}));
        assert_eq!(fields["payload"]["task_type"], "wattetheria.mission");
        // A draft has no id and the publish names none: the gateway makes it.
        assert_eq!(mission.mission_id, "");
        assert!(fields.get("mission_id").is_none(), "{fields}");
        assert!(fields["payload"].get("task_id").is_none(), "{fields}");

        let view = civil_mission(&recorded(
            &fields,
            "iw_made-by-the-gateway",
            "did:key:publisher",
            "completed",
        ))
        .expect("readable record");
        assert_eq!(view.mission_id, "iw_made-by-the-gateway");
        assert_eq!(view.publisher, "captain-alpha");
        assert_eq!(view.publisher_kind, MissionPublisherKind::Organization);
        assert_eq!(view.status, MissionStatus::Completed);
        assert_eq!(view.claimed_by.as_deref(), Some("did:key:claimer"));
        assert_eq!(view.completed_by.as_deref(), Some("did:key:claimer"));
        assert_eq!(view.completion_result, Some(json!({"done": true})));
        assert_eq!(view.reward, mission.reward);
        assert_eq!(view.payload, mission.payload);
        assert_eq!(view.expires_at, Some(1_900_000_000));
        assert_eq!(view.subnet_id.as_deref(), Some("subnet-1"));
    }

    #[test]
    fn gateway_statuses_map_onto_the_view() {
        for (gateway, view) in [
            ("published", MissionStatus::Open),
            ("claimed", MissionStatus::Claimed),
            ("completed", MissionStatus::Completed),
            ("settled", MissionStatus::Settled),
            ("cancelled", MissionStatus::Cancelled),
            // The view has no status of its own for a rejected hand-in: `gateway_status` says it.
            ("rejected", MissionStatus::Cancelled),
        ] {
            assert_eq!(view_status(gateway), view, "{gateway}");
        }
    }

    #[test]
    fn a_record_without_civil_fields_falls_back_to_the_gateway_publisher() {
        let record = json!({
            "mission_id": "m-2",
            "kind": "standard",
            "scope": "real_world",
            "publisher": "did:key:publisher",
            "title": "Plain",
            "domain": "wealth",
            "payload": {},
            "reward": {"kind": "none", "amount": 0, "reputation": 0, "capacity": 0},
            "place": {},
            "status": "published",
        });
        let view = civil_mission(&record).unwrap();
        assert_eq!(view.publisher, "did:key:publisher");
        assert_eq!(view.publisher_kind, MissionPublisherKind::Player);
        assert_eq!(view.payload, Value::Null);
        let shown = mission_json(&record).unwrap();
        assert_eq!(shown["gateway_status"], "published");
        assert_eq!(shown["kind"], "standard");
        assert_eq!(shown["publisher_agent_did"], "did:key:publisher");
    }
}
