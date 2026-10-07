use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngCore;

pub(super) fn generate() -> String {
    let mut bytes = [0_u8; 32];
    rand::rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

pub(super) fn load_or_create(path: &Path) -> Result<String> {
    let secret = generate();
    match write_new(path, &secret) {
        Ok(()) => Ok(secret),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let metadata = fs::metadata(path).context("read MCP URL secret metadata")?;
            if !metadata.is_file() {
                bail!("MCP URL secret path is not a file");
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if metadata.permissions().mode() & 0o077 != 0 {
                    bail!("MCP URL secret has permissions wider than 0600");
                }
            }
            let secret = fs::read_to_string(path).context("read MCP URL secret")?;
            if secret.len() != 43
                || URL_SAFE_NO_PAD
                    .decode(&secret)
                    .map_or(true, |bytes| bytes.len() != 32)
            {
                bail!("MCP URL secret must encode exactly 32 bytes as unpadded base64url");
            }
            Ok(secret)
        }
        Err(error) => Err(error).context("create MCP URL secret"),
    }
}

pub(super) fn replace(path: &Path, secret: &str) -> Result<()> {
    let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    write_new(&temporary, secret).context("write replacement MCP URL secret")?;
    if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(error).context("replace MCP URL secret");
    }
    Ok(())
}

fn write_new(path: &Path, secret: &str) -> std::io::Result<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file: File = options.open(path)?;
    if let Err(error) = file
        .write_all(secret.as_bytes())
        .and_then(|()| file.sync_all())
    {
        let _ = fs::remove_file(path);
        return Err(error);
    }
    Ok(())
}
