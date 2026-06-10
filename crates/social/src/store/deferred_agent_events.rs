use super::SocialStore;
use crate::domain::deferred_agent_events::DeferredAgentEvent;
use crate::types::{SocialError, SocialResult};
use rusqlite::{OptionalExtension, params};

impl SocialStore {
    pub fn defer_agent_event(&self, event: &DeferredAgentEvent) -> SocialResult<()> {
        let event_json = serde_json::to_string(&event.event_json)
            .map_err(|error| SocialError::Storage(format!("serialize deferred event: {error}")))?;
        self.conn()?
            .execute(
                "INSERT OR IGNORE INTO deferred_agent_events (
                    event_id, local_public_id, remote_public_id, remote_node_id,
                    source_agent_id, status, event_json, reason, created_at,
                    updated_at, replayed_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    event.event_id,
                    event.local_public_id,
                    event.remote_public_id,
                    event.remote_node_id,
                    event.source_agent_id,
                    event.status,
                    event_json,
                    event.reason,
                    event.created_at,
                    event.updated_at,
                    event.replayed_at,
                ],
            )
            .map_err(|error| SocialError::Storage(format!("defer agent event: {error}")))?;
        Ok(())
    }

    pub fn list_waiting_deferred_agent_events(
        &self,
        local_public_id: &str,
        remote_public_id: &str,
        limit: usize,
    ) -> SocialResult<Vec<DeferredAgentEvent>> {
        let conn = self.conn()?;
        let mut stmt = conn
            .prepare(
                "SELECT event_id, local_public_id, remote_public_id, remote_node_id,
                        source_agent_id, status, event_json, reason, created_at,
                        updated_at, replayed_at
                 FROM deferred_agent_events
                 WHERE status = 'waiting_for_friendship'
                   AND local_public_id = ?1
                   AND remote_public_id = ?2
                 ORDER BY created_at ASC, event_id ASC
                 LIMIT ?3",
            )
            .map_err(|error| {
                SocialError::Storage(format!("prepare deferred event query: {error}"))
            })?;
        let rows = stmt
            .query_map(
                params![local_public_id, remote_public_id, limit as i64],
                row_to_deferred_agent_event,
            )
            .map_err(|error| SocialError::Storage(format!("query deferred events: {error}")))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| SocialError::Storage(format!("collect deferred events: {error}")))
    }

    pub fn get_deferred_agent_event(
        &self,
        event_id: &str,
    ) -> SocialResult<Option<DeferredAgentEvent>> {
        self.conn()?
            .query_row(
                "SELECT event_id, local_public_id, remote_public_id, remote_node_id,
                        source_agent_id, status, event_json, reason, created_at,
                        updated_at, replayed_at
                 FROM deferred_agent_events
                 WHERE event_id = ?1",
                params![event_id],
                row_to_deferred_agent_event,
            )
            .optional()
            .map_err(|error| SocialError::Storage(format!("get deferred event: {error}")))
    }

    pub fn mark_deferred_agent_event_replayed(
        &self,
        event_id: &str,
        replayed_at: i64,
    ) -> SocialResult<()> {
        self.conn()?
            .execute(
                "UPDATE deferred_agent_events
                 SET status = 'replayed', replayed_at = ?2, updated_at = ?2
                 WHERE event_id = ?1 AND status = 'waiting_for_friendship'",
                params![event_id, replayed_at],
            )
            .map_err(|error| {
                SocialError::Storage(format!("mark deferred event replayed: {error}"))
            })?;
        Ok(())
    }
}

fn row_to_deferred_agent_event(row: &rusqlite::Row<'_>) -> rusqlite::Result<DeferredAgentEvent> {
    let event_json = row.get::<_, String>(6)?;
    Ok(DeferredAgentEvent {
        event_id: row.get(0)?,
        local_public_id: row.get(1)?,
        remote_public_id: row.get(2)?,
        remote_node_id: row.get(3)?,
        source_agent_id: row.get(4)?,
        status: row.get(5)?,
        event_json: serde_json::from_str(&event_json).map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                6,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?,
        reason: row.get(7)?,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
        replayed_at: row.get(10)?,
    })
}
