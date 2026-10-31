use std::collections::BTreeMap;

use anyhow::{Context, Result, ensure};
use rand::Rng;
use serde::{Deserialize, Serialize};
use wattetheria_gateway_contract::space::validate_activity;
use wattetheria_gateway_contract::{SignedSpacePresence, SpacePresenceRecord};
use wattetheria_kernel::identity::did_key_from_public_key_b64;
use wattetheria_kernel::local_db::LocalDb;
use wattetheria_kernel::network_agent_registration::{
    PERMISSION_STATUS_ACTIVE, load_network_permission_checkpoint, network_permission_update,
};
use wattetheria_kernel::signing::verify_payload;
use wattetheria_kernel::space::{SpaceCatalog, SpacePackage};

use crate::state::{ControlPlaneState, StreamEvent};

pub const OWN_SPACE_PRESENCE_DOMAIN: &str =
    wattetheria_kernel::local_db::domain::OWN_SPACE_PRESENCE;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OwnSpacePresence {
    pub networks: BTreeMap<String, BTreeMap<String, SignedSpacePresence>>,
    #[serde(skip)]
    pub active_space: Option<(String, String)>,
}

impl OwnSpacePresence {
    /// The look the agent chose most recently, across every Space and network it has records
    /// in: the built-in skin index plus the custom skin, if any. Own records win ties with the
    /// gateway's copies. A brand-new agent gets a random built-in skin.
    fn assigned_skin(&self, recovered: &[SignedSpacePresence]) -> (u8, Option<String>) {
        recovered
            .iter()
            .chain(self.networks.values().flat_map(BTreeMap::values))
            .filter(|signed| signed.record.avatar_id.is_some())
            .max_by_key(|signed| signed.record.updated_at)
            .map_or_else(
                || {
                    let random = rand::rng()
                        .random_range(0..wattetheria_gateway_contract::space::SPACE_AVATAR_COUNT);
                    (random, None)
                },
                |signed| {
                    (
                        signed.record.avatar_id.unwrap_or_default(),
                        signed.record.skin_id.clone(),
                    )
                },
            )
    }

    pub fn load(
        db: &LocalDb,
        catalog: Option<&SpaceCatalog>,
        local_agent_did: &str,
    ) -> Result<Self> {
        let Some(value) = db.load_domain::<serde_json::Value>(OWN_SPACE_PRESENCE_DOMAIN)? else {
            return Ok(Self::default());
        };
        let upgrade = value.get("networks").is_none();
        let presence = if upgrade {
            let spaces = value
                .get("spaces")
                .and_then(serde_json::Value::as_object)
                .context("stored presence has no spaces")?;
            let network = verified_checkpoint_network(db, local_agent_did)?.unwrap_or_default();
            let mut own = BTreeMap::new();
            for (space_id, record) in spaces {
                if record.get("record").is_some() {
                    own.insert(space_id.clone(), serde_json::from_value(record.clone())?);
                } else {
                    let records: BTreeMap<String, SignedSpacePresence> =
                        serde_json::from_value(record.clone())?;
                    for (agent_id, signed) in records {
                        ensure!(
                            signed.record.agent_did == agent_id,
                            "presence registry key does not match signed record"
                        );
                        ensure!(
                            agent_id == local_agent_did,
                            "stored presence is not the local agent"
                        );
                        own.insert(space_id.clone(), signed);
                    }
                }
            }
            Self {
                networks: BTreeMap::from([(network, own)]),
                active_space: None,
            }
        } else {
            serde_json::from_value(value)?
        };
        for (network, spaces) in &presence.networks {
            for (space_id, signed) in spaces {
                ensure!(
                    signed.record.space_id == *space_id,
                    "presence registry key does not match signed record"
                );
                ensure!(
                    signed.record.agent_did == local_agent_did,
                    "stored presence is not the local agent"
                );
                ensure!(
                    signed.record.network_id.is_empty() || signed.record.network_id == *network,
                    "presence registry network does not match signed record"
                );
                verify_presence_signature(signed)?;
                if let Some(package) = catalog
                    .and_then(|catalog| catalog.version(space_id, &signed.record.space_version))
                {
                    package.validate_record(&signed.record)?;
                }
            }
        }
        if upgrade {
            db.save_domain(OWN_SPACE_PRESENCE_DOMAIN, &presence)?;
        }
        Ok(presence)
    }

