//! Skill package format, installation, and local registry management.

use anyhow::{Context, Result, bail};
use chrono::Utc;
use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

use crate::capabilities::TrustLevel;

const DEFAULT_CONFORMANCE_REPORT: &str = "conformance/report.json";
const DEFAULT_TRUSTED_SIGNATURE_FILE: &str = "signatures/trusted.json";
const TRUSTED_ISSUER: &str = "wattetheria-official";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillManifestFile {
    pub id: String,
    pub version: String,
    pub entry: String,
    #[serde(default)]
    pub required_caps: Vec<String>,
    #[serde(default)]
    pub resources: Vec<String>,
    #[serde(default = "default_trust")]
    pub trust: TrustLevel,
    #[serde(default)]
    pub conformance_report: Option<String>,
    #[serde(default)]
    pub signature_file: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SkillPackage {
    pub root: PathBuf,
    pub manifest: SkillManifestFile,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledSkill {
    pub id: String,
    pub version: String,
    pub entry: String,
    pub required_caps: Vec<String>,
    pub resources: Vec<String>,
    pub trust: TrustLevel,
    pub conformance_report: Option<String>,
    pub signature_file: Option<String>,
    pub source: String,
    pub enabled: bool,
    pub installed_at: i64,
    pub install_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SkillRegistryState {
    pub skills: Vec<InstalledSkill>,
}

#[derive(Debug, Clone)]
pub struct SkillRegistry {
    path: PathBuf,
    state: SkillRegistryState,
}

#[derive(Debug, Clone, Deserialize)]
struct TrustedSignatureFile {
    issuer: String,
    alg: String,
    manifest_sha256: String,
    #[serde(default)]
    entry_sha256: Option<String>,
}

fn default_trust() -> TrustLevel {
    TrustLevel::Untrusted
}

impl SkillPackage {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let root = path.as_ref().to_path_buf();
        if !root.is_dir() {
            bail!("skill package path must be a directory");
        }

        let manifest_path = root.join("manifest.json");
        let manifest_raw = fs::read_to_string(&manifest_path)
            .with_context(|| format!("read skill manifest at {}", manifest_path.display()))?;
        let manifest: SkillManifestFile =
            serde_json::from_str(&manifest_raw).context("parse skill manifest")?;

        validate_manifest(&manifest)?;
        validate_layout(&root, &manifest)?;
        validate_trust_requirements(&root, &manifest)?;

        Ok(Self { root, manifest })
    }
}

impl SkillRegistry {
    pub fn load_or_new(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).context("create skill registry directory")?;
        }

        let state = if path.exists() {
            let raw = fs::read_to_string(&path).context("read skill registry")?;
            if raw.trim().is_empty() {
                SkillRegistryState::default()
            } else {
                serde_json::from_str(&raw).context("parse skill registry")?
            }
        } else {
            SkillRegistryState::default()
        };

        let registry = Self { path, state };
        registry.persist()?;
        Ok(registry)
    }

    pub fn install(
        &mut self,
        package: &SkillPackage,
        skill_store_dir: impl AsRef<Path>,
        source: &str,
    ) -> Result<InstalledSkill> {
        fs::create_dir_all(skill_store_dir.as_ref()).context("create skill store")?;
        let folder_name = format!("{}@{}", package.manifest.id, package.manifest.version);
        let install_path = skill_store_dir.as_ref().join(folder_name);

        if install_path.exists() {
            fs::remove_dir_all(&install_path).context("replace existing skill install")?;
        }
        copy_dir_recursive(&package.root, &install_path)?;

        let installed = InstalledSkill {
            id: package.manifest.id.clone(),
            version: package.manifest.version.clone(),
            entry: package.manifest.entry.clone(),
            required_caps: package.manifest.required_caps.clone(),
            resources: package.manifest.resources.clone(),
            trust: package.manifest.trust,
            conformance_report: package.manifest.conformance_report.clone(),
            signature_file: package.manifest.signature_file.clone(),
            source: source.to_string(),
            enabled: true,
            installed_at: Utc::now().timestamp(),
            install_path: install_path.display().to_string(),
        };

        self.state
            .skills
            .retain(|skill| !(skill.id == installed.id && skill.version == installed.version));
        self.state.skills.push(installed.clone());
        self.persist()?;
        Ok(installed)
    }

    pub fn set_enabled(&mut self, id: &str, enabled: bool) -> Result<InstalledSkill> {
        let skill = self
            .state
            .skills
            .iter_mut()
            .find(|skill| skill.id == id)
            .context("skill not found")?;
        skill.enabled = enabled;
        let updated = skill.clone();
        self.persist()?;
        Ok(updated)
    }

    #[must_use]
    pub fn list(&self) -> Vec<InstalledSkill> {
        let mut skills = self.state.skills.clone();
        skills.sort_by(|a, b| a.id.cmp(&b.id).then_with(|| a.version.cmp(&b.version)));
        skills
    }

    pub fn get(&self, id: &str) -> Result<InstalledSkill> {
        self.state
            .skills
            .iter()
            .find(|skill| skill.id == id)
            .cloned()
            .context("skill not found")
    }

    fn persist(&self) -> Result<()> {
        let content = serde_json::to_string_pretty(&self.state)?;
        fs::write(&self.path, content).context("write skill registry")
    }
}

