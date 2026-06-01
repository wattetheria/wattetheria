//! SQLite-backed local storage for wattetheria agent state.
//!
//! Domain modules are stored in dedicated `SQLite` tables inside `wattetheria.db`.
//! This replaces the earlier generic `domain_state` blob table and the
//! per-module JSON file pattern (`load_or_new` / `persist`).
//!
//! `Connection` is wrapped in `std::sync::Mutex` so that `LocalDb` is both
//! `Send` and `Sync`, which is required because `ControlPlaneState` is shared
//! across axum handlers and tokio tasks.

use anyhow::{Context, Result};
use rusqlite::{Connection, Error::QueryReturnedNoRows, params};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const SCHEMA_VERSION: i64 = 4;
pub const PRIMARY_DB_FILE: &str = "wattetheria.db";
pub const LEGACY_SOCIAL_DB_FILE: &str = "social.db";

pub fn primary_db_path(data_dir: impl AsRef<Path>) -> PathBuf {
    data_dir.as_ref().join(PRIMARY_DB_FILE)
}

pub fn legacy_social_db_path(data_dir: impl AsRef<Path>) -> PathBuf {
    data_dir.as_ref().join(LEGACY_SOCIAL_DB_FILE)
}

pub fn prepare_primary_db(data_dir: impl AsRef<Path>) -> Result<PathBuf> {
    let data_dir = data_dir.as_ref();
    std::fs::create_dir_all(data_dir).context("create local db directory")?;
    Ok(primary_db_path(data_dir))
}

pub mod domain {
    pub const GOVERNANCE: &str = "governance";
    pub const MAILBOX: &str = "mailbox";
    pub const MISSION_BOARD: &str = "mission_board";
    pub const PUBLIC_IDENTITY_REGISTRY: &str = "public_identity_registry";
    pub const CONTROLLER_BINDING_REGISTRY: &str = "controller_binding_registry";
    pub const CITIZEN_REGISTRY: &str = "citizen_registry";
    pub const RELATIONSHIP_REGISTRY: &str = "relationship_registry";
    pub const ORGANIZATION_REGISTRY: &str = "organization_registry";
    pub const HIVE_REGISTRY: &str = "hive_registry";
    pub const PAYMENT_LEDGER: &str = "payment_ledger";
    pub const GALAXY_STATE: &str = "galaxy_state";
    pub const GALAXY_MAP_REGISTRY: &str = "galaxy_map_registry";
    pub const TRAVEL_STATE_REGISTRY: &str = "travel_state_registry";
    pub const ORACLE_REGISTRY: &str = "oracle_registry";
    pub const ONLINE_PROOF: &str = "online_proof";
    pub const POLICY: &str = "policy";
    pub const ECONOMIC_POLICY: &str = "economic_policy";
    pub const WATT_BALANCE_STATE: &str = "watt_balance_state";
    pub const CONTRIBUTION_EVENT_LOG: &str = "contribution_event_log";
    pub const COLLECTIVE_MISSION_RUNS: &str = "collective_mission_runs";
    pub const NETWORK_MISSION_CLAIMS: &str = "network_mission_claims";
}

const DOMAIN_TABLES: &[(&str, &str)] = &[
    (domain::GOVERNANCE, "governance_state"),
    (domain::MAILBOX, "mailbox_state"),
    (domain::MISSION_BOARD, "mission_board"),
    (
        domain::PUBLIC_IDENTITY_REGISTRY,
        "public_identity_registry_state",
    ),
    (
        domain::CONTROLLER_BINDING_REGISTRY,
        "controller_binding_registry_state",
    ),
    (domain::CITIZEN_REGISTRY, "citizen_registry_state"),
    (domain::RELATIONSHIP_REGISTRY, "relationship_registry_state"),
    (domain::ORGANIZATION_REGISTRY, "organization_registry_state"),
    (domain::HIVE_REGISTRY, "hive_registry"),
    (domain::PAYMENT_LEDGER, "payment_ledger_state"),
    (domain::GALAXY_STATE, "galaxy_state"),
    (domain::GALAXY_MAP_REGISTRY, "galaxy_map_registry"),
    (domain::TRAVEL_STATE_REGISTRY, "travel_state_registry"),
    (domain::ORACLE_REGISTRY, "oracle_registry_state"),
    (domain::ONLINE_PROOF, "online_proof_state"),
    (domain::POLICY, "policy_state"),
    (domain::ECONOMIC_POLICY, "economic_policy_state"),
    (domain::WATT_BALANCE_STATE, "watt_balance_state"),
    (domain::CONTRIBUTION_EVENT_LOG, "contribution_event_log"),
    (domain::COLLECTIVE_MISSION_RUNS, "collective_mission_runs"),
    (domain::NETWORK_MISSION_CLAIMS, "network_mission_claims"),
];

