use super::*;

fn exercise(config: &Path, key_path: &Path) {
    let store = EventStore::open(config, key_path).unwrap();
    store
        .publish(occurrence("unsubscribed", "absent-owner", b"ignored"), 1000)
        .unwrap();
    assert_eq!(
        store
            .diagnostics("absent-owner", 1000)
            .unwrap()
            .pending_outbox,
        0
    );
    exercise_subscription(&store);
    exercise_publication(&store);
    exercise_retry_delivery(&store);
    exercise_no_action_deadletter(&store);
    exercise_credentials(&store);
    drop(store);
    let reopened = EventStore::open(config, key_path).unwrap();
    assert!(
        !reopened
            .get_subscription("subscription-1")
            .unwrap()
            .unwrap()
            .active
    );
    assert!(EventStore::open(config, &key_path.with_extension("other-key")).is_err());
}

fn occurrence(event_id: &str, owner: &str, body: &[u8]) -> NewOccurrence {
    NewOccurrence {
        event_id: event_id.into(),
        event_type: "task.finalized".into(),
        owner_id: owner.into(),
        kind: "task.finalized".into(),
        event_body: body.to_vec(),
        private_content_id: None,
        private_content: None,
        expires_at: 4_000_000_000,
    }
}

fn exercise_subscription(store: &EventStore) {
    let subscription = NewSubscription {
        id: "subscription-1".into(),
        source_kind: "catalog-source".into(),
        event_type: "task.finalized".into(),
        target_agent_id: "owner-1".into(),
        credential_fingerprint: "credential-1".into(),
        owner_id: "owner-1".into(),
        callback_url: "https://callback.example/events".into(),
        callback_secret: "current-secret".into(),
        old_callback_secret: Some("old-secret".into()),
        old_secret_expires_at: Some(1200),
        canonical_arguments: br#"{"eventType":"task.finalized"}"#.to_vec(),
        expires_at: None,
    };
    store.upsert_subscription(subscription).unwrap();
    let saved = store.get_subscription("subscription-1").unwrap().unwrap();
    assert_eq!(saved.callback_secret, "current-secret");
    assert_eq!(saved.old_callback_secret.as_deref(), Some("old-secret"));
    let diagnostics = store.diagnostics("owner-1", 1000).unwrap();
    assert_eq!(diagnostics.active_subscriptions, 1);
    assert_eq!(diagnostics.subscriptions[0].expires_at, None);
    assert_eq!(
        store
            .diagnostics("other-owner", 1000)
            .unwrap()
            .active_subscriptions,
        0
    );
}

fn exercise_publication(store: &EventStore) {
    let mut input = occurrence("event-1", "owner-1", b"exact event bytes");
    input.private_content_id = Some("opaque-content-ref".into());
    input.private_content = Some(b"whitelisted private content".to_vec());
    store.publish(input, 1000).unwrap();
    store
        .publish(occurrence("event-1", "owner-1", b"duplicate event"), 1000)
        .unwrap();
    let pending = store.diagnostics("owner-1", 1000).unwrap();
    assert_eq!(pending.pending_outbox, 1);
    let row = pending
        .recent_deliveries
        .iter()
        .find(|row| row.event_id == "event-1")
        .unwrap();
    assert_eq!(row.status, DeliveryStatus::Pending);
    assert_eq!(row.retry_count, 0);
}

fn exercise_retry_delivery(store: &EventStore) {
    let delivery = store
        .claim_outbox("owner-1", "credential-1", 1000, 30, None)
        .unwrap()
        .unwrap();
    assert_eq!(delivery.callback_secret, "current-secret");
    assert_eq!(delivery.payload, b"exact event bytes");
    assert!(
        store
            .claim_outbox("owner-2", "credential-1", 1000, 30, None)
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .claim_outbox("owner-1", "wrong-credential", 1000, 30, None)
            .unwrap()
            .is_none()
    );
    store
        .complete_delivery(
            &delivery,
            DeliveryResult::Retry {
                http_status: Some(503),
                category: DeliveryErrorCategory::HttpRetryable,
                next_at: 1100,
                at: 1010,
            },
            2,
        )
        .unwrap();
    let retry_diag = store.diagnostics("owner-1", 1010).unwrap();
    let retry_row = retry_diag
        .recent_deliveries
        .iter()
        .find(|row| row.event_id == "event-1")
        .unwrap();
    assert_eq!(retry_row.status, DeliveryStatus::Retry);
    assert_eq!(retry_row.http_status, Some(503));
    assert_eq!(
        retry_row.error_category,
        Some(DeliveryErrorCategory::HttpRetryable)
    );
    assert_eq!(retry_row.retry_count, 0);
    assert_eq!(retry_row.next_attempt_at, Some(1100));
    assert!(!format!("{retry_diag:?}").contains("current-secret"));
    assert!(
        store
            .claim_outbox("owner-1", "credential-1", 1099, 30, None)
            .unwrap()
            .is_none()
    );
    let retry = store
        .claim_outbox("owner-1", "credential-1", 1100, 30, None)
        .unwrap()
        .unwrap();
    store
        .complete_delivery(
            &retry,
            DeliveryResult::Delivered {
                http_status: 204,
                at: 1110,
            },
            2,
        )
        .unwrap();
    let delivered = store.diagnostics("owner-1", 1110).unwrap();
    let delivered_row = delivered
        .recent_deliveries
        .iter()
        .find(|row| row.event_id == "event-1")
        .unwrap();
    assert_eq!(delivered_row.status, DeliveryStatus::Delivered);
    assert_eq!(delivered_row.http_status, Some(204));
    assert_eq!(delivered_row.retry_count, 1);
    assert_eq!(delivered_row.next_attempt_at, None);
    assert!(delivered_row.terminal);
    assert!(
        store
            .claim_outbox("owner-1", "credential-1", 1100, 30, None)
            .unwrap()
            .is_none()
    );
}

