use assert_cmd::Command;
use predicates::prelude::*;
use rusqlite::Connection;
use std::path::Path;
use tempfile::tempdir;

const INIT_SQL: &str = include_str!("../../store/src/migrations/0001_init.sql");

fn setup_fixture_db(db_path: &Path) {
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
            (2, '018f3aa1-0000-7000-8000-000000000002', 1, 'Archive', 'Archive', 'archive', 'special-use', 12345, 100, 50, {now});

        INSERT INTO feeds (id, public_id, account_id, url, title, site_url, etag, last_modified, interval_secs, backoff_secs, last_fetch_at, next_fetch_at, last_error, removed_at)
        VALUES
            (1, '018f3bb2-0000-7000-8000-000000000001', 2, 'https://blog.rust-lang.org/feed.xml', 'Rust Blog', 'https://blog.rust-lang.org', NULL, NULL, 3600, 0, {now}, {now} + 3600, NULL, NULL);

        INSERT INTO threads (id, public_id, account_id, display_subject, last_activity)
        VALUES
            (1, '019f3a2b-7d52-7e1c-9a4b-2c3d4e5f6a7b', 1, 'Release Checklist', {now});

        -- Mail Item 1 (cached body)
        INSERT INTO items (id, public_id, kind, account_id, title, author, published_at, sort_at, unread, starred, answered, hidden_at, thread_id, body_state, has_attachments, created_at, updated_at)
        VALUES
            (1, '3f2a9c1e-7c41-7e2a-9d3f-1a2b3c4d5e6f', 'mail', 1, 'Re: release checklist', 'Alice Chen', {now} - 100, {now} - 100, 1, 0, 0, NULL, 1, 'cached', 0, {now}, {now});

        INSERT INTO mail_items (item_id, folder_id, uid, uidvalidity, message_id, from_addr, to_addrs, cc_addrs, subject_norm, internal_date, size_bytes)
        VALUES
            (1, 1, 101, 12345, '<msg1@fastmail.com>', 'alice@fastmail.com', '["bob@fastmail.com"]', '[]', 'release checklist', {now} - 100, 1024);

        INSERT INTO item_text (item_id, title, author, body, durable, extractor_version)
        VALUES
            (1, 'Re: release checklist', 'Alice Chen', 'Hello world! The release checklist is verified.', 0, 1);

        -- Mail Item 2 (uncached body)
        INSERT INTO items (id, public_id, kind, account_id, title, author, published_at, sort_at, unread, starred, answered, hidden_at, thread_id, body_state, has_attachments, created_at, updated_at)
        VALUES
            (2, '3f8b1022-7c41-7e2a-9d3f-1a2b3c4d5e6f', 'mail', 1, 'Invoice #2291', 'Bob Martinez', {now} - 200, {now} - 200, 0, 1, 0, NULL, 1, 'none', 1, {now}, {now});

        INSERT INTO mail_items (item_id, folder_id, uid, uidvalidity, message_id, from_addr, to_addrs, cc_addrs, subject_norm, internal_date, size_bytes)
        VALUES
            (2, 1, 102, 12345, '<msg2@fastmail.com>', 'bob@fastmail.com', '["alice@fastmail.com"]', '[]', 'invoice 2291', {now} - 200, 2048);

        INSERT INTO item_text (item_id, title, author, body, durable, extractor_version)
        VALUES
            (2, 'Invoice #2291', 'Bob Martinez', '', 0, 1);

        -- Feed Item 1 (cached)
        INSERT INTO items (id, public_id, kind, account_id, title, author, published_at, sort_at, unread, starred, answered, hidden_at, thread_id, body_state, has_attachments, created_at, updated_at)
        VALUES
            (3, '7b0d44a2-7c41-7e2a-9d3f-1a2b3c4d5e6f', 'feed', 2, 'Async cancellation in practice', 'Rust Blog', {now} - 50, {now} - 50, 1, 0, 0, NULL, NULL, 'cached', 0, {now}, {now});

        INSERT INTO feed_items (item_id, feed_id, guid, link)
        VALUES
            (3, 1, 'guid-async-1', 'https://blog.rust-lang.org/async');

        INSERT INTO item_text (item_id, title, author, body, durable, extractor_version)
        VALUES
            (3, 'Async cancellation in practice', 'Rust Blog', 'Async cancellation in Rust is a critical pattern.', 1, 1);
        "#,
    ))
    .unwrap();
}

