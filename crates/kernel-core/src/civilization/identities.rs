use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ControllerKind {
    LocalWattswarm,
    ExternalRuntime,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OwnershipScope {
    Local,
    External,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PublicIdentity {
    pub public_id: String,
    pub display_name: String,
    pub agent_did: Option<String>,
    pub active: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ControllerBinding {
    pub public_id: String,
    pub controller_kind: ControllerKind,
    pub controller_ref: String,
    pub controller_node_id: Option<String>,
    pub ownership_scope: OwnershipScope,
    pub active: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct PublicIdentityRegistry {
    identities: BTreeMap<String, PublicIdentity>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct ControllerBindingRegistry {
    bindings: BTreeMap<String, ControllerBinding>,
}

impl PublicIdentityRegistry {
    pub fn load_or_new(path: impl AsRef<Path>) -> Result<Self> {
        if let Some(parent) = path.as_ref().parent() {
            fs::create_dir_all(parent).context("create public identity registry directory")?;
        }
        if !path.as_ref().exists() {
            return Ok(Self::default());
        }
        let raw = fs::read_to_string(path.as_ref()).context("read public identity registry")?;
        if raw.trim().is_empty() {
            return Ok(Self::default());
        }
        serde_json::from_str(&raw).context("parse public identity registry")
    }

    pub fn persist(&self, path: impl AsRef<Path>) -> Result<()> {
        if let Some(parent) = path.as_ref().parent() {
            fs::create_dir_all(parent).context("create public identity registry directory")?;
        }
        fs::write(path.as_ref(), serde_json::to_string_pretty(self)?)
            .context("write public identity registry")
    }

    pub fn upsert(
        &mut self,
        public_id: &str,
        display_name: String,
        agent_did: Option<String>,
        active: bool,
    ) -> PublicIdentity {
        let now = Utc::now().timestamp();
        let created_at = self
            .identities
            .get(public_id)
            .map_or(now, |identity| identity.created_at);
        let identity = PublicIdentity {
            public_id: public_id.to_string(),
            display_name,
            agent_did,
            active,
            created_at,
            updated_at: now,
        };
        self.identities
            .insert(public_id.to_string(), identity.clone());
        identity
    }

    pub fn ensure_local_default(&mut self, agent_did: &str) -> PublicIdentity {
        self.ensure_local_default_for_agent(agent_did, Some(agent_did))
    }

    pub fn ensure_local_default_for_agent(
        &mut self,
        agent_did: &str,
        bound_agent_did: Option<&str>,
    ) -> PublicIdentity {
        if let Some(identity) = self.identities.get(agent_did) {
            return identity.clone();
        }
        let short = agent_did.chars().take(12).collect::<String>();
        self.upsert(
            agent_did,
            format!("Citizen-{short}"),
            bound_agent_did.map(ToOwned::to_owned),
            true,
        )
    }

    #[must_use]
    pub fn get(&self, public_id: &str) -> Option<PublicIdentity> {
        self.identities.get(public_id).cloned()
    }

    #[must_use]
    pub fn active_for_agent_did(&self, agent_did: &str) -> Option<PublicIdentity> {
        self.identities
            .values()
            .find(|identity| identity.active && identity.agent_did.as_deref() == Some(agent_did))
            .cloned()
    }

    #[must_use]
    pub fn list(&self) -> Vec<PublicIdentity> {
        self.identities.values().cloned().collect()
    }
}

impl ControllerBindingRegistry {
    pub fn load_or_new(path: impl AsRef<Path>) -> Result<Self> {
        if let Some(parent) = path.as_ref().parent() {
            fs::create_dir_all(parent).context("create controller binding registry directory")?;
        }
        if !path.as_ref().exists() {
            return Ok(Self::default());
        }
        let raw = fs::read_to_string(path.as_ref()).context("read controller binding registry")?;
        if raw.trim().is_empty() {
            return Ok(Self::default());
        }
        serde_json::from_str(&raw).context("parse controller binding registry")
    }

    pub fn persist(&self, path: impl AsRef<Path>) -> Result<()> {
        if let Some(parent) = path.as_ref().parent() {
            fs::create_dir_all(parent).context("create controller binding registry directory")?;
        }
        fs::write(path.as_ref(), serde_json::to_string_pretty(self)?)
            .context("write controller binding registry")
    }

    pub fn upsert(
        &mut self,
        public_id: &str,
        controller_kind: ControllerKind,
        controller_ref: String,
        controller_node_id: Option<String>,
        ownership_scope: OwnershipScope,
        active: bool,
    ) -> ControllerBinding {
        let now = Utc::now().timestamp();
        let created_at = self
            .bindings
            .get(public_id)
            .map_or(now, |binding| binding.created_at);
        let binding = ControllerBinding {
            public_id: public_id.to_string(),
            controller_kind,
            controller_ref,
            controller_node_id,
            ownership_scope,
            active,
            created_at,
            updated_at: now,
        };
        self.bindings.insert(public_id.to_string(), binding.clone());
        binding
    }

    pub fn ensure_local_wattswarm(
        &mut self,
        public_id: &str,
        controller_node_id: &str,
    ) -> ControllerBinding {
        if let Some(binding) = self.bindings.get(public_id) {
            return binding.clone();
        }
        self.upsert(
            public_id,
            ControllerKind::LocalWattswarm,
            "local-default".to_string(),
            Some(controller_node_id.to_string()),
            OwnershipScope::Local,
            true,
        )
    }

    #[must_use]
    pub fn get(&self, public_id: &str) -> Option<ControllerBinding> {
        self.bindings.get(public_id).cloned()
    }

    #[must_use]
    pub fn active_for_controller(&self, controller_node_id: &str) -> Option<ControllerBinding> {
        self.bindings
            .values()
            .find(|binding| {
                binding.active && binding.controller_node_id.as_deref() == Some(controller_node_id)
            })
            .cloned()
    }

    #[must_use]
    pub fn list(&self) -> Vec<ControllerBinding> {
        self.bindings.values().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn public_identity_registry_roundtrip_and_default() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("public_identities.json");
        let mut registry = PublicIdentityRegistry::default();

        let default_identity =
            registry.ensure_local_default_for_agent("agent-a", Some("did:key:test"));
        assert_eq!(default_identity.public_id, "agent-a");
        assert_eq!(default_identity.agent_did.as_deref(), Some("did:key:test"));
        registry.persist(&path).unwrap();

        let loaded = PublicIdentityRegistry::load_or_new(&path).unwrap();
        assert!(loaded.get("agent-a").is_some());
        assert!(loaded.active_for_agent_did("did:key:test").is_some());
    }

    #[test]
    fn controller_binding_registry_roundtrip_and_default() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("controller_bindings.json");
        let mut registry = ControllerBindingRegistry::default();

        let binding = registry.ensure_local_wattswarm("public-a", "node-a");
        assert_eq!(binding.public_id, "public-a");
        assert_eq!(binding.controller_node_id.as_deref(), Some("node-a"));
        assert_eq!(binding.controller_kind, ControllerKind::LocalWattswarm);
        registry.persist(&path).unwrap();

        let loaded = ControllerBindingRegistry::load_or_new(&path).unwrap();
        assert!(loaded.get("public-a").is_some());
        assert!(loaded.active_for_controller("node-a").is_some());
    }
}