fn exercise_no_action_deadletter(store: &EventStore) {
    store
        .publish(occurrence("event-2", "owner-1", b"no action event"), 1000)
        .unwrap();
    let counts = store.diagnostics("owner-1", 1000).unwrap();
    assert_eq!(counts.active_subscriptions, 1);
    assert_eq!(counts.pending_outbox, 1);
    let no_action_delivery = store
        .claim_outbox("owner-1", "credential-1", 1000, 30, None)
        .unwrap()
        .unwrap();
    assert_eq!(no_action_delivery.event_id, "event-2");
    store
        .complete_delivery(
            &no_action_delivery,
            DeliveryResult::DeadLetter {
                http_status: Some(410),
                category: DeliveryErrorCategory::HttpPermanent,
                at: 1005,
            },
            2,
        )
        .unwrap();
    assert_eq!(store.diagnostics("owner-1", 1000).unwrap().dead_letters, 1);
    let dead = store.diagnostics("owner-1", 1005).unwrap();
    let dead_row = dead
        .recent_deliveries
        .iter()
        .find(|row| row.event_id == "event-2")
        .unwrap();
    assert_eq!(dead_row.status, DeliveryStatus::DeadLetter);
    assert_eq!(
        dead_row.error_category,
        Some(DeliveryErrorCategory::HttpPermanent)
    );
    assert_eq!(dead_row.http_status, Some(410));
    assert!(dead_row.terminal);
}

fn exercise_credentials(store: &EventStore) {
    store
        .upsert_subscription(NewSubscription {
            id: "subscription-other-owner".into(),
            source_kind: "catalog-source".into(),
            event_type: "task.finalized".into(),
            target_agent_id: "agent-2".into(),
            credential_fingerprint: "credential-2".into(),
            owner_id: "owner-2".into(),
            callback_url: "https://other.example/events".into(),
            callback_secret: "other-secret".into(),
            old_callback_secret: None,
            old_secret_expires_at: None,
            canonical_arguments: b"{}".to_vec(),
            expires_at: None,
        })
        .unwrap();
    assert_eq!(
        store
            .revoke_credentials_except("owner-1", "credential-2")
            .unwrap(),
        1
    );
    assert!(
        store
            .get_subscription("subscription-other-owner")
            .unwrap()
            .unwrap()
            .active
    );
    store
        .deactivate_subscription("subscription-1", "owner-1")
        .unwrap();
    store
        .deactivate_subscription("subscription-1", "owner-1")
        .unwrap();
    assert!(
        store
            .claim_outbox("owner-1", "credential-1", 1000, 30, None)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        store
            .revoke_credentials_except("owner-1", "credential-2")
            .unwrap(),
        0
    );
    assert!(
        store
            .get_subscription("subscription-other-owner")
            .unwrap()
            .unwrap()
            .active
    );
}

fn downgrade_occurrences_to_v3(conn: &rusqlite::Connection) {
    conn.execute_batch(
        "PRAGMA foreign_keys = OFF;
        BEGIN IMMEDIATE;
        CREATE TABLE mcp_event_receipts (
            id TEXT PRIMARY KEY, source_kind TEXT NOT NULL, source_event_id TEXT NOT NULL,
            target_agent_id TEXT NOT NULL, event_type TEXT NOT NULL, digest BLOB NOT NULL,
            payload_enc BLOB NOT NULL, state TEXT NOT NULL, decision_enc BLOB, response_enc BLOB,
            attempts INTEGER NOT NULL DEFAULT 0, lease_token TEXT, lease_until INTEGER,
            available_at INTEGER NOT NULL DEFAULT 0, expires_at INTEGER NOT NULL,
            UNIQUE (source_kind, source_event_id, target_agent_id)
        );
        INSERT INTO mcp_event_receipts
            (id,source_kind,source_event_id,target_agent_id,event_type,digest,payload_enc,state,expires_at)
        SELECT id,'task_lifecycle',event_id,owner_id,'task.finalized',X'00',X'00','Committed',expires_at
        FROM mcp_event_occurrences;
        CREATE TABLE mcp_event_occurrences_v3 (
            id TEXT PRIMARY KEY,
            receipt_id TEXT NOT NULL UNIQUE REFERENCES mcp_event_receipts(id) ON DELETE CASCADE,
            event_id TEXT NOT NULL UNIQUE, kind TEXT NOT NULL, private_content_id TEXT UNIQUE,
            private_content_enc BLOB, expires_at INTEGER NOT NULL
        );
        INSERT INTO mcp_event_occurrences_v3
        SELECT id,id,event_id,kind,private_content_id,private_content_enc,expires_at
        FROM mcp_event_occurrences;
        DROP TABLE mcp_event_occurrences;
        ALTER TABLE mcp_event_occurrences_v3 RENAME TO mcp_event_occurrences;
        CREATE INDEX mcp_event_receipt_claim_idx
            ON mcp_event_receipts (state,available_at,lease_until);
        UPDATE mcp_event_meta SET version = 3 WHERE id = 1;
        COMMIT;"
    ).unwrap();
}

