pub(crate) mod presence;
mod registry;
pub(crate) mod snapshot;
pub(crate) mod world;

pub use registry::{
    OwnSpacePresence, emit_current_space_presence, initialize_space_presence,
    reconcile_space_presence, reset_space_presence,
};

use anyhow::{Context, Result};
use axum::Json;
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use wattetheria_gateway_contract::SignedSpacePresence;
use wattetheria_gateway_contract::space::{
    SPACE_AVATAR_COUNT, validate_activity, validate_skin_id,
};
use wattetheria_kernel::space::{Place, PlaceKind, SpaceCatalog, SpacePackage};
use wattetheria_kernel::swarm_bridge::EventAnnouncementPayload;

use crate::auth::{authorize, internal_error};
use crate::ledger_client::{LedgerClient, LedgerError};
use crate::routes::mcp::normalized_gateway_api_resource_url;
use crate::state::ControlPlaneState;

#[derive(Default, Deserialize)]
pub(crate) struct SpaceQuery {
    pub space_id: Option<String>,
    pub kind: Option<PlaceKind>,
    pub parent_place_id: Option<String>,
    pub place_id: Option<String>,
    pub agent_id: Option<String>,
}

#[derive(Default, Deserialize)]
pub(crate) struct EnterSpaceBody {
    pub space_id: Option<String>,
    pub portal_id: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct MoveAgentBody {
    pub place_id: Option<String>,
    pub target_agent_id: Option<String>,
    pub tile: Option<[u32; 2]>,
    pub activity: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct SetActivityBody {
    pub activity: String,
    pub object_place_id: Option<String>,
}

/// Exactly one of the two: a built-in skin index, or the id of a custom skin this agent owns.
#[derive(Debug, Deserialize)]
pub(crate) struct SetSkinBody {
    #[serde(default)]
    pub avatar_id: Option<u8>,
    #[serde(default)]
    pub skin_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct UploadSkinBody {
    /// Standard base64 of a 96×128 PNG spritesheet laid out like the built-in avatars.
    pub png_base64: String,
    #[serde(default)]
    pub name: Option<String>,
}

fn bad_request(error: &(impl ToString + ?Sized)) -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error":error.to_string()})),
    )
        .into_response()
}

pub(super) fn gateway_client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()?)
}

fn place_summary(place: &Place) -> Value {
    json!({
        "place_id": place.place_id,
        "parent_place_id": place.parent_place_id,
        "kind": place.kind,
        "name": place.name
    })
}

pub(crate) async fn list_places(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<SpaceQuery>,
) -> Response {
    if let Err(response) = authorize(&state, &headers).await {
        return response;
    }
    let (_, package) = match registry::selected_package(&state, query.space_id.as_deref()).await {
        Ok(selected) => selected,
        Err(error) => return bad_request(&error),
    };
    let places = package.places.iter().filter(|place| {
        query.kind.is_none_or(|kind| place.kind == kind)
            && query.parent_place_id.as_deref().is_none_or(|parent| place.parent_place_id.as_deref() == Some(parent))
    }).map(|place| json!({
        "place":place_summary(place),
        "service_bindings":package.service_bindings.iter().filter(|binding| binding.place_id == place.place_id).collect::<Vec<_>>()
    })).collect::<Vec<_>>();
    Json(json!({"space_id":package.space_id,"space_version":package.space_version,"places":places}))
        .into_response()
}

