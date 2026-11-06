//! `debloatmail-ffi` — Qt-free engine trait and transport boundary for debloatmail.

pub mod error;
pub mod lock;
pub mod types;

pub use error::FfiError;
pub use lock::StandaloneLock;
pub use types::*;
