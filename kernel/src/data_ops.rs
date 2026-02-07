//! Data snapshot, recovery, migration, and backup utilities.

use anyhow::{Context, Result, bail};
use chrono::Utc;
use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use semver::Version;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use tar::{Archive, Builder};

use crate::event_log::EventLog;

const SEGMENT_EVENT_LINES: usize = 500;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotMeta {
    pub id: String,
    pub created_at: i64,
    pub event_count: usize,
    pub last_hash: Option<String>,
    pub event_file: String,
    #[serde(default)]
    pub segment_files: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationReport {
    pub from_version: String,
    pub to_version: String,
    pub steps: Vec<String>,
}

pub fn create_snapshot(
    event_log_path: impl AsRef<Path>,
    snapshot_dir: impl AsRef<Path>,
) -> Result<SnapshotMeta> {
    fs::create_dir_all(snapshot_dir.as_ref()).context("create snapshot directory")?;

    let log = EventLog::new(event_log_path.as_ref())?;
    let events = log.get_all()?;
    let last_hash = events.last().map(|event| event.hash.clone());

    let id = format!("snapshot-{}", Utc::now().timestamp());
    let event_file_name = format!("{id}.events.jsonl");
    let event_target = snapshot_dir.as_ref().join(&event_file_name);
    fs::copy(event_log_path.as_ref(), &event_target).with_context(|| {
        format!(
            "copy event log from {} to {}",
            event_log_path.as_ref().display(),
            event_target.display()
        )
    })?;

    let segment_files =
        write_snapshot_segments(event_log_path.as_ref(), snapshot_dir.as_ref(), &id)?;

    let meta = SnapshotMeta {
        id: id.clone(),
        created_at: Utc::now().timestamp(),
        event_count: events.len(),
        last_hash,
        event_file: event_file_name,
        segment_files,
    };

    let meta_path = snapshot_dir.as_ref().join(format!("{id}.meta.json"));
    fs::write(meta_path, serde_json::to_string_pretty(&meta)?)
        .context("write snapshot metadata")?;
    Ok(meta)
}

pub fn list_snapshots(snapshot_dir: impl AsRef<Path>) -> Result<Vec<SnapshotMeta>> {
    if !snapshot_dir.as_ref().exists() {
        return Ok(Vec::new());
    }

    let mut metas: Vec<SnapshotMeta> = Vec::new();
    for entry in fs::read_dir(snapshot_dir.as_ref()).context("read snapshot directory")? {
        let entry = entry?;
        let path = entry.path();
        if !path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(".meta.json"))
        {
            continue;
        }

        let raw = fs::read_to_string(path).context("read snapshot metadata")?;
        metas.push(serde_json::from_str(&raw).context("parse snapshot metadata")?);
    }

    metas.sort_by_key(|meta| std::cmp::Reverse(meta.created_at));
    Ok(metas)
}

pub fn recover_if_corrupt(
    event_log_path: impl AsRef<Path>,
    snapshot_dir: impl AsRef<Path>,
) -> Result<Option<SnapshotMeta>> {
    recover_if_corrupt_with_sources(event_log_path, snapshot_dir, &[])
}

pub fn recover_if_corrupt_with_sources(
    event_log_path: impl AsRef<Path>,
    snapshot_dir: impl AsRef<Path>,
    source_event_logs: &[PathBuf],
) -> Result<Option<SnapshotMeta>> {
    if event_log_is_valid(event_log_path.as_ref())? {
        return Ok(None);
    }

    for meta in list_snapshots(snapshot_dir.as_ref())? {
        if restore_snapshot(meta.clone(), event_log_path.as_ref(), snapshot_dir.as_ref()).is_ok()
            && event_log_is_valid(event_log_path.as_ref())?
        {
            return Ok(Some(meta));
        }
    }

    for source in source_event_logs {
        if !source.exists() {
            continue;
        }
        fs::copy(source, event_log_path.as_ref()).with_context(|| {
            format!(
                "copy external recovery source {} to {}",
                source.display(),
                event_log_path.as_ref().display()
            )
        })?;
        if event_log_is_valid(event_log_path.as_ref())? {
            let events = EventLog::new(event_log_path.as_ref())?.get_all()?;
            let meta = SnapshotMeta {
                id: format!("external-recovery-{}", Utc::now().timestamp()),
                created_at: Utc::now().timestamp(),
                event_count: events.len(),
                last_hash: events.last().map(|event| event.hash.clone()),
                event_file: source.display().to_string(),
                segment_files: Vec::new(),
            };
            return Ok(Some(meta));
        }
    }

    bail!("no valid snapshot or external source available for recovery")
}

