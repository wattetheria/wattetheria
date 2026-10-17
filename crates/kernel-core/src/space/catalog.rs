use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use super::SpacePackage;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpaceCatalogVersion {
    pub space_version: String,
    pub package_path: PathBuf,
    pub assets_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpaceCatalogEntry {
    pub space_id: String,
    pub current_version: String,
    pub versions: Vec<SpaceCatalogVersion>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpaceCatalog {
    pub default_space_id: String,
    pub spaces: Vec<SpaceCatalogEntry>,
    #[serde(skip)]
    packages: BTreeMap<String, BTreeMap<String, SpacePackage>>,
}

impl SpaceCatalog {
    pub fn load(path: &Path) -> Result<Self> {
        let mut catalog: Self = serde_json::from_slice(
            &std::fs::read(path)
                .with_context(|| format!("read Space catalog {}", path.display()))?,
        )
        .context("decode Space catalog")?;
        let base = path.parent().context("catalog path has no parent")?;
        for entry in &catalog.spaces {
            ensure!(
                !entry.space_id.trim().is_empty(),
                "catalog space ID is required"
            );
            ensure!(
                !catalog.packages.contains_key(&entry.space_id),
                "duplicate catalog space ID"
            );
            let mut versions = BTreeMap::new();
            for version in &entry.versions {
                ensure!(
                    !version.package_path.is_absolute() && !version.assets_path.is_absolute(),
                    "catalog paths must be relative"
                );
                let package: SpacePackage = serde_json::from_slice(
                    &std::fs::read(base.join(&version.package_path)).with_context(|| {
                        format!("read Space package {}", version.package_path.display())
                    })?,
                )
                .context("decode Space package")?;
                package.validate()?;
                ensure!(
                    package.space_id == entry.space_id
                        && package.space_version == version.space_version,
                    "catalog package identity mismatch"
                );
                ensure!(
                    versions
                        .insert(version.space_version.clone(), package)
                        .is_none(),
                    "duplicate catalog space version"
                );
            }
            ensure!(
                versions.contains_key(&entry.current_version),
                "catalog current version is not trusted"
            );
            catalog.packages.insert(entry.space_id.clone(), versions);
        }
        ensure!(
            catalog.current(&catalog.default_space_id).is_some(),
            "catalog default Space is unknown"
        );
        for entry in &catalog.spaces {
            let package = catalog
                .current(&entry.space_id)
                .context("missing current Space package")?;
            for portal in &package.portals {
                let target = catalog
                    .current(&portal.target_space_id)
                    .context("unknown portal target Space")?;
                target
                    .place(&portal.target_place_id)
                    .context("unknown portal target place")?;
            }
        }
        Ok(catalog)
    }

    #[must_use]
    pub fn current(&self, space_id: &str) -> Option<&SpacePackage> {
        let entry = self
            .spaces
            .iter()
            .find(|entry| entry.space_id == space_id)?;
        self.version(space_id, &entry.current_version)
    }

    #[must_use]
    pub fn version(&self, space_id: &str, version: &str) -> Option<&SpacePackage> {
        self.packages.get(space_id)?.get(version)
    }
}
