use rusqlite::{Connection, OpenFlags};
use std::path::Path;

use crate::StoreError;

pub const CURRENT_SCHEMA_VERSION: u32 = 1;
pub const INIT_SQL: &str = include_str!("migrations/0001_init.sql");

/// Apply standard performance and concurrency pragmas to a connection.
pub fn apply_pragmas(conn: &Connection) -> Result<(), StoreError> {
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         PRAGMA foreign_keys = ON;
         PRAGMA busy_timeout = 5000;
         PRAGMA temp_store = MEMORY;",
    )?;
    Ok(())
}

/// Retrieve the current schema user_version from SQLite.
pub fn get_user_version(conn: &Connection) -> Result<u32, StoreError> {
    let version: u32 = conn.query_row("PRAGMA user_version;", [], |row| row.get(0))?;
    Ok(version)
}

/// Check schema version and run initial migration if database is fresh.
pub fn initialize_schema(conn: &mut Connection) -> Result<(), StoreError> {
    apply_pragmas(conn)?;
    let version = get_user_version(conn)?;

    if version == 0 {
        let tx = conn.transaction()?;
        tx.execute_batch(INIT_SQL)?;
        tx.commit()?;
    } else if version > CURRENT_SCHEMA_VERSION {
        return Err(StoreError::VersionMismatch(version, CURRENT_SCHEMA_VERSION));
    }

    Ok(())
}

/// Bump global revision counter inside a write transaction.
pub fn bump_revision(conn: &Connection) -> Result<u64, StoreError> {
    conn.execute(
        "UPDATE meta SET value = CAST(CAST(value AS INTEGER) + 1 AS TEXT) WHERE key = 'revision';",
        [],
    )?;
    get_revision(conn)
}

/// Fetch current global revision counter value.
pub fn get_revision(conn: &Connection) -> Result<u64, StoreError> {
    let rev_str: String = conn.query_row(
        "SELECT value FROM meta WHERE key = 'revision';",
        [],
        |row| row.get(0),
    )?;
    let rev = rev_str.parse::<u64>().unwrap_or(0);
    Ok(rev)
}

/// Open a connection to the SQLite database (read/write, sole writer path).
pub fn open_store(path: impl AsRef<Path>) -> Result<Connection, StoreError> {
    let mut conn = Connection::open(path)?;
    initialize_schema(&mut conn)?;
    Ok(conn)
}

/// Open a read-only connection to the SQLite database (WAL reader path).
pub fn open_read_only(path: impl AsRef<Path>) -> Result<Connection, StoreError> {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let conn = Connection::open_with_flags(path, flags)?;
    apply_pragmas(&conn)?;
    let version = get_user_version(&conn)?;
    if version > CURRENT_SCHEMA_VERSION {
        return Err(StoreError::VersionMismatch(version, CURRENT_SCHEMA_VERSION));
    }
    Ok(conn)
}

/// Open an in-memory SQLite database initialized with the schema (for tests).
pub fn open_in_memory() -> Result<Connection, StoreError> {
    let mut conn = Connection::open_in_memory()?;
    initialize_schema(&mut conn)?;
    Ok(conn)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_init_and_revision() {
        let conn = open_in_memory().unwrap();
        assert_eq!(get_user_version(&conn).unwrap(), 1);
        assert_eq!(get_revision(&conn).unwrap(), 0);

        let rev = bump_revision(&conn).unwrap();
        assert_eq!(rev, 1);
        assert_eq!(get_revision(&conn).unwrap(), 1);

        let rev = bump_revision(&conn).unwrap();
        assert_eq!(rev, 2);
    }

    #[test]
    fn test_rejects_newer_version() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute("PRAGMA user_version = 99;", []).unwrap();
        let res = initialize_schema(&mut conn);
        assert!(matches!(res, Err(StoreError::VersionMismatch(99, 1))));
    }
}