pub fn export_backup(data_dir: impl AsRef<Path>, output_tgz: impl AsRef<Path>) -> Result<()> {
    if let Some(parent) = output_tgz.as_ref().parent() {
        fs::create_dir_all(parent).context("create backup parent directory")?;
    }

    let output = fs::File::create(output_tgz.as_ref()).context("create backup file")?;
    let encoder = GzEncoder::new(output, Compression::default());
    let mut builder = Builder::new(encoder);
    builder
        .append_dir_all("data", data_dir.as_ref())
        .context("append data directory to tarball")?;
    builder
        .into_inner()
        .context("finalize tar builder")?
        .finish()
        .context("finalize gzip")?;
    Ok(())
}

pub fn import_backup(input_tgz: impl AsRef<Path>, data_dir: impl AsRef<Path>) -> Result<()> {
    fs::create_dir_all(data_dir.as_ref()).context("create import directory")?;

    let input = fs::File::open(input_tgz.as_ref()).context("open backup file")?;
    let decoder = GzDecoder::new(input);
    let mut archive = Archive::new(decoder);

    for entry in archive.entries().context("read backup entries")? {
        let mut entry = entry.context("read backup entry")?;
        let path = entry.path().context("read backup entry path")?.into_owned();
        let rel = path
            .strip_prefix("data")
            .context("backup is missing data/ root")?;
        let target = data_dir.as_ref().join(rel);

        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        entry
            .unpack(&target)
            .with_context(|| format!("unpack {}", target.display()))?;
    }

    Ok(())
}

pub fn migrate_data_dir(data_dir: impl AsRef<Path>, target: &str) -> Result<MigrationReport> {
    fs::create_dir_all(data_dir.as_ref()).context("create data dir for migration")?;
    let target_version = Version::parse(target).context("parse target migration version")?;

    let version_path = data_dir.as_ref().join("schema.version");
    let current_raw = if version_path.exists() {
        fs::read_to_string(&version_path).context("read schema.version")?
    } else {
        "0.1.0".to_string()
    };
    let current_version =
        Version::parse(current_raw.trim()).context("parse current schema.version")?;

    if target_version < current_version {
        bail!("downgrade migration is not supported");
    }

    let mut steps = Vec::new();

    if current_version.major == 0 && current_version.minor <= 1 && target_version.minor >= 2 {
        fs::create_dir_all(data_dir.as_ref().join("policy"))?;
        fs::create_dir_all(data_dir.as_ref().join("skills/store"))?;
        fs::create_dir_all(data_dir.as_ref().join("mcp"))?;
        steps.push("created policy/, skills/store/, and mcp/ directories".to_string());
    }

    fs::write(&version_path, target_version.to_string()).context("write schema.version")?;
    steps.push(format!(
        "set schema.version from {current_version} to {target_version}"
    ));

    Ok(MigrationReport {
        from_version: current_version.to_string(),
        to_version: target_version.to_string(),
        steps,
    })
}

fn write_snapshot_segments(
    event_log_path: &Path,
    snapshot_dir: &Path,
    snapshot_id: &str,
) -> Result<Vec<String>> {
    let segments_dir = snapshot_dir.join("segments");
    fs::create_dir_all(&segments_dir).context("create snapshot segments directory")?;

    let file = fs::File::open(event_log_path).with_context(|| {
        format!(
            "open event log for segmentation: {}",
            event_log_path.display()
        )
    })?;
    let reader = BufReader::new(file);

    let mut all_segments = Vec::new();
    let mut current = Vec::new();
    let mut index: usize = 0;

    for line in reader.lines() {
        let line = line.context("read event log line for segmentation")?;
        current.push(line);
        if current.len() >= SEGMENT_EVENT_LINES {
            let segment_name = write_segment_chunk(&segments_dir, snapshot_id, index, &current)?;
            all_segments.push(segment_name);
            current.clear();
            index += 1;
        }
    }

    if !current.is_empty() {
        let segment_name = write_segment_chunk(&segments_dir, snapshot_id, index, &current)?;
        all_segments.push(segment_name);
    }

    Ok(all_segments)
}

fn write_segment_chunk(
    segments_dir: &Path,
    snapshot_id: &str,
    index: usize,
    lines: &[String],
) -> Result<String> {
    let name = format!("{snapshot_id}.seg-{index:05}.jsonl");
    let path = segments_dir.join(&name);
    let mut out = fs::File::create(&path)
        .with_context(|| format!("create snapshot segment {}", path.display()))?;
    for line in lines {
        writeln!(out, "{line}")
            .with_context(|| format!("write snapshot segment {}", path.display()))?;
    }
    Ok(format!("segments/{name}"))
}

