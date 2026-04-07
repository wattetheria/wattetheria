use thiserror::Error;

#[derive(Debug, Error)]
pub enum SocialError {
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("storage error: {0}")]
    Storage(String),
    #[error("did error: {0}")]
    Did(String),
}

pub type SocialResult<T> = Result<T, SocialError>;