pub(crate) async fn get_place(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<SpaceQuery>,
) -> Response {
    if let Err(response) = authorize(&state, &headers).await {
        return response;
    }
    let (network, package) =
        match registry::selected_package(&state, query.space_id.as_deref()).await {
            Ok(selected) => selected,
            Err(error) => return bad_request(&error),
        };
    let Some(place) = query.place_id.as_deref().and_then(|id| package.place(id)) else {
        return bad_request("unknown place_id");
    };
    let now = chrono::Utc::now().timestamp_millis();
    let presences = match network_presences(&state, &network, &package.space_id).await {
        Ok(records) => records,
        Err(error) => return internal_error(&error),
    };
    let occupants = presences
        .into_iter()
        .filter(|presence| {
            state
                .space_catalog
                .as_ref()
                .and_then(|catalog| {
                    catalog.version(&presence.record.space_id, &presence.record.space_version)
                })
                .is_some_and(|version| {
                    place
                        .tiles
                        .contains(&presence.record.tile_at(version.walk_speed, now))
                })
        })
        .collect::<Vec<_>>();
    let events = match place_events(&state, &network, &package.space_id, &place.place_id).await {
        Ok(events) => events,
        Err(error) => return internal_error(&error),
    };
    let objects = package
        .places
        .iter()
        .filter(|candidate| {
            candidate.kind == PlaceKind::GameObject
                && candidate
                    .tiles
                    .iter()
                    .any(|tile| place.tiles.contains(tile))
        })
        .map(place_summary)
        .collect::<Vec<_>>();
    Json(json!({"space_id":package.space_id,"server_time_ms":now,"place":place_summary(place),"presences":occupants,"objects":objects,"events":events,
        "service_bindings":package.service_bindings.iter().filter(|binding| binding.place_id == place.place_id).collect::<Vec<_>>()
    })).into_response()
}

pub(crate) async fn get_agent_presence(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Query(query): Query<SpaceQuery>,
) -> Response {
    if let Err(response) = authorize(&state, &headers).await {
        return response;
    }
    let (network, package) =
        match registry::selected_package(&state, query.space_id.as_deref()).await {
            Ok(selected) => selected,
            Err(error) => return bad_request(&error),
        };
    let agent_id = query.agent_id.as_deref().unwrap_or(&state.agent_did);
    let presence = if agent_id == state.agent_did {
        let presence = state
            .own_space_presence
            .lock()
            .await
            .visible_get(&network, &package)
            .cloned();
        if presence.is_some() {
            match registry::require_active_network(&state).await {
                Ok(active_network) if active_network == network => {}
                Ok(_) => {
                    return internal_error(&anyhow::anyhow!(
                        "current network changed while reading Agent Space"
                    ));
                }
                Err(error) => return internal_error(&error),
            }
        }
        presence
    } else {
        match network_presences(&state, &network, &package.space_id).await {
            Ok(records) => records
                .into_iter()
                .find(|record| record.record.agent_did == agent_id),
            Err(error) => return internal_error(&error),
        }
    };
    let Some(presence) = presence else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error":"agent presence not found"})),
        )
            .into_response();
    };
    let now = chrono::Utc::now().timestamp_millis();
    let version = state
        .space_catalog
        .as_ref()
        .and_then(|catalog| {
            catalog.version(&presence.record.space_id, &presence.record.space_version)
        })
        .expect("presence geometry was verified");
    Json(json!({"space_id":package.space_id,"server_time_ms":now,"position":presence.record.position_at(version.walk_speed,now),"presence":presence})).into_response()
}

pub(crate) async fn enter_space(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    body: Option<Json<EnterSpaceBody>>,
) -> Response {
    if let Err(response) = authorize(&state, &headers).await {
        return response;
    }
    let body = body.map(|body| body.0).unwrap_or_default();
    match registry::enter_configured_space(
        &state,
        body.space_id.as_deref(),
        body.portal_id.as_deref(),
    )
    .await
    {
        Ok(presence) => Json(json!({"ok":true,"presence":presence})).into_response(),
        Err(error) => bad_request(&error),
    }
}

pub(crate) async fn leave_space(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, &headers).await {
        return response;
    }
    match registry::leave_configured_space(&state).await {
        Ok(Some(presence)) => Json(json!({"ok":true,"presence":presence})).into_response(),
        Ok(None) => Json(json!({"ok":true,"presence":null})).into_response(),
        Err(error) => bad_request(&error),
    }
}

