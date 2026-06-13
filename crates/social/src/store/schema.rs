use crate::types::{SocialError, SocialResult};
use rusqlite::{Connection, OptionalExtension};

pub(crate) const SCHEMA_VERSION: i64 = 6;
const SCHEMA_VERSION_TABLE: &str = "social_schema_version";
const CREATE_PUBLIC_IDENTITIES_TABLE: &str = "CREATE TABLE IF NOT EXISTS public_identities (
    public_id TEXT PRIMARY KEY,
    agent_did TEXT NOT NULL,
    display_name TEXT NOT NULL,
    description TEXT,
    capabilities_json TEXT NOT NULL DEFAULT '[]',
    skills_json TEXT NOT NULL DEFAULT '[]',
    did_document_json TEXT,
    identity_state TEXT NOT NULL DEFAULT 'active',
    last_profile_fetched_at INTEGER,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);";

pub fn migrate(conn: &Connection) -> SocialResult<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS social_schema_version (
            version INTEGER NOT NULL
        );",
    )
    .map_err(|error| SocialError::Storage(format!("migrate schema version table: {error}")))?;
    conn.execute_batch(CREATE_PUBLIC_IDENTITIES_TABLE)
        .map_err(|error| {
            SocialError::Storage(format!("migrate public identities table: {error}"))
        })?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS public_transport_bindings (
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

        CREATE TABLE IF NOT EXISTS reliability_tasks (
            object_kind TEXT NOT NULL,
            object_id TEXT NOT NULL,
            status TEXT NOT NULL,
            attempt_count INTEGER NOT NULL DEFAULT 0,
            last_attempt_at INTEGER,
            next_attempt_at INTEGER NOT NULL,
            last_error TEXT,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            PRIMARY KEY(object_kind, object_id)
        );

        CREATE INDEX IF NOT EXISTS idx_reliability_tasks_status_next
            ON reliability_tasks(status, next_attempt_at ASC, object_kind, object_id);

        CREATE TABLE IF NOT EXISTS deferred_agent_events (
            event_id TEXT PRIMARY KEY,
            local_public_id TEXT NOT NULL,
            remote_public_id TEXT NOT NULL,
            remote_node_id TEXT,
            source_agent_id TEXT,
            status TEXT NOT NULL,
            event_json TEXT NOT NULL,
            reason TEXT,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            replayed_at INTEGER
        );

        CREATE INDEX IF NOT EXISTS idx_deferred_agent_events_waiting_pair
            ON deferred_agent_events(status, local_public_id, remote_public_id, created_at);

        CREATE TABLE IF NOT EXISTS friendships (
            friendship_id TEXT PRIMARY KEY,
            local_public_id TEXT NOT NULL,
            remote_public_id TEXT NOT NULL,
            display_name TEXT,
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

    migrate_public_identity_state_column(conn)?;

    let version: Option<i64> = conn
        .query_row(
            &format!("SELECT version FROM {SCHEMA_VERSION_TABLE} LIMIT 1"),
            [],
            |row| row.get(0),
        )
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

    let has_friendship_display_name = {
        let mut stmt = conn
            .prepare("PRAGMA table_info(friendships)")
            .map_err(|error| {
                SocialError::Storage(format!("prepare table_info(friendships): {error}"))
            })?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .map_err(|error| {
                SocialError::Storage(format!("query table_info(friendships): {error}"))
            })?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| {
                SocialError::Storage(format!("collect table_info(friendships): {error}"))
            })?
            .into_iter()
            .any(|name| name == "display_name")
    };
    if !has_friendship_display_name {
        conn.execute("ALTER TABLE friendships ADD COLUMN display_name TEXT", [])
            .map_err(|error| SocialError::Storage(format!("add display_name column: {error}")))?;
    }

    if version.is_none() {
        conn.execute(
            &format!("INSERT INTO {SCHEMA_VERSION_TABLE} (version) VALUES (?1)"),
            [SCHEMA_VERSION],
        )
        .map_err(|error| SocialError::Storage(format!("insert schema version: {error}")))?;
    } else {
        conn.execute(
            &format!("UPDATE {SCHEMA_VERSION_TABLE} SET version = ?1"),
            [SCHEMA_VERSION],
        )
        .map_err(|error| SocialError::Storage(format!("update schema version: {error}")))?;
    }

    Ok(())
}

fn migrate_public_identity_state_column(conn: &Connection) -> SocialResult<()> {
    let columns = table_columns(conn, "public_identities")?;
    let has_active = columns.iter().any(|column| column == "active");
    let has_identity_state = columns.iter().any(|column| column == "identity_state");
    if has_active && !has_identity_state {
        conn.execute_batch(&format!(
            "DROP TABLE IF EXISTS public_identities_legacy_active;
             ALTER TABLE public_identities RENAME TO public_identities_legacy_active;
             {CREATE_PUBLIC_IDENTITIES_TABLE}
             INSERT INTO public_identities (
                public_id, agent_did, display_name, description, capabilities_json, skills_json,
                did_document_json, identity_state, last_profile_fetched_at, created_at, updated_at
             )
             SELECT
                public_id, agent_did, display_name, description, capabilities_json, skills_json,
                did_document_json,
                CASE WHEN active = 0 THEN 'removed' ELSE 'active' END,
                last_profile_fetched_at, created_at, updated_at
             FROM public_identities_legacy_active;
             DROP TABLE public_identities_legacy_active;"
        ))
        .map_err(|error| {
            SocialError::Storage(format!("migrate public identity active state: {error}"))
        })?;
    }
    Ok(())
}

fn table_columns(conn: &Connection, table: &str) -> SocialResult<Vec<String>> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(|error| SocialError::Storage(format!("prepare table_info({table}): {error}")))?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|error| SocialError::Storage(format!("query table_info({table}): {error}")))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|error| SocialError::Storage(format!("collect table_info({table}): {error}")))
}
