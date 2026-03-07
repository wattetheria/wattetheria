use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::Path;
use wattetheria_kernel::event_log::EventLog;
use wattetheria_kernel::hashcash;
use wattetheria_kernel::identity::Identity;
use wattetheria_kernel::oracle::{OracleFeed, OracleRegistry};
use wattetheria_p2p_runtime::P2PNode;

fn build_oracle_feed_packet(feed: &OracleFeed, include_hashcash: bool) -> Value {
    let hashcash_value = if include_hashcash {
        hashcash::mint(&feed.publisher, 12, 80_000)
            .map(|stamp| json!({"stamp": stamp, "bits": 12, "resource": feed.publisher}))
    } else {
        None
    };

    json!({
        "type": "ORACLE_FEED",
        "version": "0.1",
        "feed": feed,
        "hashcash": hashcash_value,
    })
}

fn parse_oracle_feed_packet(bytes: &[u8]) -> Result<Option<OracleFeed>> {
    let value: Value = match serde_json::from_slice(bytes) {
        Ok(value) => value,
        Err(_) => return Ok(None),
    };

    if value["type"].as_str() != Some("ORACLE_FEED") {
        return Ok(None);
    }

    let feed: OracleFeed = serde_json::from_value(value["feed"].clone())
        .context("parse ORACLE_FEED packet payload")?;
    Ok(Some(feed))
}

pub fn sync_and_publish_local_oracle_feeds(
    p2p: &mut P2PNode,
    oracle_registry: &mut OracleRegistry,
    oracle_state_path: &Path,
    enable_hashcash_broadcast: bool,
    known_oracle_signatures: &mut BTreeSet<String>,
) -> Result<()> {
    *oracle_registry = OracleRegistry::load_or_new(oracle_state_path)?;
    for feed in oracle_registry.all_feeds() {
        if !known_oracle_signatures.insert(feed.signature.clone()) {
            continue;
        }
        p2p.publish_json(&build_oracle_feed_packet(&feed, enable_hashcash_broadcast))?;
    }
    Ok(())
}

pub fn handle_oracle_feed_packet(
    bytes: &[u8],
    oracle_registry: &mut OracleRegistry,
    oracle_state_path: &Path,
    event_log: &EventLog,
    identity: &Identity,
    known_oracle_signatures: &mut BTreeSet<String>,
) -> Result<()> {
    let Some(feed) = parse_oracle_feed_packet(bytes)? else {
        return Ok(());
    };

    let inserted = oracle_registry.ingest_feed(&feed, Some(identity), Some(event_log))?;
    if inserted {
        known_oracle_signatures.insert(feed.signature);
        oracle_registry.persist(oracle_state_path)?;
    }

    Ok(())
}
