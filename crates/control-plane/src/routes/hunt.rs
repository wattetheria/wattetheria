//! 狩猎 (hunting) as this agent plays it. The gateway holds every monster, fight, bag and sword;
//! the node signs its agent's commands, attaches the agent's newest signed presence so the
//! gateway judges which side of a monster or which place the agent stands on, and relays the
//! gateway's answers with the walks to where the agent must stand. Agents reach it through
//! `game_info("hunt")`, `hunt_status`, `hunt_area` and the `hunt_*` / `smithy_*` MCP tools.
//!
//! A fight is one command: the gateway works out every round at once and the agent stays busy
//! while the rounds replay, so `move_agent` waits for it like it waits for farm work.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};
use wattetheria_gateway_contract::SignedSpacePresence;
use wattetheria_gateway_contract::hunt::{HuntAction, HuntActionPayload};

use crate::auth::authorize;
use crate::ledger_client::LedgerClient;
use crate::routes::farm::{
    action_id, busy_after_lost_reply, gateway_error, gateway_unavailable, moment_ms,
    outcome_unknown, outcome_unknown_refusal, refusal, relay, walk,
};
use crate::routes::missions::agent_display_name_for_did;
use crate::routes::space::current_presence;
use crate::state::ControlPlaneState;

pub use wattetheria_gateway_contract::hunt::GAME_ID;

/// How long a command holds the agent until the gateway answers with its own time: the longest
/// fight (30 rounds of 2 s). The gateway's `busy_until` replaces it.
const HOLD_SECS: i64 = 60;
const HUNT_ACTIONS: [&str; 8] = [
    "fight",
    "pick",
    "smithy_buy",
    "smithy_forge",
    "smithy_enhance",
    "equip",
    "sell",
    "buy",
];

/// The hunt tools in the order an agent uses them, returned with the manual.
const HUNT_TOOLS: [(&str, &str); 13] = [
    (
        "hunt_status",
        "Your HP, stamina, swords, hunt bag, today's take, hunter rank and streak, with the manual.",
    ),
    (
        "hunt_area",
        "The monsters of a zone (or all), where to stand beside each and the walk there, what lies on the ground.",
    ),
    (
        "move_agent",
        "Walk to a tile beside a monster (hunt_area's stand_at), the smithy or the market. Refused while busy.",
    ),
    (
        "hunt_fight",
        "Standing beside a monster, fight it once; the reply has every round and the drops.",
    ),
    (
        "hunt_pick",
        "Within 1 tile of something a cliff wraith dropped, pick it up.",
    ),
    (
        "hunt_log",
        "Read the season's fights, yours or anyone's, to work out the monsters' weaknesses.",
    ),
    ("smithy_buy", "At the smithy, buy the training sword."),
    (
        "smithy_forge",
        "At the smithy, forge the next sword of the line from materials and Watt.",
    ),
    (
        "smithy_enhance",
        "At the smithy, enhance the sword in use by one level with a gem.",
    ),
    (
        "hunt_equip",
        "Anywhere in the Space, take another sword you own in hand; you fight with the best one until you do.",
    ),
    ("hunt_sell", "At the market, sell gems to the system."),
    ("hunt_buy", "At the market, buy large HP potions."),
    (
        "space_bag",
        "Everything you keep: crops, building pieces, hunt materials, gems, potions, swords.",
    ),
];

/// The tools `game_info("hunt")` lists.
pub(crate) fn tools() -> Value {
    HUNT_TOOLS
        .iter()
        .map(|(name, use_for)| json!({"tool": name, "use_for": use_for}))
        .collect()
}

/// `GET /v1/wattetheria/hunt/status`: this agent's hunting, with the walks to the smithy and the
/// market.
pub async fn hunt_status(State(state): State<ControlPlaneState>, headers: HeaderMap) -> Response {
    if let Err(response) = authorize(&state, &headers).await {
        return response;
    }
    let Some(gateway) = LedgerClient::from_state(&state) else {
        return gateway_unavailable();
    };
    let (status, mut body) = match own_read(&state, &gateway, "/api/hunt/status", None).await {
        Ok(reply) => reply,
        Err(error) => return gateway_error(&error),
    };
    if status.is_success() {
        let presence = current_presence(&state).await;
        for (key, place) in [
            ("smithy", body["smithy_place_id"].clone()),
            ("market", body["market_place_id"].clone()),
        ] {
            if let Some(walk) = walk(&state, presence.as_ref(), &json!({"place_id": place})) {
                body["walk_to"][key] =
                    json!({"walk_secs": walk.secs, "move_agent": walk.move_agent});
            }
        }
    }
    (status, Json(body)).into_response()
}

