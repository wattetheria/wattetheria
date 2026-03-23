//! Oracle feed publication/subscription primitives with signed payloads.

use anyhow::{Context, Result, bail};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use crate::event_log::EventLog;
use crate::identity::Identity;
use crate::signing::{sign_payload, verify_payload};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OracleFeed {
    pub feed_id: String,
    pub publisher: String,
    pub timestamp: i64,
    pub payload: Value,
    pub price_watt: i64,
    pub signature: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OracleSubscription {
    pub agent_did: String,
    pub feed_id: String,
    pub max_price_watt: i64,
    pub subscribed_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OracleSettlement {
    pub agent_did: String,
    pub feed_id: String,
    pub delivered: usize,
    pub charged_watt: i64,
    pub balance_after: i64,
}

#[derive(Debug, Serialize)]
struct SignableFeed<'a> {
    feed_id: &'a str,
    publisher: &'a str,
    timestamp: i64,
    payload: &'a Value,
    price_watt: i64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct OracleRegistry {
    feeds: BTreeMap<String, Vec<OracleFeed>>,
    subscriptions: Vec<OracleSubscription>,
    balances: BTreeMap<String, i64>,
    delivered: BTreeMap<String, BTreeSet<String>>,
}

impl OracleRegistry {
    pub fn load_or_new(path: impl AsRef<Path>) -> Result<Self> {
        if let Some(parent) = path.as_ref().parent() {
            fs::create_dir_all(parent).context("create oracle state directory")?;
        }
        if !path.as_ref().exists() {
            return Ok(Self::default());
        }
        let raw = fs::read_to_string(path.as_ref()).context("read oracle state")?;
        if raw.trim().is_empty() {
            return Ok(Self::default());
        }
        serde_json::from_str(&raw).context("parse oracle state")
    }

    pub fn persist(&self, path: impl AsRef<Path>) -> Result<()> {
        fs::write(path.as_ref(), serde_json::to_string_pretty(self)?).context("write oracle state")
    }

    pub fn publish(
        &mut self,
        feed_id: &str,
        payload: Value,
        price_watt: i64,
        identity: &Identity,
        event_log: Option<&EventLog>,
    ) -> Result<OracleFeed> {
        let signable = SignableFeed {
            feed_id,
            publisher: &identity.agent_did,
            timestamp: Utc::now().timestamp(),
            payload: &payload,
            price_watt,
        };

        let signature = sign_payload(&signable, identity)?;
        let feed = OracleFeed {
            feed_id: feed_id.to_string(),
            publisher: identity.agent_did.clone(),
            timestamp: signable.timestamp,
            payload,
            price_watt,
            signature,
        };

        self.feeds
            .entry(feed_id.to_string())
            .or_default()
            .push(feed.clone());

        if let Some(log) = event_log {
            log.append_signed(
                "ORACLE_FEED_PUBLISHED",
                serde_json::to_value(&feed).context("serialize oracle feed")?,
                identity,
            )?;
        }

        Ok(feed)
    }

    pub fn ingest_feed(
        &mut self,
        feed: &OracleFeed,
        local_identity: Option<&Identity>,
        event_log: Option<&EventLog>,
    ) -> Result<bool> {
        if !verify_feed_signature(feed)? {
            bail!("oracle feed signature verification failed");
        }

        let existing = self
            .feeds
            .get(&feed.feed_id)
            .is_some_and(|rows| rows.iter().any(|row| row.signature == feed.signature));
        if existing {
            return Ok(false);
        }

        self.feeds
            .entry(feed.feed_id.clone())
            .or_default()
            .push(feed.clone());

        if let (Some(identity), Some(log)) = (local_identity, event_log) {
            log.append_signed(
                "ORACLE_FEED_INGESTED",
                serde_json::to_value(feed).context("serialize ingested oracle feed")?,
                identity,
            )?;
        }

        Ok(true)
    }

    pub fn credit(&mut self, agent_did: &str, amount_watt: i64) -> Result<i64> {
        if amount_watt <= 0 {
            bail!("credit amount must be positive");
        }
        let balance = self.balances.entry(agent_did.to_string()).or_insert(0);
        *balance += amount_watt;
        Ok(*balance)
    }

    #[must_use]
    pub fn balance_of(&self, agent_did: &str) -> i64 {
        self.balances.get(agent_did).copied().unwrap_or(0)
    }

    pub fn subscribe(
        &mut self,
        agent_did: &str,
        feed_id: &str,
        max_price_watt: i64,
        identity: &Identity,
        event_log: Option<&EventLog>,
    ) -> Result<OracleSubscription> {
        let subscription = OracleSubscription {
            agent_did: agent_did.to_string(),
            feed_id: feed_id.to_string(),
            max_price_watt,
            subscribed_at: Utc::now().timestamp(),
        };

        self.subscriptions.push(subscription.clone());

        if let Some(log) = event_log {
            log.append_signed(
                "ORACLE_SUBSCRIBE",
                serde_json::to_value(&subscription).context("serialize oracle subscription")?,
                identity,
            )?;
        }

        Ok(subscription)
    }

    pub fn pull_for_subscriber(&self, agent_did: &str, feed_id: &str) -> Result<Vec<OracleFeed>> {
        let subscription = self
            .subscriptions
            .iter()
            .find(|subscription| {
                subscription.agent_did == agent_did && subscription.feed_id == feed_id
            })
            .context("subscription not found")?;

        let mut feeds = self.eligible_feeds(feed_id, subscription.max_price_watt);

        for feed in &feeds {
            if !verify_feed_signature(feed)? {
                bail!("oracle feed signature verification failed");
            }
        }

        feeds.sort_by_key(|feed| std::cmp::Reverse(feed.timestamp));
        Ok(feeds)
    }

    pub fn pull_for_subscriber_settled(
        &mut self,
        agent_did: &str,
        feed_id: &str,
        identity: &Identity,
        event_log: Option<&EventLog>,
    ) -> Result<(Vec<OracleFeed>, OracleSettlement)> {
        let subscription = self
            .subscriptions
            .iter()
            .find(|subscription| {
                subscription.agent_did == agent_did && subscription.feed_id == feed_id
            })
            .cloned()
            .context("subscription not found")?;
        let mut feeds = self.eligible_feeds(feed_id, subscription.max_price_watt);
        feeds.sort_by_key(|feed| feed.timestamp);

        let delivery_key = format!("{agent_did}:{feed_id}");
        let already_delivered = self
            .delivered
            .get(&delivery_key)
            .cloned()
            .unwrap_or_default();

        let mut new_items = Vec::new();
        let mut total_cost = 0_i64;
        for feed in &feeds {
            if !verify_feed_signature(feed)? {
                bail!("oracle feed signature verification failed");
            }
            if already_delivered.contains(&feed.signature) {
                continue;
            }
            total_cost += feed.price_watt;
            new_items.push(feed.clone());
        }

        let subscriber_balance = self.balance_of(agent_did);
        if total_cost > subscriber_balance {
            bail!(
                "insufficient watt balance for oracle pull: required={total_cost}, available={subscriber_balance}"
            );
        }

        if total_cost > 0 {
            let balance = self.balances.entry(agent_did.to_string()).or_insert(0);
            *balance -= total_cost;
        }
        let delivered_set = self.delivered.entry(delivery_key).or_default();
        for feed in &new_items {
            delivered_set.insert(feed.signature.clone());
            let publisher_balance = self.balances.entry(feed.publisher.clone()).or_insert(0);
            *publisher_balance += feed.price_watt;
        }

        let settlement = OracleSettlement {
            agent_did: agent_did.to_string(),
            feed_id: feed_id.to_string(),
            delivered: new_items.len(),
            charged_watt: total_cost,
            balance_after: self.balance_of(agent_did),
        };

        if let Some(log) = event_log {
            log.append_signed(
                "ORACLE_SETTLEMENT",
                serde_json::to_value(&settlement).context("serialize oracle settlement")?,
                identity,
            )?;
        }

        let mut rows = feeds;
        rows.sort_by_key(|feed| std::cmp::Reverse(feed.timestamp));
        Ok((rows, settlement))
    }

    #[must_use]
    pub fn list_subscriptions(&self, agent_did: &str) -> Vec<OracleSubscription> {
        self.subscriptions
            .iter()
            .filter(|subscription| subscription.agent_did == agent_did)
            .cloned()
            .collect()
    }

    #[must_use]
    pub fn all_feeds(&self) -> Vec<OracleFeed> {
        let mut rows: Vec<_> = self
            .feeds
            .values()
            .flat_map(|feeds| feeds.iter().cloned())
            .collect();
        rows.sort_by_key(|feed| std::cmp::Reverse(feed.timestamp));
        rows
    }

    fn eligible_feeds(&self, feed_id: &str, max_price_watt: i64) -> Vec<OracleFeed> {
        self.feeds
            .get(feed_id)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|feed| feed.price_watt <= max_price_watt)
            .collect()
    }
}

pub fn verify_feed_signature(feed: &OracleFeed) -> Result<bool> {
    let signable = SignableFeed {
        feed_id: &feed.feed_id,
        publisher: &feed.publisher,
        timestamp: feed.timestamp,
        payload: &feed.payload,
        price_watt: feed.price_watt,
    };
    verify_payload(&signable, &feed.signature, &feed.publisher)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oracle_publish_and_subscribe_flow() {
        let mut oracle = OracleRegistry::default();
        let publisher = Identity::new_random();
        let subscriber = Identity::new_random();

        let feed = oracle
            .publish(
                "btc-price",
                serde_json::json!({"price": 100_000}),
                3,
                &publisher,
                None,
            )
            .unwrap();
        assert_eq!(feed.feed_id, "btc-price");

        oracle
            .subscribe(&subscriber.agent_did, "btc-price", 5, &subscriber, None)
            .unwrap();

        let pulled = oracle
            .pull_for_subscriber(&subscriber.agent_did, "btc-price")
            .unwrap();
        assert_eq!(pulled.len(), 1);
    }

    #[test]
    fn oracle_settlement_debits_subscriber_and_credits_publisher_once() {
        let mut oracle = OracleRegistry::default();
        let publisher = Identity::new_random();
        let subscriber = Identity::new_random();

        oracle
            .publish(
                "btc-price",
                serde_json::json!({"price": 90_000}),
                2,
                &publisher,
                None,
            )
            .unwrap();
        oracle
            .publish(
                "btc-price",
                serde_json::json!({"price": 91_000}),
                3,
                &publisher,
                None,
            )
            .unwrap();
        oracle
            .subscribe(&subscriber.agent_did, "btc-price", 5, &subscriber, None)
            .unwrap();
        oracle.credit(&subscriber.agent_did, 20).unwrap();

        let (_, settlement_a) = oracle
            .pull_for_subscriber_settled(&subscriber.agent_did, "btc-price", &subscriber, None)
            .unwrap();
        assert_eq!(settlement_a.delivered, 2);
        assert_eq!(settlement_a.charged_watt, 5);
        assert_eq!(oracle.balance_of(&subscriber.agent_did), 15);
        assert_eq!(oracle.balance_of(&publisher.agent_did), 5);

        let (_, settlement_b) = oracle
            .pull_for_subscriber_settled(&subscriber.agent_did, "btc-price", &subscriber, None)
            .unwrap();
        assert_eq!(settlement_b.delivered, 0);
        assert_eq!(settlement_b.charged_watt, 0);
        assert_eq!(oracle.balance_of(&subscriber.agent_did), 15);
        assert_eq!(oracle.balance_of(&publisher.agent_did), 5);
    }

    #[test]
    fn oracle_ingest_rejects_invalid_and_dedupes() {
        let mut oracle = OracleRegistry::default();
        let publisher = Identity::new_random();

        let feed = oracle
            .publish(
                "btc-price",
                serde_json::json!({"price": 100_000}),
                2,
                &publisher,
                None,
            )
            .unwrap();

        let inserted = oracle.ingest_feed(&feed, None, None).unwrap();
        assert!(!inserted);

        let mut tampered = feed.clone();
        tampered.payload = serde_json::json!({"price": 999});
        let err = oracle.ingest_feed(&tampered, None, None).unwrap_err();
        assert!(err.to_string().contains("signature verification"));
    }
}
