//! The farm game as this agent plays it. The gateway holds every plot, seed and harvest; the
//! node signs its agent's commands, attaches the agent's newest signed presence so the gateway
//! judges where the agent stands right now, and relays the gateway's answers unchanged. Agents
//! reach it through the `list_games`, `game_info`, `farm_status`, `farm_season` and `farm_*` MCP
//! tools.

use anyhow::Context as _;
use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::Mutex;
use wattetheria_gateway_contract::SignedSpacePresence;
use wattetheria_gateway_contract::farm::{FarmAction, FarmActionPayload, GAME_ID};

use crate::auth::authorize;
use crate::ledger_client::LedgerClient;
use crate::routes::missions::agent_display_name_for_did;
use crate::routes::space::{configured_package, current_presence};
use crate::state::ControlPlaneState;

const MAX_ACTION_ID_CHARS: usize = 128;
/// How long a farm command holds an agent when the gateway's catalog cannot be read and no
/// earlier read is at hand. The gateway's answer, its status when asked, or its catalog's
/// `action_secs` come first.
const DEFAULT_ACTION_SECS: i64 = 6;
/// How long a read of the gateway's game catalog is trusted before it is read again, so a
/// running node follows the gateway when the field moves or actions get longer.
const CATALOG_TTL: std::time::Duration = std::time::Duration::from_secs(600);
const FARM_ACTIONS: [&str; 5] = ["claim_plots", "plant", "water", "harvest", "sell"];

/// The farm tools in the order an agent uses them, returned with the manual.
const FARM_TOOLS: [(&str, &str); 8] = [
    (
        "farm_status",
        "Your plots, inventory, balance and the next step to take.",
    ),
    (
        "farm_claim_plots",
        "Join this season: standing on or beside a free parcel, claim it.",
    ),
    (
        "farm_plant",
        "Standing on your empty plot, buy seed and plant it.",
    ),
    ("farm_water", "Standing on your dry plot, water it."),
    (
        "farm_harvest",
        "Standing on your ripe plot, harvest it into your inventory.",
    ),
    (
        "farm_sell",
        "Standing inside the buyback place, sell crops to the system.",
    ),
    (
        "move_agent",
        "Walk to a tile (a parcel, a plot) or a place (the buyback). Refused while busy.",
    ),
    (
        "farm_season",
        "After a season ends: the hidden rules it ran on, checkable against its hash.",
    ),
];

/// What this node needs from the gateway's game catalog: where the field is, to tell whether
/// its own map has it yet, and how long an action holds an agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FarmCatalog {
    pub space_id: String,
    pub field_place_id: String,
    pub action_secs: i64,
}

/// The last read of the catalog and when it was made.
#[derive(Default)]
pub struct FarmCatalogCache(Mutex<Option<(std::time::Instant, FarmCatalog)>>);

/// The farm's catalog entry, read again once the last read is older than `CATALOG_TTL`. When the
/// gateway cannot answer, the last read stands; `None` only if there never was one, in which case
/// the gateway's own refusals are what an agent gets.
async fn catalog(state: &ControlPlaneState, gateway: &LedgerClient) -> Option<FarmCatalog> {
    let mut cached = state.farm_catalog.0.lock().await;
    if let Some((read_at, catalog)) = cached.as_ref()
        && read_at.elapsed() < CATALOG_TTL
    {
        return Some(catalog.clone());
    }
    match read_catalog(gateway).await {
        Ok(catalog) => {
            *cached = Some((std::time::Instant::now(), catalog.clone()));
            Some(catalog)
        }
        Err(error) => {
            tracing::warn!(error = %format!("{error:#}"), "could not read the farm's catalog entry");
            cached.as_ref().map(|(_, catalog)| catalog.clone())
        }
    }
}

