use crate::cli::Cli;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use wattetheria_control_plane::{
    ClientExportQuery, ControlPlaneState, SignedPublicClientSnapshot,
    build_signed_public_client_snapshot,
};
use wattetheria_kernel::hashcash;
use wattetheria_p2p_runtime::P2PNode;

const MIN_GATEWAY_PUSH_INTERVAL_SEC: u64 = 10;
const MAX_GATEWAY_STARTUP_JITTER_SEC: u64 = 15;
const PUBLIC_CLIENT_SNAPSHOT_PACKET_TYPE: &str = "PUBLIC_CLIENT_SNAPSHOT";
const PUBLIC_CLIENT_SNAPSHOT_PACKET_VERSION: &str = "0.1";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewaySnapshotPacket {
    pub r#type: String,
    pub version: String,
    pub timestamp: i64,
    pub hashcash: Option<Value>,
    pub snapshot: SignedPublicClientSnapshot,
}

impl GatewaySnapshotPacket {
    fn new(snapshot: SignedPublicClientSnapshot, timestamp: i64, hashcash: Option<Value>) -> Self {
        Self {
            r#type: PUBLIC_CLIENT_SNAPSHOT_PACKET_TYPE.to_string(),
            version: PUBLIC_CLIENT_SNAPSHOT_PACKET_VERSION.to_string(),
            timestamp,
            hashcash,
            snapshot,
        }
    }
}

pub fn gateway_sync_enabled(cli: &Cli) -> bool {
    !cli.gateway_urls.is_empty() || !cli.gateway_registry_urls.is_empty()
}

pub fn gateway_push_interval_sec(cli: &Cli) -> u64 {
    cli.gateway_push_interval_sec
        .max(MIN_GATEWAY_PUSH_INTERVAL_SEC)
}

pub fn gateway_startup_jitter_secs(agent_did: &str, interval_sec: u64) -> u64 {
    let jitter_window = interval_sec.min(MAX_GATEWAY_STARTUP_JITTER_SEC);
    if jitter_window == 0 {
        return 0;
    }
    let mut hasher = DefaultHasher::new();
    agent_did.hash(&mut hasher);
    hasher.finish() % (jitter_window + 1)
}

pub async fn publish_public_snapshot_packet(
    p2p: &mut P2PNode,
    control_state: &ControlPlaneState,
    include_hashcash: bool,
) -> anyhow::Result<GatewaySnapshotPacket> {
    let snapshot =
        build_signed_public_client_snapshot(control_state, &default_public_snapshot_query())
            .await?;
    let packet = GatewaySnapshotPacket::new(
        snapshot,
        Utc::now().timestamp(),
        build_snapshot_hashcash(&control_state.identity.agent_did, include_hashcash),
    );
    p2p.publish_json(&packet)?;
    Ok(packet)
}

fn default_public_snapshot_query() -> ClientExportQuery {
    ClientExportQuery {
        peer_limit: Some(200),
        task_limit: Some(500),
        organization_limit: Some(500),
        rpc_log_limit: Some(50),
        leaderboard_limit: Some(200),
        ..ClientExportQuery::default()
    }
}

fn build_snapshot_hashcash(agent_did: &str, include_hashcash: bool) -> Option<Value> {
    if !include_hashcash {
        return None;
    }

    hashcash::mint(agent_did, 12, 120_000)
        .map(|stamp| json!({"stamp": stamp, "bits": 12, "resource": agent_did}))
}

#[cfg(test)]
mod tests {
    use super::{
        GatewaySnapshotPacket, gateway_push_interval_sec, gateway_startup_jitter_secs,
        gateway_sync_enabled,
    };
    use crate::Cli;
    use clap::Parser;
    use serde_json::json;
    use wattetheria_control_plane::SignedPublicClientSnapshot;

    #[test]
    fn startup_jitter_is_deterministic_and_bounded() {
        let first = gateway_startup_jitter_secs("agent-alpha", 30);
        let second = gateway_startup_jitter_secs("agent-alpha", 30);
        assert_eq!(first, second);
        assert!(first <= 15);

        let tight_interval = gateway_startup_jitter_secs("agent-alpha", 5);
        assert!(tight_interval <= 5);
    }

    #[test]
    fn sync_enablement_uses_legacy_gateway_markers() {
        let disabled = Cli::parse_from(["wattetheria-kernel"]);
        let enabled = Cli::parse_from([
            "wattetheria-kernel",
            "--gateway-url",
            "https://gateway.example",
        ]);

        assert!(!gateway_sync_enabled(&disabled));
        assert!(gateway_sync_enabled(&enabled));
    }

    #[test]
    fn push_interval_respects_minimum() {
        let cli = Cli::parse_from([
            "wattetheria-kernel",
            "--gateway-push-interval-sec",
            "3",
            "--gateway-url",
            "https://gateway.example",
        ]);

        assert_eq!(gateway_push_interval_sec(&cli), 10);
    }

    #[test]
    fn snapshot_packet_envelope_is_tagged_for_public_snapshot_gossip() {
        let snapshot: SignedPublicClientSnapshot = serde_json::from_value(json!({
            "payload": {
                "generated_at": 123,
                "node_id": "node-a",
                "public_key": "pubkey",
                "network_status": {},
                "peers": [],
                "operator": {},
                "rpc_logs": [],
                "tasks": [],
                "organizations": [],
                "leaderboard": []
            },
            "signature": "sig",
            "signer_agent_did": "did:example:node-a"
        }))
        .expect("build signed snapshot fixture");
        let packet = GatewaySnapshotPacket::new(snapshot, 456, None);

        assert_eq!(packet.r#type, "PUBLIC_CLIENT_SNAPSHOT");
        assert_eq!(packet.version, "0.1");
        assert_eq!(packet.timestamp, 456);
        assert!(packet.hashcash.is_none());
        assert_eq!(packet.snapshot.payload.node_id, "node-a");
    }
}
