use anyhow::{Context, Result, bail};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use crate::identity::{
    build_scoped_public_id, fingerprint_from_did_key, verify_public_id_ownership,
};

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

    /// Insert or update a public identity.
    ///
    /// When `agent_did` is provided, the `public_id` must be self-certifying:
    /// either a `did:key:` that matches, or a scoped slug ending with the
    /// correct fingerprint (e.g. `captain-aurora.a3f7b2c1d9e04f68`).
    pub fn upsert(
        &mut self,
        public_id: &str,
        display_name: String,
        agent_did: Option<String>,
        active: bool,
    ) -> Result<PublicIdentity> {
        if let Some(ref did) = agent_did
            && !verify_public_id_ownership(public_id, did).context("verify public_id ownership")?
        {
            bail!(
                "public_id '{public_id}' does not contain a valid fingerprint for agent_did '{did}'"
            );
        }
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
        Ok(identity)
    }

    pub fn ensure_local_default(&mut self, agent_did: &str) -> Result<PublicIdentity> {
        self.ensure_local_default_for_agent(agent_did, Some(agent_did))
    }

    /// Ensure a default public identity exists for the given agent.
    ///
    /// Looks up by `agent_did` (legacy `did:key:` key) first, then falls back
    /// to any active identity bound to the agent. If none exists, creates a new
    /// scoped identity with a fingerprint derived from the agent's key.
    pub fn ensure_local_default_for_agent(
        &mut self,
        agent_did: &str,
        bound_agent_did: Option<&str>,
    ) -> Result<PublicIdentity> {
        // Existing identity keyed by did:key (legacy).
        if let Some(identity) = self.identities.get(agent_did) {
            return Ok(identity.clone());
        }
        // Existing active identity bound to this agent_did.
        if let Some(identity) = self.active_for_agent_did(agent_did) {
            return Ok(identity);
        }
        let fingerprint = fingerprint_from_did_key(agent_did)
            .context("derive fingerprint for default public_id")?;
        let slug = slugify_did(agent_did);
        let public_id = build_scoped_public_id(&slug, &fingerprint);
        self.upsert(
            &public_id,
            format!("Citizen-{}", &slug[..slug.len().min(12)]),
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

/// Derive a short human-ish slug from a `did:key:z...` string.
/// Takes the last 8 characters of the did for brevity.
fn slugify_did(agent_did: &str) -> String {
    let raw = agent_did.trim_start_matches("did:key:");
    let suffix: String = raw
        .chars()
        .rev()
        .take(8)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("citizen-{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{Identity, extract_public_id_fingerprint, public_key_fingerprint};
    use tempfile::tempdir;

    #[test]
    fn ensure_default_creates_fingerprinted_public_id() {
        let identity = Identity::new_random();
        let mut registry = PublicIdentityRegistry::default();
        let public = registry
            .ensure_local_default_for_agent(&identity.agent_did, Some(&identity.agent_did))
            .unwrap();

        // The public_id must contain the correct fingerprint.
        let expected_fp = public_key_fingerprint(&identity.public_key).unwrap();
        let embedded_fp = extract_public_id_fingerprint(&public.public_id).unwrap();
        assert_eq!(embedded_fp, expected_fp);
        assert!(public.public_id.starts_with("citizen-"));
    }

    #[test]
    fn ensure_default_is_idempotent() {
        let identity = Identity::new_random();
        let mut registry = PublicIdentityRegistry::default();
        let first = registry
            .ensure_local_default_for_agent(&identity.agent_did, Some(&identity.agent_did))
            .unwrap();
        let second = registry
            .ensure_local_default_for_agent(&identity.agent_did, Some(&identity.agent_did))
            .unwrap();
        assert_eq!(first.public_id, second.public_id);
    }

    #[test]
    fn upsert_rejects_mismatched_fingerprint() {
        let a = Identity::new_random();
        let b = Identity::new_random();
        let fp_a = public_key_fingerprint(&a.public_key).unwrap();
        let pid = build_scoped_public_id("test-agent", &fp_a);

        let mut registry = PublicIdentityRegistry::default();
        // Binding pid (fingerprinted to A) with B's did should fail.
        let result = registry.upsert(&pid, "Test".to_string(), Some(b.agent_did.clone()), true);
        assert!(result.is_err());
    }

    #[test]
    fn upsert_accepts_matching_fingerprint() {
        let identity = Identity::new_random();
        let fp = public_key_fingerprint(&identity.public_key).unwrap();
        let pid = build_scoped_public_id("my-agent", &fp);

        let mut registry = PublicIdentityRegistry::default();
        let result = registry.upsert(
            &pid,
            "My Agent".to_string(),
            Some(identity.agent_did.clone()),
            true,
        );
        assert!(result.is_ok());
        assert_eq!(result.unwrap().public_id, pid);
    }

    #[test]
    fn upsert_accepts_did_key_as_public_id() {
        let identity = Identity::new_random();
        let mut registry = PublicIdentityRegistry::default();
        let result = registry.upsert(
            &identity.agent_did,
            "Legacy".to_string(),
            Some(identity.agent_did.clone()),
            true,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn upsert_allows_no_agent_did() {
        let mut registry = PublicIdentityRegistry::default();
        let result = registry.upsert("any-slug", "Test".to_string(), None, true);
        assert!(result.is_ok());
    }

    #[test]
    fn roundtrip_persist_and_load() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("public_identities.json");
        let identity = Identity::new_random();
        let mut registry = PublicIdentityRegistry::default();
        let public = registry
            .ensure_local_default_for_agent(&identity.agent_did, Some(&identity.agent_did))
            .unwrap();
        registry.persist(&path).unwrap();

        let loaded = PublicIdentityRegistry::load_or_new(&path).unwrap();
        assert!(loaded.get(&public.public_id).is_some());
        assert!(loaded.active_for_agent_did(&identity.agent_did).is_some());
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