fn domain_table_name(domain: &str) -> String {
    DOMAIN_TABLES
        .iter()
        .find_map(|(key, table)| (*key == domain).then_some((*table).to_owned()))
        .unwrap_or_else(|| format!("domain_{}", hex_encode(domain.as_bytes())))
}

fn domain_from_table_name(table: &str) -> Option<String> {
    DOMAIN_TABLES
        .iter()
        .find_map(|(domain, known_table)| (*known_table == table).then_some((*domain).to_owned()))
        .or_else(|| {
            table
                .strip_prefix("domain_")
                .and_then(hex_decode)
                .and_then(|bytes| String::from_utf8(bytes).ok())
        })
}

fn quote_ident(identifier: &str) -> String {
    debug_assert!(
        identifier
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    );
    format!("\"{identifier}\"")
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn hex_decode(value: &str) -> Option<Vec<u8>> {
    if !value.len().is_multiple_of(2) {
        return None;
    }
    let mut bytes = Vec::with_capacity(value.len() / 2);
    let chars = value.as_bytes();
    for chunk in chars.chunks_exact(2) {
        let high = hex_value(chunk[0])?;
        let low = hex_value(chunk[1])?;
        bytes.push((high << 4) | low);
    }
    Some(bytes)
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentActionCommitLogEntry {
    pub commit_id: String,
    pub event_id: String,
    pub decision_id: String,
    pub action_type: String,
    pub domain: String,
    pub target_id: Option<String>,
    pub expected_state: Option<String>,
    pub result_state: Option<String>,
    pub request_json: String,
    pub result_json: String,
    pub status: String,
    pub actor_public_id: Option<String>,
    pub actor_agent_did: Option<String>,
    pub created_at: String,
}

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

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS agent_action_commit_log (
                commit_id TEXT PRIMARY KEY,
                event_id TEXT NOT NULL,
                decision_id TEXT NOT NULL,
                action_type TEXT NOT NULL,
                domain TEXT NOT NULL,
                target_id TEXT,
                expected_state TEXT,
                result_state TEXT,
                request_json TEXT NOT NULL,
                result_json TEXT NOT NULL,
                status TEXT NOT NULL,
                actor_public_id TEXT,
                actor_agent_did TEXT,
                created_at TEXT NOT NULL
            );
            CREATE UNIQUE INDEX IF NOT EXISTS idx_agent_action_commit_log_event_decision_action
                ON agent_action_commit_log(event_id, decision_id, action_type);",
        )
        .context("create agent_action_commit_log table")?;

        for (_, table) in DOMAIN_TABLES {
            Self::create_domain_table(&conn, table)?;
        }

        if current.unwrap_or(0) < SCHEMA_VERSION {
            conn.execute_batch("DROP TABLE IF EXISTS domain_state;")
                .context("drop legacy domain_state table")?;
        }

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
        let table = domain_table_name(domain);
        if !Self::table_exists(&conn, &table)? {
            return Ok(None);
        }
        let json: String = match conn.query_row(
            &format!("SELECT payload FROM {} WHERE id = 1", quote_ident(&table)),
            [],
            |row| row.get(0),
        ) {
            Ok(json) => json,
            Err(QueryReturnedNoRows) => return Ok(None),
            Err(error) => {
                return Err(error).with_context(|| format!("query domain table: {domain}"));
            }
        };
        let value = serde_json::from_str(&json)
            .with_context(|| format!("deserialize domain table: {domain}"))?;
        Ok(Some(value))
    }

    pub fn load_domain_if_fresh<T: serde::de::DeserializeOwned>(
        &self,
        domain: &str,
        max_age_sec: i64,
    ) -> Result<Option<T>> {
        let conn = self.conn();
        let table = domain_table_name(domain);
        if !Self::table_exists(&conn, &table)? {
            return Ok(None);
        }
        let row: (String, String) = match conn.query_row(
            &format!(
                "SELECT payload, updated_at FROM {} WHERE id = 1",
                quote_ident(&table)
            ),
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        ) {
            Ok(row) => row,
            Err(QueryReturnedNoRows) => return Ok(None),
            Err(error) => {
                return Err(error).with_context(|| format!("query domain table: {domain}"));
            }
        };
        let updated = chrono::NaiveDateTime::parse_from_str(&row.1, "%Y-%m-%dT%H:%M:%SZ")
            .with_context(|| format!("parse updated_at for {domain}"))?
            .and_utc()
            .timestamp();
        let age = chrono::Utc::now().timestamp() - updated;
        if age > max_age_sec {
            return Ok(None);
        }
        let value = serde_json::from_str(&row.0)
            .with_context(|| format!("deserialize domain table: {domain}"))?;
        Ok(Some(value))
    }

    pub fn load_domain_or_default<T: serde::de::DeserializeOwned + Default>(
        &self,
        domain: &str,
    ) -> Result<T> {
        Ok(self.load_domain(domain)?.unwrap_or_default())
    }

    pub fn save_domain<T: serde::Serialize>(&self, domain: &str, value: &T) -> Result<()> {
        let json = serde_json::to_string(value)
            .with_context(|| format!("serialize domain table: {domain}"))?;
        let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
        let table = domain_table_name(domain);
        let conn = self.conn();
        Self::create_domain_table(&conn, &table)?;
        conn.execute(
            &format!(
                "INSERT INTO {} (id, payload, updated_at)
                 VALUES (1, ?1, ?2)
                 ON CONFLICT (id) DO UPDATE SET payload = excluded.payload, updated_at = excluded.updated_at",
                quote_ident(&table)
            ),
            params![json, now],
        )
        .with_context(|| format!("upsert domain table: {domain}"))?;
        Ok(())
    }

    pub fn delete_domain(&self, domain: &str) -> Result<()> {
        let table = domain_table_name(domain);
        self.conn()
            .execute(&format!("DROP TABLE IF EXISTS {}", quote_ident(&table)), [])
            .with_context(|| format!("drop domain table: {domain}"))?;
        Ok(())
    }

    pub fn load_agent_action_commit(
        &self,
        event_id: &str,
        decision_id: &str,
        action_type: &str,
    ) -> Result<Option<AgentActionCommitLogEntry>> {
        let conn = self.conn();
        let row = conn.query_row(
            "SELECT commit_id,
                    event_id,
                    decision_id,
                    action_type,
                    domain,
                    target_id,
                    expected_state,
                    result_state,
                    request_json,
                    result_json,
                    status,
                    actor_public_id,
                    actor_agent_did,
                    created_at
             FROM agent_action_commit_log
             WHERE event_id = ?1 AND decision_id = ?2 AND action_type = ?3
             LIMIT 1",
            params![event_id, decision_id, action_type],
            |row| {
                Ok(AgentActionCommitLogEntry {
                    commit_id: row.get(0)?,
                    event_id: row.get(1)?,
                    decision_id: row.get(2)?,
                    action_type: row.get(3)?,
                    domain: row.get(4)?,
                    target_id: row.get(5)?,
                    expected_state: row.get(6)?,
                    result_state: row.get(7)?,
                    request_json: row.get(8)?,
                    result_json: row.get(9)?,
                    status: row.get(10)?,
                    actor_public_id: row.get(11)?,
                    actor_agent_did: row.get(12)?,
                    created_at: row.get(13)?,
                })
            },
        );
        match row {
            Ok(entry) => Ok(Some(entry)),
            Err(QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(error).context("query agent action commit log"),
        }
    }

    pub fn append_agent_action_commit(&self, entry: &AgentActionCommitLogEntry) -> Result<()> {
        self.conn()
            .execute(
                "INSERT INTO agent_action_commit_log (
                    commit_id,
                    event_id,
                    decision_id,
                    action_type,
                    domain,
                    target_id,
                    expected_state,
                    result_state,
                    request_json,
                    result_json,
                    status,
                    actor_public_id,
                    actor_agent_did,
                    created_at
                 )
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
                params![
                    entry.commit_id,
                    entry.event_id,
                    entry.decision_id,
                    entry.action_type,
                    entry.domain,
                    entry.target_id,
                    entry.expected_state,
                    entry.result_state,
                    entry.request_json,
                    entry.result_json,
                    entry.status,
                    entry.actor_public_id,
                    entry.actor_agent_did,
                    entry.created_at,
                ],
            )
            .context("insert agent action commit log")?;
        Ok(())
    }

    pub fn load_or_migrate<T>(&self, domain_key: &str, json_path: &std::path::Path) -> Result<T>
    where
        T: serde::de::DeserializeOwned + serde::Serialize + Default,
    {
        if let Some(value) = self.load_domain::<T>(domain_key)? {
            return Ok(value);
        }
        let value = if json_path.exists() {
            let raw = std::fs::read_to_string(json_path).context("read legacy json")?;
            if raw.trim().is_empty() {
                T::default()
            } else {
                serde_json::from_str(&raw)
                    .with_context(|| format!("parse legacy json for {domain_key}"))?
            }
        } else {
            T::default()
        };
        self.save_domain(domain_key, &value)?;
        Ok(value)
    }

    pub fn list_domains(&self) -> Result<Vec<String>> {
        let conn = self.conn();
        let mut stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
            .context("prepare list domain tables")?;
        let rows = stmt
            .query_map([], |row| row.get(0))
            .context("query domain tables")?;
        let mut domains = Vec::new();
        for row in rows {
            let table: String = row.context("read domain table row")?;
            let Some(domain) = domain_from_table_name(&table) else {
                continue;
            };
            let has_payload: i64 = conn
                .query_row(
                    &format!(
                        "SELECT EXISTS(SELECT 1 FROM {} WHERE id = 1)",
                        quote_ident(&table)
                    ),
                    [],
                    |row| row.get(0),
                )
                .with_context(|| format!("query domain table payload: {domain}"))?;
            if has_payload != 0 {
                domains.push(domain);
            }
        }
        domains.sort();
        Ok(domains)
    }

    fn create_domain_table(conn: &Connection, table: &str) -> Result<()> {
        conn.execute_batch(&format!(
            "CREATE TABLE IF NOT EXISTS {} (
                id INTEGER PRIMARY KEY CHECK(id = 1),
                payload TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );",
            quote_ident(table)
        ))
        .with_context(|| format!("create domain table: {table}"))
    }

    fn table_exists(conn: &Connection, table: &str) -> Result<bool> {
        let exists: i64 = conn
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1
                )",
                params![table],
                |row| row.get(0),
            )
            .with_context(|| format!("check domain table exists: {table}"))?;
        Ok(exists != 0)
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
    fn migrate_does_not_create_domain_state() {
        let db = LocalDb::open_in_memory().unwrap();
        let domain_state_exists: i64 = db
            .conn()
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'domain_state'
                )",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(domain_state_exists, 0);
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
    fn save_domain_uses_dedicated_table() {
        let db = LocalDb::open_in_memory().unwrap();
        db.save_domain(domain::HIVE_REGISTRY, &"hives").unwrap();

        let saved: String = db
            .conn()
            .query_row(
                "SELECT payload FROM hive_registry WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(saved, "\"hives\"");
    }

    #[test]
    fn open_file_based_db() {
        let dir = tempfile::tempdir().unwrap();
        let db_path = dir.path().join(PRIMARY_DB_FILE);

        let db = LocalDb::open(&db_path).unwrap();
        db.save_domain("test", &"hello").unwrap();
        drop(db);

        let db2 = LocalDb::open(&db_path).unwrap();
        let loaded: String = db2.load_domain("test").unwrap().unwrap();
        assert_eq!(loaded, "hello");
    }

    #[test]
    fn prepare_primary_db_ignores_legacy_state() {
        let dir = tempfile::tempdir().unwrap();
        let legacy_path = dir.path().join("state.db");
        let legacy = LocalDb::open(&legacy_path).unwrap();
        legacy.save_domain("legacy", &"value").unwrap();
        drop(legacy);

        let primary_path = prepare_primary_db(dir.path()).unwrap();
        assert_eq!(primary_path, primary_db_path(dir.path()));

        let db = LocalDb::open(&primary_path).unwrap();
        let loaded: Option<String> = db.load_domain("legacy").unwrap();
        assert!(loaded.is_none());
        db.save_domain("primary_only", &"new").unwrap();
        drop(db);

        let primary_path_again = prepare_primary_db(dir.path()).unwrap();
        assert_eq!(primary_path_again, primary_path);
        let db = LocalDb::open(primary_path_again).unwrap();
        let loaded: String = db.load_domain("primary_only").unwrap().unwrap();
        assert_eq!(loaded, "new");
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
    fn agent_action_commit_log_roundtrip() {
        let db = LocalDb::open_in_memory().unwrap();
        let entry = AgentActionCommitLogEntry {
            commit_id: "commit-1".to_string(),
            event_id: "evt-1".to_string(),
            decision_id: "decision-1".to_string(),
            action_type: "payments.authorize".to_string(),
            domain: "payment".to_string(),
            target_id: Some("payment-1".to_string()),
            expected_state: Some("proposed".to_string()),
            result_state: Some("authorized".to_string()),
            request_json: "{\"payment_id\":\"payment-1\"}".to_string(),
            result_json: "{\"ok\":true}".to_string(),
            status: "accepted".to_string(),
            actor_public_id: Some("captain-aurora".to_string()),
            actor_agent_did: Some("did:key:zAgent".to_string()),
            created_at: "2026-01-01T00:00:00Z".to_string(),
        };
        db.append_agent_action_commit(&entry)
            .expect("append commit log");

        let loaded = db
            .load_agent_action_commit("evt-1", "decision-1", "payments.authorize")
            .expect("load commit log")
            .expect("commit exists");
        assert_eq!(loaded, entry);
    }

    #[test]
    fn local_db_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<LocalDb>();
    }
}
