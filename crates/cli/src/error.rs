//! Error types and contractual exit code mapping for `debloatmailctl`.
//!
//! Per `docs/debloatmailctl.md` §7:
//! - 0: Success
//! - 1: Unexpected internal error
//! - 2: Usage error (unknown flag, bad arguments, unknown search operator, etc.)
//! - 3: Not found (item, folder, feed, account, or op)
//! - 4: Authentication failed
//! - 5: Network or TLS failure
//! - 6: Daemon required but unavailable
//! - 7: Offline mode: data not cached, or the command requires the network
//! - 8: Lock held by another process (standalone mode)
//! - 9: Ambiguous id prefix

use debloatmail_ffi::FfiError;
use serde::Serialize;
use std::io::Write;

/// Contractual error representation for `debloatmailctl`.
#[derive(Debug, thiserror::Error)]
pub enum CliError {
    #[error("{0}")]
    Usage(String),

    #[error("{0}")]
    NotFound(String),

    #[error("{0}")]
    AuthFailed(String),

    #[error("{0}")]
    NetworkError(String),

    #[error("{0}")]
    DaemonUnavailable(String),

    #[error("{0}")]
    Offline(String),

    #[error("{0}")]
    LockHeld(String),

    #[error("id prefix '{prefix}' is ambiguous ({count} matches: {candidates})")]
    AmbiguousId {
        prefix: String,
        count: usize,
        candidates: String,
    },

    #[error("{0}")]
    Internal(String),
}

impl CliError {
    /// Contractual exit code for process termination.
    pub fn exit_code(&self) -> i32 {
        match self {
            CliError::Internal(_) => 1,
            CliError::Usage(_) => 2,
            CliError::NotFound(_) => 3,
            CliError::AuthFailed(_) => 4,
            CliError::NetworkError(_) => 5,
            CliError::DaemonUnavailable(_) => 6,
            CliError::Offline(_) => 7,
            CliError::LockHeld(_) => 8,
            CliError::AmbiguousId { .. } => 9,
        }
    }

    /// Contractual error code string used in machine JSON envelopes.
    pub fn code_str(&self) -> &'static str {
        match self {
            CliError::Internal(_) => "internal",
            CliError::Usage(_) => "usage",
            CliError::NotFound(_) => "not_found",
            CliError::AuthFailed(_) => "not_authenticated",
            CliError::NetworkError(_) => "network_error",
            CliError::DaemonUnavailable(_) => "daemon_unavailable",
            CliError::Offline(_) => "offline_data",
            CliError::LockHeld(_) => "lock_held",
            CliError::AmbiguousId { .. } => "ambiguous_id",
        }
    }

    /// Renders the error according to the output format.
    ///
    /// Per `docs/debloatmailctl.md` §7:
    /// - With `--format json`, errors are emitted as `{"error": {"code": "...", "message": "..."}}`
    ///   to **stdout** and the exit code is still set.
    /// - Otherwise, a single-line message is written to **stderr**, prefixed `error:`.
    pub fn render(&self, is_json: bool) {
        if is_json {
            let env = JsonErrorEnvelope {
                error: JsonErrorBody {
                    code: self.code_str().to_string(),
                    message: self.to_string(),
                },
            };
            if let Ok(json_str) = serde_json::to_string_pretty(&env) {
                println!("{json_str}");
            } else {
                println!(
                    r#"{{"error":{{"code":"{}","message":"{}"}}}}"#,
                    self.code_str(),
                    self
                );
            }
        } else {
            let mut stderr = anstream::stderr();
            let _ = writeln!(stderr, "error: {self}");
        }
    }
}

#[derive(Serialize)]
struct JsonErrorEnvelope {
    error: JsonErrorBody,
}

#[derive(Serialize)]
struct JsonErrorBody {
    code: String,
    message: String,
}

impl From<debloatmail_store::StoreError> for CliError {
    fn from(err: debloatmail_store::StoreError) -> Self {
        CliError::Internal(err.to_string())
    }
}