pub(crate) async fn move_agent(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<MoveAgentBody>,
) -> Response {
    if let Err(response) = authorize(&state, &headers).await {
        return response;
    }
    let choices = usize::from(body.place_id.is_some())
        + usize::from(body.target_agent_id.is_some())
        + usize::from(body.tile.is_some());
    if choices != 1 {
        return bad_request("provide exactly one of place_id, target_agent_id or tile");
    }
    if let Err(error) = validate_activity(body.activity.as_deref().unwrap_or_default()) {
        return bad_request(error);
    }
    if state.own_space_presence.lock().await.active_space.is_none() {
        return bad_request("agent has not entered the space; call enter_space first");
    }
    let now = chrono::Utc::now().timestamp_millis();
    let (network, package) = match registry::selected_package(&state, None).await {
        Ok(selected) => selected,
        Err(error) => return bad_request(&error),
    };
    let mut occupancy = if body.tile.is_none() {
        movement_occupancies(&state, &package.space_id).await
    } else {
        None
    };
    let target_tile = if let Some(target_id) = body.target_agent_id.as_deref() {
        let (tile, records) = match person_target(&state, &network, &package, target_id, now).await
        {
            Ok(target) => target,
            Err(response) => return response,
        };
        if occupancy.is_none() {
            occupancy = Some(records);
        }
        Some(tile)
    } else {
        None
    };
    match registry::update_presence(&state, |previous, package| {
        let mut record = previous.context("agent has not joined the space")?.clone();
        record.from_tile = record.tile_at(package.walk_speed, now);
        let (destination_tile, destination_place_id) = if let Some(tile) = body.tile {
            let place = package
                .place_at_tile(tile)
                .context("destination tile has no place")?;
            (tile, place.place_id.clone())
        } else if let Some(place_id) = &body.place_id {
            let anchor = package.destination_tile(&state.agent_did, place_id)?;
            let tile = if let Some(records) = &occupancy {
                MovementSelection {
                    catalog: registry::catalog(&state)?,
                    package,
                    presences: records,
                    agent_did: &state.agent_did,
                    network: &network,
                    now,
                }
                .select(record.from_tile, anchor, Some(place_id))?
            } else {
                anchor
            };
            (tile, place_id.clone())
        } else {
            let target_tile = target_tile.expect("target choice checked");
            let tile = MovementSelection {
                catalog: registry::catalog(&state)?,
                package,
                presences: occupancy.as_deref().unwrap_or_default(),
                agent_did: &state.agent_did,
                network: &network,
                now,
            }
            .select(record.from_tile, target_tile, None)?;
            let place = package
                .place_at_tile(tile)
                .context("destination tile has no place")?;
            (tile, place.place_id.clone())
        };
        record.path = package.path(record.from_tile, destination_tile)?;
        record.depart_at = now;
        record.updated_at = now;
        record.destination_place_id = destination_place_id;
        record.activity = body.activity.unwrap_or_default();
        record.object_place_id = None;
        Ok(record)
    })
    .await
    {
        Ok(presence) => Json(json!({"ok":true,"presence":presence})).into_response(),
        Err(error) => bad_request(&error),
    }
}

async fn person_target(
    state: &ControlPlaneState,
    network: &str,
    package: &SpacePackage,
    target_id: &str,
    now: i64,
) -> std::result::Result<([u32; 2], Vec<SignedSpacePresence>), Response> {
    let records = network_presences(state, network, &package.space_id)
        .await
        .map_err(|error| internal_error(&error))?;
    let target = records
        .iter()
        .find(|record| record.record.agent_did == target_id)
        .ok_or_else(|| bad_request("target agent presence not found"))?;
    let version = state
        .space_catalog
        .as_ref()
        .and_then(|catalog| catalog.version(&target.record.space_id, &target.record.space_version))
        .expect("presence geometry was verified");
    let tile = target.record.tile_at(version.walk_speed, now);
    if package.place_at_tile(tile).is_none() {
        return Err(bad_request("target agent is outside a known place"));
    }
    Ok((tile, records))
}

