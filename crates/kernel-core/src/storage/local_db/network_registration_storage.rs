use super::LocalDb;
use anyhow::{Context, Result};
use rusqlite::{Error::QueryReturnedNoRows, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub const NETWORK_AGENT_CREDENTIALS_TABLE: &str = "network_agent_credentials";
pub const NETWORK_PERMISSION_CHECKPOINTS_TABLE: &str = "network_permission_checkpoints";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct NetworkPermissionCheckpoint {
    pub network_id: String,
    pub node_id: String,
    pub agent_did: String,
    #[serde(alias = "admission_status")]
    pub permission_status: String,
    pub network_status: String,
    pub revision: u64,
    pub last_error: Option<String>,
    pub updated_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkAgentCredentialRecord {
    pub network_id: String,
    pub agent_did: String,
    pub request_id: String,
    pub credential_id: String,
    pub credential_json: String,
    pub trust_anchor_json: Option<String>,
    pub credential_hash: String,
    pub status: String,
    pub issued_at_ms: u64,
    pub credential_expires_at_ms: Option<u64>,
    pub stored_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Debug, Deserialize)]
struct LegacyCredentialState {
    #[serde(default)]
    credentials: Vec<LegacyStoredCredential>,
}

#[derive(Debug, Deserialize)]
struct LegacyStoredCredential {
    network_id: String,
    agent_did: String,
    request_id: String,
    credential: Value,
    status: String,
    stored_at_ms: u64,
}

pub(super) fn migrate(tx: &Transaction<'_>) -> Result<()> {
    migrate_permission_checkpoints(tx)?;
    migrate_agent_credentials(tx)
}

fn migrate_permission_checkpoints(tx: &Transaction<'_>) -> Result<()> {
    create_permission_checkpoints_table(tx, NETWORK_PERMISSION_CHECKPOINTS_TABLE)?;
    LocalDb::rename_table_column_if_needed(
        tx,
        NETWORK_PERMISSION_CHECKPOINTS_TABLE,
        "admission_status",
        "permission_status",
    )?;

    let has_credential_columns = [
        "credential_id",
        "credential_hash",
        "credential_expires_at_ms",
    ]
    .into_iter()
    .map(|column| LocalDb::table_column_exists(tx, NETWORK_PERMISSION_CHECKPOINTS_TABLE, column))
    .collect::<Result<Vec<_>>>()?
    .into_iter()
    .any(|exists| exists);
    if has_credential_columns {
        let replacement = "network_permission_checkpoints_v10";
        tx.execute(&format!("DROP TABLE IF EXISTS {replacement}"), [])
            .context("drop stale network permission checkpoint replacement table")?;
        create_permission_checkpoints_table(tx, replacement)?;
        tx.execute(
            &format!(
                "INSERT INTO {replacement} (
                    network_id, node_id, agent_did, permission_status,
                    network_status, revision, last_error, updated_at_ms
                 )
                 SELECT network_id, node_id, agent_did, permission_status,
                        network_status, revision, last_error, updated_at_ms
                 FROM {NETWORK_PERMISSION_CHECKPOINTS_TABLE}"
            ),
            [],
        )
        .context("copy permission checkpoints without Credential columns")?;
        tx.execute(
            &format!("DROP TABLE {NETWORK_PERMISSION_CHECKPOINTS_TABLE}"),
            [],
        )
        .context("drop legacy permission checkpoint table")?;
        tx.execute(
            &format!("ALTER TABLE {replacement} RENAME TO {NETWORK_PERMISSION_CHECKPOINTS_TABLE}"),
            [],
        )
        .context("install normalized permission checkpoint table")?;
    }
    tx.execute(
        &format!(
            "UPDATE {NETWORK_PERMISSION_CHECKPOINTS_TABLE}
             SET network_status = 'running'
             WHERE network_status = 'starting'"
        ),
        [],
    )
    .context("migrate starting network permission status to running")?;
    tx.execute(
        &format!(
            "CREATE INDEX IF NOT EXISTS idx_network_permission_checkpoints_agent
             ON {NETWORK_PERMISSION_CHECKPOINTS_TABLE}(agent_did, updated_at_ms DESC)"
        ),
        [],
    )
    .context("create network permission checkpoint Agent index")?;
    Ok(())
}

