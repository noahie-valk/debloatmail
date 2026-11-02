//! IMAP/SMTP protocol engine, confined socket/TLS transport, coroutine pumps,
//! and account state / backoff state machines for debloatmail.
//!
//! Conforms to `architecture.md` §5, `SYNC.md`, and `SECURITY.md` §5:
//! - Protocol logic uses sans-I/O coroutines (`io-imap`, `io-smtp`).
//! - All IMAP/SMTP TCP sockets live in `io.rs`; all IMAP/SMTP TLS handshakes live in `tls.rs`.
//! - Cleartext (`Security::None`) is refused for non-loopback peer addresses.
//! - `allow_invalid_cert_loopback` is permitted strictly when the connected socket's
//!   `peer_addr()` is loopback (`127.0.0.1` or `::1`), and reported explicitly.

pub mod check;
pub mod imap;
pub mod io;
pub mod smtp;
pub mod state;
pub mod tls;

pub use check::{AccountCheckReport, CheckStepReport, run_account_check};
pub use imap::{
    DiscoveredFolder, ImapConnection, NegotiatedCapabilities, discover_folders,
    extract_capabilities, fetch_attachment_section, fetch_body_peek, fetch_header_window,
    run_idle_once, uid_expunge_messages, validate_section_id,
};
pub use io::{EngineStream, MemoryStream, open_imap_stream, open_smtp_stream};
pub use smtp::{OutgoingEmail, SendOutcome, SmtpConnection, send_email};
pub use state::{
    AccountState, AccountStateMachine, Clock, FailureClass, Rng, SystemClock,
    account_reconnect_backoff, classify_imap_error, classify_smtp_error,
    deterministic_account_jitter, feed_poll_backoff, op_retry_backoff,
};
pub use tls::{TlsVerifyMode, build_tls_connector};

/// Errors produced by `debloatmail-engine`.
#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("TLS configuration or handshake error: {0}")]
    Tls(String),

    #[error("authentication / credential error: {0}")]
    Auth(#[from] debloatmail_auth::AuthError),

    #[error("cleartext connection refused for non-loopback peer address {0}")]
    CleartextRefused(std::net::IpAddr),

    #[error("loopback TLS certificate exception refused for non-loopback peer address {0}")]
    LoopbackCertExceptionRefused(std::net::IpAddr),

    #[error("IMAP session open failed: {0}")]
    ImapSession(#[from] io_imap::session::ImapSessionOpenError),

    #[error("IMAP command failed: {0}")]
    ImapClient(#[from] io_imap::client::ImapClientError),

    #[error("IMAP IDLE failed: {0}")]
    ImapIdle(#[from] io_imap::rfc2177::idle::ImapIdleError),

    #[error("SMTP session open failed: {0}")]
    SmtpSession(#[from] io_smtp::session::SmtpSessionOpenError),

    #[error("SMTP command failed: {0}")]
    SmtpClient(#[from] io_smtp::client::SmtpClientError),

    #[error("invalid protocol argument or header injection attempt: {0}")]
    InvalidArgument(String),

    #[error("server lacks required capability: {0}")]
    UnsupportedCapability(&'static str),
}
