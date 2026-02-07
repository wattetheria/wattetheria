//! libp2p transport wrapper for gossip-based message exchange with anti-spam guards.

use anyhow::Result;
use chrono::Utc;
use libp2p::autonat;
use libp2p::dcutr;
use libp2p::futures::StreamExt;
use libp2p::gossipsub::{self, IdentTopic, MessageAuthenticity};
use libp2p::identify;
use libp2p::kad;
use libp2p::relay;
use libp2p::swarm::{NetworkBehaviour, SwarmEvent};
use libp2p::{Multiaddr, Swarm, identity};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone)]
pub struct P2PConfig {
    pub max_connected_peers: usize,
    pub per_peer_msgs_per_minute: usize,
    pub per_topic_msgs_per_minute: usize,
    pub per_topic_publish_per_minute: usize,
    pub topic_shards: usize,
    pub dedupe_ttl_sec: i64,
    pub message_ttl_sec: i64,
    pub min_peer_score: i64,
}

impl Default for P2PConfig {
    fn default() -> Self {
        Self {
            max_connected_peers: 64,
            per_peer_msgs_per_minute: 240,
            per_topic_msgs_per_minute: 1_200,
            per_topic_publish_per_minute: 300,
            topic_shards: 1,
            dedupe_ttl_sec: 120,
            message_ttl_sec: 300,
            min_peer_score: -8,
        }
    }
}

#[derive(Debug, Clone)]
pub struct InboundPacket {
    pub data: Vec<u8>,
    pub source_peer: Option<String>,
    pub topic: String,
    pub received_at: i64,
}

#[derive(Debug, Clone)]
struct TrafficGuard {
    config: P2PConfig,
    seen_messages: BTreeMap<String, i64>,
    peer_windows: BTreeMap<String, Vec<i64>>,
    topic_windows: BTreeMap<String, Vec<i64>>,
    local_publish_windows: BTreeMap<String, Vec<i64>>,
    peer_scores: BTreeMap<String, i64>,
    blacklisted: BTreeSet<String>,
}

impl TrafficGuard {
    fn new(config: P2PConfig) -> Self {
        Self {
            config,
            seen_messages: BTreeMap::new(),
            peer_windows: BTreeMap::new(),
            topic_windows: BTreeMap::new(),
            local_publish_windows: BTreeMap::new(),
            peer_scores: BTreeMap::new(),
            blacklisted: BTreeSet::new(),
        }
    }

    fn allow_local_publish(&mut self, topic: &str, now: i64) -> bool {
        let window = self
            .local_publish_windows
            .entry(topic.to_string())
            .or_default();
        prune_window(window, now, 60);
        if window.len() >= self.config.per_topic_publish_per_minute {
            return false;
        }
        window.push(now);
        true
    }

    fn allow_inbound(
        &mut self,
        source_peer: Option<&str>,
        topic: &str,
        data: &[u8],
        now: i64,
    ) -> bool {
        let Some(peer) = source_peer else {
            return true;
        };
        if self.blacklisted.contains(peer) {
            return false;
        }

        if !is_message_fresh(data, now, self.config.message_ttl_sec) {
            self.penalize(peer, 2);
            return false;
        }

        let digest = hex::encode(Sha256::digest(data));
        if self
            .seen_messages
            .get(&digest)
            .is_some_and(|seen_at| now - seen_at <= self.config.dedupe_ttl_sec)
        {
            self.penalize(peer, 1);
            return false;
        }
        self.seen_messages.insert(digest, now);
        self.gc_seen(now);

        let peer_window = self.peer_windows.entry(peer.to_string()).or_default();
        prune_window(peer_window, now, 60);
        if peer_window.len() >= self.config.per_peer_msgs_per_minute {
            self.penalize(peer, 3);
            return false;
        }
        peer_window.push(now);

        let topic_key = format!("{topic}:{peer}");
        let topic_window = self.topic_windows.entry(topic_key).or_default();
        prune_window(topic_window, now, 60);
        if topic_window.len() >= self.config.per_topic_msgs_per_minute {
            self.penalize(peer, 2);
            return false;
        }
        topic_window.push(now);

        self.reward(peer, 1);
        true
    }

    fn enforce_peer_limit(&mut self, peer_count: usize, newest_peer: &str) -> bool {
        if peer_count <= self.config.max_connected_peers {
            return true;
        }
        self.blacklisted.insert(newest_peer.to_string());
        false
    }

    fn is_blacklisted(&self, peer: &str) -> bool {
        self.blacklisted.contains(peer)
    }

    fn penalize(&mut self, peer: &str, amount: i64) {
        let score = self.peer_scores.entry(peer.to_string()).or_insert(0);
        *score -= amount;
        if *score <= self.config.min_peer_score {
            self.blacklisted.insert(peer.to_string());
        }
    }

