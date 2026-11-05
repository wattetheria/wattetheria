use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};

use super::SpacePackage;

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

    /// Builds a catalog from its JSON and a reader for the packages it names (a directory, the
    /// gateway's published files, or a cache of them). Every package is decoded, validated and
    /// matched against the catalog entry that names it.
    pub fn from_sources(
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
            let current = versions
                .get(&entry.current_version)
                .context("missing current Space package")?;
            ensure!(
                current.movement_search_radius.is_some_and(|radius| {
                    radius > 0 && radius <= current.size[0].max(current.size[1])
                }),
                "catalog current Space package requires a valid movement_search_radius"
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

    /// The version `space_id` is currently published at, when the catalog knows the Space.
    #[must_use]
    pub fn current_version(&self, space_id: &str) -> Option<&str> {
        self.spaces
            .iter()
            .find(|entry| entry.space_id == space_id)
            .map(|entry| entry.current_version.as_str())
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
    fn version(&self, space_id: &str, version: &str) -> Option<&SpacePackage> {
        self.packages.get(space_id)?.get(version)
    }
}

#[cfg(test)]
mod tests {
    use super::SpaceCatalog;
    use serde_json::json;

    const FIXTURE_PACKAGE: &str = include_str!(
        "../../../../../watt-gateway/crates/gateway-space/src/map/the-ville/space-package.json"
    );

    #[test]
    fn catalog_from_sources_exposes_the_current_package_and_version() {
        let package: serde_json::Value = serde_json::from_str(FIXTURE_PACKAGE).unwrap();
        let version = package["space_version"].as_str().unwrap();
        let catalog_bytes = serde_json::to_vec(&json!({
            "default_space_id": "the-ville",
            "spaces": [{
                "space_id": "the-ville",
                "current_version": version,
                "versions": [{
                    "space_version": version,
                    "package_path": "the-ville/space-package.json",
                    "assets_path": "the-ville/"
                }]
            }]
        }))
        .unwrap();
        let catalog = SpaceCatalog::from_sources(&catalog_bytes, |path| {
            assert_eq!(path, std::path::Path::new("the-ville/space-package.json"));
            Ok(FIXTURE_PACKAGE.as_bytes().to_vec())
        })
        .unwrap();
        assert_eq!(catalog.current_version("the-ville"), Some(version));
        assert_eq!(catalog.current_version("elsewhere"), None);
        let current = catalog.current(&catalog.default_space_id).unwrap();
        assert_eq!(current.space_version, version);
        current.validate().unwrap();
    }

    #[test]
    fn catalog_rejects_a_package_whose_version_differs_from_the_entry() {
        let package: serde_json::Value = serde_json::from_str(FIXTURE_PACKAGE).unwrap();
        let catalog_bytes = serde_json::to_vec(&json!({
            "default_space_id": "the-ville",
            "spaces": [{
                "space_id": "the-ville",
                "current_version": "sha256:other",
                "versions": [{
                    "space_version": "sha256:other",
                    "package_path": "p.json",
                    "assets_path": "a/"
                }]
            }]
        }))
        .unwrap();
        let error = SpaceCatalog::from_sources(&catalog_bytes, |_| {
            Ok(serde_json::to_vec(&package).unwrap())
        })
        .unwrap_err();
        assert!(error.to_string().contains("identity mismatch"), "{error}");
    }

    #[test]
    fn catalog_rejects_a_package_without_search_radius_as_current() {
        use sha2::Digest as _;
        let mut package: serde_json::Value = serde_json::from_str(FIXTURE_PACKAGE).unwrap();
        let fields = package.as_object_mut().unwrap();
        fields.remove("movement_search_radius");
        fields.remove("space_version");
        let version = format!(
            "sha256:{}",
            hex::encode(sha2::Sha256::digest(serde_jcs::to_vec(&package).unwrap()))
        );
        package["space_version"] = json!(version);
        let catalog_bytes = serde_json::to_vec(&json!({
            "default_space_id": "the-ville",
            "spaces": [{
                "space_id": "the-ville",
                "current_version": version,
                "versions": [{
                    "space_version": version,
                    "package_path": "package.json",
                    "assets_path": "assets"
                }]
            }]
        }))
        .unwrap();

        let error = SpaceCatalog::from_sources(&catalog_bytes, |_| {
            Ok(serde_json::to_vec(&package).unwrap())
        })
        .unwrap_err();

        assert!(error.to_string().contains("movement_search_radius"));
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
