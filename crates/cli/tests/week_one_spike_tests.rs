use assert_cmd::Command;
use predicates::prelude::*;
use rusqlite::Connection;
use std::path::Path;
use tempfile::tempdir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

const INIT_SQL: &str = include_str!("../../../crates/store/src/migrations/0001_init.sql");

const SAMPLE_RSS: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<rss version="2.0">
  <channel>
    <title>Rust Blog</title>
    <link>https://blog.rust-lang.org</link>
    <description>Empowering everyone to build reliable and efficient software.</description>
    <item>
      <title>Announcing Rust 1.99.1</title>
      <link>https://blog.rust-lang.org/2026/01/15/Rust-1.99.1.html</link>
      <guid>https://blog.rust-lang.org/2026/01/15/Rust-1.99.1.html</guid>
      <pubDate>Thu, 15 Jan 2026 00:00:00 +0000</pubDate>
      <author>Rust Core Team</author>
      <description>The Rust team is happy to announce a new version of Rust, 1.99.1.</description>
    </item>
  </channel>
</rss>"#;

async fn start_mock_imap() -> (u16, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let handle = tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let (reader, mut writer) = socket.split();
                let mut buf_reader = BufReader::new(reader);
                if writer
                    .write_all(b"* OK [CAPABILITY IMAP4rev1 AUTH=PLAIN SASL-IR UIDPLUS] Mock IMAP ready\r\n")
                    .await
                    .is_err()
                {
                    return;
                }
                let _ = writer.flush().await;

                let mut line = String::new();
                while let Ok(n) = buf_reader.read_line(&mut line).await {
                    if n == 0 {
                        break;
                    }
                    let tag = line.split_whitespace().next().unwrap_or("A1").to_string();
                    let upper = line.to_uppercase();
                    if upper.contains("CAPABILITY") {
                        let _ = writer
                            .write_all(b"* CAPABILITY IMAP4rev1 AUTH=PLAIN SASL-IR UIDPLUS\r\n")
                            .await;
                        let _ = writer
                            .write_all(format!("{tag} OK CAPABILITY completed\r\n").as_bytes())
                            .await;
                    } else if upper.contains("AUTHENTICATE") || upper.contains("LOGIN") {
                        let _ = writer
                            .write_all(
                                format!("{tag} OK [CAPABILITY IMAP4rev1 AUTH=PLAIN SASL-IR UIDPLUS] Logged in\r\n")
                                    .as_bytes(),
                            )
                            .await;
                    } else if upper.contains("LIST") {
                        let _ = writer
                            .write_all(b"* LIST (\\HasNoChildren \\Inbox) \"/\" \"INBOX\"\r\n")
                            .await;
                        let _ = writer
                            .write_all(format!("{tag} OK LIST completed\r\n").as_bytes())
                            .await;
                    } else if upper.contains("SELECT") {
                        let _ = writer
                            .write_all(b"* 1 EXISTS\r\n* OK [UIDVALIDITY 12345] UIDs valid\r\n* OK [UIDNEXT 100] Next UID\r\n")
                            .await;
                        let _ = writer
                            .write_all(
                                format!("{tag} OK [READ-WRITE] SELECT completed\r\n").as_bytes(),
                            )
                            .await;
                    } else if upper.contains("LOGOUT") {
                        let _ = writer.write_all(b"* BYE logging out\r\n").await;
                        let _ = writer
                            .write_all(format!("{tag} OK LOGOUT completed\r\n").as_bytes())
                            .await;
                        let _ = writer.flush().await;
                        break;
                    } else {
                        let _ = writer
                            .write_all(format!("{tag} OK completed\r\n").as_bytes())
                            .await;
                    }
                    let _ = writer.flush().await;
                    line.clear();
                }
            });
        }
    });

    (port, handle)
}

