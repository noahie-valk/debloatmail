use assert_cmd::Command;
use rusqlite::Connection;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::tempdir;

const INIT_SQL: &str = include_str!("../../../crates/store/src/migrations/0001_init.sql");
const FIXED_NOW: i64 = 1700000000; // 2023-11-14T22:13:20Z

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("golden")
}

fn setup_golden_fixture_db(db_path: &Path) {
    let conn = Connection::open(db_path).unwrap();
    conn.execute_batch(INIT_SQL).unwrap();

    let now = FIXED_NOW;

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

fn assert_golden(actual: &str, golden_file: &str) {
    let gdir = golden_dir();
    fs::create_dir_all(&gdir).unwrap();
    let gpath = gdir.join(golden_file);

    if std::env::var("UPDATE_GOLDEN").is_ok() {
        fs::write(&gpath, actual).unwrap();
        return;
    }

    if !gpath.exists() {
        fs::write(&gpath, actual).unwrap();
        panic!(
            "Golden file '{}' was missing and has been written. Please verify and commit it.",
            gpath.display()
        );
    }

    let expected = fs::read_to_string(&gpath).unwrap();

    // Parse and normalize JSON to verify exact equality regardless of line endings
    let actual_json: serde_json::Value =
        serde_json::from_str(actual).expect("actual stdout should be valid JSON");
    let expected_json: serde_json::Value =
        serde_json::from_str(&expected).expect("golden file should be valid JSON");

    assert_eq!(
        actual_json,
        expected_json,
        "JSON output does not match golden file '{}'.\n--- Actual:\n{}\n--- Expected:\n{}",
        golden_file,
        serde_json::to_string_pretty(&actual_json).unwrap(),
        serde_json::to_string_pretty(&expected_json).unwrap()
    );
}

fn isolated_cmd(db_path: &Path, tmp_dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("debloatmailctl").unwrap();
    cmd.env("XDG_STATE_HOME", tmp_dir.join("state"))
        .env("XDG_CACHE_HOME", tmp_dir.join("cache"))
        .env("XDG_CONFIG_HOME", tmp_dir.join("config"))
        .env("XDG_DATA_HOME", tmp_dir.join("data"))
        .arg("--db")
        .arg(db_path.to_str().unwrap());
    cmd
}

#[test]
fn test_golden_list_json() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("mail.db");
    setup_golden_fixture_db(&db_path);

    let mut cmd = isolated_cmd(&db_path, tmp.path());
    cmd.args(["list", "--format", "json"]);
    let assert = cmd.assert().success();
    let output = String::from_utf8(assert.get_output().stdout.clone()).unwrap();

    assert_golden(&output, "list_default.golden.json");
}

#[test]
fn test_golden_list_threads_json() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("mail.db");
    setup_golden_fixture_db(&db_path);

    let mut cmd = isolated_cmd(&db_path, tmp.path());
    cmd.args(["list", "--threads", "--format", "json"]);
    let assert = cmd.assert().success();
    let output = String::from_utf8(assert.get_output().stdout.clone()).unwrap();

    assert_golden(&output, "list_threads.golden.json");
}

#[test]
fn test_golden_search_json() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("mail.db");
    setup_golden_fixture_db(&db_path);

    let mut cmd = isolated_cmd(&db_path, tmp.path());
    cmd.args(["search", "release", "--format", "json"]);
    let assert = cmd.assert().success();
    let output = String::from_utf8(assert.get_output().stdout.clone()).unwrap();

    assert_golden(&output, "search_query.golden.json");
}

#[test]
fn test_golden_folder_list_json() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("mail.db");
    setup_golden_fixture_db(&db_path);

    let mut cmd = isolated_cmd(&db_path, tmp.path());
    cmd.args(["folder", "list", "--format", "json"]);
    let assert = cmd.assert().success();
    let output = String::from_utf8(assert.get_output().stdout.clone()).unwrap();

    assert_golden(&output, "folder_list.golden.json");
}

#[test]
fn test_golden_feed_list_json() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("mail.db");
    setup_golden_fixture_db(&db_path);

    let mut cmd = isolated_cmd(&db_path, tmp.path());
    cmd.args(["feed", "list", "--format", "json"]);
    let assert = cmd.assert().success();
    let output = String::from_utf8(assert.get_output().stdout.clone()).unwrap();

    assert_golden(&output, "feed_list.golden.json");
}

#[test]
fn test_golden_account_list_json() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("mail.db");
    setup_golden_fixture_db(&db_path);

    let mut cmd = isolated_cmd(&db_path, tmp.path());
    cmd.args(["account", "list", "--json"]);
    let assert = cmd.assert().success();
    let output = String::from_utf8(assert.get_output().stdout.clone()).unwrap();

    assert_golden(&output, "account_list.golden.json");
}

#[test]
fn test_golden_daemon_status_stopped_json() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("mail.db");
    setup_golden_fixture_db(&db_path);

    let mut cmd = isolated_cmd(&db_path, tmp.path());
    cmd.args(["daemon", "status", "--format", "json"]);
    let assert = cmd.assert().success();
    let output = String::from_utf8(assert.get_output().stdout.clone()).unwrap();

    assert_golden(&output, "daemon_status_stopped.golden.json");
}

#[test]
fn test_golden_mark_mutation_json() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("mail.db");
    setup_golden_fixture_db(&db_path);

    let mut cmd = isolated_cmd(&db_path, tmp.path());
    cmd.args([
        "--no-daemon",
        "mark",
        "read",
        "3f2a9c1e",
        "--format",
        "json",
    ]);
    let assert = cmd.assert().success();
    let output = String::from_utf8(assert.get_output().stdout.clone()).unwrap();

    assert_golden(&output, "mutation_mark.golden.json");
}

#[test]
fn test_golden_error_unknown_operator_json() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("mail.db");
    setup_golden_fixture_db(&db_path);

    let mut cmd = isolated_cmd(&db_path, tmp.path());
    cmd.args(["search", "foo:bar", "--format", "json"]);
    // Exit code 2 for unknown operator
    let assert = cmd.assert().code(2);
    let output = String::from_utf8(assert.get_output().stdout.clone()).unwrap();

    assert_golden(&output, "error_unknown_operator.golden.json");
}

#[test]
fn test_golden_doctor_report_json() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("mail.db");
    setup_golden_fixture_db(&db_path);

    let mut cmd = isolated_cmd(&db_path, tmp.path());
    cmd.args(["doctor", "--json"]);
    let assert = cmd.assert().success();
    let output = String::from_utf8(assert.get_output().stdout.clone()).unwrap();

    assert_golden(&output, "doctor_report.golden.json");
}

#[test]
fn test_golden_error_not_found_json() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("mail.db");
    setup_golden_fixture_db(&db_path);

    let mut cmd = isolated_cmd(&db_path, tmp.path());
    cmd.args(["read", "00000000", "--format", "json"]);
    // Exit code 3 for not found
    let assert = cmd.assert().code(3);
    let output = String::from_utf8(assert.get_output().stdout.clone()).unwrap();

    assert_golden(&output, "error_not_found.golden.json");
}
