use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThreadState {
    Pending,
    Ready,
    Closed,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DirectThread {
    pub thread_id: String,
    pub local_public_id: String,
    pub remote_public_id: String,
    pub transport_thread_id: String,
    pub state: ThreadState,
    pub last_message_at: Option<i64>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl DirectThread {
    #[must_use]
    pub fn can_transition_to(&self, next: ThreadState) -> bool {
        match (self.state, next) {
            (current, next) if current == next => true,
            (
                ThreadState::Pending,
                ThreadState::Ready | ThreadState::Closed | ThreadState::Blocked,
            ) => true,
            (ThreadState::Ready, ThreadState::Closed | ThreadState::Blocked) => true,
            (ThreadState::Closed | ThreadState::Blocked, _) => false,
            _ => false,
        }
    }
}
