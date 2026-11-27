//! Who may take a mission, and item trades carried by one.
//!
//! The gateway keeps the rules and every mission's status: the agent a mission is reserved
//! for, the claim bans, the items pledged and the deadlines of that exchange. This node names
//! the agent a publisher reserves a mission for, signs its agent's pledge and the publisher's
//! rejection, and reads the gateway's guide to the market.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use wattetheria_kernel::civilization::missions::MissionScope;
use wattetheria_kernel::identity::fingerprint_from_did_key;

use crate::auth::{authorize, internal_error};
use crate::ledger_client::{LedgerClient, MissionAction};
use crate::routes::core::{MissionReview, review_mission_claim, review_mission_result};
use crate::routes::gateway_missions::{self, GatewayMissionError};
use crate::routes::missions::agent_display_name_for_did;
use crate::state::ControlPlaneState;

/// The mission tools in the order an agent uses them, returned with the guide. Each line says
/// what the tool is for and nothing about what the rules make of it: the rules are the
/// gateway's to state and to change.
const TRADE_TOOLS: [(&str, &str); 10] = [
    (
        "publish_mission",
        "Publish what you want, with the reward you offer and, to reserve it, the agent's name.",
    ),
    ("claim_mission", "Ask to take a mission."),
    (
        "reject_mission_claim",
        "As the publisher, turn down an agent that asked to take your mission.",
    ),
    (
        "pledge_mission_items",
        "As the approved claimer, hand the items to the system.",
    ),
    (
        "withdraw_mission_claim",
        "As the agent that took a mission, give it up before handing anything in.",
    ),
    (
        "get_mission",
        "See who holds a mission, what the system holds for it and its deadlines.",
    ),
    (
        "settle_mission",
        "As the publisher, accept what was handed in.",
    ),
    (
        "reject_mission_result",
        "As the publisher, reject what was handed in, which ends the mission.",
    ),
    (
        "cancel_mission",
        "As the publisher, cancel your mission while nobody has taken it.",
    ),
    (
        "mission_guide",
        "The mission market, its rules and your own claim standing.",
    ),
];

pub(crate) fn scope_name(scope: &MissionScope) -> &'static str {
    match scope {
        MissionScope::RealWorld => "real_world",
        MissionScope::InWorld => "in_world",
    }
}

fn refusal(status: StatusCode, code: &str, message: impl Into<String>, details: Value) -> Response {
    let mut body = json!({"error": message.into(), "code": code});
    if details != Value::Null {
        body["details"] = details;
    }
    (status, Json(body)).into_response()
}

fn ledger_unreachable(what: &str, detail: impl std::fmt::Display) -> Response {
    refusal(
        StatusCode::BAD_GATEWAY,
        "gateway_unavailable",
        format!("{what}: the gateway could not be asked ({detail}); try again"),
        Value::Null,
    )
}

fn no_ledger() -> Response {
    refusal(
        StatusCode::SERVICE_UNAVAILABLE,
        "gateway_unavailable",
        "this needs a gateway and this node has none configured",
        Value::Null,
    )
}

/// The gateway's answer handed on as it is, status included.
fn relay(status: StatusCode, body: Value) -> Response {
    (status, Json(body)).into_response()
}

/// The agent a publisher named for a mission.
pub(crate) struct Assignee {
    pub(crate) did: String,
    pub(crate) display_name: Option<String>,
}

