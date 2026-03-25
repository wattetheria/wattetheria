//! SQLite-backed local storage for wattetheria agent state.
//!
//! Phase 1 stores each domain module as a JSON blob keyed by a domain name.
//! This replaces the per-module JSON file pattern (`load_or_new` / `persist`)
//! with a single `.wattetheria/state.db` file.
//!
//! `Connection` is wrapped in `std::sync::Mutex` so that `LocalDb` is both
//! `Send` and `Sync`, which is required because `ControlPlaneState` is shared
//! across axum handlers and tokio tasks.

use anyhow::{Context, Result};
use rusqlite::{Connection, Error::QueryReturnedNoRows, params};
use std::path::Path;
use std::sync::Mutex;

const SCHEMA_VERSION: i64 = 1;

pub struct LocalDb {
    conn: Mutex<Connection>,
}

impl LocalDb {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        if let Some(parent) = path.as_ref().parent() {
            std::fs::create_dir_all(parent).context("create local db directory")?;
        }
        let conn = Connection::open(path.as_ref()).context("open local db")?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;")
            .context("set local db pragmas")?;
        let db = Self {
            conn: Mutex::new(conn),
        };
        db.migrate()?;
        Ok(db)
    }

    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().context("open in-memory local db")?;
        conn.execute_batch("PRAGMA foreign_keys=ON;")
            .context("set local db pragmas")?;
        let db = Self {
            conn: Mutex::new(conn),
        };
        db.migrate()?;
        Ok(db)
    }

    fn conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn.lock().expect("local db mutex poisoned")
    }

    fn migrate(&self) -> Result<()> {
        let conn = self.conn();
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_version (
                version INTEGER NOT NULL
            );",
        )
        .context("create schema_version table")?;

        let current: Option<i64> =
            match conn.query_row("SELECT version FROM schema_version LIMIT 1", [], |row| {
                row.get(0)
            }) {
                Ok(version) => Some(version),
                Err(QueryReturnedNoRows) => None,
                Err(error) => return Err(error).context("read schema version"),
            };

        if current.unwrap_or(0) >= SCHEMA_VERSION {
            return Ok(());
        }

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS domain_state (
                domain TEXT PRIMARY KEY,
                payload TEXT NOT NULL,
                updated_at INTEGER NOT NULL
            );",
        )
        .context("create domain_state table")?;

        if current.is_none() {
            conn.execute(
                "INSERT INTO schema_version (version) VALUES (?1)",
                params![SCHEMA_VERSION],
            )
            .context("insert schema version")?;
        } else {
            conn.execute(
                "UPDATE schema_version SET version = ?1",
                params![SCHEMA_VERSION],
            )
            .context("update schema version")?;
        }

        Ok(())
    }

    pub fn load_domain<T: serde::de::DeserializeOwned>(&self, domain: &str) -> Result<Option<T>> {
        let conn = self.conn();
        let json: String = match conn.query_row(
            "SELECT payload FROM domain_state WHERE domain = ?1",
            params![domain],
            |row| row.get(0),
        ) {
            Ok(json) => json,
            Err(QueryReturnedNoRows) => return Ok(None),
            Err(error) => {
                return Err(error).with_context(|| format!("query domain state: {domain}"));
            }
        };
        let value = serde_json::from_str(&json)
            .with_context(|| format!("deserialize domain state: {domain}"))?;
        Ok(Some(value))
    }

    pub fn load_domain_if_fresh<T: serde::de::DeserializeOwned>(
        &self,
        domain: &str,
        max_age_sec: i64,
    ) -> Result<Option<T>> {
        let conn = self.conn();
        let row: (String, i64) = match conn.query_row(
            "SELECT payload, updated_at FROM domain_state WHERE domain = ?1",
            params![domain],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ) {
            Ok(row) => row,
            Err(QueryReturnedNoRows) => return Ok(None),
            Err(error) => {
                return Err(error).with_context(|| format!("query domain state: {domain}"));
            }
        };
        let age = chrono::Utc::now().timestamp() - row.1;
        if age > max_age_sec {
            return Ok(None);
        }
        let value = serde_json::from_str(&row.0)
            .with_context(|| format!("deserialize domain state: {domain}"))?;
        Ok(Some(value))
    }

    pub fn save_domain<T: serde::Serialize>(&self, domain: &str, value: &T) -> Result<()> {
        let json = serde_json::to_string(value)
            .with_context(|| format!("serialize domain state: {domain}"))?;
        let now = chrono::Utc::now().timestamp();
        self.conn()
            .execute(
                "INSERT INTO domain_state (domain, payload, updated_at)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT (domain) DO UPDATE SET payload = excluded.payload, updated_at = excluded.updated_at",
                params![domain, json, now],
            )
            .with_context(|| format!("upsert domain state: {domain}"))?;
        Ok(())
    }

    pub fn delete_domain(&self, domain: &str) -> Result<()> {
        self.conn()
            .execute(
                "DELETE FROM domain_state WHERE domain = ?1",
                params![domain],
            )
            .with_context(|| format!("delete domain state: {domain}"))?;
        Ok(())
    }

    pub fn list_domains(&self) -> Result<Vec<String>> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare("SELECT domain FROM domain_state ORDER BY domain")
            .context("prepare list domains")?;
        let rows = stmt
            .query_map([], |row| row.get(0))
            .context("query domains")?;
        let mut domains = Vec::new();
        for row in rows {
            domains.push(row.context("read domain row")?);
        }
        Ok(domains)
    }
}

