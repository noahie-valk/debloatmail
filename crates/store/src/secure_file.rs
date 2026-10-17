// Secure file creation utilities for debloatmail store

use libc::{O_CREAT, O_EXCL, O_NOFOLLOW};
use std::fs::File;
use std::os::unix::fs::OpenOptionsExt; // for custom_flags and mode
use std::path::Path;

use crate::StoreError;

/// Creates a new file at `path` with permissions 0o600 and flags O_EXCL|O_NOFOLLOW.
/// Returns a `File` handle on success. If the file already exists, this function
/// does nothing and returns the existing file opened for read/write.
pub fn secure_create(path: &Path) -> Result<File, StoreError> {
    // Use OpenOptions for fine‑grained control over flags and mode.
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true);
    // If the file does not exist, create it exclusively with the required mode.
    // The custom flags combine O_CREAT (create if missing), O_EXCL (fail if exists),
    // and O_NOFOLLOW (disallow symlinks). These match SECURITY.md §8.5.
    options.custom_flags(O_CREAT | O_EXCL | O_NOFOLLOW);
    // Mode 0o600: owner read/write only.
    options.mode(0o600);
    match options.open(path) {
        Ok(file) => Ok(file),
        // If the file already exists, fall back to opening it normally while still
        // enforcing O_NOFOLLOW to avoid symlink attacks.
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let mut fallback = std::fs::OpenOptions::new();
            fallback.read(true).write(true);
            fallback.custom_flags(O_NOFOLLOW);
            fallback.open(path).map_err(|e| {
                StoreError::Sqlite(rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Blob,
                    Box::new(e),
                ))
            })
        }
        Err(e) => Err(StoreError::Sqlite(
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Blob, Box::new(e)),
        )),
    }
}
