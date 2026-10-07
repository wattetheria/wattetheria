use std::path::Path;
use std::sync::Mutex;

use anyhow::{Result, bail};
use rusqlite::{Connection, OptionalExtension, Row, TransactionBehavior, params};
use uuid::Uuid;

#[cfg(test)]
use super::store::{DeliveryDiagnostic, DeliveryErrorCategory, DeliveryStatus, StoreDiagnostics};
use super::store::{
    DeliveryResult, StoredOccurrence, StoredOutbox, StoredSubscription, SubscriptionDiagnostic,
};

const SUB_COLUMNS: &str = "id, source_kind, event_type, target_agent_id, credential_fingerprint, owner_id, callback_url_enc, callback_secret_enc, old_callback_secret_enc, old_secret_expires_at, canonical_arguments_enc, expires_at, active";
const OCCURRENCE_COLUMNS: &str = "(id TEXT PRIMARY KEY, event_id TEXT NOT NULL UNIQUE,
    owner_id TEXT NOT NULL, kind TEXT NOT NULL, private_content_id TEXT UNIQUE,
    private_content_enc BLOB, expires_at INTEGER NOT NULL)";

pub(crate) struct SqliteBackend {
    conn: Mutex<Connection>,
}

impl SqliteBackend {
    pub fn open(path: &Path, key_id: &str) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut conn = Connection::open(path)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        // The v4 migration replaces the occurrence table while preserving its outbox references.
        conn.pragma_update(None, "foreign_keys", "OFF")?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS mcp_event_meta (
                id INTEGER PRIMARY KEY CHECK (id = 1), version INTEGER NOT NULL,
                key_id TEXT NOT NULL
            );",
        )?;
        let current: Option<(i64, String)> = tx
            .query_row(
                "SELECT version, key_id FROM mcp_event_meta WHERE id = 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if current.as_ref().is_some_and(|(_, key)| key != key_id) {
            bail!("MCP event master key mismatch; restore the key file paired with this database");
        }
        if current.is_none() {
            tx.execute(
                "INSERT INTO mcp_event_meta (id, version, key_id) VALUES (1, 0, ?1)",
                [key_id],
            )?;
        }
        let version = current.map_or(0, |row| row.0);
        if version > 4 {
            bail!("MCP event SQLite schema is newer than this binary");
        }
        if version == 0 {
            tx.execute_batch(
                "CREATE TABLE mcp_event_subscriptions (
                id TEXT PRIMARY KEY, source_kind TEXT NOT NULL, event_type TEXT NOT NULL,
                target_agent_id TEXT NOT NULL, credential_fingerprint TEXT NOT NULL,
                owner_id TEXT NOT NULL, callback_url_enc BLOB NOT NULL,
                callback_secret_enc BLOB NOT NULL, old_callback_secret_enc BLOB,
                old_secret_expires_at INTEGER, canonical_arguments_enc BLOB NOT NULL,
                expires_at INTEGER, active INTEGER NOT NULL CHECK (active IN (0,1))
            );",
            )?;
            tx.execute_batch(&format!(
                "CREATE TABLE mcp_event_occurrences {OCCURRENCE_COLUMNS};"
            ))?;
            tx.execute_batch("CREATE TABLE mcp_event_outbox (
                id TEXT PRIMARY KEY,
                occurrence_id TEXT NOT NULL REFERENCES mcp_event_occurrences(id) ON DELETE CASCADE,
                subscription_id TEXT NOT NULL REFERENCES mcp_event_subscriptions(id) ON DELETE CASCADE,
                payload_enc BLOB NOT NULL, status TEXT NOT NULL DEFAULT 'Pending',
                attempts INTEGER NOT NULL DEFAULT 0, lease_token TEXT, lease_until INTEGER,
                available_at INTEGER NOT NULL DEFAULT 0, expires_at INTEGER NOT NULL,
                created_at INTEGER NOT NULL DEFAULT 0, last_http_status INTEGER,
                last_error_category TEXT, last_attempt_at INTEGER,
                UNIQUE (occurrence_id, subscription_id)
            );")?;
        } else if version < 4 {
            if version < 3 {
                tx.execute_batch(
                    "ALTER TABLE mcp_event_outbox ADD COLUMN created_at INTEGER NOT NULL DEFAULT 0;
                    ALTER TABLE mcp_event_outbox ADD COLUMN last_http_status INTEGER;
                    ALTER TABLE mcp_event_outbox ADD COLUMN last_error_category TEXT;
                    ALTER TABLE mcp_event_outbox ADD COLUMN last_attempt_at INTEGER;",
                )?;
            }
            tx.execute_batch(&format!(
                "CREATE TABLE mcp_event_occurrences_v4 {OCCURRENCE_COLUMNS};
                INSERT INTO mcp_event_occurrences_v4
                    (id,event_id,owner_id,kind,private_content_id,private_content_enc,expires_at)
                SELECT o.id,o.event_id,r.target_agent_id,o.kind,
                    o.private_content_id,o.private_content_enc,o.expires_at
                FROM mcp_event_occurrences o JOIN mcp_event_receipts r ON r.id = o.receipt_id;
                DROP TABLE mcp_event_occurrences;
                ALTER TABLE mcp_event_occurrences_v4 RENAME TO mcp_event_occurrences;
                DROP TABLE mcp_event_receipts;"
            ))?;
        }
        tx.execute_batch(
            "CREATE INDEX IF NOT EXISTS mcp_event_outbox_claim_idx
                ON mcp_event_outbox (status, available_at, lease_until);
            CREATE INDEX IF NOT EXISTS mcp_event_subscription_match_idx
                ON mcp_event_subscriptions (event_type, target_agent_id, active);
            CREATE INDEX IF NOT EXISTS mcp_event_subscription_owner_idx
                ON mcp_event_subscriptions (owner_id);
            CREATE INDEX IF NOT EXISTS mcp_event_expiry_idx
                ON mcp_event_occurrences (expires_at);
            CREATE INDEX IF NOT EXISTS mcp_event_outbox_recent_idx
                ON mcp_event_outbox (created_at DESC);",
        )?;
        tx.execute("UPDATE mcp_event_meta SET version = 4 WHERE id = 1", [])?;
        tx.commit()?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }
}

