//! Plugin metadata registry with trust-tier information.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

use crate::capabilities::TrustLevel;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginDescriptor {
    pub name: String,
    pub version: String,
    pub entry: String,
    pub trust_level: String,
    pub digest: String,
}

#[derive(Debug, Default)]
pub struct PluginRegistry {
    plugins: HashMap<String, PluginDescriptor>,
}

impl PluginRegistry {
    pub fn register(
        &mut self,
        name: &str,
        version: &str,
        entry: &str,
        trust_level: TrustLevel,
    ) -> PluginDescriptor {
        let trust_level_str = match trust_level {
            TrustLevel::Trusted => "trusted",
            TrustLevel::Verified => "verified",
            TrustLevel::Untrusted => "untrusted",
        }
        .to_string();

        let digest = hex::encode(Sha256::digest(
            format!("{name}:{version}:{entry}").as_bytes(),
        ));
        let descriptor = PluginDescriptor {
            name: name.to_string(),
            version: version.to_string(),
            entry: entry.to_string(),
            trust_level: trust_level_str,
            digest,
        };
        self.plugins
            .insert(format!("{name}@{version}"), descriptor.clone());
        descriptor
    }

    #[must_use]
    pub fn get(&self, name: &str, version: &str) -> Option<&PluginDescriptor> {
        self.plugins.get(&format!("{name}@{version}"))
    }

    #[must_use]
    pub fn list(&self) -> Vec<PluginDescriptor> {
        self.plugins.values().cloned().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_registry_works() {
        let mut registry = PluginRegistry::default();
        let descriptor = registry.register(
            "market-plugin",
            "0.1.0",
            "plugins/market.wasm",
            TrustLevel::Verified,
        );
        assert!(!descriptor.digest.is_empty());
        assert_eq!(registry.list().len(), 1);
        assert_eq!(
            registry.get("market-plugin", "0.1.0").unwrap().trust_level,
            "verified"
        );
    }
}