fn create_permission_checkpoints_table(tx: &Transaction<'_>, table: &str) -> Result<()> {
    tx.execute(
        &format!(
            "CREATE TABLE IF NOT EXISTS {table} (
                network_id TEXT NOT NULL,
                node_id TEXT NOT NULL,
                agent_did TEXT NOT NULL,
                permission_status TEXT NOT NULL,
                network_status TEXT NOT NULL,
                revision INTEGER NOT NULL,
                last_error TEXT,
                updated_at_ms INTEGER NOT NULL,
                PRIMARY KEY (network_id, node_id, agent_did)
            )"
        ),
        [],
    )
    .with_context(|| format!("create {table} table"))?;
    Ok(())
}

fn migrate_agent_credentials(tx: &Transaction<'_>) -> Result<()> {
    if !LocalDb::table_exists(tx, NETWORK_AGENT_CREDENTIALS_TABLE)? {
        return create_agent_credentials_table(tx, NETWORK_AGENT_CREDENTIALS_TABLE);
    }
    if !LocalDb::table_column_exists(tx, NETWORK_AGENT_CREDENTIALS_TABLE, "payload")? {
        if !LocalDb::table_column_exists(tx, NETWORK_AGENT_CREDENTIALS_TABLE, "trust_anchor_json")?
        {
            tx.execute(
                &format!(
                    "ALTER TABLE {NETWORK_AGENT_CREDENTIALS_TABLE}
                     ADD COLUMN trust_anchor_json TEXT"
                ),
                [],
            )
            .context("add network Credential trust anchor column")?;
        }
        return create_agent_credentials_indexes(tx);
    }

    let payload = tx
        .query_row(
            &format!("SELECT payload FROM {NETWORK_AGENT_CREDENTIALS_TABLE} WHERE id = 1"),
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .context("load legacy network Agent Credential payload")?;
    let replacement = "network_agent_credentials_v10";
    tx.execute(&format!("DROP TABLE IF EXISTS {replacement}"), [])
        .context("drop stale network Agent Credential replacement table")?;
    create_agent_credentials_table(tx, replacement)?;
    if let Some(payload) = payload {
        let state: LegacyCredentialState = serde_json::from_str(&payload)
            .context("decode legacy network Agent Credential payload")?;
        for stored in state.credentials {
            let record = legacy_credential_record(stored)?;
            insert_agent_credential(tx, replacement, &record)?;
        }
    }
    tx.execute(&format!("DROP TABLE {NETWORK_AGENT_CREDENTIALS_TABLE}"), [])
        .context("drop legacy network Agent Credential table")?;
    tx.execute(
        &format!("ALTER TABLE {replacement} RENAME TO {NETWORK_AGENT_CREDENTIALS_TABLE}"),
        [],
    )
    .context("install normalized network Agent Credential table")?;
    create_agent_credentials_indexes(tx)
}

fn create_agent_credentials_table(tx: &Transaction<'_>, table: &str) -> Result<()> {
    tx.execute(
        &format!(
            "CREATE TABLE IF NOT EXISTS {table} (
                network_id TEXT NOT NULL,
                agent_did TEXT NOT NULL,
                request_id TEXT NOT NULL,
                credential_id TEXT NOT NULL UNIQUE,
                credential_json TEXT NOT NULL,
                trust_anchor_json TEXT,
                credential_hash TEXT NOT NULL,
                status TEXT NOT NULL,
                issued_at_ms INTEGER NOT NULL,
                credential_expires_at_ms INTEGER,
                stored_at_ms INTEGER NOT NULL,
                updated_at_ms INTEGER NOT NULL,
                PRIMARY KEY (network_id, agent_did)
            )"
        ),
        [],
    )
    .with_context(|| format!("create {table} table"))?;
    Ok(())
}

fn create_agent_credentials_indexes(tx: &Transaction<'_>) -> Result<()> {
    tx.execute(
        &format!(
            "CREATE INDEX IF NOT EXISTS idx_network_agent_credentials_status
             ON {NETWORK_AGENT_CREDENTIALS_TABLE}(agent_did, status, updated_at_ms DESC)"
        ),
        [],
    )
    .context("create network Agent Credential status index")?;
    Ok(())
}

