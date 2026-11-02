//! Per-account state machine (`SYNC.md` §14), failure taxonomy (`SYNC.md` §10.1),
//! and exponential backoff + jitter schedules (`SYNC.md` §8.6, §10.2).

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use debloatmail_core::AccountId;
use io_imap::client::ImapClientError;
use io_imap::session::ImapSessionOpenError;
use io_smtp::client::SmtpClientError;
use io_smtp::session::SmtpSessionOpenError;

use crate::EngineError;

/// Injectable wall-clock source for deterministic state-machine and backoff testing (`SYNC.md` §15).
pub trait Clock: Send + Sync {
    fn now_secs(&self) -> i64;
}

/// Production [`Clock`] backed by `SystemTime::now()`.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_secs(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    }
}

/// Injectable uniform random number source in `[0.0, 1.0)` for jitter calculation (`SYNC.md` §10.2, §15).
pub trait Rng: Send {
    /// Returns a value in `[0.0, 1.0)`.
    fn next_f64(&mut self) -> f64;
}

/// Taxonomy of failure classes for IMAP, SMTP, and sync operations (`SYNC.md` §10.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureClass {
    /// DNS timeout, TCP reset, TLS EOF, or transient network drop.
    TransientNetwork,
    /// Server `BYE`, `421` / `450` / `451`, or `[UNAVAILABLE]` / `[SERVERBUG]`.
    TransientServer,
    /// `NO [AUTHENTICATIONFAILED]`, SMTP `535`, or missing keyring secret.
    /// Halts automatic schedule until credentials rotate (`SYNC.md` §10.1, §14).
    AuthFailure,
    /// Malformed greeting, `STARTTLS` injection, or invalid framing.
    ProtocolViolation,
    /// Permanent server rejection (`NO [NONEXISTENT]`, SMTP `550`, missing capability).
    Permanent,
    /// Local configuration or argument error.
    Local,
}

/// Classifies an IMAP-related [`EngineError`] into a [`FailureClass`] (`SYNC.md` §10.1).
pub fn classify_imap_error(err: &EngineError) -> FailureClass {
    match err {
        EngineError::Io(_) => FailureClass::TransientNetwork,
        EngineError::Tls(_) => FailureClass::TransientNetwork,
        EngineError::Auth(_) => FailureClass::AuthFailure,
        EngineError::CleartextRefused(_) | EngineError::LoopbackCertExceptionRefused(_) => {
            FailureClass::Local
        }
        EngineError::InvalidArgument(_) => FailureClass::Local,
        EngineError::UnsupportedCapability(_) => FailureClass::Permanent,
        EngineError::ImapSession(sess_err) => match sess_err {
            ImapSessionOpenError::Login(_)
            | ImapSessionOpenError::AuthAnonymous(_)
            | ImapSessionOpenError::AuthLogin(_)
            | ImapSessionOpenError::AuthPlain(_)
            | ImapSessionOpenError::AuthOauthbearer(_)
            | ImapSessionOpenError::AuthXoauth2(_)
            | ImapSessionOpenError::AuthScramSha256(_)
            | ImapSessionOpenError::LoginDisabled => FailureClass::AuthFailure,
            ImapSessionOpenError::StartTlsInjection => FailureClass::ProtocolViolation,
            ImapSessionOpenError::Greeting(_) | ImapSessionOpenError::Capability(_) => {
                FailureClass::TransientServer
            }
            _ => FailureClass::Permanent,
        },
        EngineError::ImapClient(client_err) => match client_err {
            ImapClientError::Io(_) => FailureClass::TransientNetwork,
            ImapClientError::Login(_)
            | ImapClientError::AuthAnonymous(_)
            | ImapClientError::AuthLogin(_)
            | ImapClientError::AuthPlain(_)
            | ImapClientError::AuthOAuthBearer(_)
            | ImapClientError::AuthXOAuth2(_)
            | ImapClientError::AuthScramSha256(_) => FailureClass::AuthFailure,
            msg => {
                let s = msg.to_string().to_ascii_uppercase();
                if s.contains("BYE") || s.contains("UNAVAILABLE") {
                    FailureClass::TransientServer
                } else if s.contains("EOF") {
                    FailureClass::TransientNetwork
                } else if s.contains("DECODE") || s.contains("POISONED") {
                    FailureClass::ProtocolViolation
                } else {
                    FailureClass::Permanent
                }
            }
        },
        EngineError::ImapIdle(idle_err) => {
            let s = idle_err.to_string().to_ascii_uppercase();
            if s.contains("EOF") {
                FailureClass::TransientNetwork
            } else if s.contains("BYE") {
                FailureClass::TransientServer
            } else {
                FailureClass::ProtocolViolation
            }
        }
        EngineError::SmtpSession(_) | EngineError::SmtpClient(_) => classify_smtp_error(err),
    }
}

