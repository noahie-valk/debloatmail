use assert_cmd::Command;
use predicates::prelude::*;
use rusqlite::Connection;
use std::path::Path;
use tempfile::tempdir;

const INIT_SQL: &str = include_str!("../../store/src/migrations/0001_init.sql");

fn setup_mutation_fixture_db(db_path: &Path) {
    let conn = Connection::open(db_path).unwrap();
    conn.execute_batch(INIT_SQL).unwrap();

    let now = chrono::Utc::now().timestamp();

    conn.execute_batch(&format!(
        r#"
        INSERT INTO accounts (id, public_id, name, kind, email, enabled, provider, created_at, updated_at)
        VALUES
            (1, '018f3a9c-e4f2-7a1b-8c3d-9f0a1b2c3d4e', 'fastmail', 'imap', 'alice@fastmail.com', 1, 'fastmail', {now}, {now}),
            (2, '018f3a9c-e4f2-7a1b-8c3d-9f0a1b2c3d4f', 'blogs', 'feed', NULL, 1, NULL, {now}, {now});

        INSERT INTO folders (id, public_id, account_id, name, display_name, role, role_source, uidvalidity, uidnext, highestmodseq, last_sync_at)
        VALUES
            (1, '018f3aa1-0000-7000-8000-000000000001', 1, 'INBOX', 'Inbox', 'inbox', 'special-use', 12345, 100, 50, {now}),
            (2, '018f3aa1-0000-7000-8000-000000000002', 1, 'Archive', 'Archive', 'archive', 'special-use', 12345, 100, 50, {now}),
            (3, '018f3aa1-0000-7000-8000-000000000003', 1, 'Trash', 'Trash', 'trash', 'special-use', 12345, 100, 50, {now});

        INSERT INTO feeds (id, public_id, account_id, url, title, site_url, etag, last_modified, interval_secs, backoff_secs, last_fetch_at, next_fetch_at, last_error, removed_at)
        VALUES
            (1, '018f3bb2-0000-7000-8000-000000000001', 2, 'https://blog.rust-lang.org/feed.xml', 'Rust Blog', 'https://blog.rust-lang.org', NULL, NULL, 3600, 0, {now}, {now} + 3600, NULL, NULL);

        -- Mail Item 1 (unread = 1, starred = 0)
        INSERT INTO items (id, public_id, kind, account_id, title, author, published_at, sort_at, unread, starred, answered, hidden_at, thread_id, body_state, has_attachments, created_at, updated_at)
        VALUES
            (1, '3f2a9c1e-7c41-7e2a-9d3f-1a2b3c4d5e6f', 'mail', 1, 'Re: release checklist', 'Alice Chen', {now} - 100, {now} - 100, 1, 0, 0, NULL, NULL, 'cached', 0, {now}, {now});

        INSERT INTO mail_items (item_id, folder_id, uid, uidvalidity, message_id, from_addr, to_addrs, cc_addrs, subject_norm, internal_date, size_bytes)
        VALUES
            (1, 1, 101, 12345, '<msg1@fastmail.com>', 'alice@fastmail.com', '["bob@fastmail.com"]', '[]', 'release checklist', {now} - 100, 1024);

        INSERT INTO item_text (item_id, title, author, body, durable, extractor_version)
        VALUES
            (1, 'Re: release checklist', 'Alice Chen', 'Hello world!', 0, 1);

        -- Feed Item 1 (unread = 1, starred = 0)
        INSERT INTO items (id, public_id, kind, account_id, title, author, published_at, sort_at, unread, starred, answered, hidden_at, thread_id, body_state, has_attachments, created_at, updated_at)
        VALUES
            (2, '7b0d44a2-7c41-7e2a-9d3f-1a2b3c4d5e6f', 'feed', 2, 'Async cancellation in practice', 'Rust Blog', {now} - 50, {now} - 50, 1, 0, 0, NULL, NULL, 'cached', 0, {now}, {now});

        INSERT INTO feed_items (item_id, feed_id, guid, link)
        VALUES
            (2, 1, 'guid-async-1', 'https://blog.rust-lang.org/async');

        INSERT INTO item_text (item_id, title, author, body, durable, extractor_version)
        VALUES
            (2, 'Async cancellation in practice', 'Rust Blog', 'Async cancellation article.', 1, 1);
        "#,
    ))
    .unwrap();
}

fn cmd_with_env(tmp: &tempfile::TempDir, db_path: &Path) -> Command {
    let mut cmd = Command::cargo_bin("debloatmailctl").unwrap();
    cmd.env("XDG_STATE_HOME", tmp.path());
    cmd.env("XDG_CACHE_HOME", tmp.path());
    cmd.env("XDG_CONFIG_HOME", tmp.path());
    cmd.arg("--no-daemon");
    cmd.arg("--db");
    cmd.arg(db_path.to_str().unwrap());
    cmd
}

