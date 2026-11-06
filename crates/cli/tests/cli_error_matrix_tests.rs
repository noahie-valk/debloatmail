use assert_cmd::Command;
use predicates::prelude::*;
use rusqlite::Connection;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::path::Path;
use tempfile::tempdir;

const INIT_SQL: &str = include_str!("../../../crates/store/src/migrations/0001_init.sql");

fn setup_test_db(db_path: &Path) {
    let conn = Connection::open(db_path).unwrap();
    conn.execute_batch(INIT_SQL).unwrap();

    let now = 1700000000;

    conn.execute_batch(&format!(
        r#"
        INSERT INTO accounts (id, public_id, name, kind, email, enabled, provider, created_at, updated_at)
        VALUES
            (1, '018f3a9c-e4f2-7a1b-8c3d-9f0a1b2c3d4e', 'fastmail', 'imap', 'alice@fastmail.com', 1, 'fastmail', {now}, {now});

        INSERT INTO folders (id, public_id, account_id, name, display_name, role, role_source, uidvalidity, uidnext, highestmodseq, last_sync_at)
        VALUES
            (1, '018f3aa1-0000-7000-8000-000000000001', 1, 'INBOX', 'Inbox', 'inbox', 'special-use', 12345, 100, 50, {now});

        -- Two items sharing prefix '3f' for ambiguity test
        INSERT INTO items (id, public_id, kind, account_id, title, author, published_at, sort_at, unread, starred, answered, hidden_at, thread_id, body_state, has_attachments, created_at, updated_at)
        VALUES
            (1, '3f2a9c1e-7c41-7e2a-9d3f-1a2b3c4d5e6f', 'mail', 1, 'Item One', 'Alice', {now}, {now}, 1, 0, 0, NULL, NULL, 'cached', 0, {now}, {now}),
            (2, '3f8b1022-7c41-7e2a-9d3f-1a2b3c4d5e6f', 'mail', 1, 'Item Two', 'Bob', {now}, {now}, 1, 0, 0, NULL, NULL, 'none', 0, {now}, {now});

        INSERT INTO mail_items (item_id, folder_id, uid, uidvalidity, message_id, from_addr, to_addrs, cc_addrs, subject_norm, internal_date, size_bytes)
        VALUES
            (1, 1, 101, 12345, '<msg1@fastmail.com>', 'alice@fastmail.com', '[]', '[]', 'item one', {now}, 1024),
            (2, 1, 102, 12345, '<msg2@fastmail.com>', 'bob@fastmail.com', '[]', '[]', 'item two', {now}, 2048);

        INSERT INTO item_text (item_id, title, author, body, durable, extractor_version)
        VALUES
            (1, 'Item One', 'Alice', 'Body of item one', 0, 1),
            (2, 'Item Two', 'Bob', '', 0, 1);
        "#
    )).unwrap();
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

// =============================================================================
// Exit Code 0: Success
// =============================================================================
#[test]
fn test_exit_code_0_success() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("mail.db");
    setup_test_db(&db_path);

    let mut cmd = isolated_cmd(&db_path, tmp.path());
    cmd.args(["list"]);
    cmd.assert().code(0);

    let mut cmd_ver = isolated_cmd(&db_path, tmp.path());
    cmd_ver.args(["version"]);
    cmd_ver.assert().code(0);

    let mut cmd_daemon = isolated_cmd(&db_path, tmp.path());
    cmd_daemon.args(["daemon", "status"]);
    cmd_daemon.assert().code(0);
}

// =============================================================================
// Exit Code 2: Usage / Syntax / Unknown Operator
// =============================================================================
#[test]
fn test_exit_code_2_usage_and_unknown_operator() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("mail.db");
    setup_test_db(&db_path);

    // 1. Unknown search operator
    let mut cmd_search = isolated_cmd(&db_path, tmp.path());
    cmd_search.args(["search", "nonexistent_op:val"]);
    cmd_search
        .assert()
        .code(2)
        .stderr(predicate::str::contains("unknown search operator"));

    // 2. Unknown CLI flag
    let mut cmd_bad_flag = isolated_cmd(&db_path, tmp.path());
    cmd_bad_flag.args(["list", "--definitely-not-a-valid-flag"]);
    cmd_bad_flag.assert().code(2);
}

// =============================================================================
// Exit Code 3: Not Found
// =============================================================================
#[test]
fn test_exit_code_3_not_found() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("mail.db");
    setup_test_db(&db_path);

    // Nonexistent item ID
    let mut cmd_item = isolated_cmd(&db_path, tmp.path());
    cmd_item.args(["read", "00000000"]);
    cmd_item
        .assert()
        .code(3)
        .stderr(predicate::str::contains("not found"));

    // Nonexistent account
    let mut cmd_acct = isolated_cmd(&db_path, tmp.path());
    cmd_acct.args(["account", "show", "missing-account"]);
    cmd_acct
        .assert()
        .code(3)
        .stderr(predicate::str::contains("not found"));
}