fn validate_manifest(manifest: &SkillManifestFile) -> Result<()> {
    if manifest.id.trim().is_empty() {
        bail!("manifest.id cannot be empty");
    }
    Version::parse(&manifest.version).context("manifest.version must be semver")?;
    if manifest.entry.trim().is_empty() {
        bail!("manifest.entry cannot be empty");
    }
    Ok(())
}

fn validate_layout(root: &Path, manifest: &SkillManifestFile) -> Result<()> {
    let schema_dir = root.join("schemas");
    if !schema_dir.is_dir() {
        bail!("skill package requires schemas/ directory");
    }

    let has_schema = fs::read_dir(&schema_dir)
        .context("read schemas directory")?
        .filter_map(std::result::Result::ok)
        .any(|entry| {
            entry
                .path()
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
        });

    if !has_schema {
        bail!("skill package requires at least one schemas/*.json file");
    }

    if manifest.entry.starts_with("builtin:") {
        return Ok(());
    }

    if let Some(rel) = manifest.entry.strip_prefix("process:") {
        if rel.trim().is_empty() {
            bail!("process entry must include a relative executable path");
        }
        let rel_path = Path::new(rel);
        if rel_path.is_absolute() {
            bail!("process entry must be a relative path");
        }

        let process_path = root.join(rel_path);
        let normalized = process_path
            .canonicalize()
            .with_context(|| format!("resolve process entry {}", process_path.display()))?;
        let root_normalized = root
            .canonicalize()
            .with_context(|| format!("resolve skill root {}", root.display()))?;

        if !normalized.starts_with(&root_normalized) {
            bail!("process entry path escapes skill package root");
        }
        if !normalized.is_file() {
            bail!("process entry path must point to a file");
        }
        return Ok(());
    }

    let wasm_path = root.join("bin").join("skill.wasm");
    if !wasm_path.exists() {
        bail!("skill package requires bin/skill.wasm for non-builtin entries");
    }

    Ok(())
}

fn validate_trust_requirements(root: &Path, manifest: &SkillManifestFile) -> Result<()> {
    match manifest.trust {
        TrustLevel::Untrusted => Ok(()),
        TrustLevel::Verified => validate_verified_report(root, manifest),
        TrustLevel::Trusted => {
            validate_verified_report(root, manifest)?;
            validate_trusted_signature(root, manifest)
        }
    }
}

