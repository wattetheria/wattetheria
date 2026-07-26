use super::ServiceAgentIdentity;
use anyhow::{Context, Result, bail};
use std::fs::{self, OpenOptions};
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

const PRIVATE_IDENTITY_FILE: &str = "identity.json";
const LAYOUT_LOCK_FILE: &str = ".service-agent-layout.lock";

pub(super) fn ensure_layout(root: &Path, legacy_root: &Path) -> Result<()> {
    if !legacy_root.exists() {
        return Ok(());
    }
    let provider_dir = root
        .parent()
        .context("Service Agent identity root has no Provider parent")?;
    fs::create_dir_all(provider_dir).context("create Provider identity directory")?;
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    options.mode(0o600);
    let layout_lock = options
        .open(provider_dir.join(LAYOUT_LOCK_FILE))
        .context("open Service Agent identity layout lock")?;
    fs2::FileExt::lock_exclusive(&layout_lock).context("lock Service Agent identity layout")?;
    if !legacy_root.exists() {
        return Ok(());
    }
    if !root.exists() {
        fs::rename(legacy_root, root)
            .context("migrate Service Agent identity directory under Provider identity")?;
        return Ok(());
    }
    validate_layout_merge(root, legacy_root)?;
    merge_legacy_layout(root, legacy_root)
}

pub(super) fn read_identity(path: &Path) -> Result<ServiceAgentIdentity> {
    let contents = fs::read_to_string(path).context("read Service Agent identity")?;
    let mut value: serde_json::Value =
        serde_json::from_str(&contents).context("parse Service Agent identity JSON")?;
    if value.get("service_agent_identity_id").is_none()
        && value.get("identity_id").is_none()
        && value.get("bound_agent_id").is_none()
        && let Some(legacy_agent_id) = value.get("agent_id").cloned()
    {
        value["bound_agent_id"] = legacy_agent_id;
    }
    serde_json::from_value(value).context("parse Service Agent identity")
}

fn validate_layout_merge(root: &Path, legacy_root: &Path) -> Result<()> {
    for entry in
        fs::read_dir(legacy_root).context("read legacy Service Agent identity directory")?
    {
        let entry = entry.context("read legacy Service Agent identity entry")?;
        if entry.file_name() == ".locks"
            || !entry
                .file_type()
                .context("read legacy Service Agent identity entry type")?
                .is_dir()
        {
            continue;
        }
        let legacy_path = entry.path().join(PRIVATE_IDENTITY_FILE);
        let current_path = root.join(entry.file_name()).join(PRIVATE_IDENTITY_FILE);
        if !legacy_path.exists() || !current_path.exists() {
            continue;
        }
        let legacy = read_identity(&legacy_path)?;
        let current = read_identity(&current_path)?;
        if legacy != current {
            bail!(
                "conflicting Service Agent identities exist at '{}' and '{}'",
                legacy_path.display(),
                current_path.display()
            );
        }
    }
    Ok(())
}

fn merge_legacy_layout(root: &Path, legacy_root: &Path) -> Result<()> {
    for entry in
        fs::read_dir(legacy_root).context("read legacy Service Agent identity directory")?
    {
        let entry = entry.context("read legacy Service Agent identity entry")?;
        if entry.file_name() == ".locks" {
            fs::remove_dir_all(entry.path())
                .context("remove legacy Service Agent identity locks")?;
            continue;
        }
        if !entry
            .file_type()
            .context("read legacy Service Agent identity entry type")?
            .is_dir()
        {
            continue;
        }
        let destination = root.join(entry.file_name());
        if !destination.exists() {
            fs::rename(entry.path(), destination)
                .context("migrate Service Agent identity record")?;
            continue;
        }
        let legacy_path = entry.path().join(PRIVATE_IDENTITY_FILE);
        let current_path = destination.join(PRIVATE_IDENTITY_FILE);
        if legacy_path.exists() && !current_path.exists() {
            fs::rename(&legacy_path, &current_path)
                .context("migrate Service Agent identity file")?;
        } else if legacy_path.exists() {
            fs::remove_file(&legacy_path)
                .context("remove duplicated legacy Service Agent identity")?;
        }
        let _ = fs::remove_dir(entry.path());
    }
    fs::remove_dir(legacy_root).context("remove migrated Service Agent identity directory")?;
    Ok(())
}