async fn read_catalog(gateway: &LedgerClient) -> anyhow::Result<FarmCatalog> {
    let (status, body) = gateway.get_public("/api/games", &[]).await?;
    anyhow::ensure!(status.is_success(), "the game catalog answered {status}");
    let farm = body["games"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|game| game["game_id"] == GAME_ID)
        .context("the game catalog has no farm")?;
    let text = |key: &str| {
        farm[key]
            .as_str()
            .map(str::to_owned)
            .with_context(|| format!("the catalog's farm entry has no {key}"))
    };
    Ok(FarmCatalog {
        space_id: text("space_id")?,
        field_place_id: text("field_place_id")?,
        action_secs: farm["action_secs"]
            .as_i64()
            .context("the catalog's farm entry has no action_secs")?,
    })
}

/// `GET /v1/wattetheria/games`: the games this network offers, from the gateway.
pub async fn list_games(State(state): State<ControlPlaneState>, headers: HeaderMap) -> Response {
    if let Err(response) = authorize(&state, &headers).await {
        return response;
    }
    let Some(gateway) = LedgerClient::from_state(&state) else {
        return gateway_unavailable();
    };
    relay(gateway.get_public("/api/games", &[]).await)
}

/// `GET /v1/wattetheria/games/{game_id}`: the full manual, with this agent's availability and
/// the tools that play the game.
pub async fn game_info(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Path(game_id): Path<String>,
) -> Response {
    if let Err(response) = authorize(&state, &headers).await {
        return response;
    }
    let Some(gateway) = LedgerClient::from_state(&state) else {
        return gateway_unavailable();
    };
    let reply = gateway
        .get_public(
            &format!("/api/games/{game_id}"),
            &[("agent_did", &state.agent_did)],
        )
        .await;
    let (status, mut body) = match reply {
        Ok(reply) => reply,
        Err(error) => return gateway_error(&error),
    };
    if status.is_success() && game_id == GAME_ID {
        body["tools"] = FARM_TOOLS
            .iter()
            .map(|(name, use_for)| json!({"tool": name, "use_for": use_for}))
            .collect();
        if let Some(catalog) = catalog(&state, &gateway).await
            && !map_has(&state, &catalog)
        {
            body["availability"]["can_join"] = json!(false);
            body["availability"]["reason"] = json!("stale_client");
            body["availability"]["message"] = json!(stale_client_message(&catalog));
        }
    }
    (status, Json(body)).into_response()
}

/// `GET /v1/wattetheria/farm/status`: this agent's farm, with the walk to the next step.
pub async fn farm_status(State(state): State<ControlPlaneState>, headers: HeaderMap) -> Response {
    if let Err(response) = authorize(&state, &headers).await {
        return response;
    }
    let Some(gateway) = LedgerClient::from_state(&state) else {
        return gateway_unavailable();
    };
    let reply = gateway
        .get_public(&format!("/api/farm/agents/{}", state.agent_did), &[])
        .await;
    let (status, mut body) = match reply {
        Ok(reply) => reply,
        Err(error) => return gateway_error(&error),
    };
    if status.is_success() {
        let presence = current_presence(&state).await;
        if let Some(walk) = walk(&state, presence.as_ref(), &body["next"]) {
            body["next"]["walk_secs"] = json!(walk.secs);
        }
        if let Some(catalog) = catalog(&state, &gateway).await {
            body["map_ready"] = json!(map_has(&state, &catalog));
        }
    }
    (status, Json(body)).into_response()
}

#[derive(Debug, Deserialize)]
pub struct SeasonQuery {
    season_no: i64,
}

/// `GET /v1/wattetheria/farm/season?season_no=`: a season's hash and, once it has ended, the
/// parameters and seed it ran on, from the gateway.
pub async fn farm_season(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<SeasonQuery>,
) -> Response {
    if let Err(response) = authorize(&state, &headers).await {
        return response;
    }
    let Some(gateway) = LedgerClient::from_state(&state) else {
        return gateway_unavailable();
    };
    relay(
        gateway
            .get_public(&format!("/api/farm/seasons/{}", query.season_no), &[])
            .await,
    )
}

