//! Credential management, Secret Service keyring integration, provider presets,
//! and SASL mechanism construction for debloatmail.
//!
//! Conforms to `SECURITY.md` §9 and `architecture.md` §7:
//! - Secrets live in the Secret Service keyring (`keyring` v4), never in `config.toml` or SQLite.
//! - The database stores only an opaque `secret_ref` handle (`debloatmail:<account_public_id>`).
//! - Secrets in memory are held in `secrecy::SecretString` (zeroed on drop, redacted in `Debug`).
//! - OAuth2 is architected from day one and deferred until post-beta.

pub mod preset;
pub mod sasl;
pub mod store;

pub use preset::{ProviderPreset, apply_preset, is_loopback_host, validate_account_security};
pub use sasl::{PasswordAuthPreference, build_sasl_credentials, select_password_mechanism};
pub use secrecy::{ExposeSecret, SecretString};
pub use store::{KeyringStore, MemorySecretStore, SERVICE_NAME, SecretStore, format_secret_ref};

/// Errors produced by `debloatmail-auth`.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("secret not found in keyring for reference '{0}'")]
    SecretNotFound(String),

    #[error("secret service keyring is unavailable: {0}")]
    KeyringUnavailable(String),

    #[error("keyring operation failed: {0}")]
    KeyringFailure(String),

    #[error("OAuth2 authentication is deferred until post-beta")]
    OAuthDeferred,

    #[error("invalid account security or credential configuration: {0}")]
    InvalidConfig(String),
}