impl From<FfiError> for CliError {
    fn from(err: FfiError) -> Self {
        match err {
            FfiError::NotFound(msg) => CliError::NotFound(msg),
            FfiError::AmbiguousId { prefix, matches } => CliError::AmbiguousId {
                count: matches.len(),
                candidates: matches.join(", "),
                prefix,
            },
            FfiError::InvalidArgument(msg) => CliError::Usage(msg),
            FfiError::Auth(err) => CliError::AuthFailed(err.to_string()),
            FfiError::DaemonUnavailable(msg) => CliError::DaemonUnavailable(msg),
            FfiError::LockHeld { path, pid } => {
                let pid_str = pid.map(|p| format!(" (pid {p})")).unwrap_or_default();
                CliError::LockHeld(format!("lock held on {}{pid_str}", path.display()))
            }
            FfiError::Store(err) => CliError::Internal(err.to_string()),
            FfiError::Sqlite(err) => CliError::Internal(err.to_string()),
            FfiError::Sync(err) => CliError::Internal(err.to_string()),
            FfiError::Engine(err) => CliError::Internal(err.to_string()),
            FfiError::Feeds(err) => CliError::NetworkError(err.to_string()),
            FfiError::Dbus(err) => CliError::NetworkError(err.to_string()),
            FfiError::Unsupported(msg) => CliError::Usage(msg),
            FfiError::Internal(msg) => CliError::Internal(msg),
        }
    }
}

impl From<rusqlite::Error> for CliError {
    fn from(err: rusqlite::Error) -> Self {
        CliError::Internal(err.to_string())
    }
}

impl From<std::io::Error> for CliError {
    fn from(err: std::io::Error) -> Self {
        CliError::Internal(err.to_string())
    }
}

impl From<toml::de::Error> for CliError {
    fn from(err: toml::de::Error) -> Self {
        CliError::Usage(format!("failed to parse TOML configuration: {err}"))
    }
}

impl From<toml::ser::Error> for CliError {
    fn from(err: toml::ser::Error) -> Self {
        CliError::Internal(format!("failed to serialize TOML configuration: {err}"))
    }
}

impl From<serde_json::Error> for CliError {
    fn from(err: serde_json::Error) -> Self {
        CliError::Internal(format!("json error: {err}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exit_codes() {
        assert_eq!(CliError::Internal("test".into()).exit_code(), 1);
        assert_eq!(CliError::Usage("test".into()).exit_code(), 2);
        assert_eq!(CliError::NotFound("test".into()).exit_code(), 3);
        assert_eq!(CliError::AuthFailed("test".into()).exit_code(), 4);
        assert_eq!(CliError::NetworkError("test".into()).exit_code(), 5);
        assert_eq!(CliError::DaemonUnavailable("test".into()).exit_code(), 6);
        assert_eq!(CliError::Offline("test".into()).exit_code(), 7);
        assert_eq!(CliError::LockHeld("test".into()).exit_code(), 8);
        assert_eq!(
            CliError::AmbiguousId {
                prefix: "3f".into(),
                count: 2,
                candidates: "3f2a, 3f8b".into()
            }
            .exit_code(),
            9
        );
    }

    #[test]
    fn test_code_strings() {
        assert_eq!(CliError::Usage("x".into()).code_str(), "usage");
        assert_eq!(CliError::NotFound("x".into()).code_str(), "not_found");
        assert_eq!(CliError::Offline("x".into()).code_str(), "offline_data");
        assert_eq!(CliError::LockHeld("x".into()).code_str(), "lock_held");
        assert_eq!(
            CliError::AmbiguousId {
                prefix: "a".into(),
                count: 1,
                candidates: "".into()
            }
            .code_str(),
            "ambiguous_id"
        );
    }

    #[test]
    fn test_ambiguous_id_formatting() {
        let err = CliError::AmbiguousId {
            prefix: "3f".into(),
            count: 3,
            candidates: "3f2a9c1e, 3f8b1022, 3fcc4410".into(),
        };
        assert_eq!(
            err.to_string(),
            "id prefix '3f' is ambiguous (3 matches: 3f2a9c1e, 3f8b1022, 3fcc4410)"
        );
    }
}