#[derive(Debug, Default, Deserialize)]
pub struct AreaQuery {
    #[serde(default)]
    pub zone: Option<String>,
}

/// `GET /v1/wattetheria/hunt/area?zone=`: one zone (or all) as this agent sees it, with the
/// nearest free tile beside each standing monster and the walk to it.
pub async fn hunt_area(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<AreaQuery>,
) -> Response {
    if let Err(response) = authorize(&state, &headers).await {
        return response;
    }
    let Some(gateway) = LedgerClient::from_state(&state) else {
        return gateway_unavailable();
    };
    let zone = query.zone.as_deref().filter(|zone| !zone.trim().is_empty());
    let (status, mut body) = match own_read(&state, &gateway, "/api/hunt/area", zone).await {
        Ok(reply) => reply,
        Err(error) => return gateway_error(&error),
    };
    if status.is_success() {
        let presence = current_presence(&state).await;
        if let Some(spawns) = body["spawns"].as_array_mut() {
            for spawn in spawns {
                if spawn["status"] != "closed" {
                    add_nearest_walk(&state, presence.as_ref(), spawn);
                }
            }
        }
        if let Some(items) = body["ground"].as_array_mut() {
            for item in items {
                let tile = item["tile"].clone();
                if let Some(walk) = walk(&state, presence.as_ref(), &json!({"tile": tile})) {
                    item["walk_secs"] = json!(walk.secs);
                    item["move_agent"] = walk.move_agent;
                }
            }
        }
    }
    (status, Json(body)).into_response()
}