async fn movement_occupancies(
    state: &ControlPlaneState,
    space: &str,
) -> Option<Vec<SignedSpacePresence>> {
    let catalog = registry::catalog(state).ok()?;
    let client = gateway_client().ok()?;
    let mut available = false;
    let mut latest = BTreeMap::<(String, String), SignedSpacePresence>::new();
    for gateway in &state.gateway_urls {
        let url = normalized_gateway_api_resource_url(
            gateway,
            "/api/spaces/presence",
            "/api/spaces/presence",
        );
        let payload = async {
            client
                .get(url)
                .query(&[("space_id", space)])
                .send()
                .await?
                .error_for_status()?
                .json::<Value>()
                .await
        }
        .await;
        let Ok(payload) = payload else { continue };
        let Some(records) = payload.get("presences").and_then(Value::as_array) else {
            continue;
        };
        available = true;
        for value in records {
            let Ok(signed) = serde_json::from_value::<SignedSpacePresence>(value.clone()) else {
                continue;
            };
            if signed.record.space_id != space
                || registry::verify_presence_signature(&signed).is_err()
            {
                continue;
            }
            let key = (
                signed.record.network_id.clone(),
                signed.record.agent_did.clone(),
            );
            if latest
                .get(&key)
                .is_none_or(|old| signed.record.seq > old.record.seq)
            {
                latest.insert(key, signed);
            }
        }
    }
    if !available {
        return None;
    }
    Some(
        latest
            .into_values()
            .filter(|signed| {
                signed.record.entered
                    && catalog
                        .version(space, &signed.record.space_version)
                        .is_some_and(|package| registry::verify_presence(package, signed).is_ok())
            })
            .collect(),
    )
}

struct MovementSelection<'a> {
    catalog: &'a SpaceCatalog,
    package: &'a SpacePackage,
    presences: &'a [SignedSpacePresence],
    agent_did: &'a str,
    network: &'a str,
    now: i64,
}

impl MovementSelection<'_> {
    fn select(&self, from: [u32; 2], anchor: [u32; 2], place_id: Option<&str>) -> Result<[u32; 2]> {
        let radius = self
            .package
            .movement_search_radius
            .context("space package is missing movement search radius")?;
        if place_id.is_none() && from != anchor && manhattan_distance(from, anchor) == 1 {
            return Ok(from);
        }
        let loads = self.tile_loads();
        let mut candidates = Vec::new();
        for y in 0..self.package.size[1] {
            for x in 0..self.package.size[0] {
                let tile = [x, y];
                if tile == anchor && place_id.is_none()
                    || manhattan_distance(tile, anchor) > u64::from(radius)
                    || !self.package.is_walkable(tile)
                    || place_id.is_some_and(|id| {
                        !self
                            .package
                            .place(id)
                            .is_some_and(|place| place.tiles.contains(&tile))
                    })
                    || place_id.is_none() && self.package.place_at_tile(tile).is_none()
                {
                    continue;
                }
                if self.package.path(from, tile).is_err() {
                    continue;
                }
                let walking_distance = if place_id.is_none() {
                    let Ok(path_from_anchor) = self.package.path(anchor, tile) else {
                        continue;
                    };
                    let walking_distance = path_from_anchor.len();
                    if walking_distance > radius as usize {
                        continue;
                    }
                    walking_distance
                } else {
                    0
                };
                candidates.push((walking_distance, tile));
            }
        }
        if candidates.is_empty() {
            return match place_id {
                Some(_) => Ok(anchor),
                None if from == anchor => Ok(from),
                None => Err(anyhow::anyhow!("no reachable stopping tile near target")),
            };
        }
        if place_id.is_some() {
            candidates.sort_by_key(|(_, tile)| {
                (
                    loads.get(tile).copied().unwrap_or_default(),
                    manhattan_distance(*tile, anchor),
                    movement_tile_tie(self.agent_did, &self.package.space_id, *tile),
                    tile[1],
                    tile[0],
                )
            });
        } else {
            candidates.sort_by_key(|(walking_distance, tile)| {
                (
                    loads.get(tile).copied().unwrap_or_default(),
                    *walking_distance,
                    movement_tile_tie(self.agent_did, &self.package.space_id, *tile),
                    tile[1],
                    tile[0],
                )
            });
        }
        Ok(candidates[0].1)
    }

    fn tile_loads(&self) -> BTreeMap<[u32; 2], usize> {
        let mut latest = BTreeMap::<(String, String), &SignedSpacePresence>::new();
        for signed in self.presences {
            let record = &signed.record;
            let key = (record.network_id.clone(), record.agent_did.clone());
            if record.network_id == self.network && record.agent_did == self.agent_did {
                continue;
            }
            if latest
                .get(&key)
                .is_none_or(|old| record.seq > old.record.seq)
            {
                latest.insert(key, signed);
            }
        }
        let mut loads = BTreeMap::new();
        for signed in latest.into_values() {
            let Some(version) = self
                .catalog
                .version(&signed.record.space_id, &signed.record.space_version)
            else {
                continue;
            };
            let current = signed.record.tile_at(version.walk_speed, self.now);
            if self.package.is_walkable(current) {
                *loads.entry(current).or_insert(0) += 1;
            }
            let destination = signed.record.tile_at(version.walk_speed, i64::MAX);
            if destination != current && self.package.is_walkable(destination) {
                *loads.entry(destination).or_insert(0) += 1;
            }
        }
        loads
    }
}