    fn reward(&mut self, peer: &str, amount: i64) {
        let score = self.peer_scores.entry(peer.to_string()).or_insert(0);
        *score = (*score + amount).min(100);
    }

    fn gc_seen(&mut self, now: i64) {
        let ttl = self.config.dedupe_ttl_sec.max(1);
        self.seen_messages
            .retain(|_, seen_at| now - *seen_at <= ttl);
    }
}

#[derive(NetworkBehaviour)]
pub struct WattBehaviour {
    pub gossipsub: gossipsub::Behaviour,
    pub identify: identify::Behaviour,
    pub dht: kad::Behaviour<kad::store::MemoryStore>,
    pub relay_client: relay::client::Behaviour,
    pub dcutr: dcutr::Behaviour,
    pub autonat: autonat::Behaviour,
}

pub struct P2PNode {
    swarm: Swarm<WattBehaviour>,
    topics: Vec<IdentTopic>,
    topic_labels: Vec<String>,
    guard: TrafficGuard,
}

impl P2PNode {
    pub fn new(topic: &str, listen_addr: Multiaddr, bootstrap: &[Multiaddr]) -> Result<Self> {
        Self::new_with_config(topic, listen_addr, bootstrap, P2PConfig::default())
    }

    pub fn new_with_config(
        topic: &str,
        listen_addr: Multiaddr,
        bootstrap: &[Multiaddr],
        config: P2PConfig,
    ) -> Result<Self> {
        let local_key = identity::Keypair::generate_ed25519();
        let gossipsub_config = gossipsub::ConfigBuilder::default()
            .validation_mode(gossipsub::ValidationMode::Strict)
            .build()
            .map_err(|error| anyhow::anyhow!("build gossipsub config: {error}"))?;

        let mut swarm = libp2p::SwarmBuilder::with_existing_identity(local_key)
            .with_tokio()
            .with_tcp(
                libp2p::tcp::Config::default().nodelay(true),
                libp2p::noise::Config::new,
                libp2p::yamux::Config::default,
            )?
            .with_dns()?
            .with_relay_client(libp2p::noise::Config::new, libp2p::yamux::Config::default)?
            .with_behaviour(|key, relay_client| {
                let gossipsub = gossipsub::Behaviour::new(
                    MessageAuthenticity::Signed(key.clone()),
                    gossipsub_config.clone(),
                )
                .expect("create gossipsub behaviour");
                let identify = identify::Behaviour::new(identify::Config::new(
                    "/wattetheria/0.1".to_string(),
                    key.public(),
                ));
                let peer_id = key.public().to_peer_id();
                let dht = kad::Behaviour::new(peer_id, kad::store::MemoryStore::new(peer_id));
                let dcutr = dcutr::Behaviour::new(peer_id);
                let autonat = autonat::Behaviour::new(peer_id, autonat::Config::default());
                WattBehaviour {
                    gossipsub,
                    identify,
                    dht,
                    relay_client,
                    dcutr,
                    autonat,
                }
            })?
            .build();

        swarm.listen_on(listen_addr)?;

        let topic_labels = build_topic_labels(topic, config.topic_shards);
        let mut topics = Vec::with_capacity(topic_labels.len());
        for label in &topic_labels {
            let topic = IdentTopic::new(label.clone());
            swarm.behaviour_mut().gossipsub.subscribe(&topic)?;
            topics.push(topic);
        }

        for peer in bootstrap {
            let _ = swarm.dial(peer.clone());
        }

        Ok(Self {
            swarm,
            topics,
            topic_labels,
            guard: TrafficGuard::new(config),
        })
    }

    pub fn publish_json<T: Serialize>(&mut self, payload: &T) -> Result<()> {
        let now = Utc::now().timestamp();
        let data = serde_json::to_vec(payload)?;
        let shard = select_shard_index(&data, self.topics.len());
        let topic = self.topics[shard].clone();
        let topic_label = &self.topic_labels[shard];

        if !self.guard.allow_local_publish(topic_label, now) {
            tracing::warn!(topic = %topic_label, "local publish rate limit exceeded");
            return Ok(());
        }

        let publish_result = self.swarm.behaviour_mut().gossipsub.publish(topic, data);
        if let Err(error) = publish_result {
            // Single-node bootstrap should still run without remote peers.
            tracing::warn!(%error, topic = %topic_label, "gossipsub publish warning");
        }
        Ok(())
    }