// =============================================================================
// Exit Code 4: Authentication Failed
// =============================================================================
#[test]
fn test_exit_code_4_auth_failed() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("mail.db");
    setup_test_db(&db_path);

    // Seed an account that fails security policy / credential checks
    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
            "INSERT INTO accounts (id, public_id, name, kind, email, enabled, provider, imap_host, imap_port, imap_security, smtp_host, smtp_port, smtp_security, created_at, updated_at)
             VALUES (99, '018f3a9c-0000-7a1b-8c3d-9f0a1b2c3d99', 'insecure', 'imap', 'user@remote.test', 1, NULL, 'remote.test', 143, 'none', 'remote.test', 25, 'none', 1700000000, 1700000000);",
            [],
        ).unwrap();
    }

    let mut cmd = isolated_cmd(&db_path, tmp.path());
    cmd.args(["--no-daemon", "account", "check", "insecure"]);
    cmd.assert()
        .code(4)
        .stderr(predicate::str::contains("account check failed"));
}

// =============================================================================
// Exit Code 5: Network / Connection Failure (e.g. timeout waiting for network send)
// =============================================================================
#[test]
fn test_exit_code_5_network_failure() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("mail.db");
    setup_test_db(&db_path);

    // compose --now with a fast timeout (100ms) will time out waiting for network send to complete
    let mut cmd = isolated_cmd(&db_path, tmp.path());
    cmd.args([
        "--no-daemon",
        "--timeout",
        "100ms",
        "compose",
        "--to",
        "bob@fastmail.com",
        "--subject",
        "urgent",
        "--body-stdin",
        "--now",
    ]);
    cmd.write_stdin("hello world");
    cmd.assert()
        .code(5)
        .stderr(predicate::str::contains("timed out"));
}

// =============================================================================
// Exit Code 6: Daemon Unavailable
// =============================================================================
#[test]
fn test_exit_code_6_daemon_unavailable() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("mail.db");
    setup_test_db(&db_path);

    // Mutating command without --no-daemon when daemon service is absent
    let mut cmd = isolated_cmd(&db_path, tmp.path());
    cmd.args(["mark", "read", "3f2a9c1e"]);
    cmd.assert()
        .code(6)
        .stderr(predicate::str::contains("org.noahie.debloatmail.Daemon"));
}

// =============================================================================
// Exit Code 7: Offline Mode Violation
// =============================================================================
#[test]
fn test_exit_code_7_offline_mode() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("mail.db");
    setup_test_db(&db_path);

    // Item 2 has body_state = 'none'. Reading it under --offline must exit 7.
    let mut cmd_read = isolated_cmd(&db_path, tmp.path());
    cmd_read.args(["--offline", "read", "3f8b1022"]);
    cmd_read
        .assert()
        .code(7)
        .stderr(predicate::str::contains("offline"));

    // Network command under --offline must exit 7
    let mut cmd_sync = isolated_cmd(&db_path, tmp.path());
    cmd_sync.args(["--offline", "sync"]);
    cmd_sync
        .assert()
        .code(7)
        .stderr(predicate::str::contains("offline"));
}

// =============================================================================
// Exit Code 8: Lock Held (Standalone Mode Contention)
// =============================================================================
#[test]
fn test_exit_code_8_lock_held_contention() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("mail.db");
    setup_test_db(&db_path);

    let state_dir = tmp.path().join("state").join("debloatmail");
    fs::create_dir_all(&state_dir).unwrap();
    let lock_path = state_dir.join("debloatmail-standalone.lock");

    // Acquire exclusive non-blocking flock in this process
    let lock_file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(&lock_path)
        .unwrap();

    let fd = lock_file.as_raw_fd();
    let flock_res = unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) };
    assert_eq!(flock_res, 0, "parent must acquire lock");

    // Write parent PID into lockfile
    let parent_pid = std::process::id();
    (&lock_file)
        .write_all(format!("{parent_pid}\n").as_bytes())
        .unwrap();

    // Spawn child standalone CLI command which should detect the lock contention
    let mut cmd = isolated_cmd(&db_path, tmp.path());
    cmd.args(["--no-daemon", "mark", "read", "3f2a9c1e"]);
    cmd.assert()
        .code(8)
        .stderr(predicate::str::contains("lock held"))
        .stderr(predicate::str::contains(parent_pid.to_string()));

    // Release lock
    unsafe { libc::flock(fd, libc::LOCK_UN) };
}

// =============================================================================
// Exit Code 9: Ambiguous Short ID Prefix
// =============================================================================
#[test]
fn test_exit_code_9_ambiguous_prefix() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("mail.db");
    setup_test_db(&db_path);

    // Prefix '3f' matches both 3f2a9c1e and 3f8b1022
    let mut cmd = isolated_cmd(&db_path, tmp.path());
    cmd.args(["read", "3f"]);
    cmd.assert()
        .code(9)
        .stderr(predicate::str::contains("is ambiguous"))
        .stderr(predicate::str::contains("3f2a9c1e"))
        .stderr(predicate::str::contains("3f8b1022"));
}