fn legacy_credential_record(
    stored: LegacyStoredCredential,
) -> Result<NetworkAgentCredentialRecord> {
    let credential = canonicalize_legacy_credential(stored.credential)?;
    let credential_id = credential_string(&credential, "credential_id")?;
    let issued_at_ms = credential_u64(&credential, "issued_at", "issued_at_ms")?;
    let credential_expires_at_ms =
        credential_optional_u64(&credential, "expires_at", "expires_at_ms")?;
    let credential_json =
        serde_json::to_string(&credential).context("encode migrated network Agent Credential")?;
    let credential_hash = credential_value_hash(&credential)?;
    Ok(NetworkAgentCredentialRecord {
        network_id: stored.network_id,
        agent_did: stored.agent_did,
        request_id: stored.request_id,
        credential_id,
        credential_json,
        trust_anchor_json: None,
        credential_hash,
        status: stored.status,
        issued_at_ms,
        credential_expires_at_ms,
        stored_at_ms: stored.stored_at_ms,
        updated_at_ms: stored.stored_at_ms,
    })
}

fn canonicalize_legacy_credential(mut credential: Value) -> Result<Value> {
    let object = credential
        .as_object_mut()
        .context("legacy Credential must be a JSON object")?;
    for (canonical, alias) in [
        ("issuer_authority_id", "issuer_genesis_id"),
        ("issued_at", "issued_at_ms"),
        ("expires_at", "expires_at_ms"),
    ] {
        if !object.contains_key(canonical)
            && let Some(value) = object.remove(alias)
        {
            object.insert(canonical.to_owned(), value);
        }
    }
    Ok(credential)
}

fn credential_string(credential: &Value, field: &str) -> Result<String> {
    credential
        .get(field)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .with_context(|| format!("legacy Credential is missing {field}"))
}

fn credential_u64(credential: &Value, field: &str, alias: &str) -> Result<u64> {
    credential
        .get(field)
        .or_else(|| credential.get(alias))
        .and_then(Value::as_u64)
        .with_context(|| format!("legacy Credential is missing {field}"))
}

fn credential_optional_u64(credential: &Value, field: &str, alias: &str) -> Result<Option<u64>> {
    let Some(value) = credential.get(field).or_else(|| credential.get(alias)) else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    value
        .as_u64()
        .map(Some)
        .with_context(|| format!("legacy Credential has invalid {field}"))
}

fn credential_value_hash(credential: &Value) -> Result<String> {
    let bytes = serde_jcs::to_vec(credential).context("canonicalize network Agent Credential")?;
    Ok(format!("sha256:{}", hex::encode(Sha256::digest(bytes))))
}

fn insert_agent_credential(
    tx: &Transaction<'_>,
    table: &str,
    record: &NetworkAgentCredentialRecord,
) -> Result<()> {
    tx.execute(
        &format!(
            "INSERT INTO {table} (
                network_id, agent_did, request_id, credential_id,
                credential_json, trust_anchor_json, credential_hash, status, issued_at_ms,
                credential_expires_at_ms, stored_at_ms, updated_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)"
        ),
        params![
            record.network_id,
            record.agent_did,
            record.request_id,
            record.credential_id,
            record.credential_json,
            record.trust_anchor_json,
            record.credential_hash,
            record.status,
            record.issued_at_ms,
            record.credential_expires_at_ms,
            record.stored_at_ms,
            record.updated_at_ms,
        ],
    )
    .context("insert migrated network Agent Credential")?;
    Ok(())
}

