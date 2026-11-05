//! Until when this agent is held where it stands. A game command that takes time (working a
//! farm plot, say) sets it, and `move_agent` refuses to walk until it passes. Commands and walks
//! take turns through its mutex, so a walk cannot slip past a command still in flight; a command
//! raises the time before it leaves, so a lost reply still holds the agent; and the time is
//! saved, so a restart does too.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use tokio::sync::{Mutex, MutexGuard};
use wattetheria_kernel::local_db::LocalDb;

const DOMAIN: &str = "busy_until_ms";

#[derive(Default)]
pub struct Busy {
    until_ms: AtomicI64,
    restored: AtomicBool,
    pub(crate) turn: Mutex<()>,
}

impl Busy {
    pub(crate) fn until_ms(&self, db: &LocalDb) -> i64 {
        if !self.restored.swap(true, Ordering::AcqRel)
            && let Ok(Some(saved)) = db.load_domain::<i64>(DOMAIN)
        {
            self.until_ms.fetch_max(saved, Ordering::AcqRel);
        }
        self.until_ms.load(Ordering::Acquire)
    }

    pub(crate) fn set(&self, db: &LocalDb, until_ms: i64) {
        self.until_ms(db);
        self.until_ms.store(until_ms, Ordering::Release);
        if let Err(error) = db.save_domain(DOMAIN, &until_ms) {
            tracing::warn!(error = %format!("{error:#}"), "failed to save the busy time");
        }
    }

    /// A walk's turn: waits for any command in flight, then refuses while the agent is still
    /// held. Hold the guard until the walk is signed.
    pub(crate) async fn walk_turn(&self, db: &LocalDb) -> Result<MutexGuard<'_, ()>, Response> {
        let turn = self.turn.lock().await;
        let until = self.until_ms(db);
        let remaining_ms = until.saturating_sub(chrono::Utc::now().timestamp_millis());
        if remaining_ms <= 0 {
            return Ok(turn);
        }
        let remaining_secs = (remaining_ms + 999) / 1000;
        let busy_until = chrono::DateTime::from_timestamp_millis(until)
            .map(|at| at.to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
        Err((
            StatusCode::CONFLICT,
            Json(json!({
                "error": "busy",
                "message": format!("still busy for {remaining_secs} s; wait before walking away"),
                "details": {"busy_until": busy_until, "remaining_secs": remaining_secs},
            })),
        )
            .into_response())
    }
}
