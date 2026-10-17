use anyhow::Result;
use tokio::time::{Duration, interval};
use wattetheria_control_plane::ControlPlaneState;
use wattetheria_kernel::identity::IdentityCompatView;
use wattetheria_kernel::online_proof::OnlineProofManager;

pub struct LoopContext<'a> {
    pub online_proof: &'a mut OnlineProofManager,
    pub identity: &'a IdentityCompatView,
    pub control_state: &'a ControlPlaneState,
}

pub async fn run_loop(ctx: LoopContext<'_>) -> Result<()> {
    let mut heartbeat = interval(Duration::from_secs(20));
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                break;
            }
            _ = heartbeat.tick() => {
                let lease = ctx.online_proof.heartbeat(&ctx.identity.agent_did);
                let _ = ctx.control_state.local_db.save_domain(
                    wattetheria_kernel::local_db::domain::ONLINE_PROOF,
                    ctx.online_proof,
                );
                if let Some(lease) = lease {
                    let _ = ctx.control_state.stream_tx.send(wattetheria_control_plane::StreamEvent {
                        kind:"node.presence.heartbeat".to_owned(),
                        timestamp:lease.last_heartbeat,
                        payload:serde_json::to_value(wattetheria_kernel::space::NodePresence {
                            node_id:ctx.control_state.agent_did.clone(),
                            agent_did:lease.agent_did,
                            last_heartbeat_ms:lease.last_heartbeat * 1_000,
                            heartbeat_interval_sec:lease.heartbeat_interval_sec,
                        })?,
                    });
                }
            }
        }
    }
    Ok(())
}