/// Classifies an SMTP-related [`EngineError`] into a [`FailureClass`] (`SYNC.md` §10.1).
pub fn classify_smtp_error(err: &EngineError) -> FailureClass {
    match err {
        EngineError::Io(_) | EngineError::Tls(_) => FailureClass::TransientNetwork,
        EngineError::Auth(_) => FailureClass::AuthFailure,
        EngineError::CleartextRefused(_)
        | EngineError::LoopbackCertExceptionRefused(_)
        | EngineError::InvalidArgument(_) => FailureClass::Local,
        EngineError::UnsupportedCapability(_) => FailureClass::Permanent,
        EngineError::SmtpSession(sess_err) => match sess_err {
            SmtpSessionOpenError::AuthAnonymous(_)
            | SmtpSessionOpenError::AuthLogin(_)
            | SmtpSessionOpenError::AuthPlain(_)
            | SmtpSessionOpenError::AuthOauthbearer(_)
            | SmtpSessionOpenError::AuthXoauth2(_)
            | SmtpSessionOpenError::AuthScramSha256(_) => FailureClass::AuthFailure,
            SmtpSessionOpenError::StartTls(_) => FailureClass::ProtocolViolation,
            SmtpSessionOpenError::Greeting(_) | SmtpSessionOpenError::Ehlo(_) => {
                FailureClass::TransientServer
            }
            _ => FailureClass::Permanent,
        },
        EngineError::SmtpClient(client_err) => match client_err {
            SmtpClientError::Io(_) => FailureClass::TransientNetwork,
            SmtpClientError::AuthAnonymous(_)
            | SmtpClientError::AuthLogin(_)
            | SmtpClientError::AuthPlain(_)
            | SmtpClientError::AuthOAuthBearer(_)
            | SmtpClientError::AuthXOAuth2(_)
            | SmtpClientError::AuthScramSha256(_) => FailureClass::AuthFailure,
            other => {
                let s = other.to_string();
                if s.contains(" 4") {
                    FailureClass::TransientServer
                } else {
                    FailureClass::Permanent
                }
            }
        },
        _ => FailureClass::Permanent,
    }
}

/// Applies `±10%` jitter (`[0.9, 1.1)`) from `rng` to `base_secs` (`SYNC.md` §10.2).
fn apply_jitter_secs(base_secs: f64, rng: &mut dyn Rng) -> Duration {
    let raw = rng.next_f64().clamp(0.0, 1.0);
    let factor = 0.9 + raw * 0.2;
    Duration::from_secs_f64((base_secs * factor).max(1.0))
}

/// Computes the account reconnect backoff delay (`SYNC.md` §10.2):
/// `min(30s * 2^failures, 900s) * jitter(0.9..1.1)`.
pub fn account_reconnect_backoff(failures: u32, rng: &mut dyn Rng) -> Duration {
    const BASE_SECS: f64 = 30.0;
    const CAP_SECS: f64 = 900.0;
    let shift = failures.min(10);
    let exp = (BASE_SECS * (1u64 << shift) as f64).min(CAP_SECS);
    apply_jitter_secs(exp, rng)
}

/// Computes the feed poll backoff delay (`SYNC.md` §10.2):
/// `min(300s * 2^consecutive_failures, 86400s) * jitter(0.9..1.1)`.
pub fn feed_poll_backoff(consecutive_failures: u32, rng: &mut dyn Rng) -> Duration {
    const BASE_SECS: f64 = 300.0;
    const CAP_SECS: f64 = 86_400.0;
    let shift = consecutive_failures.min(12);
    let exp = (BASE_SECS * (1u64 << shift) as f64).min(CAP_SECS);
    apply_jitter_secs(exp, rng)
}

/// Maximum number of retry attempts for a `pending_ops` row before it becomes terminal `failed`
/// (`SYNC.md` §8.6).
pub const MAX_PENDING_OP_ATTEMPTS: u32 = 10;

/// Computes the `pending_ops` retry backoff delay (`SYNC.md` §8.6):
/// - Returns `None` when `attempts >= 10` (terminal `failed` state; requires `queue retry`).
/// - Otherwise returns `Some(min(60s * 2^(attempts - 1), 21600s) * jitter(0.9..1.1))`.
pub fn op_retry_backoff(attempts: u32, rng: &mut dyn Rng) -> Option<Duration> {
    if attempts >= MAX_PENDING_OP_ATTEMPTS {
        return None;
    }
    const BASE_SECS: f64 = 60.0;
    const CAP_SECS: f64 = 21_600.0;
    let exp_power = attempts.saturating_sub(1).min(12);
    let exp = (BASE_SECS * (1u64 << exp_power) as f64).min(CAP_SECS);
    Some(apply_jitter_secs(exp, rng))
}