    #[must_use]
    pub fn get(&self, network_id: &str, package: &SpacePackage) -> Option<&SignedSpacePresence> {
        self.stored(network_id, &package.space_id)
            .filter(|signed| signed.record.space_version == package.space_version)
            .filter(|signed| validate_activity(&signed.record.activity).is_ok())
    }

    #[must_use]
    pub fn visible_get(
        &self,
        network_id: &str,
        package: &SpacePackage,
    ) -> Option<&SignedSpacePresence> {
        (self.active_space_id(network_id) == Some(&package.space_id))
            .then(|| {
                self.get(network_id, package).filter(|signed| {
                    signed.record.entered && signed.record.network_id == network_id
                })
            })
            .flatten()
    }

    #[must_use]
    pub fn projection(
        &self,
        network_id: &str,
        catalog: Option<&SpaceCatalog>,
    ) -> Vec<SignedSpacePresence> {
        self.networks
            .get(network_id)
            .into_iter()
            .flat_map(|spaces| spaces.values())
            .filter(|signed| signed.record.network_id == network_id)
            .filter(|signed| validate_activity(&signed.record.activity).is_ok())
            .filter(|signed| {
                !signed.record.entered
                    || (self.active_space_id(network_id) == Some(&signed.record.space_id)
                        && catalog
                            .and_then(|catalog| {
                                catalog
                                    .version(&signed.record.space_id, &signed.record.space_version)
                            })
                            .is_some_and(|package| verify_presence(package, signed).is_ok()))
            })
            .cloned()
            .collect()
    }

    #[must_use]
    pub fn stored(&self, network_id: &str, space_id: &str) -> Option<&SignedSpacePresence> {
        self.networks.get(network_id)?.get(space_id)
    }

    #[must_use]
    pub fn active_space_id(&self, network_id: &str) -> Option<&str> {
        self.active_space
            .as_ref()
            .filter(|(network, _)| network == network_id)
            .map(|(_, space)| space.as_str())
    }

    #[must_use]
    pub fn sequence_floor(&self, network_id: &str) -> u64 {
        self.networks
            .get(network_id)
            .into_iter()
            .flat_map(|spaces| spaces.values())
            .map(|signed| signed.record.seq)
            .max()
            .unwrap_or(0)
    }
}

fn verified_checkpoint_network(db: &LocalDb, agent_did: &str) -> Result<Option<String>> {
    let Some(checkpoint) = load_network_permission_checkpoint(db, agent_did, None)? else {
        return Ok(None);
    };
    let permission = network_permission_update(db, &checkpoint)?;
    Ok(permission
        .credential_id
        .is_some()
        .then_some(checkpoint.network_id))
}

pub(super) async fn current_network(state: &ControlPlaneState) -> Result<String> {
    let network = match state.swarm_bridge.current_network_id().await {
        Ok(network) if !network.trim().is_empty() => network,
        _ => verified_checkpoint_network(&state.local_db, &state.agent_did)?
            .context("current network is unavailable")?,
    };
    ensure!(!network.trim().is_empty(), "current network is unavailable");
    Ok(network)
}

pub(super) fn catalog(state: &ControlPlaneState) -> Result<&SpaceCatalog> {
    let catalog = state
        .space_catalog
        .as_deref()
        .context("Space is unavailable: configure --space-catalog-path")?;
    Ok(catalog)
}

pub(super) async fn selected_package(
    state: &ControlPlaneState,
    requested: Option<&str>,
) -> Result<(String, SpacePackage)> {
    let network = current_network(state).await?;
    let catalog = catalog(state)?;
    let registry = state.own_space_presence.lock().await;
    let space_id = requested
        .or(registry.active_space_id(&network))
        .unwrap_or(&catalog.default_space_id);
    Ok((
        network,
        catalog
            .current(space_id)
            .context("unknown configured Space")?
            .clone(),
    ))
}

pub(super) fn verify_presence(package: &SpacePackage, signed: &SignedSpacePresence) -> Result<()> {
    verify_presence_signature(signed)?;
    validate_activity(&signed.record.activity).map_err(anyhow::Error::msg)?;
    package.validate_record(&signed.record)
}

