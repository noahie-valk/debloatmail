use debloatmail_cli::resolve::{resolve_account, resolve_feed, resolve_folder, resolve_item_id};
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

        INSERT INTO accounts (id, public_id, name, kind, email)
        VALUES (1, '018f3a9c-e4f2-7a1b-8c3d-9f0a1b2c3d4e', 'fastmail', 'imap', 'alice@fastmail.com');

        INSERT INTO items (id, public_id, account_id, kind, sort_at, title)
        VALUES
            (1, '3f2a9c1e-0000-7000-8000-000000000001', 1, 'mail', 1000, 'First'),
            (2, '3f8b1022-0000-7000-8000-000000000002', 1, 'mail', 2000, 'Second'),
            (3, '7b0d44a2-0000-7000-8000-000000000003', 1, 'mail', 3000, 'Third');

        INSERT INTO folders (id, public_id, account_id, name, role)
        VALUES (1, '018f3aa1-0000-7000-8000-000000000001', 1, 'INBOX', 'inbox');

        INSERT INTO feeds (id, public_id, account_id, url, title)
        VALUES (1, '018f3bb2-0000-7000-8000-000000000001', 1, 'https://blog.rust-lang.org/feed.xml', 'Rust Blog');
    "#,
    )
    .unwrap();
    conn
}

#[test]
fn test_item_resolution_matrix() {
    let conn = setup_test_db();

    // Unique match
    let id = resolve_item_id(&conn, "7b0d").unwrap();
    assert_eq!(id, "7b0d44a2-0000-7000-8000-000000000003");

    // Full UUID match
    let full_id = resolve_item_id(&conn, "7b0d44a2-0000-7000-8000-000000000003").unwrap();
    assert_eq!(full_id, "7b0d44a2-0000-7000-8000-000000000003");

    // Ambiguous prefix -> exit 9
    let err_ambig = resolve_item_id(&conn, "3f").unwrap_err();
    assert_eq!(err_ambig.exit_code(), 9);
    assert_eq!(err_ambig.code_str(), "ambiguous_id");

    // Not found -> exit 3
    let err_not_found = resolve_item_id(&conn, "abcdef00").unwrap_err();
    assert_eq!(err_not_found.exit_code(), 3);
    assert_eq!(err_not_found.code_str(), "not_found");
}

#[test]
fn test_entities_resolution() {
    let conn = setup_test_db();

    // Account resolution
    let acc = resolve_account(&conn, "fastmail").unwrap();
    assert_eq!(acc.id, "018f3a9c-e4f2-7a1b-8c3d-9f0a1b2c3d4e");

    // Folder resolution by name and by role
    let folder_by_name = resolve_folder(&conn, Some(&acc.id), "INBOX").unwrap();
    assert_eq!(folder_by_name.name, "INBOX");
    let folder_by_role = resolve_folder(&conn, Some(&acc.id), "inbox").unwrap();
    assert_eq!(folder_by_role.name, "INBOX");

    // Feed resolution by title and url
    let feed_by_title = resolve_feed(&conn, Some(&acc.id), "Rust Blog").unwrap();
    assert_eq!(feed_by_title.url, "https://blog.rust-lang.org/feed.xml");
    let feed_by_url =
        resolve_feed(&conn, Some(&acc.id), "https://blog.rust-lang.org/feed.xml").unwrap();
    assert_eq!(feed_by_url.name, "Rust Blog");
}