/// `POST /v1/wattetheria/farm/actions/{action}`: one farm command, signed and sent with this
/// agent's current presence. The gateway's reply, refusals included, comes back as it is; a
/// refusal for standing in the wrong spot also says how to walk there.
pub async fn farm_action(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Path(action): Path<String>,
    Json(mut body): Json<Value>,
) -> Response {
    if let Err(response) = authorize(&state, &headers).await {
        return response;
    }
    if !FARM_ACTIONS.contains(&action.as_str()) {
        return refusal(
            StatusCode::NOT_FOUND,
            "invalid",
            &format!("no farm action {action}"),
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
    let action: FarmAction = match serde_json::from_value(body) {
        Ok(action) => action,
        Err(error) => {
            return refusal(StatusCode::BAD_REQUEST, "invalid", &error.to_string());
        }
    };
    let Some(gateway) = LedgerClient::from_state(&state) else {
        return gateway_unavailable();
    };
    let catalog = catalog(&state, &gateway).await;
    if let Some(catalog) = &catalog
        && !map_has(&state, catalog)
    {
        return refusal(
            StatusCode::CONFLICT,
            "stale_client",
            &stale_client_message(catalog),
        );
    }
    let action_secs = catalog.map_or(DEFAULT_ACTION_SECS, |catalog| catalog.action_secs);
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
    let timed = matches!(
        action,
        FarmAction::Plant { .. } | FarmAction::Water { .. } | FarmAction::Harvest { .. }
    );
    let payload = FarmActionPayload {
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
    // Until the gateway answers, assume it accepted: if the reply is lost the agent is still
    // working the plot as far as anyone else can tell.
    if timed {
        let worst = chrono::Utc::now().timestamp_millis() + action_secs * 1000;
        busy.set(&state.local_db, held_until.max(worst));
    }
    let sent = gateway
        .post_signed_raw(&state, "/api/farm/actions", payload)
        .await;
    let (status, mut reply) = match sent {
        Ok((status, reply)) if !outcome_unknown(status, &reply) => (status, reply),
        lost => {
            if timed {
                let until = busy_after_lost_reply(&state, &gateway, action_secs).await;
                busy.set(&state.local_db, held_until.max(until));
            }
            return outcome_unknown_refusal(&lost, &action_id);
        }
    };
    // The answer settles it: the gateway's busy time when it gave one (a `busy` refusal carries
    // it in its details), else what held before.
    let until = moment_ms(&reply["busy_until"])
        .or_else(|| moment_ms(&reply["details"]["busy_until"]))
        .unwrap_or(0);
    busy.set(&state.local_db, held_until.max(until));
    if matches!(
        reply["error"].as_str(),
        Some("not_at_tile" | "not_at_place")
    ) && let Some(walk) = walk(&state, Some(&presence), &reply["details"])
    {
        let details = &mut reply["details"];
        details["to_tile"] = json!(walk.to);
        details["walk_secs"] = json!(walk.secs);
        details["move_agent"] = walk.move_agent;
    }
    reply["action_id"] = json!(action_id);
    (status, Json(reply)).into_response()
}

/// Whether a reply leaves the command's outcome unknown: a success the node cannot read, or any
/// server error. A proxy's 502, 503 or 504 says nothing about the gateway, and even the gateway's
/// own 500 may come after its database committed (the commit's answer can be lost). Only the
/// gateway's refusals (4xx: wrong spot, no Watt, busy) are certain.
fn outcome_unknown(status: StatusCode, reply: &Value) -> bool {
    if status.is_success() {
        return !reply.is_object();
    }
    status.is_server_error()
}

/// The gateway may or may not have done the command, so the agent retries with the same
/// `action_id`, which returns the real answer. The caller keeps the agent held meanwhile.
fn outcome_unknown_refusal(
    lost: &anyhow::Result<(StatusCode, Value)>,
    action_id: &str,
) -> Response {
    let why = match lost {
        Ok((status, _)) => format!("the gateway answered {status} with no readable result"),
        Err(error) => format!("{error:#}"),
    };
    refusal(
        StatusCode::BAD_GATEWAY,
        "outcome_unknown",
        &format!(
            "{why}; the command may have been done. Retry with the same action_id \
             ({action_id}) to get its real result; it is never done twice."
        ),
    )
}

/// Until when the agent is held after a timed command whose answer was lost: what the gateway
/// says when asked now, and never less than one action from now, since the gateway may have
/// started the command at any moment before the node gave up waiting.
async fn busy_after_lost_reply(
    state: &ControlPlaneState,
    gateway: &LedgerClient,
    action_secs: i64,
) -> i64 {
    let floor = chrono::Utc::now().timestamp_millis() + action_secs * 1000;
    let told = gateway
        .get_public(&format!("/api/farm/agents/{}", state.agent_did), &[])
        .await
        .ok()
        .filter(|(status, _)| status.is_success())
        .and_then(|(_, body)| moment_ms(&body["busy_until"]));
    told.unwrap_or(0).max(floor)
}

/// A moment the gateway sends as an RFC 3339 string, in epoch milliseconds.
fn moment_ms(value: &Value) -> Option<i64> {
    let at = chrono::DateTime::parse_from_rfc3339(value.as_str()?).ok()?;
    Some(at.timestamp_millis())
}

/// Whether this node's map has the field the gateway plays on. An older map has no field
/// place: the agent could never stand on a plot, so it is told to update instead.
fn map_has(state: &ControlPlaneState, catalog: &FarmCatalog) -> bool {
    configured_package(state, &catalog.space_id)
        .is_some_and(|package| package.place(&catalog.field_place_id).is_some())
}

fn stale_client_message(catalog: &FarmCatalog) -> String {
    format!(
        "this node's map of {} has no farm field yet; update the node to play",
        catalog.space_id
    )
}

/// A walk from where the agent stands to the tile or place a step or a refusal names.
struct Walk {
    to: [u32; 2],
    secs: f64,
    /// The `move_agent` arguments that make it.
    move_agent: Value,
}

/// The walk to `goal`'s `tile`, else to its place (`move_to_place` in a next step, `place_id`
/// in a refusal).
fn walk(
    state: &ControlPlaneState,
    presence: Option<&SignedSpacePresence>,
    goal: &Value,
) -> Option<Walk> {
    let record = &presence?.record;
    let package = configured_package(state, &record.space_id)?;
    let now = chrono::Utc::now().timestamp_millis();
    let from = record.tile_at(package.walk_speed, now);
    let (to, move_agent) = if let Some(tile) = goal["tile"].as_array() {
        let tile = [
            u32::try_from(tile.first()?.as_u64()?).ok()?,
            u32::try_from(tile.get(1)?.as_u64()?).ok()?,
        ];
        (tile, json!({"tile": tile}))
    } else {
        let place_id = goal["move_to_place"]
            .as_str()
            .or_else(|| goal["place_id"].as_str())?;
        let tile = package.destination_tile(&state.agent_did, place_id).ok()?;
        (tile, json!({"place_id": place_id}))
    };
    let steps = package.path(from, to).ok()?.len();
    #[allow(clippy::cast_precision_loss)]
    let secs = (steps as f64 / package.walk_speed).ceil();
    Some(Walk {
        to,
        secs,
        move_agent,
    })
}

fn action_id(body: &Value) -> Result<String, &'static str> {
    let action_id = body["action_id"]
        .as_str()
        .ok_or("action_id is required: a key you choose, reused when retrying")?;
    if action_id.is_empty()
        || action_id.chars().count() > MAX_ACTION_ID_CHARS
        || action_id.chars().any(char::is_whitespace)
    {
        return Err("action_id must be 1 to 128 characters with no whitespace");
    }
    Ok(action_id.to_owned())
}

fn relay(reply: anyhow::Result<(StatusCode, Value)>) -> Response {
    match reply {
        Ok((status, body)) => (status, Json(body)).into_response(),
        Err(error) => gateway_error(&error),
    }
}

fn refusal(status: StatusCode, code: &str, message: &str) -> Response {
    (status, Json(json!({"error": code, "message": message}))).into_response()
}

fn gateway_unavailable() -> Response {
    refusal(
        StatusCode::SERVICE_UNAVAILABLE,
        "gateway_unavailable",
        "games live on the gateway and this node has no gateway configured",
    )
}

fn gateway_error(error: &anyhow::Error) -> Response {
    refusal(
        StatusCode::BAD_GATEWAY,
        "gateway_unreachable",
        &format!("{error:#}"),
    )
}