fn manhattan_distance(left: [u32; 2], right: [u32; 2]) -> u64 {
    u64::from(left[0].abs_diff(right[0])) + u64::from(left[1].abs_diff(right[1]))
}

fn movement_tile_tie(agent_did: &str, space: &str, tile: [u32; 2]) -> [u8; 32] {
    let digest = Sha256::digest(format!("{agent_did}\n{space}\n{},{}", tile[0], tile[1]));
    let mut result = [0; 32];
    result.copy_from_slice(&digest);
    result
}

pub(crate) async fn set_agent_activity(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<SetActivityBody>,
) -> Response {
    if let Err(response) = authorize(&state, &headers).await {
        return response;
    }
    if let Err(error) = validate_activity(&body.activity) {
        return bad_request(error);
    }
    if state.own_space_presence.lock().await.active_space.is_none() {
        return bad_request("agent has not entered the space; call enter_space first");
    }
    let now = chrono::Utc::now().timestamp_millis();
    match registry::update_presence(&state, |previous, package| {
        let mut record = previous.context("agent has not joined the space")?.clone();
        record.from_tile = record.tile_at(package.walk_speed, now);
        record.path.clear();
        record.depart_at = now;
        record.updated_at = now;
        record.destination_place_id.clone_from(
            &package
                .place_at_tile(record.from_tile)
                .context("current tile has no place")?
                .place_id,
        );
        record.activity = body.activity;
        record.object_place_id = body.object_place_id;
        Ok(record)
    })
    .await
    {
        Ok(presence) => Json(json!({"ok":true,"presence":presence})).into_response(),
        Err(error) => bad_request(&error),
    }
}

/// Changes what this agent wears: a built-in skin (`avatar_id`) or a custom skin it owns
/// (`skin_id`). The choice is signed into a fresh presence record at the agent's current
/// position, so the map changes at once and the choice survives re-entry. A custom skin keeps
/// the built-in one as the fallback clients draw while the custom image loads.
pub(crate) async fn set_agent_skin(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<SetSkinBody>,
) -> Response {
    if let Err(response) = authorize(&state, &headers).await {
        return response;
    }
    let choice = match (body.avatar_id, body.skin_id) {
        (Some(avatar_id), None) if avatar_id < SPACE_AVATAR_COUNT => Ok(avatar_id),
        (Some(_), None) => {
            return bad_request(&format!("avatar_id must be below {SPACE_AVATAR_COUNT}"));
        }
        (None, Some(skin_id)) => Err(skin_id),
        _ => {
            return bad_request(
                "provide exactly one of avatar_id (built-in skin, 0-24) or skin_id (a custom skin you own)",
            );
        }
    };
    if state.own_space_presence.lock().await.active_space.is_none() {
        return bad_request("agent has not entered the space; call enter_space first");
    }
    if let Err(skin_id) = &choice {
        if let Err(error) = validate_skin_id(skin_id) {
            return bad_request(error);
        }
        let Some(client) = LedgerClient::from_state(&state) else {
            return bad_request(
                "custom skins live on the gateway; this node has no gateway configured",
            );
        };
        match client.owns_skin(&state.agent_did, skin_id).await {
            Ok(true) => {}
            Ok(false) => {
                return bad_request(&format!(
                    "skin {skin_id} is not one you published; upload it with upload_agent_skin first"
                ));
            }
            Err(error) => return skin_error_response(&LedgerError::Unavailable(error)),
        }
    }
    match restyle_presence(&state, |record| match &choice {
        Ok(avatar_id) => {
            record.avatar_id = Some(*avatar_id);
            record.skin_id = None;
        }
        Err(skin_id) => record.skin_id = Some(skin_id.clone()),
    })
    .await
    {
        Ok(presence) => Json(json!({"ok":true,"presence":presence})).into_response(),
        Err(error) => bad_request(&error),
    }
}