pub(super) fn verify_presence_signature(signed: &SignedSpacePresence) -> Result<()> {
    ensure!(signed.record.seq > 0, "presence sequence must be positive");
    ensure!(
        !signed.record.space_id.trim().is_empty() && !signed.record.space_version.trim().is_empty(),
        "presence Space identity is required"
    );
    ensure!(
        signed
            .record
            .path
            .bytes()
            .all(|direction| b"URDL".contains(&direction)),
        "presence path contains invalid directions"
    );
    ensure!(
        did_key_from_public_key_b64(&signed.public_key)? == signed.record.agent_did,
        "presence DID does not match public key"
    );
    ensure!(
        verify_payload(&signed.record, &signed.signature, &signed.public_key)?,
        "invalid presence signature"
    );
    Ok(())
}

pub(super) async fn update_presence(
    state: &ControlPlaneState,
    update: impl FnOnce(Option<&SpacePresenceRecord>, &SpacePackage) -> Result<SpacePresenceRecord>,
) -> Result<SignedSpacePresence> {
    let network = require_active_network(state).await?;
    let catalog = catalog(state)?;
    let mut registry = state.own_space_presence.lock().await;
    let space = registry
        .active_space_id(&network)
        .context("agent has not entered the space; call enter_space first")?;
    let package = catalog
        .current(space)
        .context("active Space is unavailable")?;
    let previous = registry
        .visible_get(&network, package)
        .context("agent has not entered the current network Space")?;
    let mut record = update(Some(&previous.record), package)?;
    record.seq = next_sequence(&registry, &network, &[])?;
    let signed = persist_presence(state, &mut registry, &network, record)?;
    drop(registry);
    emit_presence(state, &signed)?;
    Ok(signed)
}

pub(super) async fn require_active_network(state: &ControlPlaneState) -> Result<String> {
    let network = current_network(state).await?;
    let checkpoint =
        load_network_permission_checkpoint(&state.local_db, &state.agent_did, Some(&network))?
            .context("active network agent permission is required to join Agent Space")?;
    let permission = network_permission_update(&state.local_db, &checkpoint)?;
    ensure!(
        permission.checkpoint.permission_status == PERMISSION_STATUS_ACTIVE
            && permission.credential_id.is_some(),
        "active network agent permission is required to join Agent Space"
    );
    Ok(network)
}

fn next_sequence(
    registry: &OwnSpacePresence,
    network: &str,
    recovered: &[SignedSpacePresence],
) -> Result<u64> {
    recovered
        .iter()
        .map(|signed| signed.record.seq)
        .fold(
            registry
                .sequence_floor(network)
                .max(registry.sequence_floor("")),
            u64::max,
        )
        .checked_add(1)
        .context("presence sequence exhausted")
}

fn persist_records(
    state: &ControlPlaneState,
    registry: &mut OwnSpacePresence,
    network: &str,
    records: Vec<SpacePresenceRecord>,
) -> Result<Vec<SignedSpacePresence>> {
    ensure!(
        state.signer.agent_did() == state.agent_did,
        "active signer is not the local agent"
    );
    let catalog = state.space_catalog.as_deref();
    let mut candidate = registry.clone();
    let mut signed_records = Vec::new();
    for mut record in records {
        ensure!(
            record.agent_did == state.agent_did,
            "presence is not the local agent"
        );
        network.clone_into(&mut record.network_id);
        if let Some(package) =
            catalog.and_then(|catalog| catalog.version(&record.space_id, &record.space_version))
        {
            package.validate_record(&record)?;
        } else {
            let saved = registry
                .stored(network, &record.space_id)
                .context("inactive unknown Space requires a saved own record")?;
            verify_presence_signature(saved)?;
            let mut inactive = saved.record.clone();
            network.clone_into(&mut inactive.network_id);
            inactive.entered = false;
            inactive.seq = record.seq;
            inactive.updated_at = record.updated_at;
            ensure!(
                !record.entered && record == inactive,
                "inactive unknown Space must preserve saved geometry and content"
            );
        }
        validate_activity(&record.activity).map_err(anyhow::Error::msg)?;
        let signed = SignedSpacePresence {
            signature: state.sign_payload(&record)?,
            public_key: state.signer.public_key().to_owned(),
            record,
        };
        candidate
            .networks
            .entry(network.to_owned())
            .or_default()
            .insert(signed.record.space_id.clone(), signed.clone());
        signed_records.push(signed);
    }
    state
        .local_db
        .save_domain(OWN_SPACE_PRESENCE_DOMAIN, &candidate)?;
    *registry = candidate;
    Ok(signed_records)
}

