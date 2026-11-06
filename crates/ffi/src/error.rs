//! Unified error types for `debloatmail-ffi`.

use std::path::PathBuf;

/// Error conditions produced across the engine boundary.
#[derive(Debug, thiserror::Error)]
pub enum FfiError {
    #[error("storage error: {0}")]
    Store(#[from] debloatmail_store::StoreError),

    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("sync error: {0}")]
    Sync(#[from] debloatmail_sync::SyncError),

    #[error("engine error: {0}")]
    Engine(#[from] debloatmail_engine::EngineError),

    #[error("auth error: {0}")]
    Auth(#[from] debloatmail_auth::AuthError),

    #[error("feed polling error: {0}")]
    Feeds(#[from] debloatmail_feeds::PollError),

    #[error("D-Bus transport error: {0}")]
    Dbus(#[from] zbus::Error),

    #[error("daemon required but unavailable: {0}")]
    DaemonUnavailable(String),

    #[error("lock held by another process: {path:?} (pid: {pid:?})")]
    LockHeld { path: PathBuf, pid: Option<u32> },

    #[error("entity not found: {0}")]
    NotFound(String),

    #[error("ambiguous id prefix '{prefix}': matches {matches:?}")]
    AmbiguousId {
        prefix: String,
        matches: Vec<String>,
    },

    #[error("invalid argument: {0}")]
    InvalidArgument(String),

    #[error("unsupported operation: {0}")]
    Unsupported(String),

    #[error("internal error: {0}")]
    Internal(String),
}