fn validate_verified_report(root: &Path, manifest: &SkillManifestFile) -> Result<()> {
    let report_rel = manifest
        .conformance_report
        .as_deref()
        .unwrap_or(DEFAULT_CONFORMANCE_REPORT);
    let report_path = root.join(report_rel);
    if !report_path.exists() {
        bail!(
            "{} trust requires conformance report at {}",
            match manifest.trust {
                TrustLevel::Verified => "verified",
                TrustLevel::Trusted => "trusted",
                TrustLevel::Untrusted => "untrusted",
            },
            report_path.display()
        );
    }

    let raw = fs::read_to_string(&report_path)
        .with_context(|| format!("read conformance report {}", report_path.display()))?;
    let value: Value = serde_json::from_str(&raw)
        .with_context(|| format!("parse conformance report {}", report_path.display()))?;

    if !value["passed"].as_bool().unwrap_or(false) {
        bail!(
            "conformance report must include passed=true for {}",
            report_path.display()
        );
    }

    Ok(())
}

fn validate_trusted_signature(root: &Path, manifest: &SkillManifestFile) -> Result<()> {
    let signature_rel = manifest
        .signature_file
        .as_deref()
        .unwrap_or(DEFAULT_TRUSTED_SIGNATURE_FILE);
    let signature_path = root.join(signature_rel);
    if !signature_path.exists() {
        bail!(
            "trusted skill requires signature metadata at {}",
            signature_path.display()
        );
    }

    let raw = fs::read_to_string(&signature_path)
        .with_context(|| format!("read signature metadata {}", signature_path.display()))?;
    let meta: TrustedSignatureFile = serde_json::from_str(&raw)
        .with_context(|| format!("parse signature metadata {}", signature_path.display()))?;

    if meta.issuer != TRUSTED_ISSUER {
        bail!("trusted issuer mismatch: expected {TRUSTED_ISSUER}");
    }
    if meta.alg != "sha256" {
        bail!("trusted signature algorithm must be sha256");
    }

    let manifest_hash = hash_file(root.join("manifest.json"))?;
    if meta.manifest_sha256 != manifest_hash {
        bail!("trusted manifest digest mismatch");
    }

    if !manifest.entry.starts_with("builtin:") {
        let entry_path = match manifest.entry.strip_prefix("process:") {
            Some(rel) => root.join(rel),
            None => root.join("bin/skill.wasm"),
        };
        let entry_hash = hash_file(entry_path)?;
        let expected = meta
            .entry_sha256
            .as_deref()
            .context("trusted signature is missing entry_sha256")?;
        if expected != entry_hash {
            bail!("trusted entry digest mismatch");
        }
    }

    Ok(())
}

fn hash_file(path: impl AsRef<Path>) -> Result<String> {
    let bytes = fs::read(path.as_ref())
        .with_context(|| format!("read file for sha256 {}", path.as_ref().display()))?;
    let digest = Sha256::digest(bytes);
    Ok(hex::encode(digest))
}