const DOWNGRADE_OUTBOX_TO_V2: &str = "DROP INDEX mcp_event_outbox_recent_idx;
    ALTER TABLE mcp_event_outbox DROP COLUMN created_at;
    ALTER TABLE mcp_event_outbox DROP COLUMN last_http_status;
    ALTER TABLE mcp_event_outbox DROP COLUMN last_error_category;
    ALTER TABLE mcp_event_outbox DROP COLUMN last_attempt_at;
    UPDATE mcp_event_meta SET version = 2 WHERE id = 1;";

fn assert_upgrade_preserved(store: &EventStore) {
    let diagnostics = store.diagnostics("owner-1", 1000).unwrap();
    let delivery = diagnostics
        .recent_deliveries
        .iter()
        .find(|row| row.event_id == "event-1")
        .unwrap();
    assert_eq!(delivery.status, DeliveryStatus::Delivered);
    assert_eq!(delivery.retry_count, 1);
}

#[test]
fn sqlite_publication_outbox_and_schema_upgrades() {
    use wattetheria_kernel::storage::local_db::{self, LocalDb, domain::POLICY};
    use wattetheria_social::store::SocialStore;

    for version in [1, 2, 3] {
        let dir = tempfile::tempdir().unwrap();
        let db = local_db::primary_db_path(dir.path());
        let local = LocalDb::open(&db).unwrap();
        let _social = SocialStore::open(&db).unwrap();
        let policy = serde_json::json!({"existing_policy": true});
        local.save_domain(POLICY, &policy).unwrap();
        let versions_sql = "SELECT (SELECT version FROM schema_version),
            (SELECT version FROM social_schema_version)";
        let versions: (i64, i64) = rusqlite::Connection::open(&db)
            .unwrap()
            .query_row(versions_sql, [], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap();
        let key = dir.path().join("master.key");
        exercise(&db, &key);
        let mut bytes = std::fs::read(&db).unwrap();
        bytes.extend(std::fs::read(db.with_extension("db-wal")).unwrap());
        for secret in [
            b"current-secret".as_slice(),
            b"old-secret",
            b"whitelisted private content",
            b"exact event bytes",
        ] {
            assert!(!bytes.windows(secret.len()).any(|window| window == secret));
        }
        let conn = rusqlite::Connection::open(&db).unwrap();
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name = 'mcp_event_receipts'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        downgrade_occurrences_to_v3(&conn);
        if version < 3 {
            conn.execute_batch(DOWNGRADE_OUTBOX_TO_V2).unwrap();
        }
        if version == 1 {
            conn.execute_batch(
                "DROP INDEX mcp_event_receipt_claim_idx;
                UPDATE mcp_event_meta SET version = 1 WHERE id = 1;",
            )
            .unwrap();
        }
        let before = outbox_snapshot(&conn);
        drop(conn);
        let restored = EventStore::open(&db, &key).unwrap();
        assert_upgrade_preserved(&restored);
        let conn = rusqlite::Connection::open(&db).unwrap();
        assert_eq!(
            conn.query_row(
                "SELECT version FROM mcp_event_meta WHERE id = 1",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            4
        );
        assert_eq!(
            conn.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name = 'mcp_event_receipts'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        assert_eq!(outbox_snapshot(&conn), before);
        assert_eq!(
            conn.query_row(versions_sql, [], |row| Ok((row.get(0)?, row.get(1)?)))
                .unwrap(),
            versions
        );
        assert_eq!(
            local.load_domain::<serde_json::Value>(POLICY).unwrap(),
            Some(policy)
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap(),
            0
        );
        drop(conn);
        restored.cleanup(4_000_000_000).unwrap();
        assert_eq!(
            restored
                .diagnostics("owner-1", 1000)
                .unwrap()
                .recent_deliveries,
            []
        );
    }
}

fn outbox_snapshot(conn: &rusqlite::Connection) -> Vec<(String, String, Vec<u8>)> {
    conn.prepare("SELECT id,occurrence_id,payload_enc FROM mcp_event_outbox ORDER BY id")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}
