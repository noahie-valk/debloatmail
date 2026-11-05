//! SQLite thin cache storage layer for debloatmail.
//!
//! Implements schema, migrations, headers cache, feed items, flags,
//! FTS5 search, and unified stream queries per `DATA-MODEL.md`.

pub mod blobs;
pub mod feed_items;
pub mod flags;
pub mod headers;
pub mod schema;
pub mod search;
pub mod secure_file;
pub mod threads;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("SQLite database error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("Domain core error: {0}")]
    Core(#[from] debloatmail_core::CoreError),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Database schema version {0} is newer than binary version {1}")]
    VersionMismatch(u32, u32),

    #[error("Migration error: {0}")]
    Migration(String),

    #[error("Item not found: {0}")]
    NotFound(String),

    #[error("Invalid pagination cursor: {0}")]
    InvalidCursor(String),

    #[error("Invalid BLAKE3 blob hash: {0}")]
    InvalidBlobHash(String),
}

pub use blobs::{
    BLOB_GC_GRACE_WINDOW_SECS, BlobRefcountDiscrepancy, blob_path, compute_blob_hash,
    decrement_blob_ref, evict_lru_attachment_blobs, evict_lru_mail_bodies, gc_blobs,
    increment_blob_ref, put_blob, read_blob, reconcile_blob_refcounts, touch_blob,
    validate_blob_hash,
};
pub use feed_items::{UpsertFeedItem, purge_feed, soft_remove_feed, upsert_feed_item};
pub use flags::{queue_flag_op, set_answered, set_hidden, set_starred, set_unread};
pub use headers::{
    NewAttachment, NewMailHeader, evict_mail_body, has_pending_flag_op, insert_mail_header,
    query_stream, reconcile_extracted_attachments, replace_provisional_attachments,
    upsert_mail_header,
};
pub use schema::{
    CURRENT_SCHEMA_VERSION, bump_revision, get_revision, get_user_version, initialize_schema,
    open_in_memory, open_read_only, open_store,
};
pub use search::{ParsedSearch, parse_search_query, search_items};
pub use threads::rebuild_account_threads;


#[cfg(test)]
mod tests {
    use super::*;
    use debloatmail_core::{SortKey, StreamSource, StreamSpec};
    use rusqlite::params;

    fn setup_test_db() -> rusqlite::Connection {
        let conn = open_in_memory().expect("failed to open in-memory db");
        // Create an imap account and an inbox folder
        conn.execute(
            "INSERT INTO accounts (public_id, kind, name, created_at, updated_at)
             VALUES ('018f6e80-7c00-7218-971e-7f62b2512f71', 'imap', 'Personal', 1000, 1000);",
            [],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO folders (public_id, account_id, name, display_name, role, role_source)
             VALUES ('018f6e80-7c00-7218-971e-7f62b2512f72', 1, 'INBOX', 'Inbox', 'inbox', 'special-use');",
            [],
        )
        .unwrap();

        // Create a feed account and a feed
        conn.execute(
            "INSERT INTO accounts (public_id, kind, name, created_at, updated_at)
             VALUES ('018f6e80-7c00-7218-971e-7f62b2512f73', 'feed', 'Feeds', 1000, 1000);",
            [],
        )
        .unwrap();

        conn.execute(
            "INSERT INTO feeds (public_id, account_id, url, title)
             VALUES ('018f6e80-7c00-7218-971e-7f62b2512f74', 2, 'https://example.com/feed.xml', 'Tech News');",
            [],
        )
        .unwrap();

        conn
    }

    #[test]
    fn test_unified_stream_and_search() {
        let conn = setup_test_db();

        // Insert mail item
        let mail_header = NewMailHeader {
            public_id: "018f6e80-7c00-7218-971e-7f62b2512001",
            account_id: 1,
            folder_id: 1,
            uid: 101,
            uidvalidity: 1,
            title: Some("Welcome to debloatmail"),
            author: Some("noah@example.com"),
            published_at: Some(1700000000),
            sort_at: 1700000000,
            unread: true,
            starred: false,
            answered: false,
            has_attachments: false,
            message_id: Some("msg-101"),
            in_reply_to: None,
            references_hdr: None,
            subject_norm: Some("Welcome to debloatmail"),
            from_addr: Some("noah@example.com"),
            to_addrs_json: None,
            cc_addrs_json: None,
            reply_to: None,
            size_bytes: Some(1024),
            flags_local: "",
            created_at: 1700000000,
        };
        let mail_item_id = insert_mail_header(&conn, &mail_header).unwrap();

        // Insert feed item
        let feed_item = UpsertFeedItem {
            feed_id: 1,
            account_id: 2,
            public_id: "018f6e80-7c00-7218-971e-7f62b2512002",
            guid: Some("entry-1"),
            link: Some("https://example.com/post1"),
            title: Some("Rust 1.93 Released"),
            author: Some("Rust Team"),
            published_at: Some(1700000100),
            sort_at: 1700000100,
            body: "Rust 1.93 features major improvements to the compiler and tooling.",
            now: 1700000100,
        };
        let feed_item_id = upsert_feed_item(&conn, &feed_item).unwrap();

        // Query unified stream
        let spec = StreamSpec {
            source: StreamSource::Unified,
            sort: SortKey::Date,
            unread_only: false,
            starred_only: false,
            since: None,
        };
        let items = query_stream(&conn, &spec, None, 10).unwrap();
        assert_eq!(items.len(), 2);
        // Ordered by published_at DESC: feed item (1700000100) first, then mail item (1700000000)
        assert_eq!(items[0].title, "Rust 1.93 Released");
        assert_eq!(items[1].title, "Welcome to debloatmail");

        // Test FTS5 search across mail and feeds
        let search_results = search_items(&conn, "compiler", 10).unwrap();
        assert_eq!(search_results.len(), 1);
        assert_eq!(search_results[0].title, "Rust 1.93 Released");

        let search_mail = search_items(&conn, "debloatmail", 10).unwrap();
        assert_eq!(search_mail.len(), 1);
        assert_eq!(search_mail[0].title, "Welcome to debloatmail");

        // Test body eviction keeps title/author searchable
        // First populate body text in mail item
        conn.execute(
            "UPDATE item_text SET body = 'Secret details inside' WHERE item_id = ?1;",
            params![mail_item_id],
        )
        .unwrap();
        let found_body = search_items(&conn, "Secret", 10).unwrap();
        assert_eq!(found_body.len(), 1);

        // Evict body
        evict_mail_body(&conn, mail_item_id).unwrap();
        let body_after_evict = search_items(&conn, "Secret", 10).unwrap();
        assert_eq!(body_after_evict.len(), 0); // body no longer searchable

        let subject_after_evict = search_items(&conn, "debloatmail", 10).unwrap();
        assert_eq!(subject_after_evict.len(), 1); // subject is still searchable!

        // Test feed upsert preserves read and starred flags
        set_unread(&conn, feed_item_id, false).unwrap();
        set_starred(&conn, feed_item_id, true).unwrap();

        // Re-poll the feed with updated body
        let feed_item_reingest = UpsertFeedItem {
            feed_id: 1,
            account_id: 2,
            public_id: "018f6e80-7c00-7218-971e-7f62b2512002",
            guid: Some("entry-1"),
            link: Some("https://example.com/post1"),
            title: Some("Rust 1.93 Released (Updated)"),
            author: Some("Rust Team"),
            published_at: Some(1700000100),
            sort_at: 1700000100,
            body: "Updated blog post contents.",
            now: 1700000200,
        };
        upsert_feed_item(&conn, &feed_item_reingest).unwrap();

        // Test suppression (hidden_at): hiding an item excludes it from stream and search
        set_hidden(&conn, mail_item_id, Some(1700000300)).unwrap();
        let items_after_hide = query_stream(&conn, &spec, None, 10).unwrap();
        assert_eq!(items_after_hide.len(), 1);
        assert_eq!(items_after_hide[0].title, "Rust 1.93 Released (Updated)");

        // Search also selects from visible_items, so hidden items are excluded
        let search_hidden = search_items(&conn, "debloatmail", 10).unwrap();
        assert_eq!(search_hidden.len(), 0);

        // Unhide item
        set_hidden(&conn, mail_item_id, None).unwrap();
        let items_after_unhide = query_stream(&conn, &spec, None, 10).unwrap();
        assert_eq!(items_after_unhide.len(), 2);

        // Test feed soft-removal: feed.removed_at is set, but articles remain in stream & search
        soft_remove_feed(&conn, 1, 1700000400).unwrap();
        let items_after_soft_remove = query_stream(&conn, &spec, None, 10).unwrap();
        assert_eq!(items_after_soft_remove.len(), 2);

        // Feed purge: deletes feed and its items permanently
        purge_feed(&conn, 1).unwrap();
        let items_after_purge = query_stream(&conn, &spec, None, 10).unwrap();
        assert_eq!(items_after_purge.len(), 1);
        assert_eq!(items_after_purge[0].title, "Welcome to debloatmail");
    }

    #[test]
    fn test_cursor_pagination() {
        let conn = setup_test_db();

        for i in 1..=5 {
            let pub_id = format!("018f6e80-7c00-7218-971e-7f62b251200{i}");
            let title = format!("Message {i}");
            let mail_header = NewMailHeader {
                public_id: &pub_id,
                account_id: 1,
                folder_id: 1,
                uid: 100 + i,
                uidvalidity: 1,
                title: Some(&title),
                author: Some("sender@example.com"),
                published_at: Some(1700000000 + i as i64 * 10),
                sort_at: 1700000000 + i as i64 * 10,
                unread: true,
                starred: false,
                answered: false,
                has_attachments: false,
                message_id: None,
                in_reply_to: None,
                references_hdr: None,
                subject_norm: None,
                from_addr: Some("sender@example.com"),
                to_addrs_json: None,
                cc_addrs_json: None,
                reply_to: None,
                size_bytes: Some(512),
                flags_local: "",
                created_at: 1700000000,
            };
            insert_mail_header(&conn, &mail_header).unwrap();
        }

        let spec = StreamSpec {
            source: StreamSource::Unified,
            sort: SortKey::Date,
            unread_only: false,
            starred_only: false,
            since: None,
        };

        // Page 1: limit 2
        let page1 = query_stream(&conn, &spec, None, 2).unwrap();
        assert_eq!(page1.len(), 2);
        assert_eq!(page1[0].title, "Message 5");
        assert_eq!(page1[1].title, "Message 4");

        // Create cursor from last item of page 1
        let cursor1 = debloatmail_core::Cursor {
            sort: SortKey::Date,
            date: page1[1].date,
            sort_at: Some(page1[1].sort_at),
            text: None,
            id: page1[1].id.clone(),
        };

        // Page 2: limit 2 with cursor
        let page2 = query_stream(&conn, &spec, Some(&cursor1), 2).unwrap();
        assert_eq!(page2.len(), 2);
        assert_eq!(page2[0].title, "Message 3");
        assert_eq!(page2[1].title, "Message 2");

        // Page 3: limit 2 with cursor from page 2
        let cursor2 = debloatmail_core::Cursor {
            sort: SortKey::Date,
            date: page2[1].date,
            sort_at: Some(page2[1].sort_at),
            text: None,
            id: page2[1].id.clone(),
        };
        let page3 = query_stream(&conn, &spec, Some(&cursor2), 2).unwrap();
        assert_eq!(page3.len(), 1);
        assert_eq!(page3[0].title, "Message 1");
    }

    #[test]
    fn test_blob_lifecycle_gc_reconciliation_and_lru_eviction() {
        let conn = setup_test_db();
        let temp_dir = std::env::temp_dir().join(format!(
            "debloatmail-blob-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&temp_dir);

        // Insert a mail item
        let mail_header = NewMailHeader {
            public_id: "018f6e80-7c00-7218-971e-7f62b2513001",
            account_id: 1,
            folder_id: 1,
            uid: 301,
            uidvalidity: 1,
            title: Some("Attachment & Blob Test"),
            author: Some("alice@example.com"),
            published_at: Some(1700001000),
            sort_at: 1700001000,
            unread: true,
            starred: false,
            answered: false,
            has_attachments: false,
            message_id: Some("msg-301@example.com"),
            in_reply_to: None,
            references_hdr: None,
            subject_norm: Some("attachment & blob test"),
            from_addr: Some("alice@example.com"),
            to_addrs_json: None,
            cc_addrs_json: None,
            reply_to: None,
            size_bytes: Some(2048),
            flags_local: "",
            created_at: 1700001000,
        };
        let item_id = insert_mail_header(&conn, &mail_header).unwrap();

        // Store raw MIME blob and link it to mail_items.raw_blob
        let raw_bytes = b"From: alice@example.com\r\nSubject: Test\r\n\r\nCached body contents here.";
        let hash = put_blob(&conn, &temp_dir, raw_bytes, 10_000).unwrap();
        increment_blob_ref(&conn, &hash, 10_000).unwrap();
        conn.execute(
            "UPDATE mail_items SET raw_blob = ?1 WHERE item_id = ?2;",
            params![hash, item_id],
        )
        .unwrap();
        conn.execute(
            "UPDATE items SET body_state = 'cached', excerpt = 'Cached body contents here.' WHERE id = ?1;",
            params![item_id],
        )
        .unwrap();
        conn.execute(
            "UPDATE item_text SET body = 'Cached body contents here.' WHERE item_id = ?1;",
            params![item_id],
        )
        .unwrap();

        assert_eq!(read_blob(&temp_dir, &hash).unwrap(), raw_bytes);
        assert!(reconcile_blob_refcounts(&conn, false).unwrap().is_empty());

        // GC should NOT collect while refcount == 1
        assert_eq!(gc_blobs(&conn, &temp_dir, 20_000).unwrap(), 0);

        // Evict LRU mail bodies with max_total_bytes = 0 -> evicts body and decrements blob refcount to 0
        let evicted = evict_lru_mail_bodies(&conn, 0, 20_000).unwrap();
        assert_eq!(evicted, 1);

        // Refcount is now 0, and reconciliation confirms no drift
        assert!(reconcile_blob_refcounts(&conn, false).unwrap().is_empty());

        // Within the 1-hour grace window (20_000 + 1800), GC keeps the blob
        assert_eq!(gc_blobs(&conn, &temp_dir, 21_800).unwrap(), 0);
        assert!(blob_path(&temp_dir, &hash).unwrap().exists());

        // After the 1-hour grace window (20_000 + 3601), GC unlinks the file and removes the row
        assert_eq!(gc_blobs(&conn, &temp_dir, 23_601).unwrap(), 1);
        assert!(!blob_path(&temp_dir, &hash).unwrap().exists());

        let _ = std::fs::remove_dir_all(&temp_dir);
    }

    #[test]
    fn test_attachment_provisional_refinement_and_jwz_threading() {
        let conn = setup_test_db();

        let h1 = NewMailHeader {
            public_id: "018f6e80-7c00-7218-971e-7f62b2514001",
            account_id: 1,
            folder_id: 1,
            uid: 401,
            uidvalidity: 1,
            title: Some("Release Checklist"),
            author: Some("alice@example.com"),
            published_at: Some(1700002000),
            sort_at: 1700002000,
            unread: true,
            starred: false,
            answered: false,
            has_attachments: false,
            message_id: Some("root-1@example.com"),
            in_reply_to: None,
            references_hdr: None,
            subject_norm: Some("release checklist"),
            from_addr: Some("alice@example.com"),
            to_addrs_json: None,
            cc_addrs_json: None,
            reply_to: None,
            size_bytes: Some(1024),
            flags_local: "",
            created_at: 1700002000,
        };
        let (item1, inserted1) = upsert_mail_header(&conn, &h1).unwrap();
        assert!(inserted1);

        // Provisional attachments from BODYSTRUCTURE: section "2" (mis-declared) and "3"
        let provisional = [
            NewAttachment {
                section_id: "2",
                filename: Some("provisional.txt"),
                mime_type: "text/plain",
                size_bytes: Some(100),
                content_id: None,
                is_pdf: false,
            },
            NewAttachment {
                section_id: "3",
                filename: Some("report.pdf"),
                mime_type: "application/octet-stream",
                size_bytes: Some(500),
                content_id: None,
                is_pdf: false,
            },
        ];
        replace_provisional_attachments(&conn, item1, &provisional).unwrap();

        // Authoritative body walk drops "2", updates "3" (is_pdf = true, mime = application/pdf), and adds "4"
        let authoritative = [
            NewAttachment {
                section_id: "3",
                filename: Some("report.pdf"),
                mime_type: "application/pdf",
                size_bytes: Some(512),
                content_id: None,
                is_pdf: true,
            },
            NewAttachment {
                section_id: "4",
                filename: Some("diagram.png"),
                mime_type: "image/png",
                size_bytes: Some(2048),
                content_id: Some("cid-1"),
                is_pdf: false,
            },
        ];
        reconcile_extracted_attachments(&conn, item1, &authoritative, 1700002050).unwrap();

        let mut stmt = conn
            .prepare("SELECT section_id, mime_type, size_bytes, is_pdf FROM attachments WHERE item_id = ?1 ORDER BY section_id;")
            .unwrap();
        let rows: Vec<(String, String, i64, i64)> = stmt
            .query_map(params![item1], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0], ("3".to_string(), "application/pdf".to_string(), 512, 1));
        assert_eq!(rows[1], ("4".to_string(), "image/png".to_string(), 2048, 0));

        // Insert a reply referencing root-1@example.com
        let h2 = NewMailHeader {
            public_id: "018f6e80-7c00-7218-971e-7f62b2514002",
            account_id: 1,
            folder_id: 1,
            uid: 402,
            uidvalidity: 1,
            title: Some("Re: Release Checklist"),
            author: Some("bob@example.com"),
            published_at: Some(1700002100),
            sort_at: 1700002100,
            unread: false,
            starred: false,
            answered: false,
            has_attachments: false,
            message_id: Some("reply-2@example.com"),
            in_reply_to: Some("root-1@example.com"),
            references_hdr: Some("root-1@example.com"),
            subject_norm: Some("release checklist"),
            from_addr: Some("bob@example.com"),
            to_addrs_json: None,
            cc_addrs_json: None,
            reply_to: None,
            size_bytes: Some(1024),
            flags_local: "",
            created_at: 1700002100,
        };
        let (item2, _) = upsert_mail_header(&conn, &h2).unwrap();

        let mut counter = 0u32;
        let thread_count = rebuild_account_threads(&conn, 1, || {
            counter += 1;
            format!("018f6e80-7c00-7218-971e-7f62b251900{counter}")
        })
        .unwrap();
        assert_eq!(thread_count, 1);

        let (item_count, unread_count, last_activity): (i64, i64, i64) = conn
            .query_row(
                "SELECT item_count, unread_count, last_activity FROM threads WHERE account_id = 1;",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(item_count, 2);
        assert_eq!(unread_count, 1);
        assert_eq!(last_activity, 1700002100);

        // Hiding item2 should exclude it from thread counts (`visible_items` rule)
        set_hidden(&conn, item2, Some(1700002200)).unwrap();
        rebuild_account_threads(&conn, 1, || "unused".to_string()).unwrap();
        let (item_count_after, last_activity_after): (i64, i64) = conn
            .query_row(
                "SELECT item_count, last_activity FROM threads WHERE account_id = 1;",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(item_count_after, 1);
        assert_eq!(last_activity_after, 1700002000);
    }
}