impl LocalDb {
    pub fn upsert_network_permission_checkpoint(
        &self,
        checkpoint: &NetworkPermissionCheckpoint,
    ) -> Result<()> {
        self.conn()
            .execute(
                &format!(
                    "INSERT INTO {NETWORK_PERMISSION_CHECKPOINTS_TABLE} (
                        network_id, node_id, agent_did, permission_status,
                        network_status, revision, last_error, updated_at_ms
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                     ON CONFLICT (network_id, node_id, agent_did) DO UPDATE SET
                        permission_status = excluded.permission_status,
                        network_status = excluded.network_status,
                        revision = excluded.revision,
                        last_error = excluded.last_error,
                        updated_at_ms = excluded.updated_at_ms"
                ),
                params![
                    checkpoint.network_id,
                    checkpoint.node_id,
                    checkpoint.agent_did,
                    checkpoint.permission_status,
                    checkpoint.network_status,
                    checkpoint.revision,
                    checkpoint.last_error,
                    checkpoint.updated_at_ms,
                ],
            )
            .context("upsert network permission checkpoint")?;
        Ok(())
    }

    pub fn load_network_permission_checkpoint(
        &self,
        agent_did: &str,
        network_id: Option<&str>,
        node_id: Option<&str>,
    ) -> Result<Option<NetworkPermissionCheckpoint>> {
        let result = self.conn().query_row(
            &format!(
                "SELECT network_id, node_id, agent_did, permission_status,
                        network_status, revision, last_error, updated_at_ms
                 FROM {NETWORK_PERMISSION_CHECKPOINTS_TABLE}
                 WHERE agent_did = ?1
                   AND (?2 IS NULL OR network_id = ?2)
                   AND (?3 IS NULL OR node_id = ?3)
                 ORDER BY updated_at_ms DESC
                 LIMIT 1"
            ),
            params![agent_did, network_id, node_id],
            decode_permission_checkpoint,
        );
        match result {
            Ok(checkpoint) => Ok(Some(checkpoint)),
            Err(QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(error).context("load network permission checkpoint"),
        }
    }

    pub fn upsert_network_agent_credential(
        &self,
        record: &NetworkAgentCredentialRecord,
    ) -> Result<bool> {
        if self
            .load_network_agent_credential(&record.agent_did, Some(&record.network_id))?
            .as_ref()
            .is_some_and(|existing| credential_records_match(existing, record))
        {
            return Ok(false);
        }
        self.conn()
            .execute(
                &format!(
                    "INSERT INTO {NETWORK_AGENT_CREDENTIALS_TABLE} (
                        network_id, agent_did, request_id, credential_id,
                        credential_json, trust_anchor_json, credential_hash, status, issued_at_ms,
                        credential_expires_at_ms, stored_at_ms, updated_at_ms
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
                     ON CONFLICT (network_id, agent_did) DO UPDATE SET
                        request_id = excluded.request_id,
                        credential_id = excluded.credential_id,
                        credential_json = excluded.credential_json,
                        trust_anchor_json = excluded.trust_anchor_json,
                        credential_hash = excluded.credential_hash,
                        status = excluded.status,
                        issued_at_ms = excluded.issued_at_ms,
                        credential_expires_at_ms = excluded.credential_expires_at_ms,
                        stored_at_ms = excluded.stored_at_ms,
                        updated_at_ms = excluded.updated_at_ms"
                ),
                params![
                    record.network_id,
                    record.agent_did,
                    record.request_id,
                    record.credential_id,
                    record.credential_json,
                    record.trust_anchor_json,
                    record.credential_hash,
                    record.status,
                    record.issued_at_ms,
                    record.credential_expires_at_ms,
                    record.stored_at_ms,
                    record.updated_at_ms,
                ],
            )
            .context("upsert network Agent Credential")?;
        Ok(true)
    }

    pub fn load_network_agent_credential(
        &self,
        agent_did: &str,
        network_id: Option<&str>,
    ) -> Result<Option<NetworkAgentCredentialRecord>> {
        let result = self.conn().query_row(
            &format!(
                "SELECT network_id, agent_did, request_id, credential_id,
                        credential_json, trust_anchor_json, credential_hash, status, issued_at_ms,
                        credential_expires_at_ms, stored_at_ms, updated_at_ms
                 FROM {NETWORK_AGENT_CREDENTIALS_TABLE}
                 WHERE agent_did = ?1 AND (?2 IS NULL OR network_id = ?2)
                 ORDER BY updated_at_ms DESC
                 LIMIT 1"
            ),
            params![agent_did, network_id],
            decode_agent_credential,
        );
        match result {
            Ok(record) => Ok(Some(record)),
            Err(QueryReturnedNoRows) => Ok(None),
            Err(error) => Err(error).context("load network Agent Credential"),
        }
    }

    pub fn update_network_agent_credential_status(
        &self,
        network_id: &str,
        agent_did: &str,
        request_id: &str,
        status: &str,
        updated_at_ms: u64,
    ) -> Result<bool> {
        let changed = self
            .conn()
            .execute(
                &format!(
                    "UPDATE {NETWORK_AGENT_CREDENTIALS_TABLE}
                     SET status = ?4, updated_at_ms = ?5
                     WHERE network_id = ?1 AND agent_did = ?2 AND request_id = ?3
                       AND status <> ?4"
                ),
                params![network_id, agent_did, request_id, status, updated_at_ms],
            )
            .context("update network Agent Credential status")?;
        Ok(changed != 0)
    }

    pub fn delete_network_agent_credential(
        &self,
        network_id: &str,
        agent_did: &str,
    ) -> Result<bool> {
        let deleted = self
            .conn()
            .execute(
                &format!(
                    "DELETE FROM {NETWORK_AGENT_CREDENTIALS_TABLE}
                     WHERE network_id = ?1 AND agent_did = ?2"
                ),
                params![network_id, agent_did],
            )
            .context("delete network Agent Credential")?;
        Ok(deleted != 0)
    }
}

fn decode_permission_checkpoint(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<NetworkPermissionCheckpoint> {
    Ok(NetworkPermissionCheckpoint {
        network_id: row.get(0)?,
        node_id: row.get(1)?,
        agent_did: row.get(2)?,
        permission_status: row.get(3)?,
        network_status: row.get(4)?,
        revision: row.get(5)?,
        last_error: row.get(6)?,
        updated_at_ms: row.get(7)?,
    })
}

fn decode_agent_credential(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<NetworkAgentCredentialRecord> {
    Ok(NetworkAgentCredentialRecord {
        network_id: row.get(0)?,
        agent_did: row.get(1)?,
        request_id: row.get(2)?,
        credential_id: row.get(3)?,
        credential_json: row.get(4)?,
        trust_anchor_json: row.get(5)?,
        credential_hash: row.get(6)?,
        status: row.get(7)?,
        issued_at_ms: row.get(8)?,
        credential_expires_at_ms: row.get(9)?,
        stored_at_ms: row.get(10)?,
        updated_at_ms: row.get(11)?,
    })
}

fn credential_records_match(
    current: &NetworkAgentCredentialRecord,
    candidate: &NetworkAgentCredentialRecord,
) -> bool {
    current.network_id == candidate.network_id
        && current.agent_did == candidate.agent_did
        && current.request_id == candidate.request_id
        && current.credential_id == candidate.credential_id
        && current.trust_anchor_json == candidate.trust_anchor_json
        && current.credential_hash == candidate.credential_hash
        && current.status == candidate.status
        && current.issued_at_ms == candidate.issued_at_ms
        && current.credential_expires_at_ms == candidate.credential_expires_at_ms
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    #[test]
    fn migrates_legacy_payload_and_removes_checkpoint_credential_columns() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wattetheria.db");
        let credential = serde_json::json!({
            "version": 1,
            "credential_id": "credential-1",
            "request_id": "request-1",
            "network_id": "network-1",
            "agent_did": "did:key:zAgent",
            "issuer_authority_id": "authority-1",
            "issued_at_ms": 100,
            "expires_at_ms": 200,
            "signing_key_id": "key-1",
            "signature_algorithm": "future-algorithm",
            "algorithm_parameters": {"curve": "future-curve"},
            "signature_hex": "proof"
        });
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE schema_version (version INTEGER NOT NULL);
                 INSERT INTO schema_version(version) VALUES (9);
                 CREATE TABLE network_permission_checkpoints (
                     network_id TEXT NOT NULL,
                     node_id TEXT NOT NULL,
                     agent_did TEXT NOT NULL,
                     permission_status TEXT NOT NULL,
                     network_status TEXT NOT NULL,
                     revision INTEGER NOT NULL,
                     credential_id TEXT,
                     credential_hash TEXT,
                     credential_expires_at_ms INTEGER,
                     last_error TEXT,
                     updated_at_ms INTEGER NOT NULL,
                     PRIMARY KEY (network_id, node_id, agent_did)
                 );
                 CREATE TABLE network_agent_credentials (
                     id INTEGER PRIMARY KEY CHECK(id = 1),
                     payload TEXT NOT NULL,
                     updated_at TEXT NOT NULL
                 );",
            )
            .unwrap();
            let payload = serde_json::json!({
                "credentials": [{
                    "network_id": "network-1",
                    "agent_did": "did:key:zAgent",
                    "request_id": "request-1",
                    "credential": credential,
                    "status": "active",
                    "stored_at_ms": 110
                }]
            });
            conn.execute(
                "INSERT INTO network_agent_credentials(id, payload, updated_at)
                 VALUES (1, ?1, '1970-01-01T00:00:00Z')",
                params![payload.to_string()],
            )
            .unwrap();
        }

        let db = LocalDb::open(&path).unwrap();
        let record = db
            .load_network_agent_credential("did:key:zAgent", Some("network-1"))
            .unwrap()
            .unwrap();
        assert_eq!(record.credential_id, "credential-1");
        assert_eq!(record.credential_expires_at_ms, Some(200));
        assert!(record.credential_hash.starts_with("sha256:"));
        let migrated: Value = serde_json::from_str(&record.credential_json).unwrap();
        assert_eq!(migrated["issuer_authority_id"], "authority-1");
        assert_eq!(migrated["issued_at"], 100);
        assert_eq!(migrated["expires_at"], 200);
        assert_eq!(migrated["signature_algorithm"], "future-algorithm");
        assert_eq!(migrated["algorithm_parameters"]["curve"], "future-curve");

        let conn = db.conn();
        let mut statement = conn
            .prepare("PRAGMA table_info(network_permission_checkpoints)")
            .unwrap();
        let columns = statement
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<rusqlite::Result<Vec<_>>>()
            .unwrap();
        assert!(!columns.iter().any(|column| column == "credential_id"));
        assert!(!columns.iter().any(|column| column == "credential_hash"));
        assert!(
            !columns
                .iter()
                .any(|column| column == "credential_expires_at_ms")
        );
    }

