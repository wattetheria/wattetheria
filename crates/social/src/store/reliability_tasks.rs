use super::{SocialStore, row_to_friend_request};
use crate::domain::friend_requests::FriendRequest;
use crate::domain::reliability_tasks::ReliabilityTask;
use crate::types::{SocialError, SocialResult};
use rusqlite::{OptionalExtension, params};

impl SocialStore {
    pub fn due_outbound_pending_friend_requests(
        &self,
        now: i64,
        min_retry_delay_sec: i64,
        limit: usize,
    ) -> SocialResult<Vec<FriendRequest>> {
        let conn = self.conn()?;
        let normalized_updated_at = "CASE
            WHEN friend_requests.updated_at > 10000000000 THEN friend_requests.updated_at / 1000
            ELSE friend_requests.updated_at
        END";
        let mut stmt = conn
            .prepare(&format!(
                "SELECT friend_requests.request_id,
                        friend_requests.local_public_id,
                        friend_requests.remote_public_id,
                        friend_requests.remote_node_id,
                        friend_requests.direction,
                        friend_requests.state,
                        friend_requests.decision_reason,
                        friend_requests.correlation_id,
                        friend_requests.created_at,
                        friend_requests.updated_at,
                        friend_requests.expires_at
                 FROM friend_requests
                 LEFT JOIN reliability_tasks
                    ON reliability_tasks.object_kind = 'friend_request'
                   AND reliability_tasks.object_id = friend_requests.request_id
                 LEFT JOIN friendships
                    ON friendships.local_public_id = friend_requests.local_public_id
                   AND friendships.remote_public_id = friend_requests.remote_public_id
                   AND friendships.state = 'active'
                 WHERE friend_requests.direction = 'outbound'
                   AND friend_requests.state = 'pending'
                   AND friend_requests.remote_node_id IS NOT NULL
                   AND trim(friend_requests.remote_node_id) <> ''
                   AND friendships.friendship_id IS NULL
                   AND COALESCE(reliability_tasks.status, 'pending') = 'pending'
                   AND COALESCE(
                        reliability_tasks.next_attempt_at,
                        ({normalized_updated_at}) + ?1
                   ) <= ?2
                   AND NOT EXISTS (
                        SELECT 1
                        FROM friend_requests newer
                        WHERE newer.local_public_id = friend_requests.local_public_id
                          AND newer.remote_public_id = friend_requests.remote_public_id
                          AND newer.direction = friend_requests.direction
                          AND newer.state = friend_requests.state
                          AND (
                            newer.updated_at > friend_requests.updated_at
                            OR (
                                newer.updated_at = friend_requests.updated_at
                                AND newer.request_id < friend_requests.request_id
                            )
                          )
                   )
                 ORDER BY COALESCE(
                        reliability_tasks.next_attempt_at,
                        ({normalized_updated_at}) + ?1
                    ) ASC,
                    friend_requests.updated_at ASC,
                    friend_requests.request_id ASC
                 LIMIT ?3"
            ))
            .map_err(|error| {
                SocialError::Storage(format!("prepare due friend request query: {error}"))
            })?;
        let rows = stmt
            .query_map(
                params![min_retry_delay_sec, now, limit as i64],
                row_to_friend_request,
            )
            .map_err(|error| SocialError::Storage(format!("query due friend requests: {error}")))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| SocialError::Storage(format!("collect due friend requests: {error}")))
    }

    pub fn get_reliability_task(
        &self,
        object_kind: &str,
        object_id: &str,
    ) -> SocialResult<Option<ReliabilityTask>> {
        let conn = self.conn()?;
        conn.query_row(
            "SELECT object_kind, object_id, status, attempt_count, last_attempt_at,
                    next_attempt_at, last_error, created_at, updated_at
             FROM reliability_tasks
             WHERE object_kind = ?1 AND object_id = ?2",
            params![object_kind, object_id],
            row_to_reliability_task,
        )
        .optional()
        .map_err(|error| SocialError::Storage(format!("get reliability task: {error}")))
    }

    pub fn defer_reliability_task(
        &self,
        object_kind: &str,
        object_id: &str,
        now: i64,
        next_attempt_at: i64,
        last_error: Option<&str>,
    ) -> SocialResult<()> {
        self.conn()?
            .execute(
                "INSERT INTO reliability_tasks (
                    object_kind, object_id, status, attempt_count, last_attempt_at,
                    next_attempt_at, last_error, created_at, updated_at
                 ) VALUES (?1, ?2, 'pending', 0, NULL, ?3, ?4, ?5, ?5)
                 ON CONFLICT(object_kind, object_id) DO UPDATE SET
                    status = 'pending',
                    next_attempt_at = excluded.next_attempt_at,
                    last_error = excluded.last_error,
                    updated_at = excluded.updated_at",
                params![object_kind, object_id, next_attempt_at, last_error, now],
            )
            .map_err(|error| SocialError::Storage(format!("defer reliability task: {error}")))?;
        Ok(())
    }

    pub fn record_reliability_attempt(
        &self,
        object_kind: &str,
        object_id: &str,
        now: i64,
        next_attempt_at: i64,
        last_error: Option<&str>,
    ) -> SocialResult<()> {
        self.conn()?
            .execute(
                "INSERT INTO reliability_tasks (
                    object_kind, object_id, status, attempt_count, last_attempt_at,
                    next_attempt_at, last_error, created_at, updated_at
                 ) VALUES (?1, ?2, 'pending', 1, ?3, ?4, ?5, ?3, ?3)
                 ON CONFLICT(object_kind, object_id) DO UPDATE SET
                    status = 'pending',
                    attempt_count = reliability_tasks.attempt_count + 1,
                    last_attempt_at = excluded.last_attempt_at,
                    next_attempt_at = excluded.next_attempt_at,
                    last_error = excluded.last_error,
                    updated_at = excluded.updated_at",
                params![object_kind, object_id, now, next_attempt_at, last_error],
            )
            .map_err(|error| {
                SocialError::Storage(format!("record reliability attempt: {error}"))
            })?;
        Ok(())
    }
}

fn row_to_reliability_task(row: &rusqlite::Row<'_>) -> rusqlite::Result<ReliabilityTask> {
    Ok(ReliabilityTask {
        object_kind: row.get(0)?,
        object_id: row.get(1)?,
        status: row.get(2)?,
        attempt_count: row.get(3)?,
        last_attempt_at: row.get(4)?,
        next_attempt_at: row.get(5)?,
        last_error: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
    })
}