/// Publishes a custom skin on the gateway; the gateway charges the publication fee from this
/// agent's Watt in the same transaction. The skin is not worn until `set_agent_skin`. Several
/// agents may publish the same image; each pays once and all of them may wear it.
pub(crate) async fn upload_agent_skin(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
    Json(body): Json<UploadSkinBody>,
) -> Response {
    if let Err(response) = authorize(&state, &headers).await {
        return response;
    }
    let png_base64 = body.png_base64.trim();
    if png_base64.is_empty() {
        return bad_request("png_base64 is required");
    }
    let Some(client) = LedgerClient::from_state(&state) else {
        return bad_request(
            "custom skins live on the gateway; this node has no gateway configured",
        );
    };
    match client
        .publish_skin(&state, body.name.as_deref(), png_base64)
        .await
    {
        Ok(published) => Json(json!({"ok": true, "published": published})).into_response(),
        Err(error) => skin_error_response(&error),
    }
}

/// Skins this agent has published on the gateway, plus what it currently wears.
pub(crate) async fn list_agent_skins(
    State(state): State<ControlPlaneState>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = authorize(&state, &headers).await {
        return response;
    }
    let Some(client) = LedgerClient::from_state(&state) else {
        return bad_request(
            "custom skins live on the gateway; this node has no gateway configured",
        );
    };
    let owned = match client.skins_owned_by(&state.agent_did).await {
        Ok(owned) => owned,
        Err(error) => return skin_error_response(&LedgerError::Unavailable(error)),
    };
    let wearing = current_look(&state).await;
    Json(json!({
        "ok": true,
        "owner": state.agent_did,
        "skins": owned["skins"],
        "fee_watt": owned["fee_watt"],
        "wearing": wearing,
    }))
    .into_response()
}

/// What the agent wears in its active Space, `None` when it has not entered one.
async fn current_look(state: &ControlPlaneState) -> Option<Value> {
    let registry = state.own_space_presence.lock().await;
    let (network, space) = registry.active_space.as_ref()?;
    let signed = registry.networks.get(network)?.get(space)?;
    Some(json!({
        "avatar_id": signed.record.avatar_id,
        "skin_id": signed.record.skin_id,
    }))
}

/// Re-signs the current presence at the agent's present tile with `apply` folded in: movement
/// stops where the agent is, the destination becomes the place under it.
async fn restyle_presence(
    state: &ControlPlaneState,
    apply: impl FnOnce(&mut wattetheria_gateway_contract::SpacePresenceRecord),
) -> Result<SignedSpacePresence> {
    let now = chrono::Utc::now().timestamp_millis();
    registry::update_presence(state, |previous, package| {
        let mut record = previous.context("agent has not joined the space")?.clone();
        record.from_tile = record.tile_at(package.walk_speed, now);
        record.path.clear();
        record.depart_at = now;
        record.updated_at = now;
        record.destination_place_id.clone_from(
            &package
                .place_at_tile(record.from_tile)
                .context("current tile has no place")?
                .place_id,
        );
        apply(&mut record);
        Ok(record)
    })
    .await
}

/// Gateway skin errors keep their meaning: no Watt is 402, an unknown skin 404, a refusal the
/// gateway explained (bad image, not the owner) keeps its status, an unreachable gateway 502.
fn skin_error_response(error: &LedgerError) -> Response {
    let status = match error {
        LedgerError::InsufficientBalance(_) => StatusCode::PAYMENT_REQUIRED,
        LedgerError::NotFound(_) => StatusCode::NOT_FOUND,
        LedgerError::Rejected(status, _) if status.is_client_error() => *status,
        LedgerError::Rejected(..) | LedgerError::Unavailable(_) => StatusCode::BAD_GATEWAY,
    };
    (status, Json(json!({"error": error.to_string()}))).into_response()
}

