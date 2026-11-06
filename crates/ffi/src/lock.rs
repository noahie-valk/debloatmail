//! Standalone flock management for debloatmail processes.
//!
//! When running in standalone mode (`--no-daemon`), only one writer process may access
//! the SQLite database. This module provides advisory file locking via `libc::flock`.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

use crate::error::FfiError;

/// Exclusive lock guarding standalone execution against concurrent writers.
#[derive(Debug)]
pub struct StandaloneLock {
    file: File,
    path: PathBuf,
}

impl StandaloneLock {
    /// Attempts to acquire an exclusive, non-blocking lock on the specified lockfile.
    pub fn acquire(path: PathBuf) -> Result<Self, FfiError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| FfiError::Internal(e.to_string()))?;
        }

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(&path)
            .map_err(|e| {
                FfiError::Internal(format!("failed to open lockfile {}: {e}", path.display()))
            })?;

        let fd = file.as_raw_fd();
        let ret = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };

        if ret != 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EWOULDBLOCK)
                || err.raw_os_error() == Some(libc::EAGAIN)
            {
                let mut existing_pid = None;
                let mut content = String::new();
                if let Ok(mut read_file) = File::open(&path)
                    && read_file.read_to_string(&mut content).is_ok()
                {
                    existing_pid = content.trim().parse::<u32>().ok();
                }
                return Err(FfiError::LockHeld {
                    path,
                    pid: existing_pid,
                });
            }
            return Err(FfiError::Internal(format!(
                "flock error on {}: {err}",
                path.display()
            )));
        }

        let mut lock = Self { file, path };
        let _ = lock.file.set_len(0);
        let _ = writeln!(lock.file, "{}", std::process::id());
        let _ = lock.file.sync_all();

        Ok(lock)
    }

    /// Returns the standard path for the standalone lockfile:
    /// `$XDG_STATE_HOME/debloatmail/debloatmail-standalone.lock`
    pub fn default_path() -> PathBuf {
        let state_dir = std::env::var("XDG_STATE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| {
                let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
                PathBuf::from(home).join(".local").join("state")
            });
        state_dir
            .join("debloatmail")
            .join("debloatmail-standalone.lock")
    }

    /// Returns the path to the lockfile.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for StandaloneLock {
    fn drop(&mut self) {
        let fd = self.file.as_raw_fd();
        unsafe {
            libc::flock(fd, libc::LOCK_UN);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lock_acquire_and_release() {
        let tmp = tempfile::tempdir().unwrap();
        let lock_path = tmp.path().join("test.lock");

        // First lock should succeed
        let lock1 = StandaloneLock::acquire(lock_path.clone()).expect("lock1 should succeed");
        assert_eq!(lock1.path(), lock_path.as_path());

        // Verify PID was written
        let mut content = String::new();
        File::open(&lock_path)
            .unwrap()
            .read_to_string(&mut content)
            .unwrap();
        assert_eq!(content.trim(), std::process::id().to_string());

        // Second lock while lock1 is held should fail with LockHeld
        match StandaloneLock::acquire(lock_path.clone()) {
            Err(FfiError::LockHeld { path, pid }) => {
                assert_eq!(path, lock_path);
                assert_eq!(pid, Some(std::process::id()));
            }
            other => panic!("expected LockHeld, got {:?}", other),
        }

        // Dropping lock1 frees the lock
        drop(lock1);

        // Third lock should now succeed
        let lock2 = StandaloneLock::acquire(lock_path).expect("lock2 should succeed after drop");
        drop(lock2);
    }
}
