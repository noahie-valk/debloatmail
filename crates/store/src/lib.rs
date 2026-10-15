//! SQLite thin cache storage layer for debloatmail.
//!
//! Implements schema, migrations, headers cache, feed items, flags,
//! FTS5 search, and unified stream queries per `DATA-MODEL.md`.

pub mod feed_items;
pub mod flags;
pub mod headers;
pub mod schema;
pub mod search;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("SQLite database error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("Domain core error: {0}")]
    Core(#[from] debloatmail_core::CoreError),

    #[error("Database schema version {0} is newer than binary version {1}")]
    VersionMismatch(u32, u32),

    #[error("Migration error: {0}")]
    Migration(String),

    #[error("Item not found: {0}")]
    NotFound(String),

    #[error("Invalid pagination cursor: {0}")]
    InvalidCursor(String),
}

pub use feed_items::{purge_feed, soft_remove_feed, upsert_feed_item, UpsertFeedItem};
pub use flags::{queue_flag_op, set_answered, set_hidden, set_starred, set_unread};
pub use headers::{evict_mail_body, insert_mail_header, query_stream, NewMailHeader};
pub use schema::{
    bump_revision, get_revision, get_user_version, initialize_schema, open_in_memory,
    open_read_only, open_store, CURRENT_SCHEMA_VERSION,
};
pub use search::{parse_search_query, search_items, ParsedSearch};

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
            published_at: page1[1].date,
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
            published_at: page2[1].date,
            sort_at: Some(page2[1].sort_at),
            text: None,
            id: page2[1].id.clone(),
        };
        let page3 = query_stream(&conn, &spec, Some(&cursor2), 2).unwrap();
        assert_eq!(page3.len(), 1);
        assert_eq!(page3[0].title, "Message 1");
    }
}
