//! Synchronization orchestration, priority scheduler, `UIDVALIDITY` 7-step staged recovery,
//! windowed header sync, on-demand `BODY.PEEK` body/attachment fetching, `pending_ops` drain,
//! feed polling + hybrid 15 s validation, and crash-safe startup recovery (`SYNC.md`).
//!
//! # Architectural Invariants (`GEMINI.md`, `architecture.md` §2, `SYNC.md`)
//! - Zero sockets and zero TLS in `crates/sync`: all IMAP/SMTP protocol calls run over
//!   generic `ImapConnection<S: AsyncByteStream>` and `SmtpConnection<S: AsyncByteStream>`,
//!   and all feed HTTP calls run through `FeedFetcher` (backed by `crates/feeds`).
//! - Never issues plain `EXPUNGE` (`UID EXPUNGE` only when `UIDPLUS` is negotiated).
//! - Every body and MIME section fetch uses `BODY.PEEK[...]`, never `BODY[...]`.

pub mod body;
pub mod feeds;
pub mod id;
pub mod mail;
pub mod ops;
pub mod recovery;
pub mod scheduler;

pub use body::{
    FetchedAttachmentOutcome, FetchedBodyOutcome, fetch_item_attachment, fetch_item_body,
};
pub use feeds::{
    AddFeedOutcome, DEFAULT_FEED_POLL_TIMEOUT, DueFeedRow, FEED_ADD_VALIDATION_TIMEOUT,
    FeedFetcher, FeedHardFailureKind, FeedPollReport, PendingValidationOutcome, ReqwestFeedFetcher,
    add_feed_hybrid, complete_pending_feed_validation, derive_entry_guid, poll_feed_with_fetcher,
    query_due_feeds,
};
pub use id::{mint_uuid_v7, mint_uuid_v7_from_clock};
pub use mail::{
    FALLBACK_FLAG_SYNC_WINDOW_UIDS, FolderReconcileReport, FolderRowState, FolderSyncReport,
    ProvisionalAttachment, extract_provisional_attachments_from_bodystructure, load_folder_state,
    parse_imap_mailbox, reconcile_account_folders, reconcile_discovered_folders,
    run_uidvalidity_staged_recovery, select_folder, sync_folder, sync_folder_flags_and_expunges,
    sync_folder_headers_windowed,
};
pub use ops::{
    DeleteOpPayload, DrainOpsReport, ExpungeOpPayload, FlagOpPayload, MAX_OP_SERVER_ATTEMPTS,
    MoveOpPayload, SendOpPayload, drain_account_ops, queue_send_from_draft,
};
pub use recovery::{
    StartupRecoveryReport, TERMINAL_OP_RETENTION_SECS, purge_expired_terminal_ops,
    run_startup_recovery,
};
pub use scheduler::{
    COALESCE_RECENT_DONE_MS, CancelToken, DEFAULT_FEED_INTERVAL_SECS, DEFAULT_MAIL_INTERVAL_SECS,
    DEFAULT_WORKER_POOL_SIZE, INDETERMINATE_PERCENT, ITEMS_CHANGED_COALESCE_MS, JobId, JobRecord,
    JobScope, JobState, MAX_CONCURRENT_FETCHES_PER_HOST, MAX_CONCURRENT_HTTP_FETCHES,
    MAX_IMAP_CONNECTIONS_PER_ACCOUNT, MAX_TOTAL_OPEN_SOCKETS, MIN_FEED_INTERVAL_SECS,
    MIN_MAIL_INTERVAL_SECS, PROGRESS_THROTTLE_MS, PriorityClass, ProgressTracker, Scheduler,
    SyncEvent, SyncPhase, effective_feed_interval_secs, effective_mail_interval_secs,
    schedule_next_account_sync,
};