#[test]
fn test_cli_list_command_formats() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("mail.db");
    setup_fixture_db(&db_path);

    // 1. Human format (default)
    let mut cmd = Command::cargo_bin("debloatmailctl").unwrap();
    cmd.args(["list", "--db", db_path.to_str().unwrap()]);
    cmd.assert()
        .success()
        .stdout(predicate::str::contains("3f2a9c1e"))
        .stdout(predicate::str::contains("7b0d44a2"))
        .stdout(predicate::str::contains("Re: release checklist"));

    // 2. JSON format
    let mut cmd_json = Command::cargo_bin("debloatmailctl").unwrap();
    cmd_json.args(["list", "--format", "json", "--db", db_path.to_str().unwrap()]);
    let assert_json = cmd_json.assert().success();
    let stdout_json = String::from_utf8(assert_json.get_output().stdout.clone()).unwrap();
    assert!(stdout_json.contains("\"items\":"));
    assert!(stdout_json.contains("\"short_id\": \"3f2a9c1e\""));
    assert!(stdout_json.contains("\"kind\": \"mail\""));
    assert!(stdout_json.contains("\"kind\": \"feed\""));

    // 3. Plain format
    let mut cmd_plain = Command::cargo_bin("debloatmailctl").unwrap();
    cmd_plain.args(["list", "--format", "plain", "--db", db_path.to_str().unwrap()]);
    cmd_plain
        .assert()
        .success()
        .stdout(predicate::str::contains("3f2a9c1e\t"))
        .stdout(predicate::str::contains("Alice Chen\tRe: release checklist"));

    // 4. Plain --print0 format
    let mut cmd_print0 = Command::cargo_bin("debloatmailctl").unwrap();
    cmd_print0.args([
        "list",
        "--format",
        "plain",
        "--print0",
        "--db",
        db_path.to_str().unwrap(),
    ]);
    let assert_p0 = cmd_print0.assert().success();
    let stdout_bytes = &assert_p0.get_output().stdout;
    assert!(stdout_bytes.contains(&0u8));
}

#[test]
fn test_cli_read_command() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("mail.db");
    setup_fixture_db(&db_path);

    // Read cached message body
    let mut cmd = Command::cargo_bin("debloatmailctl").unwrap();
    cmd.args(["read", "3f2a", "--db", db_path.to_str().unwrap()]);
    cmd.assert()
        .success()
        .stdout(predicate::str::contains("Hello world! The release checklist is verified."));

    // Read with headers
    let mut cmd_hdr = Command::cargo_bin("debloatmailctl").unwrap();
    cmd_hdr.args(["read", "3f2a", "--headers", "--db", db_path.to_str().unwrap()]);
    cmd_hdr
        .assert()
        .success()
        .stdout(predicate::str::contains("From: alice@fastmail.com"))
        .stdout(predicate::str::contains("Subject: Re: release checklist"));

    // Read feed item
    let mut cmd_feed = Command::cargo_bin("debloatmailctl").unwrap();
    cmd_feed.args(["read", "7b0d", "--db", db_path.to_str().unwrap()]);
    cmd_feed
        .assert()
        .success()
        .stdout(predicate::str::contains("Async cancellation in Rust is a critical pattern."));

    // Read feed with --raw errors (exit 2)
    let mut cmd_raw_feed = Command::cargo_bin("debloatmailctl").unwrap();
    cmd_raw_feed.args(["read", "7b0d", "--raw", "--db", db_path.to_str().unwrap()]);
    cmd_raw_feed
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("--raw is mail-only"));

    // Read uncached item with --offline exits 7
    let mut cmd_offline = Command::cargo_bin("debloatmailctl").unwrap();
    cmd_offline.args([
        "read",
        "3f8b",
        "--offline",
        "--db",
        db_path.to_str().unwrap(),
    ]);
    cmd_offline
        .assert()
        .failure()
        .code(7)
        .stderr(predicate::str::contains("body not cached and --offline specified"));

    // Ambiguous prefix exits 9
    let mut cmd_ambig = Command::cargo_bin("debloatmailctl").unwrap();
    cmd_ambig.args(["read", "3f", "--db", db_path.to_str().unwrap()]);
    cmd_ambig
        .assert()
        .failure()
        .code(9)
        .stderr(predicate::str::contains("id prefix '3f' is ambiguous"));

    // Not found exits 3
    let mut cmd_nf = Command::cargo_bin("debloatmailctl").unwrap();
    cmd_nf.args(["read", "deadbeef", "--db", db_path.to_str().unwrap()]);
    cmd_nf
        .assert()
        .failure()
        .code(3)
        .stderr(predicate::str::contains("item 'deadbeef' not found"));
}

