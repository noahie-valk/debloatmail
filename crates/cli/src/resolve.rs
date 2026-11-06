//! Addressing and prefix resolution for items, accounts, folders, and feeds.
//!
//! Conforms to `docs/debloatmailctl.md` §5:
//! - 8-hex short IDs resolved client-side against SQLite.
//! - Unique match -> resolved `public_id`.
//! - Ambiguous prefix -> exit 9 with candidate matches.
//! - Not found -> exit 3.
//! - Stdin `-` reads IDs line-by-line.

use debloatmail_ffi::reader::{list_accounts_fn, list_feeds_fn, list_folders_fn};
use debloatmail_ffi::{FfiAccount, FfiFeed, FfiFolder};
use rusqlite::{Connection, params};
use std::io::{self, BufRead};

use crate::error::CliError;

/// Resolves a single item ID argument (short ID prefix or full UUIDv7) to a full `public_id`.
pub fn resolve_item_id(conn: &Connection, input: &str) -> Result<String, CliError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(CliError::Usage("item id cannot be empty".to_string()));
    }

    let mut stmt = conn.prepare(
        "SELECT public_id FROM items
         WHERE public_id = ?1 OR public_id LIKE ?2
         ORDER BY sort_at DESC, id DESC;",
    )?;

    let prefix_pattern = format!("{trimmed}%");
    let rows = stmt.query_map(params![trimmed, prefix_pattern], |r| r.get::<_, String>(0))?;

    let mut matches = Vec::new();
    for r in rows {
        let id = r?;
        if !matches.contains(&id) {
            matches.push(id);
        }
    }

    match matches.len() {
        0 => Err(CliError::NotFound(format!("item '{trimmed}' not found"))),
        1 => Ok(matches.remove(0)),
        count => {
            let short_candidates: Vec<String> = matches
                .iter()
                .map(|id| {
                    if id.len() >= 8 {
                        id[..8].to_string()
                    } else {
                        id.clone()
                    }
                })
                .collect();
            Err(CliError::AmbiguousId {
                prefix: trimmed.to_string(),
                count,
                candidates: short_candidates.join(", "),
            })
        }
    }
}

/// Resolves multiple item IDs, supporting literal `"-"` to read from stdin line-by-line.
pub fn resolve_item_ids(conn: &Connection, inputs: &[String]) -> Result<Vec<String>, CliError> {
    let mut resolved = Vec::new();

    for arg in inputs {
        if arg == "-" {
            let stdin = io::stdin();
            for line in stdin.lock().lines() {
                let l = line.map_err(|e| CliError::Internal(e.to_string()))?;
                let trimmed = l.trim();
                if !trimmed.is_empty() {
                    let id = resolve_item_id(conn, trimmed)?;
                    resolved.push(id);
                }
            }
        } else {
            let id = resolve_item_id(conn, arg)?;
            resolved.push(id);
        }
    }

    Ok(resolved)
}

/// Resolves an account identifier or human name to `FfiAccount`.
pub fn resolve_account(conn: &Connection, input: &str) -> Result<FfiAccount, CliError> {
    let trimmed = input.trim();
    let accounts = list_accounts_fn(conn)?;

    let matches: Vec<FfiAccount> = accounts
        .into_iter()
        .filter(|a| a.id == trimmed || a.name.eq_ignore_ascii_case(trimmed))
        .collect();

    match matches.len() {
        0 => Err(CliError::NotFound(format!("account '{trimmed}' not found"))),
        1 => Ok(matches.into_iter().next().unwrap()),
        _ => Err(CliError::NotFound(format!(
            "account '{trimmed}' is ambiguous"
        ))),
    }
}

/// Resolves a folder identifier, name, or role to `FfiFolder` within an optional account scope.
pub fn resolve_folder(
    conn: &Connection,
    account_id: Option<&str>,
    input: &str,
) -> Result<FfiFolder, CliError> {
    let trimmed = input.trim();
    let folders = list_folders_fn(conn, account_id)?;

    let matches: Vec<FfiFolder> = folders
        .into_iter()
        .filter(|f| {
            f.id == trimmed
                || f.name.eq_ignore_ascii_case(trimmed)
                || f.role
                    .as_deref()
                    .is_some_and(|r| r.eq_ignore_ascii_case(trimmed))
        })
        .collect();

    match matches.len() {
        0 => Err(CliError::NotFound(format!("folder '{trimmed}' not found"))),
        1 => Ok(matches.into_iter().next().unwrap()),
        _ => Err(CliError::NotFound(format!(
            "folder '{trimmed}' is ambiguous"
        ))),
    }
}