/// Finds the agent a publisher named, by display name (names are unique in the registry and an
/// agent whose node is offline is found too) or by `did:key`. The DID is what the mission
/// keeps. Refused when nobody carries the name, or when it is the publisher itself.
pub(crate) async fn resolve_assignee(
    state: &ControlPlaneState,
    name: &str,
) -> Result<Assignee, Response> {
    let name = name.trim().trim_start_matches('@').trim();
    if name.is_empty() {
        return Err(refusal(
            StatusCode::BAD_REQUEST,
            "assignee_invalid",
            "assignee must be an agent's display name",
            Value::Null,
        ));
    }
    let assignee = if name.starts_with("did:") {
        if fingerprint_from_did_key(name).is_err() {
            return Err(refusal(
                StatusCode::BAD_REQUEST,
                "assignee_invalid",
                "assignee DID must be a valid agent did:key",
                Value::Null,
            ));
        }
        Assignee {
            did: name.to_owned(),
            display_name: agent_display_name_for_did(state, name).await,
        }
    } else {
        let agents = state
            .swarm_bridge
            .search_agent_display_name(name, None)
            .await
            .map_err(|error| internal_error(&error))?;
        let Some(agent) = agents
            .into_iter()
            .find(|agent| fingerprint_from_did_key(&agent.target_agent_did).is_ok())
        else {
            return Err(refusal(
                StatusCode::NOT_FOUND,
                "assignee_not_found",
                format!(
                    "no agent named {name} was found, so the mission was not published; check the name"
                ),
                json!({"assignee": name}),
            ));
        };
        Assignee {
            did: agent.target_agent_did,
            display_name: Some(agent.display_name.unwrap_or_else(|| name.to_owned())),
        }
    };
    if assignee.did == state.identity.agent_did || assignee.did == state.agent_did {
        return Err(refusal(
            StatusCode::BAD_REQUEST,
            "assignee_is_publisher",
            "a mission cannot be reserved for the agent that publishes it",
            Value::Null,
        ));
    }
    Ok(assignee)
}

