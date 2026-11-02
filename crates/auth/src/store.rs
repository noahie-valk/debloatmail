use std::collections::HashMap;
use std::sync::RwLock;

use debloatmail_core::AccountId;
use secrecy::{ExposeSecret, SecretString};

use crate::AuthError;

/// The Secret Service application / service identifier used for keyring entries.
pub const SERVICE_NAME: &str = "org.noahie.debloatmail";

/// Formats a canonical, opaque `secret_ref` handle for an account's `public_id`.
///
/// Per `SECURITY.md` §9.1, `accounts.secret_ref` is an opaque handle, never the secret itself.
pub fn format_secret_ref(account_id: &AccountId) -> String {
    format!("debloatmail:{}", account_id.as_str())
}

/// Abstract secret store interface (`architecture.md` §7).
///
/// Production code uses [`KeyringStore`] backed by the Secret Service portal;
/// unit and integration tests use [`MemorySecretStore`].
pub trait SecretStore: Send + Sync {
    /// Store or rotate the secret associated with `secret_ref`.
    fn set_secret(&self, secret_ref: &str, secret: &SecretString) -> Result<(), AuthError>;

    /// Retrieve the secret associated with `secret_ref`.
    fn get_secret(&self, secret_ref: &str) -> Result<SecretString, AuthError>;

    /// Delete the secret associated with `secret_ref` (required on `Accounts.Remove(purge=true)`
    /// per `SECURITY.md` §9.3).
    fn delete_secret(&self, secret_ref: &str) -> Result<(), AuthError>;

    /// Verify that the underlying credential store is reachable (`doctor` check).
    fn check_status(&self) -> Result<(), AuthError>;
}

/// Secret store backed by the platform keyring (`zbus` Secret Service on Linux).
#[derive(Debug, Default, Clone, Copy)]
pub struct KeyringStore;

impl KeyringStore {
    pub fn new() -> Self {
        Self
    }

    fn map_keyring_err(secret_ref: &str, err: keyring::Error) -> AuthError {
        match err {
            keyring::Error::NoEntry => AuthError::SecretNotFound(secret_ref.to_string()),
            keyring::Error::NoDefaultStore | keyring::Error::NoStorageAccess(_) => {
                AuthError::KeyringUnavailable(err.to_string())
            }
            other => AuthError::KeyringFailure(other.to_string()),
        }
    }
}

impl SecretStore for KeyringStore {
    fn set_secret(&self, secret_ref: &str, secret: &SecretString) -> Result<(), AuthError> {
        if secret_ref.trim().is_empty() {
            return Err(AuthError::InvalidConfig(
                "secret_ref cannot be empty".to_string(),
            ));
        }
        let entry = keyring::Entry::new(SERVICE_NAME, secret_ref)
            .map_err(|e| Self::map_keyring_err(secret_ref, e))?;
        entry
            .set_password(secret.expose_secret())
            .map_err(|e| Self::map_keyring_err(secret_ref, e))
    }

    fn get_secret(&self, secret_ref: &str) -> Result<SecretString, AuthError> {
        if secret_ref.trim().is_empty() {
            return Err(AuthError::InvalidConfig(
                "secret_ref cannot be empty".to_string(),
            ));
        }
        let entry = keyring::Entry::new(SERVICE_NAME, secret_ref)
            .map_err(|e| Self::map_keyring_err(secret_ref, e))?;
        let raw = entry
            .get_password()
            .map_err(|e| Self::map_keyring_err(secret_ref, e))?;
        Ok(SecretString::from(raw))
    }

    fn delete_secret(&self, secret_ref: &str) -> Result<(), AuthError> {
        if secret_ref.trim().is_empty() {
            return Err(AuthError::InvalidConfig(
                "secret_ref cannot be empty".to_string(),
            ));
        }
        let entry = keyring::Entry::new(SERVICE_NAME, secret_ref)
            .map_err(|e| Self::map_keyring_err(secret_ref, e))?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(Self::map_keyring_err(secret_ref, e)),
        }
    }

    fn check_status(&self) -> Result<(), AuthError> {
        match keyring::Entry::store_status() {
            Ok(()) => Ok(()),
            Err(e) => Err(AuthError::KeyringUnavailable(e.to_string())),
        }
    }
}