    #[test]
    fn rejects_invalid_legacy_credential_without_dropping_the_source_table() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("wattetheria.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE schema_version (version INTEGER NOT NULL);
                 INSERT INTO schema_version(version) VALUES (9);
                 CREATE TABLE network_agent_credentials (
                     id INTEGER PRIMARY KEY CHECK(id = 1),
                     payload TEXT NOT NULL,
                     updated_at TEXT NOT NULL
                 );
                 INSERT INTO network_agent_credentials(id, payload, updated_at)
                 VALUES (1, '{\"credentials\":[{}]}', '1970-01-01T00:00:00Z');",
            )
            .unwrap();
        }

        assert!(LocalDb::open(&path).is_err());
        let conn = Connection::open(&path).unwrap();
        let payload: String = conn
            .query_row(
                "SELECT payload FROM network_agent_credentials WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(payload, "{\"credentials\":[{}]}");
    }

    #[test]
    fn credential_upsert_reports_only_semantic_changes() {
        let db = LocalDb::open_in_memory().unwrap();
        let mut record = NetworkAgentCredentialRecord {
            network_id: "network-1".to_owned(),
            agent_did: "did:key:zAgent".to_owned(),
            request_id: "request-1".to_owned(),
            credential_id: "credential-1".to_owned(),
            credential_json: "{}".to_owned(),
            trust_anchor_json: Some("{}".to_owned()),
            credential_hash: "sha256:abc".to_owned(),
            status: "active".to_owned(),
            issued_at_ms: 100,
            credential_expires_at_ms: None,
            stored_at_ms: 100,
            updated_at_ms: 100,
        };
        assert!(db.upsert_network_agent_credential(&record).unwrap());
        record.updated_at_ms = 200;
        assert!(!db.upsert_network_agent_credential(&record).unwrap());
        record.status = "disabled".to_owned();
        assert!(db.upsert_network_agent_credential(&record).unwrap());
    }
}
