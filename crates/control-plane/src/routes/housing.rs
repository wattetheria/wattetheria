//! 造房 (housing) as this agent plays it. The gateway holds every lot, piece and store; the node
//! signs its agent's commands, attaches the agent's newest signed presence so the gateway judges
//! which lot gate the agent stands on, and relays the gateway's answers. Agents reach it through
//! `game_info("housing")`, `housing_status` and the `housing_*` MCP tools.
//!
//! Walking into a lot (`housing_enter`) changes nothing on this node: the lot's inside is not
//! walkable on the map, so the agent's presence stays at the gate and the gateway records which
//! cell it stays on. The visit ends when the agent's presence leaves the gate.
//!
//! Lot names are free text written by other agents. The gateway's status and manual never carry
//! another agent's lot name, and this module adds none: names reach clients only, on the map.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use wattetheria_gateway_contract::SignedSpacePresence;
use wattetheria_gateway_contract::housing::{HousingAction, HousingActionPayload};

use crate::auth::authorize;
use crate::ledger_client::LedgerClient;
use crate::routes::farm::{
    action_id, busy_after_lost_reply, gateway_error, gateway_unavailable, moment_ms,
    outcome_unknown, outcome_unknown_refusal, refusal, walk,
};
use crate::routes::missions::agent_display_name_for_did;
use crate::routes::space::{configured_package, current_presence};
use crate::state::ControlPlaneState;

pub use wattetheria_gateway_contract::housing::GAME_ID;

/// How long a command holds the agent until the gateway answers with its own time: the longest
/// command in the first catalog (buying a lot). The gateway's `busy_until` replaces it.
const HOLD_SECS: i64 = 6;
const HOUSING_ACTIONS: [&str; 5] = ["buy_lot", "place", "remove", "name", "enter"];

/// The housing tools in the order an agent uses them, returned with the manual.
const HOUSING_TOOLS: [(&str, &str); 7] = [
    (
        "housing_status",
        "Your lot, store, level, the free lots with the walk to each, and the next step.",
    ),
    (
        "move_agent",
        "Walk to a lot's gate (place residential-district/lot-N-gate). Refused while busy.",
    ),
    (
        "housing_buy_lot",
        "Standing at a free lot's gate, buy it at its fixed price.",
    ),
    (
        "housing_place",
        "Standing at your lot's gate, put one element at a lot cell.",
    ),
    (
        "housing_remove",
        "Standing at your lot's gate, take the topmost piece off a cell into your store.",
    ),
    (
        "housing_name",
        "Standing at your lot's gate, write the name on its sign.",
    ),
    (
        "housing_enter",
        "Standing at a bought lot's gate, yours or anyone's, walk in and stay on a cell.",
    ),
];

/// The tools `game_info("housing")` lists.
pub(crate) fn tools() -> Value {
    HOUSING_TOOLS
        .iter()
        .map(|(name, use_for)| json!({"tool": name, "use_for": use_for}))
        .collect()
}

/// Whether this node's map has the residential district the gateway names in `body`
/// (`space_id`, `district_place_id`). An older map has no lot gates to stand on.
pub(crate) fn map_ready(state: &ControlPlaneState, body: &Value) -> bool {
    let (Some(space_id), Some(place_id)) = (
        body["space_id"].as_str(),
        body["district_place_id"].as_str(),
    ) else {
        return false;
    };
    configured_package(state, space_id).is_some_and(|package| package.place(place_id).is_some())
}

pub(crate) fn stale_client_message(body: &Value) -> String {
    format!(
        "this node's map of {} has no residential district yet; update the node to play",
        body["space_id"].as_str().unwrap_or("the Space")
    )
}

/// `GET /v1/wattetheria/housing/status`: this agent's housing, with the walk to each gate.
pub async fn housing_status(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, &headers).await {
        return response;
    }
    let Some(gateway) = LedgerClient::from_state(&state) else {
        return gateway_unavailable();
    };
    let reply = gateway
        .get_public(&format!("/api/housing/agents/{}", state.agent_did), &[])
        .await;
    let (status, mut body) = match reply {
        Ok(reply) => reply,
        Err(error) => return gateway_error(&error),
    };
    if status.is_success() {
        body["map_ready"] = json!(map_ready(&state, &body));
        let presence = current_presence(&state).await;
        for list in ["lots", "free_lots"] {
            if let Some(lots) = body[list].as_array_mut() {
                for lot in lots {
                    add_walk(&state, presence.as_ref(), lot, &lot["gate"].clone());
                }
            }
        }
        let gate = body["next"]["gate"].clone();
        add_walk(&state, presence.as_ref(), &mut body["next"], &gate);
    }
    (status, Json(body)).into_response()
}

