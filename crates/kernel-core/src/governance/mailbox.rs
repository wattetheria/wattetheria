//! Asynchronous cross-subnet message mailbox with signed envelopes.

use anyhow::{Context, Result, bail};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use crate::identity::Identity;
use crate::signing::{sign_payload, verify_payload};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrossSubnetMessage {
    pub message_id: String,
    pub from_agent: String,
    pub to_agent: String,
    pub from_subnet: String,
    pub to_subnet: String,
    pub timestamp: i64,
    pub payload: Value,
    pub signature: String,
}

#[derive(Debug, Clone, Serialize)]
struct MessageSignable<'a> {
    message_id: &'a str,
    from_agent: &'a str,
    to_agent: &'a str,
    from_subnet: &'a str,
    to_subnet: &'a str,
    timestamp: i64,
    payload: &'a Value,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct CrossSubnetMailbox {
    inbox: BTreeMap<String, Vec<CrossSubnetMessage>>,
}

impl CrossSubnetMailbox {
    pub fn load_or_new(path: impl AsRef<Path>) -> Result<Self> {
        if let Some(parent) = path.as_ref().parent() {
            fs::create_dir_all(parent).context("create mailbox state directory")?;
        }
        if !path.as_ref().exists() {
            return Ok(Self::default());
        }
        let raw = fs::read_to_string(path.as_ref()).context("read mailbox state")?;
        if raw.trim().is_empty() {
            return Ok(Self::default());
        }
        serde_json::from_str(&raw).context("parse mailbox state")
    }

    pub fn persist(&self, path: impl AsRef<Path>) -> Result<()> {
        if let Some(parent) = path.as_ref().parent() {
            fs::create_dir_all(parent).context("create mailbox state directory")?;
        }
        fs::write(path.as_ref(), serde_json::to_string_pretty(self)?).context("write mailbox state")
    }

    pub fn enqueue_signed(
        &mut self,
        from_identity: &Identity,
        to_agent: &str,
        from_subnet: &str,
        to_subnet: &str,
        payload: Value,
    ) -> Result<CrossSubnetMessage> {
        let message_id = uuid::Uuid::new_v4().to_string();
        let timestamp = Utc::now().timestamp();
        let payload_for_signature = payload.clone();
        let message_id_for_signature = message_id.clone();
        let signable = MessageSignable {
            message_id: &message_id_for_signature,
            from_agent: &from_identity.agent_did,
            to_agent,
            from_subnet,
            to_subnet,
            timestamp,
            payload: &payload_for_signature,
        };

        let message = CrossSubnetMessage {
            message_id,
            from_agent: from_identity.agent_did.clone(),
            to_agent: to_agent.to_string(),
            from_subnet: from_subnet.to_string(),
            to_subnet: to_subnet.to_string(),
            timestamp,
            payload,
            signature: sign_payload(&signable, from_identity)?,
        };

        self.inbox
            .entry(to_subnet.to_string())
            .or_default()
            .push(message.clone());
        Ok(message)
    }

    pub fn verify_message(message: &CrossSubnetMessage) -> Result<bool> {
        let signable = MessageSignable {
            message_id: &message.message_id,
            from_agent: &message.from_agent,
            to_agent: &message.to_agent,
            from_subnet: &message.from_subnet,
            to_subnet: &message.to_subnet,
            timestamp: message.timestamp,
            payload: &message.payload,
        };
        verify_payload(&signable, &message.signature, &message.from_agent)
    }

    #[must_use]
    pub fn fetch_for_subnet(&self, subnet: &str) -> Vec<CrossSubnetMessage> {
        self.inbox.get(subnet).cloned().unwrap_or_default()
    }

    pub fn ack(&mut self, subnet: &str, message_id: &str) -> Result<()> {
        let Some(items) = self.inbox.get_mut(subnet) else {
            bail!("subnet inbox not found");
        };
        items.retain(|item| item.message_id != message_id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    #[test]
    fn persistence_roundtrip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("mailbox.json");

        let mut mailbox = CrossSubnetMailbox::default();
        let sender = Identity::new_random();
        let receiver = Identity::new_random();

        mailbox
            .enqueue_signed(
                &sender,
                &receiver.agent_did,
                "planet-a",
                "planet-b",
                json!({"text": "persist-test"}),
            )
            .unwrap();
        mailbox.persist(&path).unwrap();

        let loaded = CrossSubnetMailbox::load_or_new(&path).unwrap();
        let pending = loaded.fetch_for_subnet("planet-b");
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].from_agent, sender.agent_did);
    }

    #[test]
    fn mailbox_cross_subnet_flow() {
        let mut mailbox = CrossSubnetMailbox::default();
        let sender = Identity::new_random();
        let receiver = Identity::new_random();

        let message = mailbox
            .enqueue_signed(
                &sender,
                &receiver.agent_did,
                "planet-a",
                "planet-b",
                json!({"text":"hello"}),
            )
            .unwrap();

        assert!(CrossSubnetMailbox::verify_message(&message).unwrap());

        let pending = mailbox.fetch_for_subnet("planet-b");
        assert_eq!(pending.len(), 1);
        mailbox.ack("planet-b", &pending[0].message_id).unwrap();
        assert!(mailbox.fetch_for_subnet("planet-b").is_empty());
    }
}
