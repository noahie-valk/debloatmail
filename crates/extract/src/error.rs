use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ExtractError {
    #[error("Message size {size} exceeds limit of {limit} bytes")]
    OversizeMessage { size: usize, limit: usize },

    #[error("Failed to parse message structure: {0}")]
    ParseFailed(String),

    #[error("Invalid data encountered: {0}")]
    InvalidData(String),
}
