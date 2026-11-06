//! Integration tests for dual-transport read parity and standalone lock contention.

use tempfile::tempdir;

use debloatmail_core::SortKey;
use debloatmail_ffi::{
    DbusEngine, Engine, EngineReads, FfiBodyState, FfiError, FfiItemKind, InProcessEngine,
    StandaloneLock,
};
use debloatmail_store::schema::open_store;

fn seed_test_database(conn: &rusqlite::Connection) {
    // 1. Account
    conn.execute(
        "INSERT INTO accounts (id, public_id, name, kind, email, enabled, provider, secret_ref, created_at, updated_at)
         VALUES (1, '018f3a9c-0000-7000-8000-000000000001', 'Work', 'imap', 'work@example.com', 1, 'fastmail', 'sec_1', 1700000000, 1700000000);",
        [],
    ).unwrap();

    // 2. Folders
    conn.execute(
        "INSERT INTO folders (id, public_id, account_id, name, display_name, delimiter, role, role_source, last_sync_at)
         VALUES (10, '018f3aa1-0000-7000-8000-000000000001', 1, 'INBOX', 'INBOX', '/', 'inbox', 'special-use', 1700000500);",
        [],
    ).unwrap();
    conn.execute(
        "INSERT INTO folders (id, public_id, account_id, name, display_name, delimiter, role, role_source, last_sync_at)
         VALUES (11, '018f3aa1-0000-7000-8000-000000000002', 1, 'Archive', 'Archive', '/', 'archive', 'name', 1700000500);",
        [],
    ).unwrap();

    // 3. Threads
    conn.execute(
        "INSERT INTO threads (id, public_id, account_id, root_msgid, subject_norm, display_subject, last_activity)
         VALUES (100, '018f3ab1-0000-7000-8000-000000000001', 1, 'msg001@example.com', 'quarterly planning kick-off', 'Quarterly Planning', 1700001200);",
        [],
    ).unwrap();

    // 4. Items (Mail)
    conn.execute(
        "INSERT INTO items (
            id, public_id, account_id, kind, title, author, published_at, sort_at,
            unread, starred, answered, hidden_at, thread_id, body_state, has_attachments,
            created_at, updated_at
         ) VALUES (
            1001, '018f3ac1-0000-7000-8000-000000000001', 1, 'mail', 'Quarterly Planning kick-off',
            'Boss <boss@example.com>', 1700001000, 1700001000, 0, 1, 0, NULL, 100, 'cached', 0,
            1700001000, 1700001000
         );",
        [],
    )
    .unwrap();

    conn.execute(
        "INSERT INTO mail_items (
            item_id, folder_id, uid, uidvalidity, message_id, in_reply_to, references_hdr,
            subject_norm, from_addr, to_addrs, cc_addrs, reply_to, internal_date, size_bytes
         ) VALUES (
            1001, 10, 501, 1, 'msg001@example.com', NULL, NULL,
            'quarterly planning kick-off', 'boss@example.com',
            '[\"work@example.com\"]', '[]', NULL, 1700001000, 2048
         );",
        [],
    )
    .unwrap();

    conn.execute(
        "INSERT INTO item_text (item_id, title, author, body)
         VALUES (1001, 'Quarterly Planning kick-off', 'Boss', 'Please review the attached quarterly goals.');",
        [],
    ).unwrap();

    // Second item (unread mail in thread)
    conn.execute(
        "INSERT INTO items (
            id, public_id, account_id, kind, title, author, published_at, sort_at,
            unread, starred, answered, hidden_at, thread_id, body_state, has_attachments,
            created_at, updated_at
         ) VALUES (
            1002, '018f3ac1-0000-7000-8000-000000000002', 1, 'mail', 'Re: Quarterly Planning kick-off',
            'Colleague <peer@example.com>', 1700001200, 1700001200, 1, 0, 0, NULL, 100, 'none', 1,
            1700001200, 1700001200
         );",
        [],
    ).unwrap();

    conn.execute(
        "INSERT INTO mail_items (
            item_id, folder_id, uid, uidvalidity, message_id, in_reply_to, references_hdr,
            subject_norm, from_addr, to_addrs, cc_addrs, reply_to, internal_date, size_bytes
         ) VALUES (
            1002, 10, 502, 1, 'msg002@example.com', 'msg001@example.com', 'msg001@example.com',
            're: quarterly planning kick-off', 'peer@example.com',
            '[\"work@example.com\"]', '[]', NULL, 1700001200, 4096
         );",
        [],
    )
    .unwrap();

    // 5. Feed Account and Feed
    conn.execute(
        "INSERT INTO accounts (id, public_id, name, kind, email, enabled, provider, secret_ref, created_at, updated_at)
         VALUES (2, '018f3a9c-0000-7000-8000-000000000002', 'Tech News', 'feed', '', 1, 'generic', '', 1700000000, 1700000000);",
        [],
    ).unwrap();

    conn.execute(
        "INSERT INTO feeds (id, public_id, account_id, url, title, site_url, interval_secs)
         VALUES (20, '018f3ad1-0000-7000-8000-000000000001', 2, 'https://example.com/rss.xml', 'Rust Blog', 'https://example.com', 3600);",
        [],
    ).unwrap();

    // Feed Item
    conn.execute(
        "INSERT INTO items (
            id, public_id, account_id, kind, title, author, published_at, sort_at,
            unread, starred, answered, hidden_at, thread_id, body_state, has_attachments,
            created_at, updated_at
         ) VALUES (
            2001, '018f3ac1-0000-7000-8000-000000000003', 2, 'feed', 'TWiR 500',
            'TWiR Team', 1700000800, 1700000800, 1, 0, 0, NULL, NULL, 'cached', 0,
            1700000800, 1700000800
         );",
        [],
    )
    .unwrap();

    conn.execute(
        "INSERT INTO feed_items (item_id, feed_id, guid, link, author)
         VALUES (2001, 20, 'guid-twir-500', 'https://example.com/twir-500', 'TWiR Team');",
        [],
    )
    .unwrap();

    conn.execute(
        "INSERT INTO item_text (item_id, title, author, body)
         VALUES (2001, 'TWiR 500', 'TWiR Team', 'Highlights of Rust 2026 developments.');",
        [],
    )
    .unwrap();

    // 6. Pending Ops
    conn.execute(
        "INSERT INTO pending_ops (id, public_id, account_id, op, item_id, payload, state, attempts, created_at)
         VALUES (1, '018f3ae1-0000-7000-8000-000000000001', 1, 'flag', 1001, '{\"flag\":\"seen\"}', 'pending', 0, 1700001050);",
        [],
    ).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn test_in_process_reads_match_store() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("debloatmail.db");
    let blobs_dir = tmp.path().join("blobs");
    std::fs::create_dir_all(&blobs_dir).unwrap();

    {
        let conn = open_store(&db_path).unwrap();
        seed_test_database(&conn);
    }

    let lock_path = tmp.path().join("standalone.lock");
    let engine =
        InProcessEngine::open_standalone(db_path.clone(), blobs_dir.clone(), Some(lock_path))
            .expect("InProcessEngine::open_standalone should succeed");

    // 1. List accounts
    let accounts = engine.list_accounts().expect("list_accounts");
    assert_eq!(accounts.len(), 2);
    assert_eq!(accounts[0].name, "Work");
    assert_eq!(accounts[1].name, "Tech News");

    // 2. List folders
    let folders = engine
        .list_folders(Some("018f3a9c-0000-7000-8000-000000000001"))
        .expect("list_folders");
    assert_eq!(folders.len(), 2);
    assert_eq!(folders[0].name, "Archive");
    assert_eq!(folders[1].name, "INBOX");
    assert_eq!(folders[1].item_count, 2);
    assert_eq!(folders[1].unread_count, 1);

    // 3. List feeds
    let feeds = engine.list_feeds(None).expect("list_feeds");
    assert_eq!(feeds.len(), 1);
    assert_eq!(feeds[0].name, "Rust Blog");
    assert_eq!(feeds[0].unread_count, 1);

    // 4. Query stream (Unified)
    let page = engine
        .query_stream(
            None,
            None,
            None,
            None,
            false,
            false,
            None,
            None,
            SortKey::Date,
            50,
            None,
        )
        .expect("query_stream unified");
    assert_eq!(page.items.len(), 3);
    // Ordered by sort_at DESC / date DESC: item 1002 (1700001200), item 1001 (1700001000), item 2001 (1700000800)
    assert_eq!(
        page.items[0].id,
        "018f3ac1-0000-7000-8000-000000000002" // Colleague reply
    );
    assert_eq!(
        page.items[1].id,
        "018f3ac1-0000-7000-8000-000000000001" // Boss kick-off
    );
    assert_eq!(
        page.items[2].id,
        "018f3ac1-0000-7000-8000-000000000003" // TWiR
    );
    assert_eq!(page.items[2].kind, FfiItemKind::Feed);

    // Filter by unread
    let unread_page = engine
        .query_stream(
            None,
            None,
            None,
            None,
            true,
            false,
            None,
            None,
            SortKey::Date,
            50,
            None,
        )
        .expect("unread query");
    assert_eq!(unread_page.items.len(), 2);

    // 5. Get item detail
    let detail = engine
        .get_item("018f3ac1-0000-7000-8000-000000000001")
        .expect("get_item");
    assert_eq!(detail.summary.title, "Quarterly Planning kick-off");
    assert_eq!(detail.summary.body_state, FfiBodyState::Cached);
    assert_eq!(
        detail.body_text,
        Some("Please review the attached quarterly goals.".to_string())
    );
    assert_eq!(detail.from_addr, Some("boss@example.com".to_string()));

    // 6. Search
    let search_res = engine
        .search_items("quarterly", None, SortKey::Date, 10, None)
        .expect("search_items");
    assert!(!search_res.items.is_empty());

    // 7. Pending ops
    let ops = engine.get_pending_ops().expect("get_pending_ops");
    assert_eq!(ops.len(), 1);
    assert_eq!(ops[0].op_id, "018f3ae1-0000-7000-8000-000000000001");
}

