use crate::types::{SocialError, SocialResult};
use rusqlite::{Connection, OptionalExtension};

const SCHEMA_VERSION: i64 = 2;

pub fn migrate(conn: &Connection) -> SocialResult<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_version (
            version INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS public_identities (
            public_id TEXT PRIMARY KEY,
            agent_did TEXT NOT NULL,
            display_name TEXT NOT NULL,
            description TEXT,
            capabilities_json TEXT NOT NULL DEFAULT '[]',
            skills_json TEXT NOT NULL DEFAULT '[]',
            did_document_json TEXT,
            active INTEGER NOT NULL,
            last_profile_fetched_at INTEGER,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS public_transport_bindings (
            public_id TEXT NOT NULL,
            transport_node_id TEXT NOT NULL,
            transport_kind TEXT NOT NULL,
            agent_did TEXT,
            binding_source TEXT NOT NULL,
            binding_confidence INTEGER NOT NULL,
            binding_proof_json TEXT,
            binding_verified INTEGER NOT NULL,
            binding_verified_at INTEGER,
            updated_at INTEGER NOT NULL,
            PRIMARY KEY(public_id, transport_node_id)
        );

        CREATE TABLE IF NOT EXISTS friend_requests (
            request_id TEXT PRIMARY KEY,
            local_public_id TEXT NOT NULL,
            remote_public_id TEXT NOT NULL,
            remote_node_id TEXT,
            direction TEXT NOT NULL,
            state TEXT NOT NULL,
            decision_reason TEXT,
            correlation_id TEXT,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            expires_at INTEGER
        );

        CREATE TABLE IF NOT EXISTS friendships (
            friendship_id TEXT PRIMARY KEY,
            local_public_id TEXT NOT NULL,
            remote_public_id TEXT NOT NULL,
            state TEXT NOT NULL,
            established_from_request_id TEXT,
            thread_id TEXT,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS public_blocks (
            block_id TEXT PRIMARY KEY,
            owner_public_id TEXT NOT NULL,
            blocked_public_id TEXT NOT NULL,
            blocked_node_id TEXT,
            reason TEXT,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS dm_threads (
            thread_id TEXT PRIMARY KEY,
            local_public_id TEXT NOT NULL,
            remote_public_id TEXT NOT NULL,
            transport_thread_id TEXT NOT NULL,
            state TEXT NOT NULL,
            last_message_at INTEGER,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        );

        CREATE TABLE IF NOT EXISTS dm_messages (
            thread_id TEXT NOT NULL,
            message_id TEXT NOT NULL,
            transport_message_id TEXT,
            local_public_id TEXT NOT NULL,
            remote_public_id TEXT NOT NULL,
            direction TEXT NOT NULL,
            message_kind TEXT NOT NULL,
            content_json TEXT NOT NULL,
            encrypted_body TEXT,
            content_encoding TEXT,
            agent_envelope_json TEXT,
            agent_signature TEXT,
            delivery_state TEXT NOT NULL,
            read_state TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            PRIMARY KEY(thread_id, message_id)
        );

        CREATE INDEX IF NOT EXISTS idx_dm_messages_thread_created
            ON dm_messages(thread_id, created_at ASC, message_id ASC);

        CREATE TABLE IF NOT EXISTS dm_message_receipts (
            message_id TEXT NOT NULL,
            receipt_kind TEXT NOT NULL,
            recorded_at INTEGER NOT NULL,
            detail TEXT,
            PRIMARY KEY(message_id, receipt_kind, recorded_at)
        );

        CREATE TABLE IF NOT EXISTS policy_rules (
            rule_id TEXT PRIMARY KEY,
            owner_public_id TEXT,
            rule_type TEXT NOT NULL,
            scope TEXT NOT NULL,
            matcher_json TEXT NOT NULL DEFAULT '{}',
            config_json TEXT NOT NULL,
            priority INTEGER NOT NULL,
            enabled INTEGER NOT NULL,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_policy_rules_owner_priority
            ON policy_rules(owner_public_id, priority ASC, rule_id ASC);

        CREATE TABLE IF NOT EXISTS policy_decision_logs (
            decision_id TEXT PRIMARY KEY,
            owner_public_id TEXT NOT NULL,
            scope TEXT NOT NULL,
            target_public_id TEXT NOT NULL,
            target_node_id TEXT,
            rule_id TEXT,
            decision TEXT NOT NULL,
            reason TEXT NOT NULL,
            context_json TEXT NOT NULL,
            created_at INTEGER NOT NULL
        );

        CREATE INDEX IF NOT EXISTS idx_policy_decision_logs_owner_created
            ON policy_decision_logs(owner_public_id, created_at DESC, decision_id ASC);",
    )
    .map_err(|error| SocialError::Storage(format!("migrate sqlite schema: {error}")))?;

    let version: Option<i64> = conn
        .query_row("SELECT version FROM schema_version LIMIT 1", [], |row| {
            row.get(0)
        })
        .optional()
        .map_err(|error| SocialError::Storage(format!("read schema version: {error}")))?;

    let has_matcher_json = {
        let mut stmt = conn
            .prepare("PRAGMA table_info(policy_rules)")
            .map_err(|error| {
                SocialError::Storage(format!("prepare table_info(policy_rules): {error}"))
            })?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .map_err(|error| {
                SocialError::Storage(format!("query table_info(policy_rules): {error}"))
            })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| {
                SocialError::Storage(format!("collect table_info(policy_rules): {error}"))
            })?
            .into_iter()
            .any(|name| name == "matcher_json")
    };
    if !has_matcher_json {
        conn.execute(
            "ALTER TABLE policy_rules ADD COLUMN matcher_json TEXT NOT NULL DEFAULT '{}'",
            [],
        )
        .map_err(|error| SocialError::Storage(format!("add matcher_json column: {error}")))?;
    }

    if version.is_none() {
        conn.execute(
            "INSERT INTO schema_version (version) VALUES (?1)",
            [SCHEMA_VERSION],
        )
        .map_err(|error| SocialError::Storage(format!("insert schema version: {error}")))?;
    } else {
        conn.execute("UPDATE schema_version SET version = ?1", [SCHEMA_VERSION])
            .map_err(|error| SocialError::Storage(format!("update schema version: {error}")))?;
    }

    Ok(())
}
