use anyhow::{Context, Result};
use wattetheria_gateway_contract::space::NodePresence;
use wattetheria_kernel::local_db::domain::ONLINE_PROOF;
use wattetheria_kernel::online_proof::OnlineProofManager;

use crate::state::ControlPlaneState;

pub(crate) fn node_presence(state: &ControlPlaneState) -> Result<Option<NodePresence>> {
    let Some(manager) = state
        .local_db
        .load_domain::<OnlineProofManager>(ONLINE_PROOF)?
    else {
        return Ok(None);
    };
    let Some(proof) = manager.get_proof(&state.agent_did) else {
        return Ok(None);
    };
    Ok(Some(NodePresence {
        node_id: state.agent_did.clone(),
        agent_did: state.agent_did.clone(),
        last_heartbeat_ms: proof["last_heartbeat"]
            .as_i64()
            .context("heartbeat proof missing last_heartbeat")?
            .checked_mul(1_000)
            .context("heartbeat timestamp overflow")?,
        heartbeat_interval_sec: proof["heartbeat_interval_sec"]
            .as_i64()
            .context("heartbeat proof missing heartbeat interval")?,
    }))
}
