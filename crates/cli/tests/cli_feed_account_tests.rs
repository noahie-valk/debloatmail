use assert_cmd::Command;
use predicates::prelude::*;
use rusqlite::Connection;
use std::path::Path;
use tempfile::tempdir;

const INIT_SQL: &str = include_str!("../../store/src/migrations/0001_init.sql");

fn setup_account_fixture_db(db_path: &Path) {
    let conn = Connection::open(db_path).unwrap();
    conn.execute_batch(INIT_SQL).unwrap();

    let now = chrono::Utc::now().timestamp();

    conn.execute_batch(&format!(
        r#"
        INSERT INTO accounts (id, public_id, name, kind, email, enabled, provider, created_at, updated_at)
        VALUES
            (1, '018f3a9c-e4f2-7a1b-8c3d-9f0a1b2c3d4e', 'fastmail', 'imap', 'alice@fastmail.com', 1, 'fastmail', {now}, {now}),
            (2, '018f3a9c-e4f2-7a1b-8c3d-9f0a1b2c3d4f', 'news', 'feed', NULL, 1, NULL, {now}, {now});

        INSERT INTO folders (id, public_id, account_id, name, display_name, role, role_source, uidvalidity, uidnext, highestmodseq, last_sync_at)
        VALUES
            (1, '018f3aa1-0000-7000-8000-000000000001', 1, 'INBOX', 'Inbox', 'inbox', 'special-use', 12345, 100, 50, {now});

        INSERT INTO feeds (id, public_id, account_id, url, title, site_url, etag, last_modified, interval_secs, backoff_secs, last_fetch_at, next_fetch_at, last_error, removed_at)
        VALUES
            (1, '018f3bb2-0000-7000-8000-000000000001', 2, 'https://example.com/feed.xml', 'Example Feed', 'https://example.com', NULL, NULL, 3600, 0, {now}, {now} + 3600, NULL, NULL);
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
fn test_cli_account_management() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("mail.db");
    setup_account_fixture_db(&db_path);

    // 1. Account check
    let mut cmd_check = cmd_with_env(&tmp, &db_path);
    cmd_check.args(["account", "check", "fastmail"]);
    cmd_check
        .assert()
        .success()
        .stdout(predicate::str::contains("fastmail:"))
        .stdout(predicate::str::contains("configuration"));

    // 2. Add an IMAP account with secret-command
    let mut cmd_add = cmd_with_env(&tmp, &db_path);
    cmd_add.args([
        "account",
        "add",
        "--kind",
        "imap",
        "--name",
        "work",
        "--email",
        "alice@work.com",
        "--imap-host",
        "imap.work.com",
        "--secret-command",
        "echo secretworkpass",
    ]);
    cmd_add
        .assert()
        .success()
        .stdout(predicate::str::contains("added account 'work'"));

    // Verify account in DB
    let conn = Connection::open(&db_path).unwrap();
    let email: String = conn
        .query_row("SELECT email FROM accounts WHERE name = 'work';", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(email, "alice@work.com");

    // 3. Update account --disable
    let mut cmd_up_dis = cmd_with_env(&tmp, &db_path);
    cmd_up_dis.args(["account", "update", "work", "--disable"]);
    cmd_up_dis
        .assert()
        .success()
        .stdout(predicate::str::contains("updated account 'work'"));

    let enabled: i64 = conn
        .query_row(
            "SELECT enabled FROM accounts WHERE name = 'work';",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(enabled, 0);

    // 4. Update account --enable
    let mut cmd_up_en = cmd_with_env(&tmp, &db_path);
    cmd_up_en.args(["account", "update", "work", "--enable"]);
    cmd_up_en.assert().success();

    let enabled_again: i64 = conn
        .query_row(
            "SELECT enabled FROM accounts WHERE name = 'work';",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(enabled_again, 1);

    // 5. Remove account without purge disables it
    let mut cmd_rem = cmd_with_env(&tmp, &db_path);
    cmd_rem.args(["account", "remove", "work"]);
    cmd_rem
        .assert()
        .success()
        .stdout(predicate::str::contains("disabled account 'work'"));

    let enabled_after_rem: i64 = conn
        .query_row(
            "SELECT enabled FROM accounts WHERE name = 'work';",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(enabled_after_rem, 0);

    // 6. Remove account with --purge and -y deletes it
    let mut cmd_purge = cmd_with_env(&tmp, &db_path);
    cmd_purge.args(["account", "remove", "work", "--purge", "-y"]);
    cmd_purge
        .assert()
        .success()
        .stdout(predicate::str::contains("purged account 'work'"));

    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM accounts WHERE name = 'work';",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn test_cli_feed_management() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("mail.db");
    setup_account_fixture_db(&db_path);

    // 1. Feed update interval
    let mut cmd_up = cmd_with_env(&tmp, &db_path);
    cmd_up.args(["feed", "update", "Example Feed", "--interval", "2h"]);
    cmd_up
        .assert()
        .success()
        .stdout(predicate::str::contains("updated feed 'Example Feed'"));

    let conn = Connection::open(&db_path).unwrap();
    let interval: u32 = conn
        .query_row("SELECT interval_secs FROM feeds WHERE id = 1;", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(interval, 7200);

    // 2. Feed remove without purge soft-removes
    let mut cmd_rem = cmd_with_env(&tmp, &db_path);
    cmd_rem.args(["feed", "remove", "Example Feed"]);
    cmd_rem
        .assert()
        .success()
        .stdout(predicate::str::contains("removed feed 'Example Feed'"));

    let removed_at: Option<i64> = conn
        .query_row("SELECT removed_at FROM feeds WHERE id = 1;", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert!(removed_at.is_some());
}

#[test]
fn test_cli_sync_and_folder_sync() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("mail.db");
    setup_account_fixture_db(&db_path);

    // 1. Sync dry-run
    let mut cmd_sync = cmd_with_env(&tmp, &db_path);
    cmd_sync.args(["--dry-run", "sync", "--account", "fastmail"]);
    cmd_sync
        .assert()
        .success()
        .stdout(predicate::str::contains("would trigger sync"));

    // 2. Folder sync dry-run
    let mut cmd_fsync = cmd_with_env(&tmp, &db_path);
    cmd_fsync.args(["--dry-run", "folder", "sync", "INBOX"]);
    cmd_fsync
        .assert()
        .success()
        .stdout(predicate::str::contains("would sync folder 'INBOX'"));
}