#[tokio::test(flavor = "multi_thread")]
async fn test_dual_transport_read_parity() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("debloatmail.db");
    let blobs_dir = tmp.path().join("blobs");
    std::fs::create_dir_all(&blobs_dir).unwrap();

    {
        let conn = open_store(&db_path).unwrap();
        seed_test_database(&conn);
    }

    let lock_path = tmp.path().join("standalone.lock");
    let in_process_engine =
        InProcessEngine::open_standalone(db_path.clone(), blobs_dir.clone(), Some(lock_path))
            .expect("InProcessEngine::open_standalone should succeed");

    // Only test DbusEngine parity if session bus connection is available in the environment
    if let Ok(dbus_conn) = zbus::Connection::session().await {
        let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
            | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
            | rusqlite::OpenFlags::SQLITE_OPEN_URI;
        let ro_conn = rusqlite::Connection::open_with_flags(&db_path, flags).unwrap();
        let dbus_engine = DbusEngine::from_parts(dbus_conn, ro_conn, blobs_dir.clone());

        // Parity 1: Accounts
        let ip_accounts = in_process_engine.list_accounts().unwrap();
        let dbus_accounts = dbus_engine.list_accounts().unwrap();
        assert_eq!(ip_accounts, dbus_accounts);

        // Parity 2: Folders
        let ip_folders = in_process_engine.list_folders(None).unwrap();
        let dbus_folders = dbus_engine.list_folders(None).unwrap();
        assert_eq!(ip_folders, dbus_folders);

        // Parity 3: Feeds
        let ip_feeds = in_process_engine.list_feeds(None).unwrap();
        let dbus_feeds = dbus_engine.list_feeds(None).unwrap();
        assert_eq!(ip_feeds, dbus_feeds);

        // Parity 4: Query Stream
        let ip_stream = in_process_engine
            .query_stream(
                None,
                None,
                None,
                None,
                false,
                false,
                None,
                None,
                SortKey::Date,
                50,
                None,
            )
            .unwrap();
        let dbus_stream = dbus_engine
            .query_stream(
                None,
                None,
                None,
                None,
                false,
                false,
                None,
                None,
                SortKey::Date,
                50,
                None,
            )
            .unwrap();
        assert_eq!(ip_stream, dbus_stream);

        // Parity 5: Item detail
        let ip_detail = in_process_engine
            .get_item("018f3ac1-0000-7000-8000-000000000001")
            .unwrap();
        let dbus_detail = dbus_engine
            .get_item("018f3ac1-0000-7000-8000-000000000001")
            .unwrap();
        assert_eq!(ip_detail, dbus_detail);

        // Parity 6: Pending ops
        let ip_ops = in_process_engine.get_pending_ops().unwrap();
        let dbus_ops = dbus_engine.get_pending_ops().unwrap();
        assert_eq!(ip_ops, dbus_ops);
    }
}

