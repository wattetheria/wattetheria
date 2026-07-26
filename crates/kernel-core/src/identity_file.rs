use crate::identity::Identity;
use anyhow::{Context, Result};
use std::fs::{self, OpenOptions};
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use uuid::Uuid;

pub(crate) fn write_private_identity(path: &Path, identity: &Identity) -> Result<()> {
    let parent = path
        .parent()
        .context("private identity path has no parent")?;
    fs::create_dir_all(parent).context("create private identity directory")?;
    let temporary_path = parent.join(format!(".pending-identity-{}.tmp", Uuid::new_v4()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options
        .open(&temporary_path)
        .context("create temporary private identity")?;
    file.write_all(serde_json::to_string_pretty(identity)?.as_bytes())
        .context("write temporary private identity")?;
    file.sync_all().context("sync temporary private identity")?;
    drop(file);
    if let Err(error) = fs::rename(&temporary_path, path) {
        let _ = fs::remove_file(&temporary_path);
        return Err(error).context("install pending private identity");
    }
    restrict_private_identity_permissions(path)
}

#[cfg(unix)]
pub(crate) fn restrict_private_identity_permissions(path: &Path) -> Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .context("restrict private identity permissions")
}

#[cfg(not(unix))]
pub(crate) fn restrict_private_identity_permissions(_path: &Path) -> Result<()> {
    Ok(())
}