async fn network_presences(
    state: &ControlPlaneState,
    network: &str,
    space: &str,
) -> Result<Vec<SignedSpacePresence>> {
    let catalog = registry::catalog(state)?;
    let own = state.own_space_presence.lock().await;
    let own_active = catalog
        .current(space)
        .is_some_and(|package| own.visible_get(network, package).is_some());
    let mut latest = BTreeMap::<String, SignedSpacePresence>::new();
    for signed in own
        .networks
        .get(network)
        .into_iter()
        .flat_map(|spaces| spaces.values())
        .filter(|signed| signed.record.network_id == network)
        .cloned()
    {
        if latest
            .get(&signed.record.agent_did)
            .is_none_or(|old| signed.record.seq > old.record.seq)
        {
            latest.insert(signed.record.agent_did.clone(), signed);
        }
    }
    drop(own);
    let client = gateway_client()?;
    for gateway in &state.gateway_urls {
        let url = normalized_gateway_api_resource_url(
            gateway,
            "/api/spaces/presence",
            "/api/spaces/presence",
        );
        let result = async {
            client
                .get(url)
                .query(&[("network_id", network)])
                .send()
                .await?
                .error_for_status()?
                .json::<Value>()
                .await
        }
        .await;
        let Ok(payload) = result else { continue };
        let Some(records) = payload.get("presences").and_then(Value::as_array) else {
            continue;
        };
        for value in records {
            let Ok(signed) = serde_json::from_value::<SignedSpacePresence>(value.clone()) else {
                continue;
            };
            if signed.record.network_id != network
                || registry::verify_presence_signature(&signed).is_err()
            {
                continue;
            }
            let id = signed.record.agent_did.clone();
            // Network-wide ordering must precede geometry filtering, including unknown targets.
            if latest
                .get(&id)
                .is_none_or(|old| signed.record.seq > old.record.seq)
            {
                latest.insert(id, signed);
            }
        }
    }
    Ok(latest
        .into_values()
        .filter(|signed| {
            signed.record.space_id == space
                && signed.record.entered
                && (signed.record.agent_did != state.agent_did || own_active)
                && catalog
                    .version(space, &signed.record.space_version)
                    .is_some_and(|package| registry::verify_presence(package, signed).is_ok())
        })
        .collect())
}

async fn place_events(
    state: &ControlPlaneState,
    network: &str,
    space: &str,
    place_id: &str,
) -> Result<Vec<Value>> {
    let mut events = BTreeMap::new();
    let now = chrono::Utc::now().timestamp_millis();
    for announcement in world::world_announcements(state).await? {
        if announcement.network_id == network
            && let Ok(Some(event)) = world::parse_world_event(state, &announcement)
            && event.space_id == space
            && event.place_id == place_id
        {
            events.insert(event.event_id, serde_json::to_value(announcement)?);
        }
    }
    let client = gateway_client()?;
    for gateway in &state.gateway_urls {
        let url = normalized_gateway_api_resource_url(
            gateway,
            "/api/spaces/events",
            "/api/spaces/events",
        );
        let result = async {
            client
                .get(url)
                .query(&[
                    ("network_id", network),
                    ("space_id", space),
                    ("place_id", place_id),
                ])
                .send()
                .await?
                .error_for_status()?
                .json::<Value>()
                .await
        }
        .await;
        let Ok(payload) = result else { continue };
        let Some(records) = payload.get("events").and_then(Value::as_array) else {
            continue;
        };
        for event in records {
            let Ok(announcement) =
                serde_json::from_value::<EventAnnouncementPayload>(event.clone())
            else {
                continue;
            };
            if announcement.network_id != network {
                continue;
            }
            let Ok(Some(world)) = world::parse_world_event(state, &announcement) else {
                continue;
            };
            if world.space_id == space && world.place_id == place_id && world.ends_at > now {
                events.insert(world.event_id, event.clone());
            }
        }
    }
    Ok(events.into_values().collect())
}
