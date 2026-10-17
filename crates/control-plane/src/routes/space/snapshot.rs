use anyhow::Result;
use wattetheria_gateway_contract::{SignedSpacePresence, space::NodePresence};
use wattetheria_kernel::swarm_bridge::EventAnnouncementPayload;

use crate::state::ControlPlaneState;

pub(crate) struct SpaceSnapshot {
    pub space_presences: Vec<SignedSpacePresence>,
    pub world_events: Vec<EventAnnouncementPayload>,
    pub node_presence: Option<NodePresence>,
}

pub(crate) async fn export(state: &ControlPlaneState) -> Result<SpaceSnapshot> {
    let space_presences = match super::registry::current_network(state).await {
        Ok(network) => state
            .own_space_presence
            .lock()
            .await
            .projection(&network, state.space_catalog.as_deref()),
        _ => Vec::new(),
    };
    Ok(SpaceSnapshot {
        space_presences,
        world_events: super::world::world_announcements(state)
            .await
            .unwrap_or_default(),
        node_presence: super::presence::node_presence(state)?,
    })
}