#[test]
fn test_cli_search_command() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("mail.db");
    setup_fixture_db(&db_path);

    // Search query
    let mut cmd = Command::cargo_bin("debloatmailctl").unwrap();
    cmd.args(["search", "checklist", "--db", db_path.to_str().unwrap()]);
    cmd.assert()
        .success()
        .stdout(predicate::str::contains("3f2a9c1e"));

    // Search --explain
    let mut cmd_exp = Command::cargo_bin("debloatmailctl").unwrap();
    cmd_exp.args([
        "search",
        "release",
        "--explain",
        "--db",
        db_path.to_str().unwrap(),
    ]);
    cmd_exp
        .assert()
        .success()
        .stdout(predicate::str::contains("FTS5 Expression:"))
        .stdout(predicate::str::contains("SQL Query:"));

    // Unknown search operator exits 2
    let mut cmd_bad = Command::cargo_bin("debloatmailctl").unwrap();
    cmd_bad.args([
        "search",
        "unknown:operator",
        "--db",
        db_path.to_str().unwrap(),
    ]);
    cmd_bad
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("unknown search operator 'unknown:'"));
}

#[test]
fn test_cli_inspecting_and_utility_commands() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("mail.db");
    setup_fixture_db(&db_path);

    // Folder list
    let mut cmd_folder = Command::cargo_bin("debloatmailctl").unwrap();
    cmd_folder.args(["folder", "list", "--db", db_path.to_str().unwrap()]);
    cmd_folder
        .assert()
        .success()
        .stdout(predicate::str::contains("INBOX"))
        .stdout(predicate::str::contains("Archive"));

    // Feed list
    let mut cmd_feed = Command::cargo_bin("debloatmailctl").unwrap();
    cmd_feed.args(["feed", "list", "--db", db_path.to_str().unwrap()]);
    cmd_feed
        .assert()
        .success()
        .stdout(predicate::str::contains("Rust Blog"));

    // Feed show
    let mut cmd_fshow = Command::cargo_bin("debloatmailctl").unwrap();
    cmd_fshow.args(["feed", "show", "Rust Blog", "--db", db_path.to_str().unwrap()]);
    cmd_fshow
        .assert()
        .success()
        .stdout(predicate::str::contains("https://blog.rust-lang.org/feed.xml"));

    // Account list
    let mut cmd_acc = Command::cargo_bin("debloatmailctl").unwrap();
    cmd_acc.args(["account", "list", "--db", db_path.to_str().unwrap()]);
    cmd_acc
        .assert()
        .success()
        .stdout(predicate::str::contains("fastmail"))
        .stdout(predicate::str::contains("blogs"));

    // Account show
    let mut cmd_ashow = Command::cargo_bin("debloatmailctl").unwrap();
    cmd_ashow.args(["account", "show", "fastmail", "--db", db_path.to_str().unwrap()]);
    cmd_ashow
        .assert()
        .success()
        .stdout(predicate::str::contains("alice@fastmail.com"))
        .stdout(predicate::str::contains("Credentials: ••• (in keyring)"));

    // Config path
    let mut cmd_cfg = Command::cargo_bin("debloatmailctl").unwrap();
    cmd_cfg.args(["config", "path"]);
    cmd_cfg.assert().success();

    // Generate completions
    let mut cmd_comp = Command::cargo_bin("debloatmailctl").unwrap();
    cmd_comp.args(["generate", "completions", "bash"]);
    cmd_comp
        .assert()
        .success()
        .stdout(predicate::str::contains("complete -F _debloatmailctl"));

    // Generate schema
    let mut cmd_schema = Command::cargo_bin("debloatmailctl").unwrap();
    cmd_schema.args(["generate", "schema"]);
    cmd_schema
        .assert()
        .success()
        .stdout(predicate::str::contains("CREATE TABLE items"));

    // Version
    let mut cmd_ver = Command::cargo_bin("debloatmailctl").unwrap();
    cmd_ver.args(["version"]);
    cmd_ver
        .assert()
        .success()
        .stdout(predicate::str::contains("debloatmailctl 0.1.0"));
}
