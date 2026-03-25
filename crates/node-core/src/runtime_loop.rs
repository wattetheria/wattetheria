use crate::oracle_sync::{handle_oracle_feed_packet, sync_and_publish_local_oracle_feeds};
use crate::snapshot_broadcast::publish_public_snapshot_packet;
use anyhow::Result;
use std::collections::BTreeSet;
use std::path::Path;
use tokio::time::{Duration, Instant, interval, interval_at};
use tracing::{info, warn};
use wattetheria_control_plane::ControlPlaneState;
use wattetheria_kernel::admission::{
    AdmissionConfig, AdmissionVerdict, NonceTracker, validate_gossip_packet_with_nonce,
};
use wattetheria_kernel::event_log::EventLog;
use wattetheria_kernel::identity::IdentityCompatView;
use wattetheria_kernel::online_proof::OnlineProofManager;
use wattetheria_kernel::oracle::OracleRegistry;
use wattetheria_kernel::trust::WebOfTrust;
use wattetheria_p2p_runtime::P2PNode;

pub struct LoopContext<'a> {
    pub online_proof: &'a mut OnlineProofManager,
    pub online_proof_path: &'a Path,
    pub identity: &'a IdentityCompatView,
    pub control_state: &'a ControlPlaneState,
    pub admission_config: &'a AdmissionConfig,
    pub nonce_tracker: &'a mut NonceTracker,
    pub web_of_trust: &'a mut WebOfTrust,
    pub event_log: &'a EventLog,
    pub oracle_registry: &'a mut OracleRegistry,
    pub oracle_state_path: &'a Path,
    pub gateway_publish_enabled: bool,
    pub gateway_push_interval_sec: u64,
    pub gateway_startup_jitter_sec: u64,
    pub enable_hashcash_broadcast: bool,
}

pub fn log_listeners(p2p: &P2PNode) {
    for listener in p2p.listeners() {
        info!(%listener, "listening");
    }
}

pub async fn run_loop(p2p: &mut P2PNode, ctx: LoopContext<'_>) -> Result<()> {
    let mut heartbeat = interval(Duration::from_secs(20));
    let mut oracle_sync = interval(Duration::from_secs(15));
    let gateway_start = Instant::now() + Duration::from_secs(ctx.gateway_startup_jitter_sec);
    let mut gateway_sync = interval_at(
        gateway_start,
        Duration::from_secs(ctx.gateway_push_interval_sec),
    );
    let mut known_oracle_signatures: BTreeSet<String> = BTreeSet::new();
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {
                break;
            }
            _ = heartbeat.tick() => {
                let _ = ctx.online_proof.heartbeat(&ctx.identity.agent_did);
                let _ = ctx.online_proof.persist(ctx.online_proof_path);
            }
            _ = oracle_sync.tick() => {
                sync_and_publish_local_oracle_feeds(
                    p2p,
                    ctx.oracle_registry,
                    ctx.oracle_state_path,
                    ctx.enable_hashcash_broadcast,
                    &mut known_oracle_signatures,
                )?;
            }
            _ = gateway_sync.tick(), if ctx.gateway_publish_enabled => {
                let packet = publish_public_snapshot_packet(
                    p2p,
                    ctx.control_state,
                    ctx.enable_hashcash_broadcast,
                ).await?;
                info!(
                    node_id = %packet.snapshot.payload.node_id,
                    generated_at = packet.snapshot.payload.generated_at,
                    "published public client snapshot over wattswarm"
                );
            }
            msg = p2p.poll_once() => {
                if let Some(packet) = msg? {
                    match validate_gossip_packet_with_nonce(&packet.data, ctx.admission_config, ctx.nonce_tracker) {
                        AdmissionVerdict::Accept => {
                            info!(
                                size = packet.data.len(),
                                topic = %packet.topic,
                                peer = %packet.source_peer.clone().unwrap_or_else(|| "unknown".to_string()),
                                "received gossip packet"
                            );
                            handle_oracle_feed_packet(
                                &packet.data,
                                ctx.oracle_registry,
                                ctx.oracle_state_path,
                                ctx.event_log,
                                &ctx.control_state.identity,
                                ctx.control_state.signer.as_ref(),
                                &mut known_oracle_signatures,
                            )?;
                        }
                        AdmissionVerdict::Reject(reason) => {
                            if let Some(peer) = &packet.source_peer {
                                let report = ctx.web_of_trust.report_peer(peer, &ctx.identity.agent_did, &reason);
                                if ctx.web_of_trust.is_blacklisted(peer) {
                                    warn!(
                                        peer = %peer,
                                        reason = %report.reason,
                                        "peer entered local web-of-trust blacklist"
                                    );
                                }
                            }
                            warn!(
                                peer = %packet.source_peer.clone().unwrap_or_else(|| "unknown".to_string()),
                                topic = %packet.topic,
                                %reason,
                                "dropped gossip packet by admission policy"
                            );
                        }
                    }
                }
            }
        }
    }
    Ok(())
}