/// Resolves a feed identifier, name, title, or URL to `FfiFeed`.
pub fn resolve_feed(
    conn: &Connection,
    account_id: Option<&str>,
    input: &str,
) -> Result<FfiFeed, CliError> {
    let trimmed = input.trim();
    let feeds = list_feeds_fn(conn, account_id)?;

    let matches: Vec<FfiFeed> = feeds
        .into_iter()
        .filter(|f| {
            f.id == trimmed
                || f.name.eq_ignore_ascii_case(trimmed)
                || f.title
                    .as_deref()
                    .is_some_and(|t| t.eq_ignore_ascii_case(trimmed))
                || f.url == trimmed
        })
        .collect();

    match matches.len() {
        0 => Err(CliError::NotFound(format!("feed '{trimmed}' not found"))),
        1 => Ok(matches.into_iter().next().unwrap()),
        _ => Err(CliError::NotFound(format!("feed '{trimmed}' is ambiguous"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn setup_test_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE accounts (
                id INTEGER PRIMARY KEY,
                public_id TEXT NOT NULL UNIQUE,
                name TEXT NOT NULL UNIQUE,
                kind TEXT NOT NULL,
                email TEXT,
                enabled INTEGER NOT NULL DEFAULT 1,
                provider TEXT
            );

            CREATE TABLE items (
                id INTEGER PRIMARY KEY,
                public_id TEXT NOT NULL UNIQUE,
                account_id INTEGER NOT NULL REFERENCES accounts(id),
                kind TEXT NOT NULL,
                date INTEGER,
                sort_at INTEGER NOT NULL,
                unread INTEGER NOT NULL DEFAULT 1,
                starred INTEGER NOT NULL DEFAULT 0,
                hidden_at INTEGER,
                title TEXT NOT NULL,
                author TEXT
            );

            CREATE TABLE folders (
                id INTEGER PRIMARY KEY,
                public_id TEXT NOT NULL UNIQUE,
                account_id INTEGER NOT NULL REFERENCES accounts(id),
                name TEXT NOT NULL,
                role TEXT,
                role_source TEXT,
                last_sync_at INTEGER
            );

            CREATE TABLE feeds (
                id INTEGER PRIMARY KEY,
                public_id TEXT NOT NULL UNIQUE,
                account_id INTEGER NOT NULL REFERENCES accounts(id),
                url TEXT NOT NULL UNIQUE,
                title TEXT,
                interval_secs INTEGER NOT NULL DEFAULT 0,
                last_fetch_at INTEGER,
                next_fetch_at INTEGER,
                removed_at INTEGER
            );

            CREATE VIEW visible_items AS
            SELECT i.id, i.public_id, i.account_id, i.kind, i.date, i.sort_at,
                   i.unread, i.starred, i.title, i.author, NULL as folder_id, NULL as feed_id
            FROM items i
            WHERE i.hidden_at IS NULL;

            INSERT INTO accounts (id, public_id, name, kind)
            VALUES (1, '018f3a9c-e4f2-7a1b-8c3d-9f0a1b2c3d4e', 'fastmail', 'imap');

            INSERT INTO items (id, public_id, account_id, kind, sort_at, title)
            VALUES
                (1, '3f2a9c1e-0000-7000-8000-000000000001', 1, 'mail', 1000, 'First'),
                (2, '3f8b1022-0000-7000-8000-000000000002', 1, 'mail', 2000, 'Second'),
                (3, '7b0d44a2-0000-7000-8000-000000000003', 1, 'mail', 3000, 'Third');

            INSERT INTO folders (id, public_id, account_id, name, role)
            VALUES (1, '018f3aa1-0000-7000-8000-000000000001', 1, 'INBOX', 'inbox');
        "#,
        )
        .unwrap();
        conn
    }

    #[test]
    fn test_unique_short_id_resolution() {
        let conn = setup_test_db();
        let id = resolve_item_id(&conn, "7b0d").unwrap();
        assert_eq!(id, "7b0d44a2-0000-7000-8000-000000000003");
    }

    #[test]
    fn test_ambiguous_short_id_resolution() {
        let conn = setup_test_db();
        let err = resolve_item_id(&conn, "3f").unwrap_err();
        match err {
            CliError::AmbiguousId {
                prefix,
                count,
                candidates,
            } => {
                assert_eq!(prefix, "3f");
                assert_eq!(count, 2);
                assert!(candidates.contains("3f2a9c1e"));
                assert!(candidates.contains("3f8b1022"));
            }
            _ => panic!("expected AmbiguousId, got {err:?}"),
        }
    }

    #[test]
    fn test_not_found_id_resolution() {
        let conn = setup_test_db();
        let err = resolve_item_id(&conn, "deadbeef").unwrap_err();
        assert_eq!(err.exit_code(), 3);
    }

    #[test]
    fn test_resolve_account_and_folder() {
        let conn = setup_test_db();
        let acc = resolve_account(&conn, "fastmail").unwrap();
        assert_eq!(acc.id, "018f3a9c-e4f2-7a1b-8c3d-9f0a1b2c3d4e");

        let folder = resolve_folder(&conn, Some(&acc.id), "inbox").unwrap();
        assert_eq!(folder.name, "INBOX");
    }
}