/// Thread-safe in-memory secret store for headless tests and CI.
#[derive(Default)]
pub struct MemorySecretStore {
    entries: RwLock<HashMap<String, String>>,
    unavailable_reason: RwLock<Option<String>>,
}

impl std::fmt::Debug for MemorySecretStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let count = self.entries.read().map(|m| m.len()).unwrap_or(0);
        f.debug_struct("MemorySecretStore")
            .field("entry_count", &count)
            .finish()
    }
}

impl MemorySecretStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Simulate an unavailable keyring for testing error paths.
    pub fn set_unavailable(&self, reason: Option<String>) {
        if let Ok(mut guard) = self.unavailable_reason.write() {
            *guard = reason;
        }
    }

    fn ensure_available(&self) -> Result<(), AuthError> {
        if let Ok(guard) = self.unavailable_reason.read()
            && let Some(reason) = guard.as_ref()
        {
            return Err(AuthError::KeyringUnavailable(reason.clone()));
        }
        Ok(())
    }
}

impl SecretStore for MemorySecretStore {
    fn set_secret(&self, secret_ref: &str, secret: &SecretString) -> Result<(), AuthError> {
        self.ensure_available()?;
        if secret_ref.trim().is_empty() {
            return Err(AuthError::InvalidConfig(
                "secret_ref cannot be empty".to_string(),
            ));
        }
        let mut map = self
            .entries
            .write()
            .map_err(|_| AuthError::KeyringFailure("lock poisoned".to_string()))?;
        map.insert(
            secret_ref.to_string(),
            secret.expose_secret().to_string(),
        );
        Ok(())
    }

    fn get_secret(&self, secret_ref: &str) -> Result<SecretString, AuthError> {
        self.ensure_available()?;
        if secret_ref.trim().is_empty() {
            return Err(AuthError::InvalidConfig(
                "secret_ref cannot be empty".to_string(),
            ));
        }
        let map = self
            .entries
            .read()
            .map_err(|_| AuthError::KeyringFailure("lock poisoned".to_string()))?;
        match map.get(secret_ref) {
            Some(val) => Ok(SecretString::from(val.clone())),
            None => Err(AuthError::SecretNotFound(secret_ref.to_string())),
        }
    }

    fn delete_secret(&self, secret_ref: &str) -> Result<(), AuthError> {
        self.ensure_available()?;
        if secret_ref.trim().is_empty() {
            return Err(AuthError::InvalidConfig(
                "secret_ref cannot be empty".to_string(),
            ));
        }
        let mut map = self
            .entries
            .write()
            .map_err(|_| AuthError::KeyringFailure("lock poisoned".to_string()))?;
        map.remove(secret_ref);
        Ok(())
    }

    fn check_status(&self) -> Result<(), AuthError> {
        self.ensure_available()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_memory_secret_store_lifecycle_and_redaction() {
        let store = MemorySecretStore::new();
        let acc_id = AccountId::new("018f6e80-7c00-7218-971e-7f62b2512f71").unwrap();
        let sref = format_secret_ref(&acc_id);
        assert_eq!(sref, "debloatmail:018f6e80-7c00-7218-971e-7f62b2512f71");

        let secret = SecretString::from("super-secret-app-password".to_string());
        // Verify Debug never leaks the secret value
        let debug_repr = format!("{secret:?}");
        assert!(!debug_repr.contains("super-secret-app-password"));

        store.set_secret(&sref, &secret).unwrap();

        let store_debug = format!("{store:?}");
        assert!(!store_debug.contains("super-secret-app-password"));

        let loaded = store.get_secret(&sref).unwrap();
        assert_eq!(loaded.expose_secret(), "super-secret-app-password");

        // Rotate credential
        let rotated = SecretString::from("new-rotated-password".to_string());
        store.set_secret(&sref, &rotated).unwrap();
        assert_eq!(
            store.get_secret(&sref).unwrap().expose_secret(),
            "new-rotated-password"
        );

        // Delete credential on purge
        store.delete_secret(&sref).unwrap();
        assert!(matches!(
            store.get_secret(&sref),
            Err(AuthError::SecretNotFound(_))
        ));
    }
}
