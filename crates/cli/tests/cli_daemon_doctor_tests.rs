use assert_cmd::Command;
use predicates::prelude::*;
use rusqlite::Connection;
use std::path::Path;
use tempfile::tempdir;

const INIT_SQL: &str = include_str!("../../store/src/migrations/0001_init.sql");

fn setup_doctor_fixture_db(db_path: &Path) {
    let conn = Connection::open(db_path).unwrap();
    conn.execute_batch(INIT_SQL).unwrap();

    let now = chrono::Utc::now().timestamp();

    conn.execute_batch(&format!(
        r#"
        INSERT INTO accounts (id, public_id, name, kind, email, enabled, provider, created_at, updated_at)
        VALUES
            (1, '018f3a9c-e4f2-7a1b-8c3d-9f0a1b2c3d4e', 'fastmail', 'imap', 'alice@fastmail.com', 1, 'fastmail', {now}, {now});

        INSERT INTO folders (id, public_id, account_id, name, display_name, role, role_source, uidvalidity, uidnext, highestmodseq, last_sync_at)
        VALUES
            (1, '018f3aa1-0000-7000-8000-000000000001', 1, 'INBOX', 'Inbox', 'inbox', 'special-use', 12345, 100, 50, {now});

        INSERT INTO items (id, public_id, kind, account_id, title, author, published_at, sort_at, unread, starred, answered, hidden_at, thread_id, body_state, has_attachments, created_at, updated_at)
        VALUES
            (1, '3f2a9c1e-7c41-7e2a-9d3f-1a2b3c4d5e6f', 'mail', 1, 'Welcome', 'Alice', {now}, {now}, 0, 0, 0, NULL, NULL, 'cached', 0, {now}, {now});

        INSERT INTO mail_items (item_id, folder_id, uid, uidvalidity, message_id, from_addr, to_addrs, cc_addrs, subject_norm, internal_date, size_bytes)
        VALUES
            (1, 1, 101, 12345, '<msg1@fastmail.com>', 'alice@fastmail.com', '[]', '[]', 'welcome', {now}, 100);

        INSERT INTO item_text (item_id, title, author, body, durable, extractor_version)
        VALUES
            (1, 'Welcome', 'Alice', 'Welcome to debloatmail.', 0, 1);
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
fn test_cli_daemon_status_when_stopped() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("mail.db");
    setup_doctor_fixture_db(&db_path);

    // 1. Human format: exits 0 and prints Daemon: stopped
    let mut cmd = cmd_with_env(&tmp, &db_path);
    cmd.args(["daemon", "status"]);
    cmd.assert()
        .success()
        .stdout(predicate::str::contains("Daemon: stopped"));

    // 2. JSON format: exits 0 and returns valid JSON {"daemon": {"running": false}}
    let mut cmd_json = cmd_with_env(&tmp, &db_path);
    cmd_json.args(["--format", "json", "daemon", "status"]);
    cmd_json
        .assert()
        .success()
        .stdout(predicate::str::contains("\"running\": false"));
}

#[test]
fn test_cli_doctor_diagnostics() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("mail.db");
    setup_doctor_fixture_db(&db_path);

    // 1. Human doctor run
    let mut cmd = cmd_with_env(&tmp, &db_path);
    cmd.args(["doctor"]);
    cmd.assert()
        .success()
        .stdout(predicate::str::contains("debloatmail Doctor Diagnostics"))
        .stdout(predicate::str::contains("configuration"))
        .stdout(predicate::str::contains("database_version"))
        .stdout(predicate::str::contains("sqlite_integrity"))
        .stdout(predicate::str::contains("fts5_consistency"));

    // 2. JSON doctor run
    let mut cmd_json = cmd_with_env(&tmp, &db_path);
    cmd_json.args(["doctor", "--json"]);
    cmd_json
        .assert()
        .success()
        .stdout(predicate::str::contains("\"diagnostics\":"))
        .stdout(predicate::str::contains("\"name\": \"database_version\""))
        .stdout(predicate::str::contains("\"name\": \"fts5_consistency\""));
}