async fn start_mock_smtp() -> (u16, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let handle = tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let (reader, mut writer) = socket.split();
                let mut buf_reader = BufReader::new(reader);
                if writer
                    .write_all(b"220 127.0.0.1 ESMTP Mock SMTP\r\n")
                    .await
                    .is_err()
                {
                    return;
                }
                let _ = writer.flush().await;

                let mut line = String::new();
                while let Ok(n) = buf_reader.read_line(&mut line).await {
                    if n == 0 {
                        break;
                    }
                    let upper = line.to_uppercase();
                    if upper.starts_with("EHLO") || upper.starts_with("HELO") {
                        let _ = writer
                            .write_all(b"250-127.0.0.1\r\n250 AUTH PLAIN\r\n")
                            .await;
                    } else if upper.starts_with("AUTH") {
                        let _ = writer
                            .write_all(b"235 2.7.0 Authentication successful\r\n")
                            .await;
                    } else if upper.starts_with("MAIL FROM") {
                        let _ = writer.write_all(b"250 2.1.0 Ok\r\n").await;
                    } else if upper.starts_with("RCPT TO") {
                        let _ = writer.write_all(b"250 2.1.5 Ok\r\n").await;
                    } else if upper.starts_with("DATA") {
                        let _ = writer
                            .write_all(b"354 End data with <CR><LF>.<CR><LF>\r\n")
                            .await;
                        let _ = writer.flush().await;
                        let mut data_line = String::new();
                        while let Ok(dn) = buf_reader.read_line(&mut data_line).await {
                            if dn == 0 || data_line == ".\r\n" || data_line == ".\n" {
                                break;
                            }
                            data_line.clear();
                        }
                        let _ = writer.write_all(b"250 2.0.0 Ok: queued as 12345\r\n").await;
                    } else if upper.starts_with("QUIT") {
                        let _ = writer.write_all(b"221 2.0.0 Bye\r\n").await;
                        let _ = writer.flush().await;
                        break;
                    } else {
                        let _ = writer.write_all(b"250 Ok\r\n").await;
                    }
                    let _ = writer.flush().await;
                    line.clear();
                }
            });
        }
    });

    (port, handle)
}

async fn start_mock_feed_server() -> (u16, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let handle = tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let (reader, mut writer) = socket.split();
                let mut buf_reader = BufReader::new(reader);
                let mut req_line = String::new();
                if buf_reader.read_line(&mut req_line).await.is_ok() {
                    let mut h = String::new();
                    while let Ok(hn) = buf_reader.read_line(&mut h).await {
                        if hn <= 2 && (h == "\r\n" || h == "\n") {
                            break;
                        }
                        h.clear();
                    }
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/rss+xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        SAMPLE_RSS.len(),
                        SAMPLE_RSS
                    );
                    let _ = writer.write_all(resp.as_bytes()).await;
                    let _ = writer.flush().await;
                }
            });
        }
    });

    (port, handle)
}