fn persist_presence(
    state: &ControlPlaneState,
    registry: &mut OwnSpacePresence,
    network: &str,
    record: SpacePresenceRecord,
) -> Result<SignedSpacePresence> {
    persist_records(state, registry, network, vec![record])?
        .pop()
        .context("missing persisted presence")
}

pub(super) fn emit_presence(state: &ControlPlaneState, signed: &SignedSpacePresence) -> Result<()> {
    let _ = state.stream_tx.send(StreamEvent {
        kind: "space.presence.updated".to_owned(),
        timestamp: signed.record.updated_at / 1_000,
        payload: serde_json::to_value(signed)?,
    });
    Ok(())
}

pub async fn emit_current_space_presence(state: &ControlPlaneState) -> Result<()> {
    let Ok(network) = current_network(state).await else {
        return Ok(());
    };
    let records = state
        .own_space_presence
        .lock()
        .await
        .projection(&network, state.space_catalog.as_deref());
    for signed in records {
        emit_presence(state, &signed)?;
    }
    Ok(())
}

pub async fn initialize_space_presence(state: &ControlPlaneState) -> Result<SignedSpacePresence> {
    enter_configured_space(state, None, None).await
}

#[expect(
    clippy::too_many_lines,
    reason = "Keep the existing entry transaction together with its saved appearance assignment"
)]
pub(super) async fn enter_configured_space(
    state: &ControlPlaneState,
    requested: Option<&str>,
    portal_id: Option<&str>,
) -> Result<SignedSpacePresence> {
    let network = require_active_network(state).await?;
    let catalog = catalog(state)?;
    let mut registry = state.own_space_presence.lock().await;
    let recovery = recover_gateway_sequence(state, &network).await?;
    ensure!(
        registry.sequence_floor(&network) > 0
            || state.gateway_urls.is_empty()
            || recovery.successful_query,
        "gateway presence sequence recovery unavailable; retry enter_space when a gateway is reachable"
    );
    let local = registry.networks.get(&network);
    let history = recovery
        .records
        .iter()
        .filter(|signed| signed.record.network_id == network)
        .chain(local.into_iter().flat_map(BTreeMap::values));
    let last = history.clone().max_by_key(|signed| signed.record.seq);
    let active = registry.active_space_id(&network);
    let last_available = last.and_then(|signed| catalog.current(&signed.record.space_id));
    let reset_to_default = active.is_none() && last.is_some() && last_available.is_none();
    let resume = active
        .or(last_available.map(|package| package.space_id.as_str()))
        .unwrap_or(&catalog.default_space_id);
    ensure!(
        active.is_some() || requested.is_none_or(|requested| requested == resume),
        if reset_to_default {
            "last Space is unavailable; enter the default Space first"
        } else if last.is_some() {
            "return to the last Space before using a portal"
        } else {
            "first entry must use the default Space"
        }
    );
    let target = catalog
        .current(requested.unwrap_or(resume))
        .context("unknown configured Space")?;
    if let Some(existing) = registry.visible_get(&network, target)
        && existing.record.avatar_id.is_some()
        && !sequence_conflicts(existing, &recovery.records)
    {
        return Ok(existing.clone());
    }
    let now = chrono::Utc::now().timestamp_millis();
    let mut seq = next_sequence(&registry, &network, &recovery.records)?;
    let mut records = Vec::new();
    let landing = if active != Some(target.space_id.as_str())
        && let Some(source_package) = active.and_then(|space| catalog.current(space))
        && let Some(source) = registry.visible_get(&network, source_package)
    {
        let portal = source_package
            .portals
            .iter()
            .find(|portal| Some(portal.portal_id.as_str()) == portal_id)
            .context("cross-Space entry requires a source portal")?;
        ensure!(
            portal.target_space_id == target.space_id,
            "portal target Space mismatch"
        );
        let source_place = source_package
            .place(&portal.source_place_id)
            .context("unknown portal source")?;
        let tile = source.record.tile_at(source_package.walk_speed, now);
        let elapsed =
            chrono::Duration::milliseconds(now.saturating_sub(source.record.depart_at).max(0))
                .as_seconds_f64();
        let travelled = elapsed * source_package.walk_speed;
        ensure!(
            source_place.tiles.contains(&tile)
                && travelled >= f64::from(u32::try_from(source.record.path.len())?),
            "agent must have arrived at the source portal"
        );
        let mut leaving = source.record.clone();
        leaving.entered = false;
        leaving.seq = seq;
        leaving.updated_at = now;
        records.push(leaving);
        seq = seq.checked_add(1).context("presence sequence exhausted")?;
        Some(portal.target_place_id.as_str())
    } else {
        None
    };
    let mut record = if let Some(landing) = landing {
        spawn_record(state, target, landing, now)?
    } else {
        let saved = history
            .filter(|signed| !reset_to_default && signed.record.space_id == target.space_id)
            .max_by_key(|signed| signed.record.seq);
        restore_record(state, catalog, target, saved, now)?
    };
    record.entered = true;
    record.seq = seq;
    let (avatar_id, skin_id) = registry.assigned_skin(&recovery.records);
    record.avatar_id = Some(avatar_id);
    record.skin_id = match skin_id {
        Some(skin_id) => confirm_skin_ownership(state, skin_id).await,
        None => None,
    };
    records.push(record);
    let signed = persist_records(state, &mut registry, &network, records)?;
    registry.active_space = Some((network, target.space_id.clone()));
    drop(registry);
    for record in &signed {
        emit_presence(state, record)?;
    }
    signed.last().cloned().context("missing target presence")
}

