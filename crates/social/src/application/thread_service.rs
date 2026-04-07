use crate::domain::threads::{DirectThread, ThreadState};
use crate::ports::repositories::ThreadRepository;
use crate::types::{SocialError, SocialResult};

pub fn upsert_thread<R>(repository: &R, thread: &DirectThread) -> SocialResult<()>
where
    R: ThreadRepository,
{
    if thread.thread_id.trim().is_empty() {
        return Err(SocialError::InvalidInput(
            "thread_id is required".to_owned(),
        ));
    }
    if thread.local_public_id.trim().is_empty() || thread.remote_public_id.trim().is_empty() {
        return Err(SocialError::InvalidInput(
            "local_public_id and remote_public_id are required".to_owned(),
        ));
    }
    if thread.transport_thread_id.trim().is_empty() {
        return Err(SocialError::InvalidInput(
            "transport_thread_id is required".to_owned(),
        ));
    }
    if thread.created_at > thread.updated_at {
        return Err(SocialError::InvalidInput(
            "updated_at must be >= created_at".to_owned(),
        ));
    }
    if let Some(last_message_at) = thread.last_message_at
        && (last_message_at < thread.created_at || last_message_at > thread.updated_at)
    {
        return Err(SocialError::InvalidInput(
            "last_message_at must be within thread lifetime".to_owned(),
        ));
    }
    if let Some(existing) =
        repository.find_thread(&thread.local_public_id, &thread.remote_public_id)?
        && !existing.can_transition_to(thread.state)
    {
        return Err(SocialError::Conflict(format!(
            "invalid thread transition: {:?} -> {:?}",
            existing.state, thread.state
        )));
    }
    repository.upsert_thread(thread)
}

pub fn find_thread<R>(
    repository: &R,
    local_public_id: &str,
    remote_public_id: &str,
) -> SocialResult<Option<DirectThread>>
where
    R: ThreadRepository,
{
    if local_public_id.trim().is_empty() || remote_public_id.trim().is_empty() {
        return Err(SocialError::InvalidInput(
            "local_public_id and remote_public_id are required".to_owned(),
        ));
    }
    repository.find_thread(local_public_id, remote_public_id)
}

pub fn list_threads<R>(repository: &R, local_public_id: &str) -> SocialResult<Vec<DirectThread>>
where
    R: ThreadRepository,
{
    if local_public_id.trim().is_empty() {
        return Err(SocialError::InvalidInput(
            "local_public_id is required".to_owned(),
        ));
    }
    repository.list_threads(local_public_id)
}

pub fn list_threads_by_state<R>(
    repository: &R,
    local_public_id: &str,
    state: ThreadState,
) -> SocialResult<Vec<DirectThread>>
where
    R: ThreadRepository,
{
    if local_public_id.trim().is_empty() {
        return Err(SocialError::InvalidInput(
            "local_public_id is required".to_owned(),
        ));
    }
    repository.list_threads_by_state(local_public_id, state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::repositories::ThreadRepository;
    use std::sync::Mutex;

    #[derive(Default)]
    struct FakeRepository {
        threads: Mutex<Vec<DirectThread>>,
    }

    impl ThreadRepository for FakeRepository {
        fn upsert_thread(&self, thread: &DirectThread) -> SocialResult<()> {
            let mut threads = self.threads.lock().expect("threads mutex");
            if let Some(existing) = threads.iter_mut().find(|item| {
                item.local_public_id == thread.local_public_id
                    && item.remote_public_id == thread.remote_public_id
            }) {
                *existing = thread.clone();
            } else {
                threads.push(thread.clone());
            }
            Ok(())
        }

        fn find_thread(
            &self,
            local_public_id: &str,
            remote_public_id: &str,
        ) -> SocialResult<Option<DirectThread>> {
            Ok(self
                .threads
                .lock()
                .expect("threads mutex")
                .iter()
                .find(|item| {
                    item.local_public_id == local_public_id
                        && item.remote_public_id == remote_public_id
                })
                .cloned())
        }

        fn list_threads_by_state(
            &self,
            local_public_id: &str,
            state: ThreadState,
        ) -> SocialResult<Vec<DirectThread>> {
            Ok(self
                .threads
                .lock()
                .expect("threads mutex")
                .iter()
                .filter(|item| item.local_public_id == local_public_id && item.state == state)
                .cloned()
                .collect())
        }

        fn list_threads(&self, local_public_id: &str) -> SocialResult<Vec<DirectThread>> {
            Ok(self
                .threads
                .lock()
                .expect("threads mutex")
                .iter()
                .filter(|item| item.local_public_id == local_public_id)
                .cloned()
                .collect())
        }
    }

    #[test]
    fn rejects_invalid_thread_transition() {
        let repository = FakeRepository::default();
        let mut thread = DirectThread {
            thread_id: "thread-1".to_owned(),
            local_public_id: "did:key:alice".to_owned(),
            remote_public_id: "did:key:bob".to_owned(),
            transport_thread_id: "transport-1".to_owned(),
            state: ThreadState::Ready,
            last_message_at: Some(1),
            created_at: 1,
            updated_at: 1,
        };
        upsert_thread(&repository, &thread).expect("save ready");

        thread.state = ThreadState::Closed;
        thread.updated_at = 2;
        upsert_thread(&repository, &thread).expect("save closed");

        thread.state = ThreadState::Ready;
        thread.updated_at = 3;
        let error =
            upsert_thread(&repository, &thread).expect_err("reject invalid thread transition");

        assert!(matches!(error, SocialError::Conflict(_)));
    }
}