#[test]
fn test_cli_mark_mutations() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("mail.db");
    setup_mutation_fixture_db(&db_path);

    // 1. Mark read (short ID)
    let mut cmd = cmd_with_env(&tmp, &db_path);
    cmd.args(["mark", "read", "3f2a"]);
    cmd.assert()
        .success()
        .stdout(predicate::str::contains("marked 1"));

    // Verify in DB
    let conn = Connection::open(&db_path).unwrap();
    let unread: i64 = conn
        .query_row("SELECT unread FROM items WHERE id = 1;", [], |r| r.get(0))
        .unwrap();
    assert_eq!(unread, 0);

    // 2. Mark starred
    let mut cmd_star = cmd_with_env(&tmp, &db_path);
    cmd_star.args(["mark", "starred", "3f2a"]);
    cmd_star
        .assert()
        .success()
        .stdout(predicate::str::contains("marked 1"));

    let starred: i64 = conn
        .query_row("SELECT starred FROM items WHERE id = 1;", [], |r| r.get(0))
        .unwrap();
    assert_eq!(starred, 1);

    // 3. Mark unread and unstarred via stdin "-"
    let mut cmd_stdin = cmd_with_env(&tmp, &db_path);
    cmd_stdin
        .args(["mark", "unread", "-"])
        .write_stdin("3f2a9c1e\n7b0d44a2\n");
    cmd_stdin
        .assert()
        .success()
        .stdout(predicate::str::contains("marked 2"));

    // 4. Test missing ID prefix exits 3
    let mut cmd_missing = cmd_with_env(&tmp, &db_path);
    cmd_missing.args(["mark", "read", "ffffffff"]);
    cmd_missing
        .assert()
        .failure()
        .code(3)
        .stderr(predicate::str::contains("not found"));

    // 5. Test --dry-run
    let mut cmd_dry = cmd_with_env(&tmp, &db_path);
    cmd_dry.args(["--dry-run", "mark", "read", "3f2a"]);
    cmd_dry
        .assert()
        .success()
        .stdout(predicate::str::contains("marked 1"));
}

#[test]
fn test_cli_move_copy_delete_hide_restrictions() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("mail.db");
    setup_mutation_fixture_db(&db_path);

    // 1. Move mail item to Archive
    let mut cmd_move = cmd_with_env(&tmp, &db_path);
    cmd_move.args(["move", "3f2a", "--to", "Archive"]);
    cmd_move
        .assert()
        .success()
        .stdout(predicate::str::contains("moved 1"));

    // Verify folder_id updated in DB
    let conn = Connection::open(&db_path).unwrap();
    let folder_id: i64 = conn
        .query_row(
            "SELECT folder_id FROM mail_items WHERE item_id = 1;",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(folder_id, 2);

    // 2. Reject moving feed item (must exit 2)
    let mut cmd_move_feed = cmd_with_env(&tmp, &db_path);
    cmd_move_feed.args(["move", "7b0d", "--to", "Archive"]);
    cmd_move_feed
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("move is mail-only"));

    // 3. Reject copying feed item (must exit 2)
    let mut cmd_copy_feed = cmd_with_env(&tmp, &db_path);
    cmd_copy_feed.args(["copy", "7b0d", "--to", "Archive"]);
    cmd_copy_feed
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("copy is mail-only"));

    // 4. Reject deleting feed item (must exit 2)
    let mut cmd_del_feed = cmd_with_env(&tmp, &db_path);
    cmd_del_feed.args(["delete", "7b0d"]);
    cmd_del_feed
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("use hide for feed items"));

    // 5. Hide feed item requires -y when non-TTY
    let mut cmd_hide_no_y = cmd_with_env(&tmp, &db_path);
    cmd_hide_no_y.args(["hide", "7b0d"]);
    cmd_hide_no_y
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("requires -y"));

    // 6. Hide feed item with -y succeeds
    let mut cmd_hide = cmd_with_env(&tmp, &db_path);
    cmd_hide.args(["-y", "hide", "7b0d"]);
    cmd_hide
        .assert()
        .success()
        .stdout(predicate::str::contains("hidden 1"));

    // 7. Reject hiding mail item (must exit 2)
    let mut cmd_hide_mail = cmd_with_env(&tmp, &db_path);
    cmd_hide_mail.args(["-y", "hide", "3f2a"]);
    cmd_hide_mail
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("hide is feed-only"));

    // 8. Delete mail item
    let mut cmd_del = cmd_with_env(&tmp, &db_path);
    cmd_del.args(["delete", "3f2a"]);
    cmd_del
        .assert()
        .success()
        .stdout(predicate::str::contains("deleted 1"));
}