pub(super) async fn leave_configured_space(
    state: &ControlPlaneState,
) -> Result<Option<SignedSpacePresence>> {
    let mut registry = state.own_space_presence.lock().await;
    if registry.active_space.is_none() {
        return Ok(None);
    }
    let network = require_active_network(state).await?;
    let catalog = catalog(state)?;
    let Some(space) = registry.active_space_id(&network) else {
        return Ok(None);
    };
    let package = catalog
        .current(space)
        .context("active Space is unavailable")?;
    let previous = registry
        .visible_get(&network, package)
        .context("agent has not entered the current network Space")?;
    let recovery = recover_gateway_sequence(state, &network).await?;
    let now = chrono::Utc::now().timestamp_millis();
    let mut record = previous.record.clone();
    let tile = record.tile_at(package.walk_speed, now);
    record.from_tile = tile;
    record.path.clear();
    record.depart_at = now;
    record.destination_place_id.clone_from(
        &package
            .place_at_tile(tile)
            .context("current tile has no place")?
            .place_id,
    );
    if record.object_place_id.as_deref().is_some_and(|object| {
        !package
            .place(object)
            .is_some_and(|place| place.tiles.contains(&tile))
    }) {
        record.object_place_id = None;
    }
    record.entered = false;
    record.seq = next_sequence(&registry, &network, &recovery.records)?;
    record.updated_at = now;
    let signed = persist_presence(state, &mut registry, &network, record)?;
    registry.active_space = None;
    drop(registry);
    emit_presence(state, &signed)?;
    Ok(Some(signed))
}

fn spawn_record(
    state: &ControlPlaneState,
    package: &SpacePackage,
    place: &str,
    now: i64,
) -> Result<SpacePresenceRecord> {
    Ok(SpacePresenceRecord {
        agent_did: state.agent_did.clone(),
        avatar_id: None,
        skin_id: None,
        network_id: String::new(),
        space_id: package.space_id.clone(),
        space_version: package.space_version.clone(),
        entered: false,
        seq: 1,
        from_tile: package.destination_tile(&state.agent_did, place)?,
        path: String::new(),
        depart_at: now,
        destination_place_id: place.to_owned(),
        activity: String::new(),
        object_place_id: None,
        updated_at: now,
    })
}

