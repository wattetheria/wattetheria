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
use std::collections::BTreeMap;
use wattetheria_gateway_contract::SignedSpacePresence;
use wattetheria_gateway_contract::space::validate_activity;
use wattetheria_kernel::space::{Place, PlaceKind};
use wattetheria_kernel::swarm_bridge::EventAnnouncementPayload;

use crate::auth::{authorize, internal_error};
use crate::routes::mcp::{McpNoOp, normalized_gateway_api_resource_url};
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
    pub activity: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct SetActivityBody {
    pub activity: String,
    pub object_place_id: Option<String>,
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
        Ok(None) => {
            let mut response = Json(json!({"ok":true,"presence":null})).into_response();
            response.extensions_mut().insert(McpNoOp);
            response
        }
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
    if body.place_id.is_some() == body.target_agent_id.is_some() {
        return bad_request("provide exactly one of place_id or target_agent_id");
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
    let destination = if let Some(place_id) = &body.place_id {
        match package.destination_tile(&state.agent_did, place_id) {
            Ok(tile) => (tile, place_id.clone()),
            Err(error) => return bad_request(&error),
        }
    } else {
        let target_id = body
            .target_agent_id
            .as_deref()
            .expect("target choice checked");
        let records = match network_presences(&state, &network, &package.space_id).await {
            Ok(records) => records,
            Err(error) => return internal_error(&error),
        };
        let Some(target) = records
            .iter()
            .find(|record| record.record.agent_did == target_id)
        else {
            return bad_request("target agent presence not found");
        };
        let version = state
            .space_catalog
            .as_ref()
            .and_then(|catalog| {
                catalog.version(&target.record.space_id, &target.record.space_version)
            })
            .expect("presence geometry was verified");
        let tile = target.record.tile_at(version.walk_speed, now);
        let Some(place) = package.place_at_tile(tile) else {
            return bad_request("target agent is outside a known place");
        };
        (tile, place.place_id.clone())
    };
    match registry::update_presence(&state, |previous, package| {
        let mut record = previous.context("agent has not joined the space")?.clone();
        record.from_tile = record.tile_at(package.walk_speed, now);
        record.path = package.path(record.from_tile, destination.0)?;
        record.depart_at = now;
        record.updated_at = now;
        record.destination_place_id = destination.1;
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