impl SqliteBackend {
    pub(crate) fn upsert_subscription(&self, s: &StoredSubscription) -> Result<()> {
        let changed = self.conn.lock().unwrap().execute("INSERT INTO mcp_event_subscriptions
            (id,source_kind,event_type,target_agent_id,credential_fingerprint,owner_id,callback_url_enc,callback_secret_enc,old_callback_secret_enc,old_secret_expires_at,canonical_arguments_enc,expires_at,active)
            VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)
            ON CONFLICT(id) DO UPDATE SET source_kind=excluded.source_kind,event_type=excluded.event_type,
              target_agent_id=excluded.target_agent_id,credential_fingerprint=excluded.credential_fingerprint,
              owner_id=excluded.owner_id,callback_url_enc=excluded.callback_url_enc,
              callback_secret_enc=excluded.callback_secret_enc,old_callback_secret_enc=excluded.old_callback_secret_enc,
              old_secret_expires_at=excluded.old_secret_expires_at,canonical_arguments_enc=excluded.canonical_arguments_enc,
              expires_at=excluded.expires_at,active=excluded.active
            WHERE mcp_event_subscriptions.owner_id = excluded.owner_id
              AND mcp_event_subscriptions.target_agent_id = excluded.target_agent_id",
            params![s.id,s.source_kind,s.event_type,s.target_agent_id,s.credential_fingerprint,s.owner_id,
                s.callback_url_enc,s.callback_secret_enc,s.old_callback_secret_enc,s.old_secret_expires_at,
                s.canonical_arguments_enc,s.expires_at,s.active])?;
        require_transition(changed, "upsert subscription")
    }

    pub(crate) fn get_subscription(&self, id: &str) -> Result<Option<StoredSubscription>> {
        let conn = self.conn.lock().unwrap();
        let sql = format!("SELECT {SUB_COLUMNS} FROM mcp_event_subscriptions WHERE id = ?1");
        Ok(conn
            .query_row(&sql, [id], |r| subscription(r, 0))
            .optional()?)
    }

    pub(crate) fn deactivate_subscription(&self, id: &str, owner_id: &str) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE mcp_event_subscriptions SET active = 0
            WHERE id = ?1 AND owner_id = ?2",
            params![id, owner_id],
        )?;
        Ok(())
    }

    pub(crate) fn revoke_credentials_except(
        &self,
        owner_id: &str,
        fingerprint: &str,
    ) -> Result<u64> {
        Ok(self.conn.lock().unwrap().execute(
            "UPDATE mcp_event_subscriptions SET active = 0
             WHERE owner_id = ?1 AND active = 1 AND credential_fingerprint <> ?2",
            params![owner_id, fingerprint],
        )? as u64)
    }

    pub(crate) fn active_subscriptions(
        &self,
        owner_id: &str,
        now: i64,
    ) -> Result<Vec<SubscriptionDiagnostic>> {
        active_subscription_diagnostics(&self.conn.lock().unwrap(), owner_id, now)
    }

    pub(crate) fn publish(
        &self,
        occurrence: &StoredOccurrence,
        payload_enc: &[u8],
        event_type: &str,
        now: i64,
    ) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let subscriptions = {
            let mut stmt = tx.prepare(&format!(
                "SELECT {SUB_COLUMNS}
                FROM mcp_event_subscriptions
                WHERE active = 1 AND (expires_at IS NULL OR expires_at > ?1)
                    AND owner_id = ?2 AND target_agent_id = ?2
                    AND (event_type = '*' OR event_type = ?3)"
            ))?;
            stmt.query_map(params![now, occurrence.owner_id, event_type], |r| {
                subscription(r, 0)
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?
        };
        if subscriptions.is_empty() {
            return Ok(());
        }
        let inserted = tx.execute(
            "INSERT INTO mcp_event_occurrences
                (id,event_id,owner_id,kind,private_content_id,private_content_enc,expires_at)
                VALUES (?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(event_id) DO NOTHING",
            params![
                occurrence.id,
                occurrence.event_id,
                occurrence.owner_id,
                occurrence.kind,
                occurrence.private_content_id,
                occurrence.private_content_enc,
                occurrence.expires_at
            ],
        )?;
        if inserted == 0 {
            return Ok(());
        }
        for subscription in subscriptions {
            tx.execute(
                "INSERT INTO mcp_event_outbox
                    (id,occurrence_id,subscription_id,payload_enc,expires_at,created_at)
                    VALUES (?1,?2,?3,?4,?5,?6)",
                params![
                    Uuid::new_v4().to_string(),
                    occurrence.id,
                    subscription.id,
                    payload_enc,
                    occurrence.expires_at,
                    now
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn claim_outbox(
        &self,
        owner_id: &str,
        fingerprint: &str,
        now: i64,
        lease_until: i64,
        token: &str,
        source_kind: Option<&str>,
    ) -> Result<Option<StoredOutbox>> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let cols = SUB_COLUMNS
            .split(", ")
            .map(|c| format!("s.{c}"))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT o.id,o.occurrence_id,o.payload_enc,o.attempts,p.event_id,{cols}
            FROM mcp_event_outbox o JOIN mcp_event_subscriptions s ON s.id = o.subscription_id
              JOIN mcp_event_occurrences p ON p.id = o.occurrence_id
            WHERE o.status IN ('Pending','Retry','Leased') AND o.available_at <= ?1
              AND (o.lease_until IS NULL OR o.lease_until <= ?1)
              AND o.expires_at > ?1 AND s.active = 1 AND (s.expires_at IS NULL OR s.expires_at > ?1)
              AND s.owner_id = ?2 AND s.credential_fingerprint = ?3
              AND (?4 IS NULL OR s.source_kind = ?4)
            ORDER BY o.available_at,o.id LIMIT 1"
        );
        let selected = tx
            .query_row(
                &sql,
                params![now, owner_id, fingerprint, source_kind],
                |r| {
                    Ok(StoredOutbox {
                        id: r.get(0)?,
                        occurrence_id: r.get(1)?,
                        payload_enc: r.get(2)?,
                        attempts: r.get(3)?,
                        event_id: r.get(4)?,
                        subscription: subscription(r, 5)?,
                        lease_token: None,
                    })
                },
            )
            .optional()?;
        let Some(mut row) = selected else {
            tx.commit()?;
            return Ok(None);
        };
        tx.execute(
            "UPDATE mcp_event_outbox SET status = 'Leased', lease_token = ?1,
            lease_until = ?2, attempts = attempts + 1, last_attempt_at = ?3 WHERE id = ?4",
            params![token, lease_until, now, row.id],
        )?;
        row.lease_token = Some(token.to_owned());
        row.attempts += 1;
        tx.commit()?;
        Ok(Some(row))
    }

    pub(crate) fn cancel_subscription(&self, id: &str, owner_id: &str, now: i64) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        cancel_subscription_in_transaction(&tx, id, owner_id, now)?;
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn cancel_subscriptions_by_source_kind(
        &self,
        source_kind: &str,
        now: i64,
    ) -> Result<Vec<String>> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let subscriptions = tx
            .prepare("SELECT id, owner_id FROM mcp_event_subscriptions WHERE source_kind = ?1")?
            .query_map([source_kind], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (id, owner_id) in &subscriptions {
            cancel_subscription_in_transaction(&tx, id, owner_id, now)?;
        }
        tx.commit()?;
        Ok(subscriptions.into_iter().map(|(id, _)| id).collect())
    }

    pub(crate) fn complete_delivery(
        &self,
        id: &str,
        token: &str,
        result: DeliveryResult,
        max_attempts: i32,
    ) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let attempts: Option<i32> = tx
            .query_row(
                "SELECT attempts FROM mcp_event_outbox
            WHERE id = ?1 AND lease_token = ?2 AND status = 'Leased'",
                params![id, token],
                |r| r.get(0),
            )
            .optional()?;
        let Some(attempts) = attempts else {
            bail!("MCP event state transition rejected: complete delivery")
        };
        let (status, http_status, category, next_at, at) = result.fields(attempts, max_attempts);
        let changed = tx.execute(
            "UPDATE mcp_event_outbox SET status = ?1, available_at = ?2,
            last_http_status = ?3, last_error_category = ?4, last_attempt_at = ?5,
            lease_token = NULL, lease_until = NULL
            WHERE id = ?6 AND lease_token = ?7 AND status = 'Leased'",
            params![status, next_at, http_status, category, at, id, token],
        )?;
        require_transition(changed, "complete delivery")?;
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn cleanup(&self, now: i64) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute("DELETE FROM mcp_event_outbox WHERE expires_at <= ?1", [now])?;
        tx.execute(
            "DELETE FROM mcp_event_occurrences WHERE expires_at <= ?1",
            [now],
        )?;
        tx.execute(
            "DELETE FROM mcp_event_subscriptions WHERE expires_at <= ?1",
            [now],
        )?;
        tx.commit()?;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn diagnostics(&self, owner_id: &str, now: i64) -> Result<StoreDiagnostics> {
        let conn = self.conn.lock().unwrap();
        let active_subscriptions: i64 = conn.query_row(
            "SELECT COUNT(*) FROM mcp_event_subscriptions
            WHERE owner_id = ?1 AND active = 1 AND (expires_at IS NULL OR expires_at > ?2)",
            params![owner_id, now],
            |r| r.get(0),
        )?;
        let (pending_outbox, dead_letters): (i64, i64) = conn.query_row(
            "SELECT
            COALESCE(SUM(CASE WHEN b.status IN ('Pending','Retry','Leased') THEN 1 ELSE 0 END),0),
            COALESCE(SUM(CASE WHEN b.status = 'DeadLetter' THEN 1 ELSE 0 END),0)
            FROM mcp_event_outbox b JOIN mcp_event_subscriptions s ON s.id = b.subscription_id
            WHERE s.owner_id = ?1 AND b.expires_at > ?2",
            params![owner_id, now],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let subscriptions = active_subscription_diagnostics(&conn, owner_id, now)?;
        let mut delivery_stmt = conn.prepare("SELECT b.id,p.event_id,b.subscription_id,b.status,
            b.last_http_status,b.last_error_category,b.attempts,b.available_at,b.lease_until,b.last_attempt_at
            FROM mcp_event_outbox b JOIN mcp_event_subscriptions s ON s.id = b.subscription_id
              JOIN mcp_event_occurrences p ON p.id = b.occurrence_id
            WHERE s.owner_id = ?1 AND b.expires_at > ?2
            ORDER BY COALESCE(b.last_attempt_at,b.created_at) DESC,b.id DESC LIMIT 50")?;
        let raw = delivery_stmt
            .query_map(params![owner_id, now], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, Option<i32>>(4)?,
                    r.get::<_, Option<String>>(5)?,
                    r.get::<_, i32>(6)?,
                    r.get::<_, i64>(7)?,
                    r.get::<_, Option<i64>>(8)?,
                    r.get::<_, Option<i64>>(9)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let recent_deliveries = raw
            .into_iter()
            .map(
                |(
                    id,
                    event_id,
                    subscription_id,
                    raw_status,
                    http,
                    error,
                    attempts,
                    available,
                    lease,
                    last,
                )| {
                    let status = DeliveryStatus::parse(&raw_status)?;
                    let next_attempt_at = match status {
                        DeliveryStatus::Retry => Some(available),
                        DeliveryStatus::Leased => lease,
                        DeliveryStatus::Pending if available > 0 => Some(available),
                        _ => None,
                    };
                    Ok(DeliveryDiagnostic {
                        id,
                        event_id,
                        subscription_id,
                        status,
                        http_status: http.map(u16::try_from).transpose()?,
                        error_category: error
                            .as_deref()
                            .map(DeliveryErrorCategory::parse)
                            .transpose()?,
                        retry_count: attempts.cast_unsigned().saturating_sub(1),
                        next_attempt_at,
                        last_attempt_at: last,
                        terminal: status.is_terminal(),
                    })
                },
            )
            .collect::<Result<Vec<_>>>()?;
        Ok(StoreDiagnostics {
            active_subscriptions: active_subscriptions.cast_unsigned(),
            pending_outbox: pending_outbox.cast_unsigned(),
            dead_letters: dead_letters.cast_unsigned(),
            subscriptions,
            recent_deliveries,
        })
    }
}

fn active_subscription_diagnostics(
    conn: &Connection,
    owner_id: &str,
    now: i64,
) -> Result<Vec<SubscriptionDiagnostic>> {
    let mut stmt = conn.prepare(
        "SELECT id,event_type,target_agent_id,expires_at
        FROM mcp_event_subscriptions WHERE owner_id = ?1 AND active = 1
          AND (expires_at IS NULL OR expires_at > ?2) ORDER BY id",
    )?;
    let subscriptions = stmt
        .query_map(params![owner_id, now], |r| {
            Ok(SubscriptionDiagnostic {
                id: r.get(0)?,
                event_type: r.get(1)?,
                target_agent_id: r.get(2)?,
                expires_at: r.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(subscriptions)
}

fn require_transition(changed: usize, action: &str) -> Result<()> {
    if changed != 1 {
        bail!("MCP event state transition rejected: {action}");
    }
    Ok(())
}

fn cancel_subscription_in_transaction(
    tx: &rusqlite::Transaction<'_>,
    id: &str,
    owner_id: &str,
    now: i64,
) -> Result<()> {
    tx.execute(
        "UPDATE mcp_event_subscriptions SET active = 0 WHERE id = ?1 AND owner_id = ?2",
        params![id, owner_id],
    )?;
    tx.execute(
        "UPDATE mcp_event_outbox SET expires_at = MIN(expires_at, ?3)
        WHERE subscription_id = ?1 AND EXISTS (
            SELECT 1 FROM mcp_event_subscriptions WHERE id = ?1 AND owner_id = ?2)",
        params![id, owner_id, now],
    )?;
    Ok(())
}

fn subscription(r: &Row<'_>, offset: usize) -> rusqlite::Result<StoredSubscription> {
    Ok(StoredSubscription {
        id: r.get(offset)?,
        source_kind: r.get(offset + 1)?,
        event_type: r.get(offset + 2)?,
        target_agent_id: r.get(offset + 3)?,
        credential_fingerprint: r.get(offset + 4)?,
        owner_id: r.get(offset + 5)?,
        callback_url_enc: r.get(offset + 6)?,
        callback_secret_enc: r.get(offset + 7)?,
        old_callback_secret_enc: r.get(offset + 8)?,
        old_secret_expires_at: r.get(offset + 9)?,
        canonical_arguments_enc: r.get(offset + 10)?,
        expires_at: r.get(offset + 11)?,
        active: r.get(offset + 12)?,
    })
}