fn body_string(body: &Value, field: &str) -> Option<String> {
    body[field]
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

async fn response_json(response: Response) -> (StatusCode, Value) {
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap_or_default();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

/// `POST /v1/wattetheria/missions/{mission_id}/pledge`: this node's agent, the mission's
/// approved claimer, hands `units` of `item` from its `kind` bag to the gateway, then reports
/// the mission completed with what it pledged so its publisher is told. The gateway's refusal
/// comes back as it is.
pub(crate) async fn mission_pledge_by_id(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Path(mission_id): Path<String>,
    Json(mut body): Json<Value>,
) -> Response {
    if let Err(response) = authorize(&state, &headers).await {
        return response;
    }
    let (Some(kind), Some(item), Some(units)) = (
        body_string(&body, "kind"),
        body_string(&body, "item"),
        body["units"].as_i64(),
    ) else {
        return refusal(
            StatusCode::BAD_REQUEST,
            "invalid",
            "pledge needs kind, item and units",
            Value::Null,
        );
    };
    let pledged = match gateway_missions::command(
        &state,
        &mission_id,
        MissionAction::Pledge,
        json!({"kind": kind, "item": item, "units": units}),
    )
    .await
    {
        Ok(pledged) => gateway_missions::mission_json_or_record(&pledged),
        Err(error) => return error.into_response(),
    };
    // The pledge is the delivery: the mission is reported completed so its publisher is told
    // and can look at what is held.
    if let Some(object) = body.as_object_mut() {
        for field in ["kind", "item", "units"] {
            object.remove(field);
        }
        object
            .entry("agent_did")
            .or_insert_with(|| json!(state.identity.agent_did));
        object
            .entry("result")
            .or_insert_with(|| json!({"pledged": {"kind": kind, "item": item, "units": units}}));
    }
    let completion = crate::routes::missions::mission_complete_by_id(
        State(state),
        headers,
        Path(mission_id.clone()),
        Json(body),
    )
    .await;
    let (status, completion) = response_json(completion).await;
    if status.is_success() {
        return Json(
            json!({"mission_id": mission_id, "mission": pledged, "completion": completion}),
        )
        .into_response();
    }
    Json(json!({
        "mission_id": mission_id,
        "mission": pledged,
        "completion_error": completion,
        "notice": "The items are held by the system, but the publisher was not told. Call complete_mission for this mission so its publisher is told.",
    }))
    .into_response()
}

/// The publisher rejects what the claimer handed in (`reject_result`): the
/// gateway ends the mission as rejected, the Watt going back to the publisher and held items
/// to the claimer. Work still wanted is published as a new mission. Returns the mission as the
/// gateway keeps it, or `None` when nothing was handed in on the gateway: the decision is then
/// only the answer to the claimer.
pub(crate) async fn reject_hand_in(
    state: &ControlPlaneState,
    mission_id: &str,
    reason: Option<&str>,
) -> Result<Option<Value>, Response> {
    match gateway_missions::command(
        state,
        mission_id,
        MissionAction::Reject,
        json!({"reason": reason}),
    )
    .await
    {
        Ok(mission) => Ok(Some(gateway_missions::mission_json_or_record(&mission))),
        Err(GatewayMissionError::Refused(_, body))
            if body["code"].as_str() == Some("nothing_handed_in") =>
        {
            Ok(None)
        }
        Err(error) => Err(error.into_response()),
    }
}

/// The review a tool asks for: the agent names the mission and, at most, the claimer, its node
/// and a reason; the rest is what the brain's decision on the event would carry.
fn review_from_tool(
    action: &str,
    mission_id: String,
    agent_did: String,
    target_node_id: Option<String>,
    body: &Value,
) -> MissionReview {
    MissionReview {
        action: action.to_owned(),
        task_id: mission_id.clone(),
        execution_id: format!("wattetheria:{mission_id}:{agent_did}"),
        reason: body_string(body, "reason"),
        target_agent_id: Some(agent_did.clone()),
        target_node_id,
        context: Value::Null,
        mission_id,
        agent_did,
    }
}

/// `POST /v1/wattetheria/missions/{mission_id}/claims/reject` (`reject_mission_claim`): the
/// publisher turns down `agent_did`'s claim, the same as its `reject_claim` decision on the
/// claim event. The gateway's refusal comes back as it is.
pub(crate) async fn mission_claim_reject_by_id(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Path(mission_id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let Some(agent_did) = body_string(&body, "agent_did") else {
        return refusal(
            StatusCode::BAD_REQUEST,
            "invalid",
            "rejecting a claim needs agent_did: the agent whose claim is turned down",
            Value::Null,
        );
    };
    // Without the claimer's node, this node stands in, as it does when a claim is approved.
    let claimer_node_id = match body_string(&body, "claimer_node_id") {
        Some(node_id) => Some(node_id),
        None => state.swarm_bridge.local_node_id().await.ok(),
    };
    let review = review_from_tool(
        "reject_claim",
        mission_id,
        agent_did,
        claimer_node_id,
        &body,
    );
    review_mission_claim(&state, auth, review).await
}

/// `POST /v1/wattetheria/missions/{mission_id}/reject` (`reject_mission_result`): the publisher
/// rejects what the claimer handed in, the same as its `reject_result` decision on the result
/// event. Refused when nothing was handed in.
pub(crate) async fn mission_reject_by_id(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Path(mission_id): Path<String>,
    Json(body): Json<Value>,
) -> Response {
    let auth = match authorize(&state, &headers).await {
        Ok(token) => token,
        Err(response) => return response,
    };
    let record = match gateway_missions::fetch(&state, &mission_id).await {
        Ok(Some(record)) => record,
        Ok(None) => {
            return refusal(
                StatusCode::NOT_FOUND,
                "not_found",
                format!("mission {mission_id} not found"),
                Value::Null,
            );
        }
        Err(error) => return error.into_response(),
    };
    let Some(agent_did) = body_string(&record, "claimer")
        .filter(|_| gateway_missions::status(&record) == "completed")
    else {
        return refusal(
            StatusCode::CONFLICT,
            "nothing_handed_in",
            "nothing has been handed in for this mission, so there is nothing to reject",
            Value::Null,
        );
    };
    let claimer_node_id = body_string(&body, "claimer_node_id");
    let review = review_from_tool(
        "reject_result",
        mission_id,
        agent_did,
        claimer_node_id,
        &body,
    );
    review_mission_result(&state, auth, review).await
}

/// `GET /v1/wattetheria/missions/guide`: the gateway's guide to the mission market (what it is,
/// the rules of each scope, the numbers in force), with this agent's own claim standing and
/// the mission tools in the order an agent uses them.
pub(crate) async fn mission_guide(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, &headers).await {
        return response;
    }
    let Some(ledger) = LedgerClient::from_state(&state) else {
        return no_ledger();
    };
    let (status, mut body) = match ledger.get_public("/api/missions/guide", &[]).await {
        Ok(reply) => reply,
        Err(error) => return ledger_unreachable("mission guide not read", format!("{error:#}")),
    };
    if status.is_success() {
        if let Ok((standing_status, standing)) =
            ledger.claim_standing(&state.identity.agent_did).await
            && standing_status.is_success()
        {
            body["standing"] = standing;
        }
        body["tools"] = TRADE_TOOLS
            .iter()
            .map(|(name, use_for)| json!({"tool": name, "use_for": use_for}))
            .collect();
    }
    relay(status, body)
}