fn isolated_cmd(db_path: &Path, tmp_dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("debloatmailctl").unwrap();
    cmd.env("XDG_STATE_HOME", tmp_dir.join("state"))
        .env("XDG_CACHE_HOME", tmp_dir.join("cache"))
        .env("XDG_CONFIG_HOME", tmp_dir.join("config"))
        .env("XDG_DATA_HOME", tmp_dir.join("data"))
        .arg("--no-daemon")
        .arg("--db")
        .arg(db_path.to_str().unwrap());
    cmd
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_week_one_validation_spike_scenario() {
    let (imap_port, _imap_srv) = start_mock_imap().await;
    let (smtp_port, _smtp_srv) = start_mock_smtp().await;
    let (feed_port, _feed_srv) = start_mock_feed_server().await;

    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("mail.db");

    // Initialize clean SQLite database schema
    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute_batch(INIT_SQL).unwrap();
    }

    // =========================================================================
    // Step 1: Configure a Fastmail account (secrets from keyring/secret-command, never argv)
    // =========================================================================
    let mut cmd_step1 = isolated_cmd(&db_path, tmp.path());
    cmd_step1.args([
        "account",
        "add",
        "--kind",
        "imap",
        "--name",
        "fastmail",
        "--provider",
        "fastmail",
        "--email",
        "me@fastmail.com",
        "--secret-command",
        "printf 'app-password-123'",
        "--imap-host",
        "127.0.0.1",
        "--imap-port",
        &imap_port.to_string(),
        "--imap-security",
        "none",
        "--smtp-host",
        "127.0.0.1",
        "--smtp-port",
        &smtp_port.to_string(),
        "--smtp-security",
        "none",
    ]);
    cmd_step1
        .assert()
        .success()
        .stdout(predicate::str::contains("added account 'fastmail'"));

    // =========================================================================
    // Step 2: Validate before syncing (account check)
    // =========================================================================
    let mut cmd_step2 = isolated_cmd(&db_path, tmp.path());
    cmd_step2.args(["account", "check", "fastmail"]);
    cmd_step2
        .assert()
        .success()
        .stdout(predicate::str::contains("fastmail:"))
        .stdout(predicate::str::contains("keyring        OK"))
        .stdout(predicate::str::contains("imap_auth      OK"))
        .stdout(predicate::str::contains("imap_mailbox   OK"))
        .stdout(predicate::str::contains("smtp_auth      OK"));

    // =========================================================================
    // Step 3: Initial header sync (standalone — no daemon yet)
    // =========================================================================
    // Seed an INBOX folder and a cached header item to simulate header sync outcome
    {
        let conn = Connection::open(&db_path).unwrap();
        let now = 1768435200; // 2026-01-15T00:00:00Z
        conn.execute_batch(&format!(
            r#"
            INSERT INTO folders (id, public_id, account_id, name, display_name, role, role_source, uidvalidity, uidnext, highestmodseq, last_sync_at)
            VALUES (1, '018f3aa1-0000-7000-8000-000000000001', 1, 'INBOX', 'Inbox', 'inbox', 'special-use', 12345, 101, 50, {now});

            INSERT INTO items (id, public_id, kind, account_id, title, author, published_at, sort_at, unread, starred, answered, hidden_at, thread_id, body_state, has_attachments, created_at, updated_at)
            VALUES (1, '018f3a2b-7c41-7e2a-9d3f-1a2b3c4d5e6f', 'mail', 1, 'Welcome to Fastmail', 'Fastmail Support', {now}, {now}, 1, 0, 0, NULL, NULL, 'none', 0, {now}, {now});

            INSERT INTO mail_items (item_id, folder_id, uid, uidvalidity, message_id, from_addr, to_addrs, cc_addrs, subject_norm, internal_date, size_bytes)
            VALUES (1, 1, 100, 12345, '<welcome@fastmail.com>', 'support@fastmail.com', '["me@fastmail.com"]', '[]', 'welcome to fastmail', {now}, 512);

            INSERT INTO item_text (item_id, title, author, body, durable, extractor_version)
            VALUES (1, 'Welcome to Fastmail', 'Fastmail Support', '', 0, 1);
            "#
        )).unwrap();
    }

    let mut cmd_step3 = isolated_cmd(&db_path, tmp.path());
    cmd_step3.args(["sync", "--wait", "--full"]);
    cmd_step3
        .assert()
        .success()
        .stdout(predicate::str::contains("synced"));

    // =========================================================================
    // Step 4: The unified list, from the local cache
    // =========================================================================
    let mut cmd_step4 = isolated_cmd(&db_path, tmp.path());
    cmd_step4.args(["list", "--limit", "20"]);
    cmd_step4
        .assert()
        .success()
        .stdout(predicate::str::contains("018f3a2b"))
        .stdout(predicate::str::contains("Welcome to Fastmail"))
        .stdout(predicate::str::contains("Fastmail Support"));

    // =========================================================================
    // Step 5: On-demand body fetch + plaintext read
    // =========================================================================
    let mut cmd_step5 = isolated_cmd(&db_path, tmp.path());
    cmd_step5.args(["read", "018f3a2b", "--mark-read"]);
    cmd_step5
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Content of message: Welcome to Fastmail",
        ));

    // Verify mark-read took effect: item is no longer unread
    {
        let conn = Connection::open(&db_path).unwrap();
        let unread: i64 = conn
            .query_row(
                "SELECT unread FROM items WHERE public_id = '018f3a2b-7c41-7e2a-9d3f-1a2b3c4d5e6f';",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(unread, 0, "read --mark-read must clear unread flag");
    }

    // =========================================================================
    // Step 6: Offline read (proves the cache is real)
    // =========================================================================
    let mut cmd_step6 = isolated_cmd(&db_path, tmp.path());
    cmd_step6.args(["--offline", "read", "018f3a2b"]);
    cmd_step6
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Content of message: Welcome to Fastmail",
        ));

    // Verify offline read on an uncached item fails with contractual exit code 7
    {
        let conn = Connection::open(&db_path).unwrap();
        conn.execute(
            "INSERT INTO items (id, public_id, kind, account_id, title, author, published_at, sort_at, unread, starred, answered, hidden_at, thread_id, body_state, has_attachments, created_at, updated_at)
             VALUES (99, '018f3999-7c41-7e2a-9d3f-1a2b3c4d5e6f', 'mail', 1, 'Uncached Item', 'Sender', 1700000000, 1700000000, 1, 0, 0, NULL, NULL, 'none', 0, 1700000000, 1700000000);",
            [],
        ).unwrap();
    }
    let mut cmd_step6_offline_uncached = isolated_cmd(&db_path, tmp.path());
    cmd_step6_offline_uncached.args(["--offline", "read", "018f3999"]);
    cmd_step6_offline_uncached
        .assert()
        .code(7)
        .stderr(predicate::str::contains("offline"));

    // =========================================================================
    // Step 7: Queue and send a plaintext message
    // =========================================================================
    let mut cmd_step7_compose = isolated_cmd(&db_path, tmp.path());
    cmd_step7_compose.args([
        "compose",
        "--to",
        "someone@example.com",
        "--subject",
        "test",
        "--body-stdin",
    ]);
    cmd_step7_compose.write_stdin("hello world message body");
    cmd_step7_compose
        .assert()
        .success()
        .stdout(predicate::str::contains("queued op"));

    // Verify pending_ops has 1 pending send operation
    {
        let conn = Connection::open(&db_path).unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pending_ops WHERE op = 'send' AND state = 'pending';",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "compose must enqueue op into pending_ops");
    }

    // Drain the send queue with standalone sync
    let mut cmd_step7_sync = isolated_cmd(&db_path, tmp.path());
    cmd_step7_sync.args(["sync", "--wait"]);
    cmd_step7_sync
        .assert()
        .success()
        .stdout(predicate::str::contains("synced"));

    // Verify pending op has drained to 'done'
    {
        let conn = Connection::open(&db_path).unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM pending_ops WHERE op = 'send' AND state = 'done';",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1, "sync must drain pending send op to done");
    }

    // =========================================================================
    // Step 8: Add feed under feed account, then see mail and feeds in one list
    // =========================================================================
    let mut cmd_step8_acct = isolated_cmd(&db_path, tmp.path());
    cmd_step8_acct.args(["account", "add", "--kind", "feed", "--name", "blogs"]);
    cmd_step8_acct
        .assert()
        .success()
        .stdout(predicate::str::contains("added account 'blogs'"));

    let feed_url = format!("http://127.0.0.1:{feed_port}/feed.xml");
    let mut cmd_step8_feed = isolated_cmd(&db_path, tmp.path());
    cmd_step8_feed.args(["feed", "add", &feed_url, "--account", "blogs"]);
    cmd_step8_feed.assert().success();

    let mut cmd_step8_sync = isolated_cmd(&db_path, tmp.path());
    cmd_step8_sync.args(["sync", "--wait"]);
    cmd_step8_sync
        .assert()
        .success()
        .stdout(predicate::str::contains("synced"));

    // Unified stream: both mail and feeds in one list
    let mut cmd_step8_list = isolated_cmd(&db_path, tmp.path());
    cmd_step8_list.args(["list", "--limit", "20"]);
    cmd_step8_list
        .assert()
        .success()
        .stdout(predicate::str::contains("Welcome to Fastmail")) // Mail item
        .stdout(predicate::str::contains("Announcing Rust 1.99.1")); // Feed item

    // =========================================================================
    // Step 9: Search across both kinds
    // =========================================================================
    let mut cmd_step9_search = isolated_cmd(&db_path, tmp.path());
    cmd_step9_search.args(["search", "rust after:2026-01-01"]);
    cmd_step9_search
        .assert()
        .success()
        .stdout(predicate::str::contains("Announcing Rust 1.99.1"));
}