fn restore_record(
    state: &ControlPlaneState,
    catalog: &SpaceCatalog,
    package: &SpacePackage,
    saved: Option<&SignedSpacePresence>,
    now: i64,
) -> Result<SpacePresenceRecord> {
    let Some(saved) = saved else {
        return spawn_record(state, package, &package.spawn_place_id, now);
    };
    let Some(old) = catalog.version(&saved.record.space_id, &saved.record.space_version) else {
        return spawn_record(state, package, &package.spawn_place_id, now);
    };
    old.validate_record(&saved.record)?;
    let mut record = saved.record.clone();
    record.space_version.clone_from(&package.space_version);
    record.updated_at = now;
    if validate_activity(&record.activity).is_err() {
        record.activity.clear();
    }
    if old.walk_speed.to_bits() == package.walk_speed.to_bits()
        && package.validate_record(&record).is_ok()
    {
        return Ok(record);
    }
    let tile = saved.record.tile_at(old.walk_speed, now);
    if !package.is_walkable(tile) {
        return spawn_record(state, package, &package.spawn_place_id, now);
    }
    record.from_tile = tile;
    record.depart_at = now;
    if let Ok(path) = package
        .destination_tile(&state.agent_did, &record.destination_place_id)
        .and_then(|destination| package.path(tile, destination))
    {
        record.path = path;
    } else {
        record.path.clear();
        record.destination_place_id.clone_from(
            &package
                .place_at_tile(tile)
                .context("current tile has no place")?
                .place_id,
        );
        record.object_place_id = None;
    }
    if package.validate_record(&record).is_err() {
        record.object_place_id = None;
    }
    package.validate_record(&record)?;
    Ok(record)
}

pub async fn reconcile_space_presence(state: &ControlPlaneState) -> Result<()> {
    if state.own_space_presence.lock().await.active_space.is_none() {
        return reset_space_presence(state).await;
    }
    if state.space_catalog.is_none() {
        return Ok(());
    }
    let network = current_network(state).await?;
    let Some(checkpoint) =
        load_network_permission_checkpoint(&state.local_db, &state.agent_did, Some(&network))?
    else {
        return Ok(());
    };
    let permission = network_permission_update(&state.local_db, &checkpoint)?;
    if permission.checkpoint.permission_status != PERMISSION_STATUS_ACTIVE
        || permission.credential_id.is_none()
    {
        return Ok(());
    }
    let catalog = catalog(state)?;
    let recovered = recover_gateway_sequence(state, &network).await?.records;
    let mut registry = state.own_space_presence.lock().await;
    let Some(package) = registry
        .active_space_id(&network)
        .and_then(|space| catalog.current(space))
    else {
        return Ok(());
    };
    let Some(existing) = registry
        .visible_get(&network, package)
        .filter(|signed| sequence_conflicts(signed, &recovered))
        .cloned()
    else {
        return Ok(());
    };
    let signed = persist_recovered_sequence(state, &mut registry, &network, existing, &recovered)?;
    drop(registry);
    emit_presence(state, &signed)
}

pub async fn reset_space_presence(state: &ControlPlaneState) -> Result<()> {
    let network = match current_network(state).await {
        Ok(network) => network,
        Err(error) => {
            tracing::warn!(%error, "Space reset deferred: current network is unavailable");
            return Ok(());
        }
    };
    let recovered = recover_gateway_sequence(state, &network).await?.records;
    let mut registry = state.own_space_presence.lock().await;
    if registry.active_space.is_some() {
        return Ok(());
    }
    let mut candidate = registry.clone();
    let candidates = candidate.networks.entry(network.clone()).or_default();
    let local_latest = registry
        .networks
        .get(&network)
        .into_iter()
        .flat_map(|spaces| spaces.values())
        .max_by_key(|signed| signed.record.seq);
    let recovered_latest = recovered
        .iter()
        .filter(|signed| signed.record.network_id == network)
        .max_by_key(|signed| (signed.record.seq, signed.record.entered))
        .filter(|signed| {
            signed.record.seq > registry.sequence_floor(&network)
                || (signed.record.entered
                    && local_latest.is_some_and(|local| {
                        !local.record.entered
                            && sequence_conflicts(local, std::slice::from_ref(*signed))
                    }))
        });
    if let Some(signed) = recovered_latest {
        if let Some(package) = state.space_catalog.as_deref().and_then(|catalog| {
            catalog.version(&signed.record.space_id, &signed.record.space_version)
        }) {
            package.validate_record(&signed.record)?;
        }
        candidates.insert(signed.record.space_id.clone(), signed.clone());
    }
    let mut records = Vec::new();
    if let Some(saved) = candidates
        .values()
        .max_by_key(|signed| (signed.record.seq, signed.record.entered))
        && saved.record.entered
        && validate_activity(&saved.record.activity).is_ok()
    {
        let now = chrono::Utc::now().timestamp_millis();
        let mut record = saved.record.clone();
        record.entered = false;
        record.updated_at = now;
        record.seq = next_sequence(&registry, &network, &recovered)?;
        records.push(record);
    }
    if records.is_empty() && recovered_latest.is_none() {
        return Ok(());
    }
    let records = persist_records(state, &mut candidate, &network, records)?;
    *registry = candidate;
    drop(registry);
    for signed in records {
        emit_presence(state, &signed)?;
    }
    Ok(())
}