#[test]
fn test_standalone_lock_contention() {
    let tmp = tempdir().unwrap();
    let lock_path = tmp.path().join("debloatmail-standalone.lock");

    // Acquire lock 1
    let lock1 =
        StandaloneLock::acquire(lock_path.clone()).expect("first lock acquisition must succeed");

    // Attempt acquire lock 2 on same path: must fail with LockHeld
    let lock2_err =
        StandaloneLock::acquire(lock_path.clone()).expect_err("second lock acquisition must fail");

    match lock2_err {
        FfiError::LockHeld { path, pid } => {
            assert_eq!(path, lock_path);
            assert_eq!(pid, Some(std::process::id()));
        }
        other => panic!("expected LockHeld error, got {other:?}"),
    }

    // Drop first lock
    drop(lock1);

    // Now acquisition should succeed
    let lock3 = StandaloneLock::acquire(lock_path.clone())
        .expect("lock acquisition after drop must succeed");
    drop(lock3);
}

#[tokio::test(flavor = "multi_thread")]
async fn test_box_dyn_engine_boundary_and_concurrency() {
    let tmp = tempdir().unwrap();
    let engine = InProcessEngine::open_memory(tmp.path().to_path_buf()).unwrap();

    // Trait object test: Box<dyn Engine> is Send + Sync
    let engine_box: Box<dyn Engine> = Box::new(engine);

    let handle = tokio::spawn(async move {
        let accounts = engine_box.list_accounts().expect("list_accounts on dyn");
        assert_eq!(accounts.len(), 0);
    });

    handle.await.unwrap();
}