impl std::fmt::Debug for LocalDb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalDb").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
    struct SampleState {
        name: String,
        count: u64,
    }

    #[test]
    fn open_in_memory_and_migrate() {
        let db = LocalDb::open_in_memory().unwrap();
        let version: i64 = db
            .conn()
            .query_row("SELECT version FROM schema_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[test]
    fn save_and_load_domain() {
        let db = LocalDb::open_in_memory().unwrap();
        let state = SampleState {
            name: "test".to_string(),
            count: 42,
        };
        db.save_domain("sample", &state).unwrap();

        let loaded: SampleState = db.load_domain("sample").unwrap().unwrap();
        assert_eq!(loaded, state);
    }

    #[test]
    fn load_missing_domain_returns_none() {
        let db = LocalDb::open_in_memory().unwrap();
        let loaded: Option<SampleState> = db.load_domain("missing").unwrap();
        assert!(loaded.is_none());
    }

    #[test]
    fn save_domain_upserts() {
        let db = LocalDb::open_in_memory().unwrap();
        let state1 = SampleState {
            name: "v1".to_string(),
            count: 1,
        };
        let state2 = SampleState {
            name: "v2".to_string(),
            count: 2,
        };
        db.save_domain("sample", &state1).unwrap();
        db.save_domain("sample", &state2).unwrap();

        let loaded: SampleState = db.load_domain("sample").unwrap().unwrap();
        assert_eq!(loaded, state2);
    }

    #[test]
    fn delete_domain_removes_entry() {
        let db = LocalDb::open_in_memory().unwrap();
        let state = SampleState {
            name: "temp".to_string(),
            count: 0,
        };
        db.save_domain("temp", &state).unwrap();
        db.delete_domain("temp").unwrap();

        let loaded: Option<SampleState> = db.load_domain("temp").unwrap();
        assert!(loaded.is_none());
    }

    #[test]
    fn list_domains_returns_sorted() {
        let db = LocalDb::open_in_memory().unwrap();
        db.save_domain("governance", &"g").unwrap();
        db.save_domain("topics", &"t").unwrap();
        db.save_domain("identity", &"i").unwrap();

        let domains = db.list_domains().unwrap();
        assert_eq!(domains, vec!["governance", "identity", "topics"]);
    }

    #[test]
    fn open_file_based_db() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join("state.db");

        let db = LocalDb::open(&db_path).unwrap();
        db.save_domain("test", &"hello").unwrap();
        drop(db);

        let db2 = LocalDb::open(&db_path).unwrap();
        let loaded: String = db2.load_domain("test").unwrap().unwrap();
        assert_eq!(loaded, "hello");
    }

    #[test]
    fn migrate_is_idempotent() {
        let db = LocalDb::open_in_memory().unwrap();
        db.migrate().unwrap();
        db.migrate().unwrap();

        let version: i64 = db
            .conn()
            .query_row("SELECT version FROM schema_version", [], |row| row.get(0))
            .unwrap();
        assert_eq!(version, SCHEMA_VERSION);
    }

    #[test]
    fn local_db_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<LocalDb>();
    }
}