/// Computes a deterministic `±10%` offset in seconds for `account_id` relative to `interval_secs`
/// so multiple accounts do not fire periodic syncs simultaneously (`SYNC.md` §3.1).
pub fn deterministic_account_jitter(account_id: &AccountId, interval_secs: u32) -> i64 {
    // FNV-1a 64-bit hash over the account ID string.
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in account_id.as_str().as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    let unit = (hash as f64) / (u64::MAX as f64); // [0.0, 1.0]
    let signed = (unit * 2.0) - 1.0; // [-1.0, 1.0]
    let max_delta = f64::from(interval_secs) * 0.10;
    (signed * max_delta).round() as i64
}

/// Runtime states of a single account worker (`SYNC.md` §14).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountState {
    Offline,
    Connecting,
    Syncing,
    /// Holding `IDLE` on `INBOX` or awaiting the next periodic timer tick.
    /// Maps to `"idle"` in `accounts.state` (`DATA-MODEL.md` §5.1).
    Watching,
    /// Waiting out an exponential backoff timer after a transient failure.
    /// Maps to `"error"` in `accounts.state` (`DATA-MODEL.md` §5.1).
    Backoff {
        next_retry_at: i64,
        last_error: String,
    },
    /// Halted on authentication failure (`NO [AUTHENTICATIONFAILED]`, SMTP `535`).
    /// Automatic schedule is suspended until credentials rotate (`SYNC.md` §10.1, §14).
    AuthFailed {
        last_error: String,
    },
}

impl AccountState {
    /// Maps the runtime worker state to the 5-value persisted string stored in `accounts.state`
    /// (`DATA-MODEL.md` §5.1, `SYNC.md` §14):
    /// `'offline' | 'connecting' | 'syncing' | 'idle' | 'error'`.
    pub fn as_db_state_str(&self) -> &'static str {
        match self {
            Self::Offline => "offline",
            Self::Connecting => "connecting",
            Self::Syncing => "syncing",
            Self::Watching => "idle",
            Self::Backoff { .. } | Self::AuthFailed { .. } => "error",
        }
    }
}

/// Per-account state machine tracking connection lifecycle, consecutive failures,
/// and authentication halt (`SYNC.md` §14).
#[derive(Debug, Clone)]
pub struct AccountStateMachine {
    state: AccountState,
    consecutive_failures: u32,
}

impl Default for AccountStateMachine {
    fn default() -> Self {
        Self::new()
    }
}

impl AccountStateMachine {
    pub fn new() -> Self {
        Self {
            state: AccountState::Offline,
            consecutive_failures: 0,
        }
    }

    pub fn state(&self) -> &AccountState {
        &self.state
    }

    pub fn consecutive_failures(&self) -> u32 {
        self.consecutive_failures
    }

    /// Returns `true` if automatic timer ticks are allowed to start a sync cycle.
    /// Returns `false` when halted in [`AccountState::AuthFailed`] (`SYNC.md` §10.1, §14).
    pub fn can_auto_sync(&self, now_secs: i64) -> bool {
        match &self.state {
            AccountState::AuthFailed { .. } => false,
            AccountState::Backoff { next_retry_at, .. } => now_secs >= *next_retry_at,
            _ => true,
        }
    }

    /// Transitions to `Connecting` unless halted by `AuthFailed`.
    pub fn begin_connect(&mut self) -> bool {
        if matches!(self.state, AccountState::AuthFailed { .. }) {
            return false;
        }
        self.state = AccountState::Connecting;
        true
    }

    /// Transitions to `Syncing` after session open succeeds.
    pub fn on_connected(&mut self) {
        self.state = AccountState::Syncing;
    }

    /// Transitions to `Watching` (`accounts.state = "idle"`) and resets `consecutive_failures` to 0.
    pub fn on_sync_succeeded(&mut self) {
        self.consecutive_failures = 0;
        self.state = AccountState::Watching;
    }

    /// Transitions to `Offline` (e.g. when network connectivity is lost or daemon shuts down).
    pub fn on_offline(&mut self) {
        self.state = AccountState::Offline;
    }

    /// Records a failure and transitions either to `AuthFailed` (halting automatic retries)
    /// or `Backoff` with exponential delay (`SYNC.md` §10.1, §10.2, §14).
    pub fn on_failure(
        &mut self,
        class: FailureClass,
        error_msg: impl Into<String>,
        clock: &dyn Clock,
        rng: &mut dyn Rng,
    ) {
        let last_error = error_msg.into();
        if class == FailureClass::AuthFailure {
            self.state = AccountState::AuthFailed { last_error };
            return;
        }

        let delay = account_reconnect_backoff(self.consecutive_failures, rng);
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        let next_retry_at = clock
            .now_secs()
            .saturating_add(delay.as_secs() as i64);

        self.state = AccountState::Backoff {
            next_retry_at,
            last_error,
        };
    }

