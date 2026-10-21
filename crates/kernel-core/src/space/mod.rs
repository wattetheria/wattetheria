mod catalog;
mod package;
mod routing;

#[cfg(test)]
mod tests;

use serde::{Deserialize, Serialize};

pub use catalog::{SpaceCatalog, SpaceCatalogEntry, SpaceCatalogVersion};
pub use wattetheria_gateway_contract::space::NodePresence;
pub use wattetheria_gateway_contract::{SpacePresenceRecord, WorldEvent};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SpacePackage {
    pub space_id: String,
    pub space_version: String,
    pub size: [u32; 2],
    pub tile_size: u32,
    pub walkable_grid: Vec<Vec<u8>>,
    pub places: Vec<Place>,
    pub spawn_place_id: String,
    pub walk_speed: f64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub movement_search_radius: Option<u32>,
    pub service_bindings: Vec<ServiceBinding>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub portals: Vec<Portal>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Portal {
    pub portal_id: String,
    pub source_place_id: String,
    pub target_space_id: String,
    pub target_place_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Place {
    pub place_id: String,
    pub parent_place_id: Option<String>,
    pub kind: PlaceKind,
    pub name: PlaceName,
    pub tiles: Vec<[u32; 2]>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum PlaceKind {
    World,
    Sector,
    Arena,
    GameObject,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PlaceName {
    pub en: String,
    pub zh: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ServiceBinding {
    pub place_id: String,
    pub agent_id: String,
}