/// Adds `stand_at`, `walk_secs` and the `move_agent` arguments for the tile beside `target`
/// (its `around`) that is the shortest walk from where the agent stands.
fn add_nearest_walk(
    state: &ControlPlaneState,
    presence: Option<&SignedSpacePresence>,
    target: &mut Value,
) {
    let Some(around) = target["around"].as_array() else {
        return;
    };
    let best = around
        .iter()
        .filter_map(|tile| walk(state, presence, &json!({"tile": tile})))
        .min_by(|a, b| a.secs.total_cmp(&b.secs));
    if let Some(walk) = best {
        target["stand_at"] = json!(walk.to);
        target["walk_secs"] = json!(walk.secs);
        target["move_agent"] = walk.move_agent;
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct LogQuery {
    #[serde(default)]
    pub agent_did: Option<String>,
    #[serde(default)]
    pub monster: Option<String>,
    #[serde(default)]
    pub zone: Option<String>,
    #[serde(default)]
    pub season_no: Option<i64>,
    #[serde(default)]
    pub since: Option<String>,
    #[serde(default)]
    pub until: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
}

/// `GET /v1/wattetheria/hunt/log`: the season's fights, newest first; `agent_did=self` is this
/// agent.
pub async fn hunt_log(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<LogQuery>,
) -> Response {
    if let Err(response) = authorize(&state, &headers).await {
        return response;
    }
    let Some(gateway) = LedgerClient::from_state(&state) else {
        return gateway_unavailable();
    };
    let agent = query.agent_did.as_deref().map(|did| {
        if did == "self" {
            state.agent_did.as_str()
        } else {
            did
        }
    });
    let season = query.season_no.map(|no| no.to_string());
    let limit = query.limit.map(|limit| limit.to_string());
    let params: Vec<(&str, &str)> = [
        ("agent_did", agent),
        ("monster", query.monster.as_deref()),
        ("zone", query.zone.as_deref()),
        ("season_no", season.as_deref()),
        ("since", query.since.as_deref()),
        ("until", query.until.as_deref()),
        ("limit", limit.as_deref()),
    ]
    .into_iter()
    .filter_map(|(key, value)| {
        value
            .filter(|value| !value.is_empty())
            .map(|value| (key, value))
    })
    .collect();
    relay(gateway.get_public("/api/hunt/fights", &params).await)
}

/// `GET /v1/wattetheria/space/bag`: everything this agent keeps, by game, read from the gateway's
/// public bag of this agent.
pub async fn space_bag(State(state): State<ControlPlaneState>, headers: HeaderMap) -> Response {
    if let Err(response) = authorize(&state, &headers).await {
        return response;
    }
    let Some(gateway) = LedgerClient::from_state(&state) else {
        return gateway_unavailable();
    };
    let path = format!("/api/agents/{}/bag", state.agent_did);
    relay(gateway.get_public(&path, &[]).await)
}

/// Reads this agent's own data from the gateway: the request is signed by the agent's key,
/// like the growth hints, since the gateway gives it to nobody else.
async fn own_read(
    state: &ControlPlaneState,
    gateway: &LedgerClient,
    path: &str,
    zone: Option<&str>,
) -> anyhow::Result<(StatusCode, Value)> {
    let mut payload = json!({
        "agent_did": state.agent_did,
        "issued_at": chrono::Utc::now().to_rfc3339(),
    });
    if let Some(zone) = zone {
        payload["zone"] = json!(zone);
    }
    gateway.post_signed_raw(state, path, payload).await
}

/// `POST /v1/wattetheria/hunt/actions/{action}`: one hunt command, signed and sent with this
/// agent's current presence. The gateway's reply, refusals included, comes back as it is; a
/// refusal for standing in the wrong spot also says how to walk there.
pub async fn hunt_action(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Path(action): Path<String>,
    Json(mut body): Json<Value>,
) -> Response {
    if let Err(response) = authorize(&state, &headers).await {
        return response;
    }
    if !HUNT_ACTIONS.contains(&action.as_str()) {
        return refusal(
            StatusCode::NOT_FOUND,
            "invalid",
            &format!("no hunt action {action}"),
        );
    }
    let action_id = match action_id(&body) {
        Ok(action_id) => action_id,
        Err(message) => return refusal(StatusCode::BAD_REQUEST, "invalid", message),
    };
    let Some(object) = body.as_object_mut() else {
        return refusal(StatusCode::BAD_REQUEST, "invalid", "body must be an object");
    };
    object.remove("action_id");
    object.insert("action".to_owned(), Value::String(action));
    let action: HuntAction = match serde_json::from_value(body) {
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
    let payload = HuntActionPayload {
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
    // A fight holds the agent beside the monster. Until the gateway answers, assume it fought:
    // if the reply is lost the agent is still at it as far as anyone can tell.
    let worst = chrono::Utc::now().timestamp_millis() + HOLD_SECS * 1000;
    busy.set(&state.local_db, held_until.max(worst));
    let sent = gateway
        .post_signed_raw(&state, "/api/hunt/actions", payload)
        .await;
    let (status, mut reply) = match sent {
        Ok((status, reply)) if !outcome_unknown(status, &reply) => (status, reply),
        lost => {
            let until = busy_after_lost_reply(&state, &gateway, "hunt", HOLD_SECS).await;
            busy.set(&state.local_db, held_until.max(until));
            return outcome_unknown_refusal(&lost, &action_id);
        }
    };
    let until = moment_ms(&reply["busy_until"])
        .or_else(|| moment_ms(&reply["details"]["busy_until"]))
        .unwrap_or(0);
    busy.set(&state.local_db, held_until.max(until));
    add_walk_hint(&state, &presence, &mut reply);
    reply["action_id"] = json!(action_id);
    (status, Json(reply)).into_response()
}

/// A refusal for standing in the wrong spot also says how to walk there: beside the monster,
/// into the smithy or the market, or up to the thing on the ground.
fn add_walk_hint(state: &ControlPlaneState, presence: &SignedSpacePresence, reply: &mut Value) {
    match reply["error"].as_str() {
        Some("not_beside") => add_nearest_walk(state, Some(presence), &mut reply["details"]),
        Some("not_at_place" | "too_far") => {
            if let Some(walk) = walk(state, Some(presence), &reply["details"]) {
                let details = &mut reply["details"];
                details["to_tile"] = json!(walk.to);
                details["walk_secs"] = json!(walk.secs);
                details["move_agent"] = walk.move_agent;
            }
        }
        _ => {}
    }
}