    /// Resets an `AuthFailed` halt when the user updates credentials (`account auth` or `SetSecret`),
    /// allowing the worker to reconnect immediately (`SYNC.md` §10.1, §14).
    pub fn on_credentials_rotated(&mut self) {
        self.consecutive_failures = 0;
        self.state = AccountState::Offline;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FixedClock(i64);
    impl Clock for FixedClock {
        fn now_secs(&self) -> i64 {
            self.0
        }
    }

    /// Returns `0.5` so jitter factor `0.9 + 0.5 * 0.2 == 1.0` (exact base formula).
    struct MidpointRng;
    impl Rng for MidpointRng {
        fn next_f64(&mut self) -> f64 {
            0.5
        }
    }

    #[test]
    fn test_backoff_curves_and_caps() {
        let mut rng = MidpointRng;

        // Account reconnect: 30s * 2^failures, capped at 900s
        assert_eq!(account_reconnect_backoff(0, &mut rng).as_secs(), 30);
        assert_eq!(account_reconnect_backoff(1, &mut rng).as_secs(), 60);
        assert_eq!(account_reconnect_backoff(2, &mut rng).as_secs(), 120);
        assert_eq!(account_reconnect_backoff(5, &mut rng).as_secs(), 900);
        assert_eq!(account_reconnect_backoff(20, &mut rng).as_secs(), 900);

        // Feed poll: 300s * 2^consecutive_failures, capped at 86400s
        assert_eq!(feed_poll_backoff(0, &mut rng).as_secs(), 300);
        assert_eq!(feed_poll_backoff(1, &mut rng).as_secs(), 600);
        assert_eq!(feed_poll_backoff(10, &mut rng).as_secs(), 86_400);

        // Pending op retry: 60s * 2^(attempts - 1), capped at 21600s; None at attempts >= 10
        assert_eq!(op_retry_backoff(1, &mut rng).unwrap().as_secs(), 60);
        assert_eq!(op_retry_backoff(2, &mut rng).unwrap().as_secs(), 120);
        assert_eq!(op_retry_backoff(3, &mut rng).unwrap().as_secs(), 240);
        assert_eq!(op_retry_backoff(9, &mut rng).unwrap().as_secs(), 15_360);
        assert!(op_retry_backoff(10, &mut rng).is_none());
        assert!(op_retry_backoff(11, &mut rng).is_none());
    }

    #[test]
    fn test_account_state_machine_transitions_and_auth_halt() {
        let clock = FixedClock(1_000);
        let mut rng = MidpointRng;
        let mut sm = AccountStateMachine::new();

        assert_eq!(sm.state().as_db_state_str(), "offline");
        assert!(sm.begin_connect());
        assert_eq!(sm.state().as_db_state_str(), "connecting");

        sm.on_connected();
        assert_eq!(sm.state().as_db_state_str(), "syncing");

        sm.on_sync_succeeded();
        assert_eq!(sm.state(), &AccountState::Watching);
        assert_eq!(sm.state().as_db_state_str(), "idle");

        // Transient network error -> Backoff (30s at failure 0) -> DB state "error"
        sm.on_failure(
            FailureClass::TransientNetwork,
            "connection reset",
            &clock,
            &mut rng,
        );
        assert_eq!(sm.state().as_db_state_str(), "error");
        assert_eq!(sm.consecutive_failures(), 1);
        assert!(!sm.can_auto_sync(1_015));
        assert!(sm.can_auto_sync(1_030));

        // Auth failure -> AuthFailed -> halts automatic sync regardless of time!
        sm.on_failure(
            FailureClass::AuthFailure,
            "NO [AUTHENTICATIONFAILED]",
            &clock,
            &mut rng,
        );
        assert_eq!(sm.state().as_db_state_str(), "error");
        assert!(!sm.can_auto_sync(999_999));
        assert!(!sm.begin_connect());

        // Rotating credentials clears the halt and resets failures to 0
        sm.on_credentials_rotated();
        assert_eq!(sm.consecutive_failures(), 0);
        assert!(sm.can_auto_sync(1_000));
        assert!(sm.begin_connect());
    }

    #[test]
    fn test_deterministic_account_jitter_within_ten_percent() {
        let id = AccountId::new("01J8X6Z4M9R3W2Q7V5N8K1P0T2");
        let jitter = deterministic_account_jitter(&id, 300);
        assert!((-30..=30).contains(&jitter));
        // Deterministic across calls
        assert_eq!(jitter, deterministic_account_jitter(&id, 300));
    }
}
