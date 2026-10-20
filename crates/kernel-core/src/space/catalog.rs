use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use super::SpacePackage;

include!(concat!(env!("OUT_DIR"), "/bundled_spaces.rs"));

fn is_relative_catalog_path(path: &Path) -> bool {
    path.components()
        .all(|component| matches!(component, Component::Normal(_) | Component::CurDir))
}

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
        let bytes = std::fs::read(path)
            .with_context(|| format!("read Space catalog {}", path.display()))?;
        let base = path.parent().context("catalog path has no parent")?;
        Self::from_sources(&bytes, |package_path| {
            std::fs::read(base.join(package_path))
                .with_context(|| format!("read Space package {}", package_path.display()))
        })
    }

    pub fn bundled() -> Result<Option<Self>> {
        let Some(catalog) = BUNDLED_CATALOG else {
            return Ok(None);
        };
        Self::from_sources(catalog.as_bytes(), |package_path| {
            let package = BUNDLED_PACKAGES
                .iter()
                .find_map(|(bundled_path, package)| {
                    (Path::new(bundled_path) == package_path).then_some(*package)
                })
                .with_context(|| {
                    format!("missing bundled Space package {}", package_path.display())
                })?;
            Ok(package.as_bytes().to_vec())
        })
        .map(Some)
    }

    fn from_sources(
        catalog_bytes: &[u8],
        read_package: impl Fn(&Path) -> Result<Vec<u8>>,
    ) -> Result<Self> {
        let mut catalog: Self =
            serde_json::from_slice(catalog_bytes).context("decode Space catalog")?;
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
                    is_relative_catalog_path(&version.package_path)
                        && is_relative_catalog_path(&version.assets_path),
                    "catalog paths must be relative and cannot contain '..'"
                );
                let package_bytes = read_package(&version.package_path)?;
                let package: SpacePackage =
                    serde_json::from_slice(&package_bytes).context("decode Space package")?;
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

#[cfg(test)]
mod tests {
    use super::SpaceCatalog;
    use serde_json::json;

    #[test]
    fn bundled_catalog_has_a_valid_default_and_current_package() {
        let Some(catalog) = SpaceCatalog::bundled().unwrap() else {
            return;
        };
        let default = catalog
            .spaces
            .iter()
            .find(|entry| entry.space_id == catalog.default_space_id)
            .expect("default Space exists in catalog");
        let package = catalog
            .current(&catalog.default_space_id)
            .expect("default current package");

        assert!(
            default
                .versions
                .iter()
                .any(|version| version.space_version == default.current_version)
        );
        package.validate().expect("current package validates");
    }

    #[test]
    fn catalog_rejects_parent_and_absolute_paths() {
        for (package_path, assets_path) in [
            ("../package.json", "assets"),
            ("package.json", "../assets"),
            ("/package.json", "assets"),
            ("package.json", "/assets"),
        ] {
            let bytes = serde_json::to_vec(&json!({
                "default_space_id": "example-space",
                "spaces": [{
                    "space_id": "example-space",
                    "current_version": "v1",
                    "versions": [{
                        "space_version": "v1",
                        "package_path": package_path,
                        "assets_path": assets_path
                    }]
                }]
            }))
            .unwrap();

            let error = SpaceCatalog::from_sources(&bytes, |_| Ok(Vec::new())).unwrap_err();
            assert!(
                error.to_string().contains("relative") && error.to_string().contains("'..'"),
                "unexpected path validation error: {error}"
            );
        }
    }
}