/// `POST /v1/wattetheria/housing/actions/{action}`: one housing command, signed and sent with
/// this agent's current presence. The gateway's reply, refusals included, comes back as it is; a
/// refusal for standing away from the gate also says how to walk there.
pub async fn housing_action(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Path(action): Path<String>,
    Json(mut body): Json<Value>,
) -> Response {
    if let Err(response) = authorize(&state, &headers).await {
        return response;
    }
    if !HOUSING_ACTIONS.contains(&action.as_str()) {
        return refusal(
            StatusCode::NOT_FOUND,
            "invalid",
            &format!("no housing action {action}"),
        );
    }
    let action_id = match action_id(&body) {
        Ok(action_id) => action_id,
        Err(message) => return refusal(StatusCode::BAD_REQUEST, "invalid", message),
    };
    let Some(object) = body.as_object_mut() else {
        return refusal(StatusCode::BAD_REQUEST, "invalid", "body must be an object");
    };
    object.insert("action".to_owned(), Value::String(action));
    let action: HousingAction = match serde_json::from_value(body) {
        Ok(action) => action,
        Err(error) => return refusal(StatusCode::BAD_REQUEST, "invalid", &error.to_string()),
    };
    let Some(gateway) = LedgerClient::from_state(&state) else {
        return gateway_unavailable();
    };
    let busy = &state.busy;
    let _turn = busy.turn.lock().await;
    let held_until = busy.until_ms(&state.local_db);
    let Some(presence) = current_presence(&state).await else {
        return refusal(
            StatusCode::CONFLICT,
            "not_in_space",
            "enter the Space first (enter_space)",
        );
    };
    let payload = HousingActionPayload {
        action_id: action_id.clone(),
        agent_did: state.agent_did.clone(),
        action,
        presence: Some(presence.clone()),
        agent_label: agent_display_name_for_did(&state, &state.agent_did).await,
    };
    let payload = match serde_json::to_value(&payload) {
        Ok(payload) => payload,
        Err(error) => return gateway_error(&error.into()),
    };
    // A housing command can hold the agent at the gate. Until the gateway answers, assume it
    // accepted: if the reply is lost the agent is still at work as far as anyone can tell.
    let worst = chrono::Utc::now().timestamp_millis() + HOLD_SECS * 1000;
    busy.set(&state.local_db, held_until.max(worst));
    let sent = gateway
        .post_signed_raw(&state, "/api/housing/actions", payload)
        .await;
    let (status, mut reply) = match sent {
        Ok((status, reply)) if !outcome_unknown(status, &reply) => (status, reply),
        lost => {
            let until = busy_after_lost_reply(&state, &gateway, "housing", HOLD_SECS).await;
            busy.set(&state.local_db, held_until.max(until));
            return outcome_unknown_refusal(&lost, &action_id);
        }
    };
    // The answer settles it: the gateway's busy time when it gave one (a `busy` refusal carries
    // it in its details), else what held before.
    let until = moment_ms(&reply["busy_until"])
        .or_else(|| moment_ms(&reply["details"]["busy_until"]))
        .unwrap_or(0);
    busy.set(&state.local_db, held_until.max(until));
    if reply["error"] == "not_at_gate" {
        let details = &reply["details"];
        let gate = if details["gate"].is_array() {
            details["gate"].clone()
        } else {
            details["gates"][0].clone()
        };
        add_walk(&state, Some(&presence), &mut reply["details"], &gate);
    }
    reply["action_id"] = json!(action_id);
    (status, Json(reply)).into_response()
}

/// Adds `walk_secs` and the `move_agent` arguments for walking to `gate` from where the agent
/// stands. Leaves `target` alone when the agent is not in the Space or the gate is unknown.
fn add_walk(
    state: &ControlPlaneState,
    presence: Option<&SignedSpacePresence>,
    target: &mut Value,
    gate: &Value,
) {
    if !gate.is_array() || !target.is_object() {
        return;
    }
    if let Some(walk) = walk(state, presence, &json!({"tile": gate})) {
        target["walk_secs"] = json!(walk.secs);
        target["move_agent"] = walk.move_agent;
    }
}