fn copy_dir_recursive(source: &Path, dest: &Path) -> Result<()> {
    for entry in WalkDir::new(source) {
        let entry = entry.context("walk skill package")?;
        let rel_path = entry
            .path()
            .strip_prefix(source)
            .context("strip skill package prefix")?;
        let target = dest.join(rel_path);

        if entry.file_type().is_dir() {
            fs::create_dir_all(&target)
                .with_context(|| format!("create directory {}", target.display()))?;
        } else {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(entry.path(), &target)
                .with_context(|| format!("copy file {}", target.display()))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_conformance_report(root: &Path) {
        fs::create_dir_all(root.join("conformance")).unwrap();
        fs::write(
            root.join("conformance/report.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "suite": "wattetheria-conformance",
                "passed": true,
                "timestamp": 1_700_000_000
            }))
            .unwrap(),
        )
        .unwrap();
    }

    fn write_sample_skill(root: &Path) {
        fs::create_dir_all(root.join("schemas")).unwrap();
        fs::write(root.join("schemas/input.json"), "{}").unwrap();
        write_conformance_report(root);
        fs::write(
            root.join("manifest.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "id": "echo-skill",
                "version": "0.1.0",
                "entry": "builtin:echo",
                "required_caps": ["model.invoke"],
                "resources": ["docs"],
                "trust": "verified",
                "conformance_report": "conformance/report.json"
            }))
            .unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn install_enable_disable_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let pkg_dir = dir.path().join("pkg");
        write_sample_skill(&pkg_dir);

        let package = SkillPackage::load(&pkg_dir).unwrap();
        let mut registry =
            SkillRegistry::load_or_new(dir.path().join("skills/registry.json")).unwrap();
        let installed = registry
            .install(&package, dir.path().join("skills/store"), "path:pkg")
            .unwrap();

        assert_eq!(installed.id, "echo-skill");
        assert!(installed.enabled);

        let disabled = registry.set_enabled("echo-skill", false).unwrap();
        assert!(!disabled.enabled);

        let list = registry.list();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].trust, TrustLevel::Verified);
    }

    #[test]
    fn process_skill_entry_requires_binary() {
        let dir = tempfile::tempdir().unwrap();
        let pkg_dir = dir.path().join("pkg-process");
        fs::create_dir_all(pkg_dir.join("schemas")).unwrap();
        fs::write(pkg_dir.join("schemas/input.json"), "{}").unwrap();
        fs::write(
            pkg_dir.join("manifest.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "id": "process-skill",
                "version": "0.1.0",
                "entry": "process:bin/skill.sh",
                "required_caps": ["fs.read"],
                "resources": [],
                "trust": "untrusted"
            }))
            .unwrap(),
        )
        .unwrap();

        let err = SkillPackage::load(&pkg_dir).unwrap_err();
        assert!(err.to_string().contains("resolve process entry"));

        fs::create_dir_all(pkg_dir.join("bin")).unwrap();
        fs::write(
            pkg_dir.join("bin/skill.sh"),
            "#!/usr/bin/env sh\necho '{}'\n",
        )
        .unwrap();
        SkillPackage::load(&pkg_dir).unwrap();
    }
    #[test]
    fn verified_skill_requires_conformance_report() {
        let dir = tempfile::tempdir().unwrap();
        let pkg_dir = dir.path().join("pkg-no-report");
        fs::create_dir_all(pkg_dir.join("schemas")).unwrap();
        fs::write(pkg_dir.join("schemas/input.json"), "{}").unwrap();
        fs::write(
            pkg_dir.join("manifest.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "id": "no-report",
                "version": "0.1.0",
                "entry": "builtin:echo",
                "required_caps": [],
                "resources": [],
                "trust": "verified"
            }))
            .unwrap(),
        )
        .unwrap();

        let err = SkillPackage::load(&pkg_dir).unwrap_err();
        assert!(err.to_string().contains("conformance report"));
    }

    #[test]
    fn trusted_skill_requires_signature_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let pkg_dir = dir.path().join("pkg-trusted");
        fs::create_dir_all(pkg_dir.join("schemas")).unwrap();
        fs::write(pkg_dir.join("schemas/input.json"), "{}").unwrap();
        write_conformance_report(&pkg_dir);
        fs::write(
            pkg_dir.join("manifest.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "id": "trusted-skill",
                "version": "0.1.0",
                "entry": "builtin:echo",
                "required_caps": ["model.invoke"],
                "resources": [],
                "trust": "trusted",
                "conformance_report": "conformance/report.json"
            }))
            .unwrap(),
        )
        .unwrap();

        let err = SkillPackage::load(&pkg_dir).unwrap_err();
        assert!(err.to_string().contains("signature metadata"));

        fs::create_dir_all(pkg_dir.join("signatures")).unwrap();
        let manifest_hash = hash_file(pkg_dir.join("manifest.json")).unwrap();
        fs::write(
            pkg_dir.join("signatures/trusted.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "issuer": "wattetheria-official",
                "alg": "sha256",
                "manifest_sha256": manifest_hash,
                "entry_sha256": null
            }))
            .unwrap(),
        )
        .unwrap();

        SkillPackage::load(&pkg_dir).unwrap();
    }
}
