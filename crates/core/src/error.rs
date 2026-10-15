use std::fmt;

/// Errors arising from core domain logic and validation.
#[derive(Debug, PartialEq, Eq, Clone)]
pub enum CoreError {
    /// Identifier validation failure.
    InvalidId(String),
    /// Unrecognized enum string token.
    InvalidToken {
        expected: &'static str,
        found: String,
    },
}

impl fmt::Display for CoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CoreError::InvalidId(msg) => write!(f, "invalid public ID: {msg}"),
            CoreError::InvalidToken { expected, found } => {
                write!(f, "invalid token '{found}'; expected one of {expected}")
            }
        }
    }
}

impl std::error::Error for CoreError {}