fn persist_recovered_sequence(
    state: &ControlPlaneState,
    registry: &mut OwnSpacePresence,
    network: &str,
    existing: SignedSpacePresence,
    recovered: &[SignedSpacePresence],
) -> Result<SignedSpacePresence> {
    let mut record = existing.record;
    record.seq = next_sequence(registry, network, recovered)?;
    if validate_activity(&record.activity).is_err() {
        record.activity.clear();
    }
    record.updated_at = chrono::Utc::now().timestamp_millis();
    persist_presence(state, registry, network, record)
}

fn sequence_conflicts(existing: &SignedSpacePresence, recovered: &[SignedSpacePresence]) -> bool {
    recovered.iter().any(|signed| {
        signed.record.seq > existing.record.seq
            || (signed.record.seq == existing.record.seq && signed.record != existing.record)
    })
}

#[derive(Default)]
struct GatewaySequenceRecovery {
    records: Vec<SignedSpacePresence>,
    successful_query: bool,
}

/// Keeps a remembered custom skin only while the gateway still lists this agent among its
/// publishers (a reset gateway would refuse the entry otherwise). Without a reachable gateway
/// the remembered choice stands.
async fn confirm_skin_ownership(state: &ControlPlaneState, skin_id: String) -> Option<String> {
    let Some(client) = crate::ledger_client::LedgerClient::from_state(state) else {
        return Some(skin_id);
    };
    match client.owns_skin(&state.agent_did, &skin_id).await {
        Ok(true) => Some(skin_id),
        Ok(false) => {
            tracing::info!(
                skin_id,
                "custom skin unknown to the gateway; entering with the built-in skin"
            );
            None
        }
        Err(error) => {
            tracing::warn!(%error, "skin ownership check unavailable; keeping the remembered skin");
            Some(skin_id)
        }
    }
}

async fn recover_gateway_sequence(
    state: &ControlPlaneState,
    network: &str,
) -> Result<GatewaySequenceRecovery> {
    if state.gateway_urls.is_empty() {
        return Ok(GatewaySequenceRecovery::default());
    }
    let client = super::gateway_client()?;
    let mut recovery = GatewaySequenceRecovery::default();
    for (gateway_index, gateway_url) in state.gateway_urls.iter().enumerate() {
        // The isolated legacy namespace contributes only a numeric sequence floor.
        for queried_network in [network, ""] {
            let recovered = async {
                let mut url = reqwest::Url::parse(&super::normalized_gateway_api_resource_url(
                    gateway_url,
                    "/api/spaces/presence",
                    "/api/spaces/presence",
                ))?;
                url.path_segments_mut()
                    .map_err(|()| anyhow::anyhow!("gateway URL cannot be a base"))?
                    .pop_if_empty()
                    .extend([state.agent_did.as_str(), "record"]);
                let response = client
                    .get(url)
                    .query(&[("network_id", queried_network)])
                    .send()
                    .await?;
                if response.status() == reqwest::StatusCode::NOT_FOUND {
                    return Ok(None);
                }
                let signed: SignedSpacePresence = response.error_for_status()?.json().await?;
                ensure!(
                    signed.record.agent_did == state.agent_did
                        && signed.record.network_id == queried_network,
                    "recovered presence does not match local network and agent"
                );
                verify_presence_signature(&signed)?;
                Ok::<Option<SignedSpacePresence>, anyhow::Error>(Some(signed))
            }
            .await;
            if let Ok(signed) = recovered {
                if queried_network == network {
                    recovery.successful_query = true;
                }
                if let Some(signed) = signed {
                    recovery.records.push(signed);
                }
            } else {
                tracing::warn!(gateway_index, "gateway presence sequence unavailable");
            }
        }
    }
    Ok(recovery)
}
