use std::collections::BTreeSet;

use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};

use super::{Place, PlaceKind, SpacePackage, SpacePresenceRecord, WorldEvent};

impl SpacePackage {
    pub fn validate(&self) -> Result<()> {
        ensure!(!self.space_id.trim().is_empty(), "space ID is required");
        ensure!(self.size[0] > 0 && self.size[1] > 0, "space size is empty");
        ensure!(self.tile_size > 0, "tile size is zero");
        ensure!(
            self.walk_speed.is_finite() && self.walk_speed > 0.0,
            "walk speed must be positive and finite"
        );
        ensure!(
            self.walkable_grid.len() == self.size[1] as usize
                && self.walkable_grid.iter().all(|row| {
                    row.len() == self.size[0] as usize && row.iter().all(|cell| *cell <= 1)
                }),
            "walkable grid does not match space size or contains invalid cells"
        );
        let mut ids = BTreeSet::new();
        for place in &self.places {
            ensure!(
                !place.place_id.trim().is_empty() && ids.insert(&place.place_id),
                "place IDs must be nonempty and unique"
            );
            ensure!(
                !place.tiles.is_empty(),
                "place {} has no tiles",
                place.place_id
            );
            ensure!(
                place.tiles.iter().all(|tile| self.is_walkable(*tile)),
                "place {} contains blocked or out-of-bounds tiles",
                place.place_id
            );
            ensure!(
                place.tiles.iter().collect::<BTreeSet<_>>().len() == place.tiles.len(),
                "place {} contains duplicate tiles",
                place.place_id
            );
        }
        for place in &self.places {
            match &place.parent_place_id {
                Some(id) => {
                    let parent = self.place(id).context("unknown parent place")?;
                    let expected_kind = match place.kind {
                        PlaceKind::World => anyhow::bail!("world must not have a parent"),
                        PlaceKind::Sector => PlaceKind::World,
                        PlaceKind::Arena => PlaceKind::Sector,
                        PlaceKind::GameObject => PlaceKind::Arena,
                    };
                    ensure!(parent.kind == expected_kind, "invalid place hierarchy");
                    let parent_tiles: BTreeSet<_> = parent.tiles.iter().collect();
                    ensure!(
                        place.tiles.iter().all(|tile| parent_tiles.contains(tile)),
                        "place {} contains tiles outside its parent",
                        place.place_id
                    );
                }
                None => ensure!(
                    place.kind == PlaceKind::World && place.place_id == self.space_id,
                    "root place must be the space world"
                ),
            }
        }
        ensure!(
            self.place(&self.space_id).is_some(),
            "space world place is missing"
        );
        self.place(&self.spawn_place_id)
            .context("spawn place is unknown")?;
        for binding in &self.service_bindings {
            ensure!(
                self.place(&binding.place_id).is_some(),
                "unknown binding place"
            );
            ensure!(
                !binding.agent_id.trim().is_empty(),
                "binding agent ID is required"
            );
        }
        self.validate_portals()?;
        let mut value = serde_json::to_value(self)?;
        value
            .as_object_mut()
            .context("space package must be an object")?
            .remove("space_version");
        let hash = hex::encode(Sha256::digest(serde_jcs::to_vec(&value)?));
        ensure!(
            self.space_version == format!("sha256:{hash}"),
            "space version does not match package content"
        );
        Ok(())
    }

    fn validate_portals(&self) -> Result<()> {
        let mut portal_ids = BTreeSet::new();
        for portal in &self.portals {
            ensure!(
                !portal.portal_id.trim().is_empty() && portal_ids.insert(&portal.portal_id),
                "portal IDs must be nonempty and unique"
            );
            ensure!(
                self.place(&portal.source_place_id).is_some(),
                "unknown portal source place"
            );
            ensure!(
                !portal.target_space_id.trim().is_empty(),
                "portal target space is required"
            );
            ensure!(
                !portal.target_place_id.trim().is_empty(),
                "portal target place is required"
            );
        }
        Ok(())
    }

    #[must_use]
    pub fn place(&self, place_id: &str) -> Option<&Place> {
        self.places.iter().find(|place| place.place_id == place_id)
    }

    #[must_use]
    pub fn places_at_tile(&self, tile: [u32; 2]) -> Vec<&Place> {
        let mut places: Vec<_> = self
            .places
            .iter()
            .filter(|place| place.tiles.contains(&tile))
            .collect();
        places.sort_by(|left, right| {
            left.kind
                .cmp(&right.kind)
                .then(left.place_id.cmp(&right.place_id))
        });
        places
    }

    #[must_use]
    pub fn place_at_tile(&self, tile: [u32; 2]) -> Option<&Place> {
        self.places_at_tile(tile).into_iter().last()
    }

    #[must_use]
    pub fn is_walkable(&self, tile: [u32; 2]) -> bool {
        tile[0] < self.size[0]
            && tile[1] < self.size[1]
            && self
                .walkable_grid
                .get(tile[1] as usize)
                .and_then(|row| row.get(tile[0] as usize))
                == Some(&1)
    }

    pub fn validate_record(&self, record: &SpacePresenceRecord) -> Result<()> {
        ensure!(record.space_id == self.space_id, "space ID mismatch");
        ensure!(
            record.space_version == self.space_version,
            "space version mismatch"
        );
        ensure!(!record.agent_did.trim().is_empty(), "agent DID is required");
        ensure!(record.seq > 0, "presence sequence must be positive");
        ensure!(
            self.is_walkable(record.from_tile),
            "presence starts on blocked or out-of-bounds tile"
        );
        let destination = self
            .place(&record.destination_place_id)
            .context("unknown destination place")?;
        let mut tile = record.from_tile;
        for direction in record.path.bytes() {
            tile = self
                .step(tile, direction)
                .context("presence path leaves the walkable grid")?;
        }
        ensure!(
            destination.tiles.contains(&tile),
            "presence path does not end at destination place"
        );
        if let Some(id) = &record.object_place_id {
            let object = self.place(id).context("unknown object place")?;
            ensure!(
                object.kind == PlaceKind::GameObject,
                "activity object must be a game object"
            );
            ensure!(
                object.tiles.contains(&tile),
                "activity object is not at destination tile"
            );
        }
        Ok(())
    }

    pub fn validate_world_event(&self, event: &WorldEvent) -> Result<()> {
        ensure!(
            event.space_id == self.space_id,
            "world event space ID mismatch"
        );
        ensure!(
            !event.event_id.trim().is_empty(),
            "world event ID is required"
        );
        ensure!(
            !event.title.trim().is_empty(),
            "world event title is required"
        );
        ensure!(
            self.place(&event.place_id).is_some(),
            "unknown world event place"
        );
        ensure!(
            event.ends_at > event.starts_at,
            "world event end must follow start"
        );
        Ok(())
    }
}
