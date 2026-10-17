use anyhow::{Context, Result, ensure};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use chrono::Utc;
use serde_json::Value;
use std::collections::BTreeMap;
use wattetheria_gateway_contract::WorldEvent;
use wattetheria_kernel::identity::verify_with_public_key;
use wattetheria_kernel::signing::canonical_bytes;
use wattetheria_kernel::swarm_bridge::{EventAnnouncementPayload, SwarmGlobalMessageView};

use crate::routes::agent_events::AgentEventEnvelope;
use crate::state::{ControlPlaneState, StreamEvent};

#[derive(Default)]
pub struct WorldEventRegistry {
    network_id: Option<String>,
    after_sequence: u64,
    active: BTreeMap<String, (i64, EventAnnouncementPayload)>,
}

impl WorldEventRegistry {
    fn set_network(&mut self, network_id: &str) {
        if self.network_id.as_deref() != Some(network_id) {
            self.network_id = Some(network_id.to_owned());
            self.after_sequence = 0;
            self.active.clear();
        }
    }

    fn insert(&mut self, world: &WorldEvent, announcement: EventAnnouncementPayload) {
        if world.ends_at > Utc::now().timestamp_millis() {
            self.active
                .entry(announcement.announcement_id.clone())
                .or_insert((world.ends_at, announcement));
        }
    }

    fn active_announcements(&mut self) -> Vec<EventAnnouncementPayload> {
        let now = Utc::now().timestamp_millis();
        self.active.retain(|_, (ends_at, _)| *ends_at > now);
        self.active
            .values()
            .map(|(_, value)| value.clone())
            .collect()
    }
}

pub(crate) fn parse_world_event(
    state: &ControlPlaneState,
    announcement: &EventAnnouncementPayload,
) -> Result<Option<WorldEvent>> {
    let Some(catalog) = state.space_catalog.as_deref() else {
        return Ok(None);
    };
    let Some(package) = announcement
        .content
        .get("space_id")
        .and_then(Value::as_str)
        .and_then(|space| catalog.current(space))
    else {
        return Ok(None);
    };
    let event: WorldEvent = serde_json::from_value(announcement.content.clone())
        .context("decode world event content")?;
    package.validate_world_event(&event)?;
    ensure!(
        event.event_id == announcement.announcement_id,
        "world event ID does not match announcement ID"
    );
    let key =
        hex::decode(&announcement.authority_node_id).context("decode authority public key")?;
    let signature =
        hex::decode(&announcement.authority_signature).context("decode authority signature")?;
    ensure!(
        verify_with_public_key(
            &canonical_bytes(&announcement.unsigned_payload())?,
            &STANDARD.encode(signature),
            &STANDARD.encode(key)
        )?,
        "invalid authority signature"
    );
    Ok(Some(event))
}

fn announcement_from_message(
    message: &SwarmGlobalMessageView,
) -> Result<Option<EventAnnouncementPayload>> {
    if message.content.get("type").and_then(Value::as_str) != Some("EventAnnouncement") {
        return Ok(None);
    }
    Ok(Some(
        serde_json::from_value(message.content["payload"].clone())
            .context("decode global announcement projection")?,
    ))
}

pub(crate) async fn world_announcements(
    state: &ControlPlaneState,
) -> Result<Vec<EventAnnouncementPayload>> {
    let mut registry = state.world_event_registry.lock().await;
    if let Ok(Err(error)) = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        sync_world_announcements(state, &mut registry),
    )
    .await
    {
        tracing::debug!(%error, "WorldEvent projection refresh unavailable");
    }
    Ok(registry.active_announcements())
}

async fn sync_world_announcements(
    state: &ControlPlaneState,
    registry: &mut WorldEventRegistry,
) -> Result<()> {
    loop {
        let after_sequence = registry.after_sequence;
        let page = state
            .swarm_bridge
            .list_global_messages_after(200, after_sequence)
            .await?;
        let changed_network = registry.network_id.as_deref() != Some(&page.network_id);
        registry.set_network(&page.network_id);
        if changed_network && after_sequence != 0 {
            continue;
        }
        for message in &page.messages {
            let Ok(Some(announcement)) = announcement_from_message(message) else {
                continue;
            };
            if announcement.network_id == page.network_id
                && let Ok(Some(world)) = parse_world_event(state, &announcement)
            {
                registry.insert(&world, announcement);
            }
        }
        if let Some(sequence) = page.messages.last().map(|message| message.sequence) {
            registry.after_sequence = registry.after_sequence.max(sequence);
        }
        let Some(next_sequence) = page.next_after_sequence else {
            return Ok(());
        };
        ensure!(
            next_sequence > after_sequence,
            "Global forward cursor did not advance"
        );
        registry.after_sequence = registry.after_sequence.max(next_sequence);
    }
}

pub(crate) async fn receive_world_announcement(
    state: &ControlPlaneState,
    event: &mut AgentEventEnvelope,
) -> Result<()> {
    if event.event_type != "global_announcement_received" {
        return Ok(());
    }
    let received: EventAnnouncementPayload = serde_json::from_value(event.payload.clone())
        .context("decode global announcement callback")?;
    let Some(world) = parse_world_event(state, &received)? else {
        return Ok(());
    };
    let stored = state
        .swarm_bridge
        .get_global_announcement(&received.network_id, &received.announcement_id)
        .await?
        .context("announcement is not present in the verified Wattswarm Global projection")?;
    ensure!(
        stored == received,
        "announcement callback does not match verified Wattswarm projection"
    );
    let stored_world = parse_world_event(state, &stored)?
        .context("stored announcement is outside the active Agent Space")?;
    if stored_world.ends_at <= Utc::now().timestamp_millis() {
        return Ok(());
    }
    {
        let mut registry = state.world_event_registry.lock().await;
        registry.set_network(&stored.network_id);
        registry.insert(&stored_world, stored);
    }
    "world_event".clone_into(&mut event.event_type);
    event.event_id.clone_from(&received.announcement_id);
    event.dedupe_key = Some(world.event_id);
    event.allowed_actions = vec!["ignore".to_owned()];
    event.requires_commit = false;
    event.payload = serde_json::to_value(&received)?;
    let _ = state.stream_tx.send(StreamEvent {
        kind: "world.event.received".to_owned(),
        timestamp: i64::try_from(received.issued_at / 1_000)?,
        payload: serde_json::to_value(received)?,
    });
    Ok(())
}

pub(crate) fn world_event_for_board(
    state: &ControlPlaneState,
    message: &SwarmGlobalMessageView,
) -> Option<Value> {
    let announcement = announcement_from_message(message).ok()??;
    let event = parse_world_event(state, &announcement).ok()??;
    serde_json::to_value(event).ok()
}