/// Errors produced by `debloatmail-sync`.
#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("storage error: {0}")]
    Store(#[from] debloatmail_store::StoreError),

    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("protocol engine error: {0}")]
    Engine(#[from] debloatmail_engine::EngineError),

    #[error("MIME extraction error: {0}")]
    Extract(#[from] debloatmail_extract::ExtractError),

    #[error("feed error: {0}")]
    Feeds(#[from] debloatmail_feeds::PollError),

    #[error("core domain error: {0}")]
    Core(#[from] debloatmail_core::CoreError),

    #[error("JSON serialization error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("invalid sync state: {0}")]
    InvalidState(String),

    #[error("feed validation hard failure ({kind:?}): {message}")]
    HardFeedFailure {
        kind: FeedHardFailureKind,
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Mutex;
    use std::time::Duration;

    use debloatmail_core::{AccountId, BodyState, FolderId, PublicId, RoleSource};
    use debloatmail_engine::{
        Clock, DiscoveredFolder, ImapConnection, MemoryStream, NegotiatedCapabilities, Rng,
        SmtpConnection,
    };
    use debloatmail_feeds::{FetchError, FetchResult, RedirectViolation};
    use debloatmail_store::{
        NewAttachment, NewMailHeader, blob_path, gc_blobs, increment_blob_ref, open_in_memory,
        put_blob, replace_provisional_attachments, upsert_mail_header,
    };
    use rusqlite::{Connection, params};

    use super::*;

    struct TestClock(i64);
    impl Clock for TestClock {
        fn now_secs(&self) -> i64 {
            self.0
        }
    }

    struct DeterministicRng(u64);
    impl Rng for DeterministicRng {
        fn next_f64(&mut self) -> f64 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1);
            ((self.0 >> 11) as f64) / ((1u64 << 53) as f64)
        }
    }

    struct MockFeedFetcher {
        responses: Mutex<VecDeque<Result<FetchResult, FetchError>>>,
    }

    impl MockFeedFetcher {
        fn new(items: impl IntoIterator<Item = Result<FetchResult, FetchError>>) -> Self {
            Self {
                responses: Mutex::new(items.into_iter().collect()),
            }
        }
    }

    impl FeedFetcher for MockFeedFetcher {
        fn fetch<'a>(
            &'a self,
            _url: &'a str,
            _etag: Option<&'a str>,
            _last_modified: Option<&'a str>,
            _timeout: Duration,
        ) -> Pin<Box<dyn Future<Output = Result<FetchResult, FetchError>> + Send + 'a>> {
            let next = self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or(Err(FetchError::Timeout));
            Box::pin(async move { next })
        }
    }

    fn seed_imap_account_and_inbox(conn: &Connection) -> (i64, AccountId, i64, FolderId) {
        let acc_pid = AccountId::new("018f6e80-0000-7000-8000-000000000001").unwrap();
        let fld_pid = FolderId::new("018f6e80-0000-7000-8000-000000000002").unwrap();
        conn.execute(
            "INSERT INTO accounts (
                public_id, kind, name, email, auth_kind, enabled, created_at, updated_at
             ) VALUES (?1, 'imap', 'Work', 'alice@example.org', 'password', 1, 1000, 1000)",
            params![acc_pid.as_str()],
        )
        .unwrap();
        let account_id = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO folders (
                public_id, account_id, name, display_name, role, role_source,
                uidvalidity, uidnext, highestmodseq, subscribed
             ) VALUES (?1, ?2, 'INBOX', 'INBOX', 'inbox', 'name', 100, 10, 50, 1)",
            params![fld_pid.as_str(), account_id],
        )
        .unwrap();
        let folder_id = conn.last_insert_rowid();

        (account_id, acc_pid, folder_id, fld_pid)
    }

    // =========================================================================
    // Scenario 1: 7-Step UIDVALIDITY Staged Recovery (SYNC.md §5.4, §15 #1)
    // =========================================================================
    #[tokio::test]
    async fn test_scenario_1_uidvalidity_staged_recovery_preserves_ids_and_rebinds_ops() {
        let mut conn = open_in_memory().unwrap();
        let (account_id, _acc_pid, folder_id, _fld_pid) = seed_imap_account_and_inbox(&conn);
        let clock = TestClock(2_000);
        let mut rng = DeterministicRng(7);

        let pid_a = PublicId::new("018f6e80-0000-7000-8000-00000000001a").unwrap();
        let pid_b = PublicId::new("018f6e80-0000-7000-8000-00000000001b").unwrap();
        let pid_c = PublicId::new("018f6e80-0000-7000-8000-00000000001c").unwrap();

        let item_a = {
            let tx = conn.transaction().unwrap();
            let (id_a, _) = upsert_mail_header(
                &tx,
                &NewMailHeader {
                    public_id: pid_a.as_str(),
                    account_id,
                    folder_id,
                    uidvalidity: 100,
                    uid: 1,
                    message_id: Some("msg-a@example.org"),
                    in_reply_to: None,
                    references_hdr: None,
                    from_addr: Some("bob@example.org"),
                    to_addrs_json: Some("[\"alice@example.org\"]"),
                    cc_addrs_json: None,
                    reply_to: None,
                    subject_norm: Some("alpha"),
                    title: Some("Alpha"),
                    author: Some("Bob"),
                    published_at: Some(1_500),
                    sort_at: 1_500,
                    size_bytes: Some(120),
                    unread: true,
                    starred: false,
                    answered: false,
                    has_attachments: false,
                    flags_local: "[]",
                    created_at: 1_500,
                },
            )
            .unwrap();
            let _ = upsert_mail_header(
                &tx,
                &NewMailHeader {
                    public_id: pid_b.as_str(),
                    account_id,
                    folder_id,
                    uidvalidity: 100,
                    uid: 2,
                    message_id: Some("msg-b@example.org"),
                    in_reply_to: None,
                    references_hdr: None,
                    from_addr: Some("bob@example.org"),
                    to_addrs_json: Some("[\"alice@example.org\"]"),
                    cc_addrs_json: None,
                    reply_to: None,
                    subject_norm: Some("beta"),
                    title: Some("Beta"),
                    author: Some("Bob"),
                    published_at: Some(1_510),
                    sort_at: 1_510,
                    size_bytes: Some(130),
                    unread: true,
                    starred: false,
                    answered: false,
                    has_attachments: false,
                    flags_local: "[]",
                    created_at: 1_510,
                },
            )
            .unwrap();
            let (id_c, _) = upsert_mail_header(
                &tx,
                &NewMailHeader {
                    public_id: pid_c.as_str(),
                    account_id,
                    folder_id,
                    uidvalidity: 100,
                    uid: 3,
                    message_id: Some("msg-c@example.org"),
                    in_reply_to: None,
                    references_hdr: None,
                    from_addr: Some("bob@example.org"),
                    to_addrs_json: Some("[\"alice@example.org\"]"),
                    cc_addrs_json: None,
                    reply_to: None,
                    subject_norm: Some("gamma"),
                    title: Some("Gamma"),
                    author: Some("Bob"),
                    published_at: Some(1_520),
                    sort_at: 1_520,
                    size_bytes: Some(140),
                    unread: true,
                    starred: false,
                    answered: false,
                    has_attachments: false,
                    flags_local: "[]",
                    created_at: 1_520,
                },
            )
            .unwrap();

            // Queue pending flag op on msg-a and move op on msg-c
            let flag_payload = serde_json::to_string(&FlagOpPayload {
                add_flags: vec!["\\Seen".to_string()],
                remove_flags: vec![],
            })
            .unwrap();
            tx.execute(
                "INSERT INTO pending_ops (
                    public_id, account_id, op, item_id, payload, state, attempts, created_at
                 ) VALUES ('018f6e80-0000-7000-8000-000000000091', ?1, 'flag', ?2, ?3, 'pending', 0, 1600)",
                params![account_id, id_a, flag_payload],
            )
            .unwrap();

            let move_payload = serde_json::to_string(&MoveOpPayload {
                target_folder: "Trash".to_string(),
            })
            .unwrap();
            tx.execute(
                "INSERT INTO pending_ops (
                    public_id, account_id, op, item_id, payload, state, attempts, created_at
                 ) VALUES ('018f6e80-0000-7000-8000-000000000092', ?1, 'move', ?2, ?3, 'pending', 0, 1601)",
                params![account_id, id_c, move_payload],
            )
            .unwrap();

            tx.commit().unwrap();
            id_a
        };

        // Server SELECT reports UIDVALIDITY = 200 (changed from 100!) and UIDNEXT = 103.
        // Fetch window returns msg-b at UID 101 and msg-a at UID 102; msg-c is gone.
        let mem = MemoryStream::new([
            b"* 2 EXISTS\r\n* OK [UIDVALIDITY 200] UIDs valid\r\n* OK [UIDNEXT 103] Predicted next UID\r\n* OK [HIGHESTMODSEQ 99] Modseq\r\n<TAG> OK [READ-WRITE] SELECT completed\r\n".to_vec(),
            b"* 1 FETCH (UID 101 FLAGS (\\Flagged) INTERNALDATE \"05-Nov-2026 12:01:00 +0000\" RFC822.SIZE 130 ENVELOPE (\"Thu, 05 Nov 2026 12:01:00 +0000\" \"Beta\" ((\"Bob\" NIL \"bob\" \"example.org\")) NIL NIL ((NIL NIL \"alice\" \"example.org\")) NIL NIL NIL \"<msg-b@example.org>\") BODYSTRUCTURE (\"TEXT\" \"PLAIN\" (\"CHARSET\" \"UTF-8\") NIL NIL \"7BIT\" 15 1 NIL NIL NIL NIL))\r\n* 2 FETCH (UID 102 FLAGS () INTERNALDATE \"05-Nov-2026 12:00:00 +0000\" RFC822.SIZE 120 ENVELOPE (\"Thu, 05 Nov 2026 12:00:00 +0000\" \"Alpha\" ((\"Bob\" NIL \"bob\" \"example.org\")) NIL NIL ((NIL NIL \"alice\" \"example.org\")) NIL NIL NIL \"<msg-a@example.org>\") BODYSTRUCTURE (\"TEXT\" \"PLAIN\" (\"CHARSET\" \"UTF-8\") NIL NIL \"7BIT\" 12 1 NIL NIL NIL NIL))\r\n<TAG> OK UID FETCH completed\r\n".to_vec(),
            // Drain of rebound flag op on msg-a (now at UID 102): SELECT + UID STORE 102 +FLAGS.SILENT (\Seen)
            b"* 2 EXISTS\r\n* OK [UIDVALIDITY 200] UIDs valid\r\n* OK [UIDNEXT 103] Predicted next UID\r\n<TAG> OK [READ-WRITE] SELECT completed\r\n".to_vec(),
            b"<TAG> OK UID STORE completed\r\n".to_vec(),
        ]);

        let mut imap_conn =
            ImapConnection::from_authenticated_memory(mem, NegotiatedCapabilities::default());
        let cancel = CancelToken::new();

        let report = sync_folder(
            &mut conn,
            &mut imap_conn,
            folder_id,
            false,
            &cancel,
            || false,
            &clock,
            &mut rng,
        )
        .await
        .unwrap();

        assert!(report.uidvalidity_reset);
        assert_eq!(report.inserted_items, 2);
        assert_eq!(report.unpreserved_ids, 0);
        assert_eq!(report.rebound_ops, 1);

        // Verify msg-a and msg-b kept their exact public_ids despite getting new rowids and UIDs!
        let new_a: (i64, String, i64) = conn
            .query_row(
                "SELECT i.id, i.public_id, m.uid
                 FROM items i JOIN mail_items m ON m.item_id = i.id
                 WHERE m.message_id = 'msg-a@example.org'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_ne!(new_a.0, item_a);
        assert_eq!(new_a.1, pid_a.as_str());
        assert_eq!(new_a.2, 102);

        // Drain ops: op 1 (on msg-a) succeeds against UID 101; op 2 (on missing msg-c) fails cleanly!
        let drain_rep = drain_account_ops::<_, MemoryStream>(
            &mut conn,
            &mut imap_conn,
            None,
            account_id,
            &clock,
        )
        .await
        .unwrap();

        assert_eq!(drain_rep.completed, 1);
        assert_eq!(drain_rep.failed_permanently, 1);

        let (op2_state, op2_err): (String, Option<String>) = conn
            .query_row(
                "SELECT state, last_error FROM pending_ops WHERE public_id = '018f6e80-0000-7000-8000-000000000092'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(op2_state, "failed");
        assert_eq!(op2_err.as_deref(), Some("target item no longer exists"));
    }

    // =========================================================================
    // Scenario 2: Pending-Op-Wins During CONDSTORE Flag Sync (SYNC.md §5.6, §15 #2)
    // =========================================================================
    #[tokio::test]
    async fn test_scenario_2_pending_op_wins_during_condstore_flag_sync() {
        let mut conn = open_in_memory().unwrap();
        let (account_id, _acc_pid, folder_id, _fld_pid) = seed_imap_account_and_inbox(&conn);
        let clock = TestClock(3_000);

        let pid_1 = PublicId::new("018f6e80-0000-7000-8000-000000000021").unwrap();
        let pid_2 = PublicId::new("018f6e80-0000-7000-8000-000000000022").unwrap();

        let (item1_id, item2_id) = {
            let tx = conn.transaction().unwrap();
            let (id1, _) = upsert_mail_header(
                &tx,
                &NewMailHeader {
                    public_id: pid_1.as_str(),
                    account_id,
                    folder_id,
                    uidvalidity: 100,
                    uid: 5,
                    message_id: Some("m5@example.org"),
                    in_reply_to: None,
                    references_hdr: None,
                    from_addr: None,
                    to_addrs_json: None,
                    cc_addrs_json: None,
                    reply_to: None,
                    subject_norm: Some("one"),
                    title: Some("One"),
                    author: None,
                    published_at: Some(2_000),
                    sort_at: 2_000,
                    size_bytes: None,
                    unread: false, // Locally marked read!
                    starred: false,
                    answered: false,
                    has_attachments: false,
                    flags_local: "[]",
                    created_at: 2_000,
                },
            )
            .unwrap();
            let (id2, _) = upsert_mail_header(
                &tx,
                &NewMailHeader {
                    public_id: pid_2.as_str(),
                    account_id,
                    folder_id,
                    uidvalidity: 100,
                    uid: 6,
                    message_id: Some("m6@example.org"),
                    in_reply_to: None,
                    references_hdr: None,
                    from_addr: None,
                    to_addrs_json: None,
                    cc_addrs_json: None,
                    reply_to: None,
                    subject_norm: Some("two"),
                    title: Some("Two"),
                    author: None,
                    published_at: Some(2_010),
                    sort_at: 2_010,
                    size_bytes: None,
                    unread: true,
                    starred: false,
                    answered: false,
                    has_attachments: false,
                    flags_local: "[]",
                    created_at: 2_010,
                },
            )
            .unwrap();

            // Item 1 has an undrained flag op
            tx.execute(
                "INSERT INTO pending_ops (
                    public_id, account_id, op, item_id, payload, state, attempts, created_at
                 ) VALUES ('018f6e80-0000-7000-8000-000000000095', ?1, 'flag', ?2, '{}', 'pending', 0, 2100)",
                params![account_id, id1],
            )
            .unwrap();
            tx.commit().unwrap();
            (id1, id2)
        };

        // Server CHANGEDSINCE 50 reports UID 5 as unread `FLAGS ()` and UID 6 as read+flagged `FLAGS (\Seen \Flagged)`.
        // UID SEARCH ALL returns UIDs 5 and 6.
        let mem = MemoryStream::new([
            b"* 1 FETCH (UID 5 FLAGS ())\r\n* 2 FETCH (UID 6 FLAGS (\\Seen \\Flagged))\r\n<TAG> OK UID FETCH completed\r\n".to_vec(),
            b"* SEARCH 5 6\r\n<TAG> OK UID SEARCH completed\r\n".to_vec(),
        ]);

        let mut imap_conn = ImapConnection::from_authenticated_memory(
            mem,
            NegotiatedCapabilities {
                condstore: true,
                ..Default::default()
            },
        );

        let folder_state = load_folder_state(&conn, folder_id).unwrap();
        let select_data = io_imap::rfc3501::select::ImapMailboxSelectData {
            uid_validity: std::num::NonZeroU32::new(100),
            uid_next: std::num::NonZeroU32::new(10),
            highest_mod_seq: Some(75),
            ..Default::default()
        };

        let expunged = sync_folder_flags_and_expunges(
            &mut conn,
            &mut imap_conn,
            &folder_state,
            &select_data,
            false,
            clock.now_secs(),
        )
        .await
        .unwrap();
        assert_eq!(expunged, 0);

        // Item 1 (with pending flag op) MUST stay unread = 0!
        let item1_unread: i64 = conn
            .query_row(
                "SELECT unread FROM items WHERE id = ?1",
                params![item1_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(item1_unread, 0);

        // Item 2 (no pending flag op) updated to unread = 0, starred = 1!
        let (item2_unread, item2_starred): (i64, i64) = conn
            .query_row(
                "SELECT unread, starred FROM items WHERE id = ?1",
                params![item2_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(item2_unread, 0);
        assert_eq!(item2_starred, 1);

        let written = String::from_utf8(imap_conn.stream.all_written_bytes()).unwrap();
        assert!(written.contains("CHANGEDSINCE 50"));
    }

    // =========================================================================
    // Scenario 3 & 4: Send NoVerdict Duplicate Guard, Offline Attempts, & Append (SYNC.md §8.2, §8.4, §8.5)
    // =========================================================================
    #[tokio::test]
    async fn test_scenario_3_and_4_send_no_verdict_confirmation_and_offline_attempts() {
        let mut conn = open_in_memory().unwrap();
        let (account_id, _acc_pid, _folder_id, _fld_pid) = seed_imap_account_and_inbox(&conn);
        let clock = TestClock(4_000);
        let mut rng = DeterministicRng(99);

        // Add a Sent folder for the account
        conn.execute(
            "INSERT INTO folders (
                public_id, account_id, name, display_name, role, role_source,
                uidvalidity, uidnext, highestmodseq, subscribed
             ) VALUES ('018f6e80-0000-7000-8000-000000000003', ?1, 'Sent', 'Sent', 'sent', 'special-use', 100, 1, 0, 1)",
            params![account_id],
        )
        .unwrap();

        // Create a draft and queue_send_from_draft
        conn.execute(
            "INSERT INTO drafts (
                public_id, account_id, to_addrs, subject, body, created_at, updated_at
             ) VALUES ('018f6e80-0000-7000-8000-000000000050', ?1, '[\"bob@example.org\"]', 'Hello Bob', 'Draft body text', 3900, 3900)",
            params![account_id],
        )
        .unwrap();
        let draft_row_id = conn.last_insert_rowid();

        let op_id =
            queue_send_from_draft(&mut conn, draft_row_id, None, true, &clock, &mut rng).unwrap();

        // Draft row must be deleted immediately on queue_send_from_draft (DATA-MODEL.md §11.5)
        let remaining_drafts: i64 = conn
            .query_row("SELECT COUNT(*) FROM drafts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(remaining_drafts, 0);

        // Attempt 1: SMTP connection drops mid-DATA after 354 before 250 OK (SendOutcome::NoVerdict)
        let smtp_mem_1 = MemoryStream::new([
            b"250 2.1.0 Mail Ok\r\n".to_vec(),
            b"250 2.1.5 Rcpt Ok\r\n".to_vec(),
            b"354 End data with <CR><LF>.<CR><LF>\r\n".to_vec(),
            // EOF before 250 OK!
        ]);
        let mut smtp_conn_1 = SmtpConnection::from_authenticated_memory(smtp_mem_1);
        let mut imap_conn_1 = ImapConnection::from_authenticated_memory(
            MemoryStream::default(),
            NegotiatedCapabilities::default(),
        );

        let rep1 = drain_account_ops(
            &mut conn,
            &mut imap_conn_1,
            Some(&mut smtp_conn_1),
            account_id,
            &clock,
        )
        .await
        .unwrap();
        assert_eq!(rep1.completed, 0);
        assert_eq!(rep1.deferred_retry, 1);

        // Verify state is 'in_flight', attempts is STILL 0, and needs_confirmation is true!
        let (state_1, attempts_1, payload_str_1): (String, i64, String) = conn
            .query_row(
                "SELECT state, attempts, payload FROM pending_ops WHERE public_id = ?1",
                params![op_id.as_str()],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(state_1, "in_flight");
        assert_eq!(attempts_1, 0);
        let parsed_payload_1: SendOpPayload = serde_json::from_str(&payload_str_1).unwrap();
        assert!(parsed_payload_1.needs_confirmation);

        // Attempt 2: Reconnect! Drain searches Sent folder for Message-ID first.
        // Suppose the server DID receive the message and placed it in Sent at UID 77:
        let imap_mem_2 = MemoryStream::new([
            b"* 1 EXISTS\r\n* OK [UIDVALIDITY 100] UIDs valid\r\n* OK [UIDNEXT 78] Next\r\n<TAG> OK [READ-WRITE] SELECT completed\r\n".to_vec(),
            b"* SEARCH 77\r\n<TAG> OK UID SEARCH completed\r\n".to_vec(),
        ]);
        let mut imap_conn_2 = ImapConnection::from_authenticated_memory(
            imap_mem_2,
            NegotiatedCapabilities::default(),
        );
        let mut smtp_conn_2 = SmtpConnection::from_authenticated_memory(MemoryStream::default());

        let rep2 = drain_account_ops(
            &mut conn,
            &mut imap_conn_2,
            Some(&mut smtp_conn_2),
            account_id,
            &clock,
        )
        .await
        .unwrap();
        assert_eq!(rep2.completed, 1);

        // Verify SMTP connection 2 had ZERO bytes written to it (no duplicate send!)
        assert!(smtp_conn_2.stream.all_written_bytes().is_empty());

        let (state_2, last_err_2): (String, Option<String>) = conn
            .query_row(
                "SELECT state, last_error FROM pending_ops WHERE public_id = ?1",
                params![op_id.as_str()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(state_2, "done");
        assert!(
            last_err_2
                .as_deref()
                .unwrap_or("")
                .contains("confirmed via Sent folder Message-ID search")
        );
    }

    // =========================================================================
    // Scenario 5, 6, 10: Feed 304, Upsert Flag Preservation, 410 Retention, & Hybrid Add (SYNC.md §7)
    // =========================================================================
    #[tokio::test]
    async fn test_scenario_5_6_10_feeds_hybrid_add_poll_304_and_410_retention() {
        let mut conn = open_in_memory().unwrap();
        let clock = TestClock(10_000);
        let mut rng = DeterministicRng(123);

        conn.execute(
            "INSERT INTO accounts (
                public_id, kind, name, auth_kind, enabled, created_at, updated_at
             ) VALUES ('018f6e80-0000-7000-8000-0000000000f0', 'feed', 'Feeds', 'password', 1, 1000, 1000)",
            [],
        )
        .unwrap();
        let feed_acc_id = conn.last_insert_rowid();

        // 1. Hard failure on Feeds.Add (404 Gone and SSRF redirect) -> creates NO row!
        let hard_fail_fetcher = MockFeedFetcher::new([
            Err(FetchError::Gone(404)),
            Err(FetchError::RedirectRejected(
                RedirectViolation::ForbiddenTargetHost("127.0.0.1".to_string()),
            )),
        ]);
        let err_404 = add_feed_hybrid(
            &mut conn,
            &hard_fail_fetcher,
            feed_acc_id,
            "https://example.com/missing.xml",
            None,
            None,
            None,
            1_800,
            &clock,
            &mut rng,
        )
        .await
        .unwrap_err();
        assert!(matches!(
            err_404,
            SyncError::HardFeedFailure {
                kind: FeedHardFailureKind::Gone,
                ..
            }
        ));

        let err_ssrf = add_feed_hybrid(
            &mut conn,
            &hard_fail_fetcher,
            feed_acc_id,
            "https://example.com/ssrf.xml",
            None,
            None,
            None,
            1_800,
            &clock,
            &mut rng,
        )
        .await
        .unwrap_err();
        assert!(matches!(
            err_ssrf,
            SyncError::HardFeedFailure {
                kind: FeedHardFailureKind::RedirectRejected,
                ..
            }
        ));

        let feed_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM feeds", [], |r| r.get(0))
            .unwrap();
        assert_eq!(feed_count, 0);

        // 2. Timeout on Feeds.Add -> creates transient validated: false row (last_fetch_at IS NULL),
        // then background validation completes with valid XML!
        let xml_v1 = br#"<?xml version="1.0" encoding="utf-8"?>
        <feed xmlns="http://www.w3.org/2005/Atom">
          <title>Kernel Log</title>
          <link href="https://example.com/"/>
          <entry>
            <id>tag:example.com,2026:1</id>
            <title>Release 1.0</title>
            <updated>2026-11-05T12:00:00Z</updated>
            <content type="html">&lt;p&gt;First article body&lt;/p&gt;</content>
          </entry>
        </feed>"#
            .to_vec();

        let timeout_then_ok_fetcher = MockFeedFetcher::new([
            Err(FetchError::Timeout),
            Ok(FetchResult {
                status: 200,
                body: xml_v1,
                etag: Some("\"etag-v1\"".to_string()),
                last_modified: None,
                redirected_url: None,
                permanent_redirect_url: None,
            }),
        ]);

        let added = add_feed_hybrid(
            &mut conn,
            &timeout_then_ok_fetcher,
            feed_acc_id,
            "https://example.com/atom.xml",
            Some("Kernel"),
            None,
            None,
            1_800,
            &clock,
            &mut rng,
        )
        .await
        .unwrap();
        assert!(!added.validated);

        let bg_outcome = complete_pending_feed_validation(
            &mut conn,
            &timeout_then_ok_fetcher,
            added.row_id,
            Duration::from_secs(15),
            &clock,
            &mut rng,
        )
        .await
        .unwrap();
        assert_eq!(
            bg_outcome,
            PendingValidationOutcome::Validated {
                feed_id: added.feed_id.clone(),
                inserted_items: 1,
            }
        );

        // User marks the article read (unread = 0) and starred (starred = 1)
        conn.execute("UPDATE items SET unread = 0, starred = 1", [])
            .unwrap();

        // 3. Poll gets 304 Not Modified -> emits NO FeedsUpdated event
        let poll_fetcher = MockFeedFetcher::new([
            Ok(FetchResult {
                status: 304,
                body: Vec::new(),
                etag: None,
                last_modified: None,
                redirected_url: None,
                permanent_redirect_url: None,
            }),
            // 4. Poll gets 200 OK with updated content for the same entry guid + a 301 redirect
            Ok(FetchResult {
                status: 200,
                body: br#"<?xml version="1.0" encoding="utf-8"?>
                <feed xmlns="http://www.w3.org/2005/Atom">
                  <title>Kernel Log</title>
                  <entry>
                    <id>tag:example.com,2026:1</id>
                    <title>Release 1.0 (Updated)</title>
                    <updated>2026-11-05T13:00:00Z</updated>
                    <content type="html">&lt;p&gt;Updated article body&lt;/p&gt;</content>
                  </entry>
                </feed>"#
                    .to_vec(),
                etag: Some("\"etag-v2\"".to_string()),
                last_modified: None,
                redirected_url: Some("https://example.com/new-atom.xml".to_string()),
                permanent_redirect_url: Some("https://example.com/new-atom.xml".to_string()),
            }),
            // 5. Poll gets 410 Gone -> sets last_error and backoff, retains article!
            Err(FetchError::Gone(410)),
        ]);

        let rep_304 = poll_feed_with_fetcher(
            &mut conn,
            &poll_fetcher,
            added.row_id,
            None,
            1_800,
            &clock,
            &mut rng,
        )
        .await
        .unwrap();
        assert!(rep_304.not_modified);
        assert!(rep_304.event.is_none());

        let rep_200 = poll_feed_with_fetcher(
            &mut conn,
            &poll_fetcher,
            added.row_id,
            None,
            1_800,
            &clock,
            &mut rng,
        )
        .await
        .unwrap();
        assert_eq!(rep_200.inserted_count, 0);
        assert_eq!(rep_200.updated_count, 1);
        assert!(rep_200.event.is_none()); // Only existing entry updated, 0 new items

        // Verify unread = 0 and starred = 1 were preserved while title/body updated, and 301 URL persisted!
        let (title, unread, starred, body): (String, i64, i64, String) = conn
            .query_row(
                "SELECT i.title, i.unread, i.starred, t.body
                 FROM items i JOIN item_text t ON t.item_id = i.id",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(title, "Release 1.0 (Updated)");
        assert_eq!(unread, 0);
        assert_eq!(starred, 1);
        assert_eq!(body, "Updated article body");

        let stored_url: String = conn
            .query_row(
                "SELECT url FROM feeds WHERE id = ?1",
                params![added.row_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(stored_url, "https://example.com/new-atom.xml");

        // Poll 410 Gone -> retains cached article and remains eligible in query_due_feeds once next_fetch_at arrives
        let rep_410 = poll_feed_with_fetcher(
            &mut conn,
            &poll_fetcher,
            added.row_id,
            None,
            1_800,
            &clock,
            &mut rng,
        )
        .await
        .unwrap();
        assert!(rep_410.error.is_some());

        let visible_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM visible_items", [], |r| r.get(0))
            .unwrap();
        assert_eq!(visible_count, 1);

        let due = query_due_feeds(&conn, rep_410.next_fetch_at).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].id, added.row_id);
    }

    // =========================================================================
    // Scenario 7, 8, 9: On-Demand BODY.PEEK[], Startup Recovery, & Window Preemption
    // =========================================================================
    #[tokio::test]
    async fn test_scenario_7_8_9_body_peek_attachment_refinement_recovery_and_window_preemption() {
        let mut conn = open_in_memory().unwrap();
        let (account_id, _acc_pid, folder_id, _fld_pid) = seed_imap_account_and_inbox(&conn);
        let clock = TestClock(20_000);
        let mut rng = DeterministicRng(55);

        let cache_dir =
            std::env::temp_dir().join(format!("debloatmail-sync-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&cache_dir);

        // Seed an item with a provisional attachment at section "2"
        let pid = PublicId::new("018f6e80-0000-7000-8000-000000000077").unwrap();
        let item_id = {
            let tx = conn.transaction().unwrap();
            let (id, _) = upsert_mail_header(
                &tx,
                &NewMailHeader {
                    public_id: pid.as_str(),
                    account_id,
                    folder_id,
                    uidvalidity: 100,
                    uid: 42,
                    message_id: Some("m42@example.org"),
                    in_reply_to: None,
                    references_hdr: None,
                    from_addr: Some("alice@example.org"),
                    to_addrs_json: Some("[\"bob@example.org\"]"),
                    cc_addrs_json: None,
                    reply_to: None,
                    subject_norm: Some("report"),
                    title: Some("Report"),
                    author: Some("Alice"),
                    published_at: Some(19_000),
                    sort_at: 19_000,
                    size_bytes: Some(500),
                    unread: true,
                    starred: false,
                    answered: false,
                    has_attachments: true,
                    flags_local: "[]",
                    created_at: 19_000,
                },
            )
            .unwrap();
            replace_provisional_attachments(
                &tx,
                id,
                &[NewAttachment {
                    section_id: "2",
                    filename: Some("provisional.pdf"),
                    mime_type: "application/pdf",
                    size_bytes: Some(100),
                    content_id: None,
                    is_pdf: true,
                }],
            )
            .unwrap();
            tx.commit().unwrap();
            id
        };

        let raw_mime = b"From: Alice <alice@example.org>\r\nSubject: Report\r\nMessage-ID: <m42@example.org>\r\nReferences: <root@example.org>\r\nContent-Type: multipart/mixed; boundary=\"B\"\r\n\r\n--B\r\nContent-Type: text/plain\r\n\r\nExtracted body paragraph.\r\n--B\r\nContent-Type: application/pdf; name=\"final.pdf\"\r\nContent-Disposition: attachment; filename=\"final.pdf\"\r\n\r\n%PDF-1.5 content\r\n--B--\r\n";

        let mem = MemoryStream::new([
            // SELECT + BODY.PEEK[]
            b"* 1 EXISTS\r\n* OK [UIDVALIDITY 100] ok\r\n* OK [UIDNEXT 43] ok\r\n<TAG> OK [READ-WRITE] SELECT completed\r\n".to_vec(),
            format!(
                "* 1 FETCH (UID 42 BODY[] {{{}}}\r\n{})\r\n<TAG> OK UID FETCH completed\r\n",
                raw_mime.len(),
                String::from_utf8_lossy(raw_mime)
            )
            .into_bytes(),
            // SELECT + BODY.PEEK[2]
            b"* 1 EXISTS\r\n* OK [UIDVALIDITY 100] ok\r\n* OK [UIDNEXT 43] ok\r\n<TAG> OK [READ-WRITE] SELECT completed\r\n".to_vec(),
            b"* 1 FETCH (UID 42 BODY[2] {16}\r\n%PDF-1.5 content)\r\n<TAG> OK UID FETCH completed\r\n".to_vec(),
        ]);

        let mut imap_conn =
            ImapConnection::from_authenticated_memory(mem, NegotiatedCapabilities::default());

        let body_out = fetch_item_body(
            &mut conn,
            &mut imap_conn,
            &cache_dir,
            item_id,
            &clock,
            &mut rng,
        )
        .await
        .unwrap();
        assert_eq!(body_out.body_state, BodyState::Cached);
        assert_eq!(body_out.body, "Extracted body paragraph.");
        assert_eq!(body_out.attachment_count, 1);

        // Fetch attachment section "2" -> cache miss fetches via BODY.PEEK[2]
        let att_out =
            fetch_item_attachment(&mut conn, &mut imap_conn, &cache_dir, item_id, "2", &clock)
                .await
                .unwrap();
        assert!(!att_out.from_cache);
        assert!(att_out.path.is_file());

        // Second fetch of section "2" -> served from BlobStore cache and touches last_used_at!
        let later_clock = TestClock(25_000);
        let att_cached = fetch_item_attachment(
            &mut conn,
            &mut imap_conn,
            &cache_dir,
            item_id,
            "2",
            &later_clock,
        )
        .await
        .unwrap();
        assert!(att_cached.from_cache);

        let written = String::from_utf8(imap_conn.stream.all_written_bytes()).unwrap();
        assert!(written.contains("BODY.PEEK[]"));
        assert!(written.contains("BODY.PEEK[2]"));

        // Startup recovery: set item body_state = 'pending' and verify run_startup_recovery resets it to 'none'
        conn.execute(
            "UPDATE items SET body_state = 'pending' WHERE id = ?1",
            params![item_id],
        )
        .unwrap();

        // Write an unreferenced orphan blob file with last_used_at = 25_000
        let orphan_hash = put_blob(&conn, &cache_dir, b"orphan-bytes", 25_000).unwrap();
        let orphan_path = blob_path(&cache_dir, &orphan_hash).unwrap();
        increment_blob_ref(&conn, &orphan_hash, 25_000).unwrap();
        conn.execute(
            "UPDATE blobs SET refcount = 0 WHERE hash = ?1",
            params![orphan_hash],
        )
        .unwrap();
        // Since last_used_at = 25_000 and now = 26_000 (< 3600s grace window), gc_blobs MUST protect it!
        assert_eq!(gc_blobs(&conn, &cache_dir, 26_000).unwrap(), 0);
        assert!(orphan_path.exists());

        let rec_report = run_startup_recovery(&conn, 26_000).unwrap();
        assert_eq!(rec_report.reset_pending_bodies, 1);

        let _ = std::fs::remove_dir_all(&cache_dir);
    }

    #[test]
    fn test_reconcile_discovered_folders_preserves_manual_role_and_unsubscribes_missing() {
        let mut conn = open_in_memory().unwrap();
        let (account_id, _acc_pid, _folder_id, _fld_pid) = seed_imap_account_and_inbox(&conn);
        let clock = TestClock(5_000);
        let mut rng = DeterministicRng(11);

        // Add a folder with a manual 'archive' role and another folder 'OldProject' that will disappear
        conn.execute(
            "INSERT INTO folders (
                public_id, account_id, name, display_name, role, role_source,
                uidvalidity, uidnext, highestmodseq, subscribed
             ) VALUES
                ('018f6e80-0000-7000-8000-000000000010', ?1, 'MyArchive', 'MyArchive', 'archive', 'manual', 0, 0, 0, 1),
                ('018f6e80-0000-7000-8000-000000000011', ?1, 'OldProject', 'OldProject', NULL, NULL, 0, 0, 0, 1)",
            params![account_id],
        )
        .unwrap();

        let discovered = vec![
            DiscoveredFolder {
                name: "INBOX".to_string(),
                display_name: "INBOX".to_string(),
                delimiter: Some('/'),
                role: Some(debloatmail_core::FolderRole::Inbox),
                role_source: Some(RoleSource::Name),
                selectable: true,
                sync_by_default: true,
            },
            // MyArchive appears in LIST with no special-use role, but manual role MUST be preserved!
            DiscoveredFolder {
                name: "MyArchive".to_string(),
                display_name: "MyArchive".to_string(),
                delimiter: Some('/'),
                role: None,
                role_source: None,
                selectable: true,
                sync_by_default: true,
            },
            // Server also reports an 'Archive' folder with SPECIAL-USE \Archive, which must NOT steal 'archive' from MyArchive!
            DiscoveredFolder {
                name: "ServerArchive".to_string(),
                display_name: "ServerArchive".to_string(),
                delimiter: Some('/'),
                role: Some(debloatmail_core::FolderRole::Archive),
                role_source: Some(RoleSource::SpecialUse),
                selectable: true,
                sync_by_default: true,
            },
        ];

        let rep =
            reconcile_discovered_folders(&mut conn, account_id, &discovered, &clock, &mut rng)
                .unwrap();
        assert_eq!(rep.inserted, 1);
        assert_eq!(rep.updated, 2);
        assert_eq!(rep.unsubscribed_missing, 1);

        let (my_role, my_src): (Option<String>, Option<String>) = conn
            .query_row(
                "SELECT role, role_source FROM folders WHERE account_id = ?1 AND name = 'MyArchive'",
                params![account_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(my_role.as_deref(), Some("archive"));
        assert_eq!(my_src.as_deref(), Some("manual"));

        let srv_role: Option<String> = conn
            .query_row(
                "SELECT role FROM folders WHERE account_id = ?1 AND name = 'ServerArchive'",
                params![account_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(srv_role, None);

        let old_sub: i64 = conn
            .query_row(
                "SELECT subscribed FROM folders WHERE account_id = ?1 AND name = 'OldProject'",
                params![account_id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(old_sub, 0);
    }

    #[tokio::test]
    async fn test_windowed_header_sync_preemption_and_resume_plus_server_rejection_attempts() {
        let mut conn = open_in_memory().unwrap();
        let (account_id, _acc_pid, folder_id, _fld_pid) = seed_imap_account_and_inbox(&conn);
        let clock = TestClock(30_000);
        let mut rng = DeterministicRng(42);

        // Set folder uidnext = 1 so server_uidnext = 2501 spans 2 windows: 1..=2000 and 2001..=2500
        conn.execute(
            "UPDATE folders SET uidnext = 1 WHERE id = ?1",
            params![folder_id],
        )
        .unwrap();

        let mem_win1 = MemoryStream::new([
            b"* 1 FETCH (UID 1 FLAGS () INTERNALDATE \"05-Nov-2026 12:00:00 +0000\" RFC822.SIZE 100 ENVELOPE (\"Thu, 05 Nov 2026 12:00:00 +0000\" \"Win1\" ((\"Bob\" NIL \"bob\" \"example.org\")) NIL NIL ((NIL NIL \"alice\" \"example.org\")) NIL NIL NIL \"<w1@example.org>\") BODYSTRUCTURE (\"TEXT\" \"PLAIN\" (\"CHARSET\" \"UTF-8\") NIL NIL \"7BIT\" 10 1 NIL NIL NIL NIL))\r\n<TAG> OK UID FETCH completed\r\n".to_vec(),
        ]);
        let mut imap_win1 =
            ImapConnection::from_authenticated_memory(mem_win1, NegotiatedCapabilities::default());
        let cancel = CancelToken::new();
        let folder_state = load_folder_state(&conn, folder_id).unwrap();

        // Preempt immediately after window 1 commits!
        let empty_map = std::collections::HashMap::new();
        let rep1 = sync_folder_headers_windowed(
            &mut conn,
            &mut imap_win1,
            &folder_state,
            100,
            2501,
            &empty_map,
            &cancel,
            || true,
            &clock,
            &mut rng,
        )
        .await
        .unwrap();

        assert_eq!(rep1.windows_committed, 1);
        assert!(rep1.preempted);
        assert_eq!(rep1.inserted_items, 1);

        // Verify folders.uidnext advanced monotonically to 2001 in the window 1 transaction
        let folder_after_w1 = load_folder_state(&conn, folder_id).unwrap();
        assert_eq!(folder_after_w1.uidnext, 2001);

        // Resume sync for window 2 (2001..=2500)
        let mem_win2 = MemoryStream::new([
            b"* 1 FETCH (UID 2500 FLAGS () INTERNALDATE \"05-Nov-2026 12:05:00 +0000\" RFC822.SIZE 100 ENVELOPE (\"Thu, 05 Nov 2026 12:05:00 +0000\" \"Win2\" ((\"Bob\" NIL \"bob\" \"example.org\")) NIL NIL ((NIL NIL \"alice\" \"example.org\")) NIL NIL NIL \"<w2@example.org>\") BODYSTRUCTURE (\"TEXT\" \"PLAIN\" (\"CHARSET\" \"UTF-8\") NIL NIL \"7BIT\" 10 1 NIL NIL NIL NIL))\r\n<TAG> OK UID FETCH completed\r\n".to_vec(),
        ]);
        let mut imap_win2 =
            ImapConnection::from_authenticated_memory(mem_win2, NegotiatedCapabilities::default());
        let rep2 = sync_folder_headers_windowed(
            &mut conn,
            &mut imap_win2,
            &folder_after_w1,
            100,
            2501,
            &empty_map,
            &cancel,
            || false,
            &clock,
            &mut rng,
        )
        .await
        .unwrap();

        assert_eq!(rep2.windows_committed, 1);
        assert!(!rep2.preempted);
        let folder_after_w2 = load_folder_state(&conn, folder_id).unwrap();
        assert_eq!(folder_after_w2.uidnext, 2501);

        // Now test Scenario 4: Offline (ConnectionFailed) vs Server Rejection (5xx) attempts counter
        conn.execute(
            "INSERT INTO drafts (
                public_id, account_id, to_addrs, subject, body, created_at, updated_at
             ) VALUES ('018f6e80-0000-7000-8000-000000000060', ?1, '[\"bob@example.org\"]', 'Subj', 'Body', 29000, 29000)",
            params![account_id],
        )
        .unwrap();
        let draft_id = conn.last_insert_rowid();
        let op_id =
            queue_send_from_draft(&mut conn, draft_id, None, true, &clock, &mut rng).unwrap();

        // 1. Offline / EOF before 354 -> SendOutcome::ConnectionFailed -> attempts stays 0!
        let mut smtp_offline = SmtpConnection::from_authenticated_memory(MemoryStream::default());
        let mut imap_dummy = ImapConnection::from_authenticated_memory(
            MemoryStream::default(),
            NegotiatedCapabilities::default(),
        );
        drain_account_ops(
            &mut conn,
            &mut imap_dummy,
            Some(&mut smtp_offline),
            account_id,
            &clock,
        )
        .await
        .unwrap();

        let (state_off, attempts_off): (String, i64) = conn
            .query_row(
                "SELECT state, attempts FROM pending_ops WHERE public_id = ?1",
                params![op_id.as_str()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(state_off, "in_flight");
        assert_eq!(attempts_off, 0);

        // 2. Server rejection (451 4.3.0 Temporary server error) increments attempts up to 5 and then marks failed!
        for expected_attempt in 1..=MAX_OP_SERVER_ATTEMPTS {
            let mut smtp_4xx = SmtpConnection::from_authenticated_memory(MemoryStream::new([
                b"451 4.3.0 Temporary server error\r\n".to_vec(),
            ]));
            drain_account_ops(
                &mut conn,
                &mut imap_dummy,
                Some(&mut smtp_4xx),
                account_id,
                &clock,
            )
            .await
            .unwrap();

            let (st, att): (String, i64) = conn
                .query_row(
                    "SELECT state, attempts FROM pending_ops WHERE public_id = ?1",
                    params![op_id.as_str()],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .unwrap();
            assert_eq!(att, i64::from(expected_attempt));
            if expected_attempt < MAX_OP_SERVER_ATTEMPTS {
                assert_eq!(st, "in_flight");
            } else {
                assert_eq!(st, "failed");
            }
        }
    }
}