fn restore_snapshot(meta: SnapshotMeta, event_log_path: &Path, snapshot_dir: &Path) -> Result<()> {
    let source = snapshot_dir.join(&meta.event_file);
    if source.exists() {
        fs::copy(&source, event_log_path).with_context(|| {
            format!(
                "restore event log from {} to {}",
                source.display(),
                event_log_path.display()
            )
        })?;
        return Ok(());
    }

    if meta.segment_files.is_empty() {
        bail!("snapshot event file missing and no segment files available");
    }

    let mut out = fs::File::create(event_log_path).with_context(|| {
        format!(
            "create event log for segment recovery {}",
            event_log_path.display()
        )
    })?;

    for rel in meta.segment_files {
        let segment_path = snapshot_dir.join(rel);
        let bytes = fs::read(&segment_path)
            .with_context(|| format!("read segment {}", segment_path.display()))?;
        out.write_all(&bytes)
            .with_context(|| format!("write recovered segment {}", segment_path.display()))?;
    }

    Ok(())
}

fn event_log_is_valid(path: &Path) -> Result<bool> {
    let log = EventLog::new(path)?;
    let verify = match log.verify_chain() {
        Ok((valid, _)) => valid,
        Err(_) => false,
    };
    Ok(verify)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Identity;
    use serde_json::json;

    #[test]
    fn snapshot_and_recover_flow() {
        let dir = tempfile::tempdir().unwrap();
        let event_path = dir.path().join("events.jsonl");
        let snapshot_dir = dir.path().join("snapshots");

        let log = EventLog::new(&event_path).unwrap();
        let identity = Identity::new_random();
        log.append_signed("TASK_SETTLED", json!({"task":"a"}), &identity)
            .unwrap();

        let snapshot = create_snapshot(&event_path, &snapshot_dir).unwrap();
        assert_eq!(snapshot.event_count, 1);
        assert!(!snapshot.segment_files.is_empty());

        fs::write(&event_path, "{bad json\n").unwrap();
        let recovered = recover_if_corrupt(&event_path, &snapshot_dir).unwrap();
        assert!(recovered.is_some());

        let verify = EventLog::new(&event_path).unwrap().verify_chain().unwrap();
        assert!(verify.0);
    }

    #[test]
    fn recover_from_segments_when_snapshot_event_file_missing() {
        let dir = tempfile::tempdir().unwrap();
        let event_path = dir.path().join("events.jsonl");
        let snapshot_dir = dir.path().join("snapshots");

        let log = EventLog::new(&event_path).unwrap();
        let identity = Identity::new_random();
        for i in 0..3 {
            log.append_signed("TASK_SETTLED", json!({"task":i}), &identity)
                .unwrap();
        }

        let snapshot = create_snapshot(&event_path, &snapshot_dir).unwrap();
        fs::remove_file(snapshot_dir.join(snapshot.event_file)).unwrap();

        fs::write(&event_path, "{corrupt\n").unwrap();
        let recovered = recover_if_corrupt(&event_path, &snapshot_dir).unwrap();
        assert!(recovered.is_some());

        let verify = EventLog::new(&event_path).unwrap().verify_chain().unwrap();
        assert!(verify.0);
    }

    #[test]
    fn recover_from_external_source_when_snapshot_is_missing() {
        let dir = tempfile::tempdir().unwrap();
        let event_path = dir.path().join("events.jsonl");
        let snapshot_dir = dir.path().join("snapshots");
        let external = dir.path().join("peer.events.jsonl");

        let local_log = EventLog::new(&event_path).unwrap();
        let identity = Identity::new_random();
        local_log
            .append_signed("TASK_SETTLED", json!({"task":"local"}), &identity)
            .unwrap();

        let peer_log = EventLog::new(&external).unwrap();
        peer_log
            .append_signed("TASK_SETTLED", json!({"task":"peer"}), &identity)
            .unwrap();

        fs::write(&event_path, "{corrupt\n").unwrap();
        let recovered = recover_if_corrupt_with_sources(
            &event_path,
            &snapshot_dir,
            std::slice::from_ref(&external),
        )
        .unwrap()
        .unwrap();
        assert!(recovered.id.starts_with("external-recovery-"));

        let verify = EventLog::new(&event_path).unwrap().verify_chain().unwrap();
        assert!(verify.0);
    }

    #[test]
    fn backup_export_and_import_work() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        let target = dir.path().join("target");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("identity.json"), "{}").unwrap();

        let archive = dir.path().join("backup.tar.gz");
        export_backup(&source, &archive).unwrap();
        import_backup(&archive, &target).unwrap();

        assert!(target.join("identity.json").exists());
    }

    #[test]
    fn migration_updates_schema_version() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("schema.version"), "0.1.1").unwrap();

        let report = migrate_data_dir(dir.path(), "0.2.0").unwrap();
        assert_eq!(report.to_version, "0.2.0");
        assert!(dir.path().join("policy").exists());
    }
}