    pub async fn poll_once(&mut self) -> Result<Option<InboundPacket>> {
        let Some(event) = self.swarm.next().await else {
            return Ok(None);
        };

        match event {
            SwarmEvent::ConnectionEstablished { peer_id, .. } => {
                let peer = peer_id.to_string();
                let connected = self.swarm.connected_peers().count();
                if !self.guard.enforce_peer_limit(connected, &peer) {
                    tracing::warn!(peer = %peer, "disconnecting peer due to peer limit");
                    let _ = self.swarm.disconnect_peer_id(peer_id);
                }
                Ok(None)
            }
            SwarmEvent::Behaviour(WattBehaviourEvent::Dcutr(event)) => {
                tracing::info!(?event, "dcutr event");
                Ok(None)
            }
            SwarmEvent::Behaviour(WattBehaviourEvent::Autonat(event)) => {
                tracing::info!(?event, "autonat event");
                Ok(None)
            }
            SwarmEvent::Behaviour(WattBehaviourEvent::RelayClient(event)) => {
                tracing::info!(?event, "relay client event");
                Ok(None)
            }
            SwarmEvent::Behaviour(WattBehaviourEvent::Gossipsub(gossipsub::Event::Message {
                propagation_source,
                message,
                ..
            })) => {
                let now = Utc::now().timestamp();
                let source = Some(propagation_source.to_string());
                let topic = message.topic.to_string();
                if !self
                    .guard
                    .allow_inbound(source.as_deref(), &topic, &message.data, now)
                {
                    return Ok(None);
                }
                Ok(Some(InboundPacket {
                    data: message.data,
                    source_peer: source,
                    topic,
                    received_at: now,
                }))
            }
            _ => Ok(None),
        }
    }

    pub fn listeners(&self) -> Vec<String> {
        self.swarm.listeners().map(ToString::to_string).collect()
    }

    #[must_use]
    pub fn is_peer_blacklisted(&self, peer: &str) -> bool {
        self.guard.is_blacklisted(peer)
    }
}

fn prune_window(entries: &mut Vec<i64>, now: i64, window_sec: i64) {
    let min_ts = now - window_sec;
    entries.retain(|ts| *ts >= min_ts);
}

fn is_message_fresh(bytes: &[u8], now: i64, max_age_sec: i64) -> bool {
    let Ok(payload) = serde_json::from_slice::<serde_json::Value>(bytes) else {
        return true;
    };
    let Some(ts) = payload.get("timestamp").and_then(serde_json::Value::as_i64) else {
        return true;
    };
    (now - ts).abs() <= max_age_sec
}

fn build_topic_labels(base: &str, shards: usize) -> Vec<String> {
    let shard_count = shards.max(1);
    if shard_count == 1 {
        return vec![base.to_string()];
    }

    (0..shard_count)
        .map(|index| format!("{base}.shard.{index}"))
        .collect()
}

fn select_shard_index(data: &[u8], shard_count: usize) -> usize {
    let count = shard_count.max(1);
    if count == 1 {
        return 0;
    }

    let digest = Sha256::digest(data);
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&digest[..8]);

    let hash = u64::from_be_bytes(bytes);
    let count_u64 = u64::try_from(count).unwrap_or(u64::MAX);
    let index_u64 = hash % count_u64;
    usize::try_from(index_u64).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn guard_rejects_duplicates_and_stale_messages() {
        let mut guard = TrafficGuard::new(P2PConfig::default());
        let now = 1_700_000_000;
        let message = serde_json::to_vec(&json!({"timestamp": now, "value": 1})).unwrap();

        assert!(guard.allow_inbound(Some("peer-a"), "topic", &message, now));
        assert!(!guard.allow_inbound(Some("peer-a"), "topic", &message, now + 1));

        let stale = serde_json::to_vec(&json!({"timestamp": now - 1_000, "value": 2})).unwrap();
        assert!(!guard.allow_inbound(Some("peer-a"), "topic", &stale, now));
    }

    #[test]
    fn guard_applies_per_peer_rate_limit_and_blacklist() {
        let mut guard = TrafficGuard::new(P2PConfig {
            per_peer_msgs_per_minute: 2,
            min_peer_score: 0,
            ..P2PConfig::default()
        });
        let now = 1_700_000_100;
        let msg1 = serde_json::to_vec(&json!({"timestamp": now, "n": 1})).unwrap();
        let msg2 = serde_json::to_vec(&json!({"timestamp": now, "n": 2})).unwrap();
        let msg3 = serde_json::to_vec(&json!({"timestamp": now, "n": 3})).unwrap();

        assert!(guard.allow_inbound(Some("peer-z"), "topic", &msg1, now));
        assert!(guard.allow_inbound(Some("peer-z"), "topic", &msg2, now));
        assert!(!guard.allow_inbound(Some("peer-z"), "topic", &msg3, now));
        assert!(guard.is_blacklisted("peer-z"));
    }

    #[test]
    fn topic_shards_are_deterministic_and_bounded() {
        let labels = build_topic_labels("wattetheria.v0.1", 4);
        assert_eq!(labels.len(), 4);
        assert_eq!(labels[0], "wattetheria.v0.1.shard.0");

        let data = serde_json::to_vec(&json!({"kind":"TASK_RESULT","id":"abc"})).unwrap();
        let shard_a = select_shard_index(&data, 4);
        let shard_b = select_shard_index(&data, 4);
        assert_eq!(shard_a, shard_b);
        assert!(shard_a < 4);
    }
}
