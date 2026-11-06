use assert_cmd::Command;
use predicates::prelude::*;
use rusqlite::Connection;
use std::fs;
use std::path::Path;
use tempfile::tempdir;

const INIT_SQL: &str = include_str!("../../store/src/migrations/0001_init.sql");

fn setup_compose_fixture_db(db_path: &Path, blobs_dir: &Path) {
    let conn = Connection::open(db_path).unwrap();
    conn.execute_batch(INIT_SQL).unwrap();

    let now = chrono::Utc::now().timestamp();

    // Create a sample blob
    fs::create_dir_all(blobs_dir).unwrap();
    let sample_blob = b"Hello from attachment raw blob";
    let blob_hash = "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789";
    let blob_path = debloatmail_store::blobs::blob_path(blobs_dir, blob_hash).unwrap();
    if let Some(parent) = blob_path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(&blob_path, sample_blob).unwrap();

    conn.execute_batch(&format!(
        r#"
        INSERT INTO accounts (id, public_id, name, kind, email, enabled, provider, created_at, updated_at)
        VALUES
            (1, '018f3a9c-e4f2-7a1b-8c3d-9f0a1b2c3d4e', 'fastmail', 'imap', 'alice@fastmail.com', 1, 'fastmail', {now}, {now}),
            (2, '018f3a9c-e4f2-7a1b-8c3d-9f0a1b2c3d4f', 'blogs', 'feed', NULL, 1, NULL, {now}, {now});

        INSERT INTO folders (id, public_id, account_id, name, display_name, role, role_source, uidvalidity, uidnext, highestmodseq, last_sync_at)
        VALUES
            (1, '018f3aa1-0000-7000-8000-000000000001', 1, 'INBOX', 'Inbox', 'inbox', 'special-use', 12345, 100, 50, {now});

        INSERT INTO blobs (hash, size_bytes, refcount, last_used_at, created_at)
        VALUES
            ('{blob_hash}', 30, 1, {now}, {now});

        -- Mail Item 1
        INSERT INTO items (id, public_id, kind, account_id, title, author, published_at, sort_at, unread, starred, answered, hidden_at, thread_id, body_state, has_attachments, created_at, updated_at)
        VALUES
            (1, '3f2a9c1e-7c41-7e2a-9d3f-1a2b3c4d5e6f', 'mail', 1, 'Project Alpha Status', 'Bob Martinez', {now} - 100, {now} - 100, 1, 0, 0, NULL, NULL, 'cached', 1, {now}, {now});

        INSERT INTO mail_items (item_id, folder_id, uid, uidvalidity, message_id, from_addr, to_addrs, cc_addrs, subject_norm, internal_date, size_bytes)
        VALUES
            (1, 1, 101, 12345, '<msg1@fastmail.com>', 'bob@fastmail.com', '["alice@fastmail.com"]', '["carol@fastmail.com"]', 'project alpha status', {now} - 100, 1024);

        INSERT INTO item_text (item_id, title, author, body, durable, extractor_version)
        VALUES
            (1, 'Project Alpha Status', 'Bob Martinez', 'Here is the weekly update on Alpha.', 0, 1);

        INSERT INTO attachments (item_id, section_id, filename, mime_type, size_bytes, blob_hash)
        VALUES
            (1, '2', 'report.pdf', 'application/pdf', 30, '{blob_hash}');

        -- Feed Item 1
        INSERT INTO items (id, public_id, kind, account_id, title, author, published_at, sort_at, unread, starred, answered, hidden_at, thread_id, body_state, has_attachments, created_at, updated_at)
        VALUES
            (2, '7b0d44a2-7c41-7e2a-9d3f-1a2b3c4d5e6f', 'feed', 2, 'Rust Article', 'Rust Team', {now} - 50, {now} - 50, 1, 0, 0, NULL, NULL, 'cached', 0, {now}, {now});
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
fn test_cli_compose_dry_run_and_save_draft() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("mail.db");
    let blobs_dir = tmp.path().join("blobs");
    setup_compose_fixture_db(&db_path, &blobs_dir);

    // 1. Compose dry-run
    let mut cmd_dry = cmd_with_env(&tmp, &db_path);
    cmd_dry
        .args([
            "compose",
            "--to",
            "charlie@example.com",
            "--subject",
            "Status Check",
            "--body-stdin",
            "--dry-run",
        ])
        .write_stdin("Hello Charlie, are you free today?\n");

    cmd_dry
        .assert()
        .success()
        .stdout(predicate::str::contains("From: alice@fastmail.com"))
        .stdout(predicate::str::contains("To: charlie@example.com"))
        .stdout(predicate::str::contains("Subject: Status Check"))
        .stdout(predicate::str::contains(
            "Hello Charlie, are you free today?",
        ));

    // 2. Compose save-draft
    let mut cmd_draft = cmd_with_env(&tmp, &db_path);
    cmd_draft
        .args([
            "compose",
            "--to",
            "charlie@example.com",
            "--subject",
            "Draft Message",
            "--body-stdin",
            "--save-draft",
        ])
        .write_stdin("Draft content here.\n");

    cmd_draft
        .assert()
        .success()
        .stdout(predicate::str::contains("saved draft"));

    let conn = Connection::open(&db_path).unwrap();
    let draft_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM drafts;", [], |r| r.get(0))
        .unwrap();
    assert_eq!(draft_count, 1);

    // 3. Compose queueing
    let mut cmd_send = cmd_with_env(&tmp, &db_path);
    cmd_send
        .args([
            "compose",
            "--to",
            "charlie@example.com",
            "--subject",
            "Queued",
            "--body-stdin",
        ])
        .write_stdin("Message to be queued.\n");

    cmd_send
        .assert()
        .success()
        .stdout(predicate::str::contains("queued op"));

    let pending_ops: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pending_ops WHERE op = 'send';",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(pending_ops, 1);
}

#[test]
fn test_cli_reply_and_forward() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("mail.db");
    let blobs_dir = tmp.path().join("blobs");
    setup_compose_fixture_db(&db_path, &blobs_dir);

    // 1. Reply dry-run with --quote
    let mut cmd_reply = cmd_with_env(&tmp, &db_path);
    cmd_reply
        .args(["reply", "3f2a", "--quote", "--body-stdin", "--dry-run"])
        .write_stdin("Thanks for the update, Bob.\n");

    cmd_reply
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Subject: Re: Project Alpha Status",
        ))
        .stdout(predicate::str::contains("To: bob@fastmail.com"))
        .stdout(predicate::str::contains("In-Reply-To: <msg1@fastmail.com>"))
        .stdout(predicate::str::contains("Thanks for the update, Bob."))
        .stdout(predicate::str::contains(
            "> Here is the weekly update on Alpha.",
        ));

    // 2. Reply to feed item exits 2
    let mut cmd_reply_feed = cmd_with_env(&tmp, &db_path);
    cmd_reply_feed.args(["reply", "7b0d"]);
    cmd_reply_feed
        .assert()
        .failure()
        .code(2)
        .stderr(predicate::str::contains("feed items have no sender"));

    // 3. Forward dry-run
    let mut cmd_fwd = cmd_with_env(&tmp, &db_path);
    cmd_fwd
        .args([
            "forward",
            "3f2a",
            "--to",
            "david@example.com",
            "--body-stdin",
            "--dry-run",
        ])
        .write_stdin("Forwarding for review.\n");

    cmd_fwd
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Subject: Fwd: Project Alpha Status",
        ))
        .stdout(predicate::str::contains("To: david@example.com"))
        .stdout(predicate::str::contains("Forwarding for review."))
        .stdout(predicate::str::contains(
            "---------- Forwarded message ---------",
        ));
}

#[test]
fn test_cli_attachment_commands() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("mail.db");
    let blobs_dir = tmp.path().join("debloatmail/blobs");
    setup_compose_fixture_db(&db_path, &blobs_dir);

    // 1. Attachment list
    let mut cmd_list = cmd_with_env(&tmp, &db_path);
    cmd_list.args(["attachment", "list", "3f2a"]);
    cmd_list
        .assert()
        .success()
        .stdout(predicate::str::contains("report.pdf"))
        .stdout(predicate::str::contains("application/pdf"));

    // 2. Attachment download
    let out_file = tmp.path().join("downloaded.pdf");
    let mut cmd_dl = cmd_with_env(&tmp, &db_path);
    cmd_dl.args([
        "attachment",
        "download",
        "3f2a",
        "2",
        "--output",
        out_file.to_str().unwrap(),
    ]);
    cmd_dl
        .assert()
        .success()
        .stdout(predicate::str::contains("saved to"));

    assert!(out_file.exists());
    let content = fs::read_to_string(&out_file).unwrap();
    assert_eq!(content, "Hello from attachment raw blob");
}
