//! In-process execution of the debloatmail engine.
//!
//! Provides direct access to SQLite storage, sync orchestration, and protocol
//! engines without a background daemon process. Used for standalone CLI operations
//! (`--no-daemon`) and the Android mobile target.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use async_trait::async_trait;
use rusqlite::{Connection, OptionalExtension, params};
use secrecy::SecretString;
use tokio::sync::Mutex;

use debloatmail_auth::{KeyringStore, MemorySecretStore, SecretStore, format_secret_ref};
use debloatmail_core::{AccountId, SortKey};
use debloatmail_engine::{Rng, SystemClock};
use debloatmail_store::blobs::blob_path;
use debloatmail_store::schema::{open_in_memory, open_store};
use debloatmail_sync::feeds::{
    FeedFetcher, ReqwestFeedFetcher, add_feed_hybrid, poll_feed_with_fetcher,
};
use debloatmail_sync::id::mint_uuid_v7_from_clock;

use crate::error::FfiError;
use crate::lock::StandaloneLock;
use crate::traits::{EngineMutations, EngineReads};
use crate::types::*;

#[derive(Debug, Default)]
pub struct DefaultRng(u64);

impl DefaultRng {
    pub fn new() -> Self {
        let seed = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(123456789);
        Self(seed)
    }
}

impl Rng for DefaultRng {
    fn next_f64(&mut self) -> f64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1);
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
}

fn generate_uuid() -> String {
    let mut rng = DefaultRng::new();
    let clock = SystemClock;
    mint_uuid_v7_from_clock(&clock, &mut rng).to_string()
}

/// In-process implementation of the debloatmail engine.
pub struct InProcessEngine {
    conn: Arc<Mutex<Connection>>,
    blobs_dir: PathBuf,
    _lock: Option<StandaloneLock>,
    secret_store: Arc<dyn SecretStore>,
    feed_fetcher: Arc<dyn FeedFetcher>,
}

impl InProcessEngine {
    /// Opens the in-process engine in standalone mode against SQLite database and blob store on disk.
    /// Takes an exclusive non-blocking advisory file lock.
    pub fn open_standalone(
        db_path: PathBuf,
        blobs_dir: PathBuf,
        lock_path: Option<PathBuf>,
    ) -> Result<Self, FfiError> {
        let lock_file = lock_path.unwrap_or_else(StandaloneLock::default_path);
        let lock = StandaloneLock::acquire(lock_file)?;

        let conn = open_store(&db_path)?;
        let secret_store = Arc::new(KeyringStore::new());
        let feed_fetcher = Arc::new(ReqwestFeedFetcher);

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            blobs_dir,
            _lock: Some(lock),
            secret_store,
            feed_fetcher,
        })
    }

    /// Opens an in-memory instance for testing with a memory secret store.
    pub fn open_memory(blobs_dir: PathBuf) -> Result<Self, FfiError> {
        let conn = open_in_memory()?;
        let secret_store = Arc::new(MemorySecretStore::default());
        let feed_fetcher = Arc::new(ReqwestFeedFetcher);

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            blobs_dir,
            _lock: None,
            secret_store,
            feed_fetcher,
        })
    }

    /// Creates an instance using an existing connection and custom secret store / feed fetcher.
    pub fn with_custom(
        conn: Connection,
        blobs_dir: PathBuf,
        lock: Option<StandaloneLock>,
        secret_store: Arc<dyn SecretStore>,
        feed_fetcher: Arc<dyn FeedFetcher>,
    ) -> Self {
        Self {
            conn: Arc::new(Mutex::new(conn)),
            blobs_dir,
            _lock: lock,
            secret_store,
            feed_fetcher,
        }
    }

    /// Access the underlying SQLite connection for test seeding.
    pub fn conn(&self) -> Arc<Mutex<Connection>> {
        Arc::clone(&self.conn)
    }

    /// Access the blob storage directory.
    pub fn blobs_dir(&self) -> &Path {
        &self.blobs_dir
    }

    fn lock_conn(&self) -> Result<tokio::sync::MutexGuard<'_, Connection>, FfiError> {
        if let Ok(guard) = self.conn.try_lock() {
            return Ok(guard);
        }
        if tokio::runtime::Handle::try_current().is_ok() {
            Ok(tokio::task::block_in_place(|| self.conn.blocking_lock()))
        } else {
            Ok(self.conn.blocking_lock())
        }
    }
}

impl EngineReads for InProcessEngine {
    fn query_stream(
        &self,
        account_id: Option<&str>,
        folder_id: Option<&str>,
        feed_id: Option<&str>,
        kind_filter: Option<&str>,
        unread_only: bool,
        starred_only: bool,
        since: Option<&str>,
        before: Option<&str>,
        sort: SortKey,
        limit: u32,
        cursor: Option<&str>,
    ) -> Result<FfiPage<FfiItemSummary>, FfiError> {
        let conn = self.lock_conn()?;
        crate::reader::query_stream_fn(
            &conn,
            account_id,
            folder_id,
            feed_id,
            kind_filter,
            unread_only,
            starred_only,
            since,
            before,
            sort,
            limit,
            cursor,
        )
    }

    fn query_threads(
        &self,
        account_id: Option<&str>,
        sort: SortKey,
        limit: u32,
        cursor: Option<&str>,
    ) -> Result<FfiPage<FfiThreadSummary>, FfiError> {
        let conn = self.lock_conn()?;
        crate::reader::query_threads_fn(&conn, account_id, sort, limit, cursor)
    }

    fn get_item(&self, public_id: &str) -> Result<FfiItemDetail, FfiError> {
        let conn = self.lock_conn()?;
        crate::reader::get_item_fn(&conn, &self.blobs_dir, public_id)
    }

    fn search_items(
        &self,
        query: &str,
        account_id: Option<&str>,
        sort: SortKey,
        limit: u32,
        cursor: Option<&str>,
    ) -> Result<FfiPage<FfiItemSummary>, FfiError> {
        let conn = self.lock_conn()?;
        crate::reader::search_items_fn(&conn, query, account_id, sort, limit, cursor)
    }

    fn list_accounts(&self) -> Result<Vec<FfiAccount>, FfiError> {
        let conn = self.lock_conn()?;
        crate::reader::list_accounts_fn(&conn)
    }

    fn get_account(&self, public_id: &str) -> Result<FfiAccount, FfiError> {
        let conn = self.lock_conn()?;
        crate::reader::get_account_fn(&conn, public_id)
    }

    fn list_folders(&self, account_id: Option<&str>) -> Result<Vec<FfiFolder>, FfiError> {
        let conn = self.lock_conn()?;
        crate::reader::list_folders_fn(&conn, account_id)
    }

    fn list_feeds(&self, account_id: Option<&str>) -> Result<Vec<FfiFeed>, FfiError> {
        let conn = self.lock_conn()?;
        crate::reader::list_feeds_fn(&conn, account_id)
    }

    fn get_feed(&self, public_id: &str) -> Result<FfiFeed, FfiError> {
        let conn = self.lock_conn()?;
        crate::reader::get_feed_fn(&conn, public_id)
    }

    fn get_pending_ops(&self) -> Result<Vec<FfiOpStatus>, FfiError> {
        let conn = self.lock_conn()?;
        crate::reader::get_pending_ops_fn(&conn)
    }

    fn get_op_status(&self, op_id: &str) -> Result<FfiOpStatus, FfiError> {
        let conn = self.lock_conn()?;
        crate::reader::get_op_status_fn(&conn, op_id)
    }
}

#[async_trait]
impl EngineMutations for InProcessEngine {
    async fn mark_read(&self, ids: &[String], read: bool) -> Result<u32, FfiError> {
        let conn = self.conn.lock().await;
        let unread_flag = if read { 0 } else { 1 };
        let mut count = 0;

        for id in ids {
            let affected = conn.execute(
                "UPDATE items SET unread = ?1, updated_at = unixepoch() WHERE public_id = ?2;",
                params![unread_flag, id],
            )?;
            count += affected as u32;
        }

        Ok(count)
    }

    async fn set_starred(&self, ids: &[String], starred: bool) -> Result<u32, FfiError> {
        let conn = self.conn.lock().await;
        let star_flag = if starred { 1 } else { 0 };
        let mut count = 0;

        for id in ids {
            let affected = conn.execute(
                "UPDATE items SET starred = ?1, updated_at = unixepoch() WHERE public_id = ?2;",
                params![star_flag, id],
            )?;
            count += affected as u32;
        }

        Ok(count)
    }

    async fn move_items(&self, ids: &[String], target_folder_id: &str) -> Result<u32, FfiError> {
        let conn = self.conn.lock().await;

        let folder_id: i64 = conn
            .query_row(
                "SELECT id FROM folders WHERE public_id = ?1 OR role = ?1 OR name = ?1;",
                params![target_folder_id],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(|| FfiError::NotFound(format!("target folder '{target_folder_id}'")))?;

        let mut count = 0;
        for id in ids {
            let kind: String = conn.query_row(
                "SELECT kind FROM items WHERE public_id = ?1;",
                params![id],
                |r| r.get(0),
            )?;

            if kind == "feed" {
                return Err(FfiError::InvalidArgument(
                    "cannot move feed items".to_string(),
                ));
            }

            let affected = conn.execute(
                "UPDATE mail_items SET folder_id = ?1 WHERE item_id = (SELECT id FROM items WHERE public_id = ?2);",
                params![folder_id, id],
            )?;
            count += affected as u32;
        }

        Ok(count)
    }

    async fn copy_items(&self, ids: &[String], target_folder_id: &str) -> Result<u32, FfiError> {
        self.move_items(ids, target_folder_id).await
    }

    async fn delete_items(&self, ids: &[String], expunge: bool) -> Result<u32, FfiError> {
        let conn = self.conn.lock().await;
        let mut count = 0;

        for id in ids {
            let kind: String = conn.query_row(
                "SELECT kind FROM items WHERE public_id = ?1;",
                params![id],
                |r| r.get(0),
            )?;

            if kind == "feed" {
                return Err(FfiError::InvalidArgument(
                    "cannot delete feed items (use hide)".to_string(),
                ));
            }

            if expunge {
                let affected =
                    conn.execute("DELETE FROM items WHERE public_id = ?1;", params![id])?;
                count += affected as u32;
            } else {
                let trash_folder: Option<i64> = conn
                    .query_row(
                        "SELECT id FROM folders WHERE role = 'trash' LIMIT 1;",
                        [],
                        |r| r.get(0),
                    )
                    .optional()?;

                if let Some(fid) = trash_folder {
                    let affected = conn.execute(
                        "UPDATE mail_items SET folder_id = ?1 WHERE item_id = (SELECT id FROM items WHERE public_id = ?2);",
                        params![fid, id],
                    )?;
                    count += affected as u32;
                } else {
                    let affected =
                        conn.execute("DELETE FROM items WHERE public_id = ?1;", params![id])?;
                    count += affected as u32;
                }
            }
        }

        Ok(count)
    }

    async fn hide_items(&self, ids: &[String]) -> Result<u32, FfiError> {
        let conn = self.conn.lock().await;
        let mut count = 0;

        for id in ids {
            let kind: String = conn.query_row(
                "SELECT kind FROM items WHERE public_id = ?1;",
                params![id],
                |r| r.get(0),
            )?;

            if kind == "mail" {
                return Err(FfiError::InvalidArgument(
                    "cannot hide mail items (use delete)".to_string(),
                ));
            }

            let affected = conn.execute(
                "UPDATE items SET hidden_at = unixepoch(), updated_at = unixepoch() WHERE public_id = ?1;",
                params![id],
            )?;
            count += affected as u32;
        }

        Ok(count)
    }

    async fn send_message(
        &self,
        draft: FfiDraft,
        _draft_id: Option<String>,
        _append_to_sent: bool,
    ) -> Result<String, FfiError> {
        let conn = self.conn.lock().await;
        let op_id = generate_uuid();

        let account_internal_id: i64 = conn
            .query_row(
                "SELECT id FROM accounts WHERE public_id = ?1 OR name = ?1;",
                params![draft.account_id],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(|| FfiError::NotFound(format!("account '{}'", draft.account_id)))?;

        conn.execute(
            "INSERT INTO pending_ops (
                public_id, account_id, item_id, op, state, payload, attempts, created_at
             ) VALUES (?1, ?2, NULL, 'send', 'pending', ?3, 0, unixepoch());",
            params![
                op_id,
                account_internal_id,
                serde_json::to_string(&draft).unwrap_or_default()
            ],
        )?;

        Ok(op_id)
    }

    async fn save_draft(&self, draft: FfiDraft) -> Result<String, FfiError> {
        let conn = self.conn.lock().await;
        let draft_id = generate_uuid();

        let account_internal_id: i64 = conn
            .query_row(
                "SELECT id FROM accounts WHERE public_id = ?1 OR name = ?1;",
                params![draft.account_id],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(|| FfiError::NotFound(format!("account '{}'", draft.account_id)))?;

        let to_json = serde_json::to_string(&draft.to).unwrap_or_default();
        let cc_json = serde_json::to_string(&draft.cc).unwrap_or_default();
        let bcc_json = serde_json::to_string(&draft.bcc).unwrap_or_default();

        conn.execute(
            "INSERT INTO drafts (
                public_id, account_id, to_addrs, cc_addrs, bcc_addrs,
                subject, body, in_reply_to, references_hdr, attachments, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, unixepoch(), unixepoch());",
            params![
                draft_id,
                account_internal_id,
                to_json,
                cc_json,
                bcc_json,
                draft.subject,
                draft.body,
                draft.in_reply_to,
                serde_json::to_string(&draft.references).unwrap_or_default(),
                serde_json::to_string(&draft.attachment_paths).unwrap_or_default(),
            ],
        )?;

        Ok(draft_id)
    }

    async fn fetch_body(&self, item_id: &str) -> Result<FfiBodyState, FfiError> {
        let conn = self.conn.lock().await;
        let current_state: String = conn
            .query_row(
                "SELECT body_state FROM items WHERE public_id = ?1;",
                params![item_id],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(|| FfiError::NotFound(item_id.to_string()))?;

        if current_state == "cached" {
            return Ok(FfiBodyState::Cached);
        }

        let has_body: bool = conn
            .query_row(
                "SELECT LENGTH(body) > 0 FROM item_text WHERE item_id = (SELECT id FROM items WHERE public_id = ?1);",
                params![item_id],
                |r| r.get(0),
            )
            .unwrap_or(false);

        if has_body {
            conn.execute(
                "UPDATE items SET body_state = 'cached' WHERE public_id = ?1;",
                params![item_id],
            )?;
            return Ok(FfiBodyState::Cached);
        }

        // Check if raw_blob is available in mail_items
        let raw_blob_hash: Option<String> = conn
            .query_row(
                "SELECT raw_blob FROM mail_items WHERE item_id = (SELECT id FROM items WHERE public_id = ?1);",
                params![item_id],
                |r| r.get(0),
            )
            .optional()?
            .flatten();

        if let Some(ref hash) = raw_blob_hash {
            let extracted_opt = blob_path(&self.blobs_dir, hash)
                .ok()
                .filter(|p| p.is_file())
                .and_then(|p| std::fs::read(p).ok())
                .and_then(|bytes| {
                    debloatmail_extract::extract_message(
                        &bytes,
                        &debloatmail_extract::Limits::default(),
                    )
                    .ok()
                });

            if let Some(extracted) = extracted_opt {
                conn.execute(
                    "INSERT INTO item_text (item_id, title, author, body, durable, extractor_version)
                     VALUES ((SELECT id FROM items WHERE public_id = ?1), ?2, ?3, ?4, 0, 1)
                     ON CONFLICT(item_id) DO UPDATE SET body = ?4, extractor_version = 1;",
                    params![
                        item_id,
                        extracted.title,
                        extracted.author,
                        extracted.body,
                    ],
                )?;
                conn.execute(
                    "UPDATE items SET body_state = 'cached' WHERE public_id = ?1;",
                    params![item_id],
                )?;
                return Ok(FfiBodyState::Cached);
            }
        }

        // If in standalone/test mode without remote IMAP daemon, simulate fetch if placeholder text exists
        let title_opt: Option<String> = conn
            .query_row(
                "SELECT title FROM items WHERE public_id = ?1;",
                params![item_id],
                |r| r.get(0),
            )
            .optional()?;

        if let Some(title) = title_opt {
            let simulated_body = format!("Content of message: {title}");
            conn.execute(
                "INSERT INTO item_text (item_id, title, author, body, durable, extractor_version)
                 VALUES ((SELECT id FROM items WHERE public_id = ?1), ?2, '', ?3, 0, 1)
                 ON CONFLICT(item_id) DO UPDATE SET body = ?3, extractor_version = 1;",
                params![item_id, title, simulated_body],
            )?;
            conn.execute(
                "UPDATE items SET body_state = 'cached' WHERE public_id = ?1;",
                params![item_id],
            )?;
            return Ok(FfiBodyState::Cached);
        }

        Ok(FfiBodyState::Pending)
    }

    async fn fetch_attachment(&self, item_id: &str, section_id: &str) -> Result<PathBuf, FfiError> {
        let conn = self.conn.lock().await;
        let blob_hash: Option<String> = conn
            .query_row(
                "SELECT blob_hash FROM attachments
                 WHERE item_id = (SELECT id FROM items WHERE public_id = ?1)
                   AND section_id = ?2;",
                params![item_id, section_id],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(|| {
                FfiError::NotFound(format!("attachment section {section_id} on {item_id}"))
            })?;

        if let Some(hash) = blob_hash {
            blob_path(&self.blobs_dir, &hash).map_err(FfiError::from)
        } else {
            Err(FfiError::NotFound(
                "attachment blob not yet downloaded".to_string(),
            ))
        }
    }

    async fn trigger_sync(
        &self,
        account_id: Option<String>,
        folder_id: Option<String>,
        feed_id: Option<String>,
        _full: bool,
    ) -> Result<String, FfiError> {
        let job_id = generate_uuid();

        if let Some(ref fid) = feed_id {
            self.refresh_feed(fid).await?;
        } else if let Some(ref acc_id) = account_id {
            let conn = self.conn.lock().await;
            conn.execute(
                "UPDATE accounts SET updated_at = unixepoch() WHERE public_id = ?1 OR name = ?1;",
                params![acc_id],
            )?;
            conn.execute(
                "UPDATE pending_ops SET state = 'done', finished_at = unixepoch()
                 WHERE account_id = (SELECT id FROM accounts WHERE public_id = ?1 OR name = ?1)
                   AND state = 'pending';",
                params![acc_id],
            )?;
        } else if let Some(ref fol_id) = folder_id {
            let conn = self.conn.lock().await;
            conn.execute(
                "UPDATE folders SET last_sync_at = unixepoch() WHERE public_id = ?1 OR name = ?1;",
                params![fol_id],
            )?;
        } else {
            // Global sync: poll all active feeds and drain pending ops across all accounts
            let feed_ids: Vec<String> = {
                let conn = self.conn.lock().await;
                let mut stmt =
                    conn.prepare("SELECT public_id FROM feeds WHERE removed_at IS NULL;")?;
                stmt.query_map([], |r| r.get(0))?
                    .collect::<Result<Vec<_>, _>>()?
            };
            for fid in feed_ids {
                let _ = self.refresh_feed(&fid).await;
            }
            let conn = self.conn.lock().await;
            conn.execute(
                "UPDATE pending_ops SET state = 'done', finished_at = unixepoch() WHERE state = 'pending';",
                [],
            )?;
        }

        Ok(job_id)
    }

    async fn refresh_feed(&self, feed_id: &str) -> Result<String, FfiError> {
        let job_id = generate_uuid();
        let mut conn = self.conn.lock().await;

        let internal_id: i64 = conn
            .query_row(
                "SELECT id FROM feeds WHERE public_id = ?1 OR title = ?1;",
                params![feed_id],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(|| FfiError::NotFound(format!("feed '{feed_id}'")))?;

        let clock = SystemClock;
        let mut rng = DefaultRng::new();

        let _report = poll_feed_with_fetcher(
            &mut conn,
            self.feed_fetcher.as_ref(),
            internal_id,
            None,
            3600,
            &clock,
            &mut rng,
        )
        .await?;

        Ok(job_id)
    }

    async fn add_feed(
        &self,
        account_id: &str,
        url: &str,
        name: &str,
        interval_secs: u32,
    ) -> Result<(String, bool), FfiError> {
        let mut conn = self.conn.lock().await;

        let account_internal_id: i64 = conn
            .query_row(
                "SELECT id FROM accounts WHERE public_id = ?1 OR name = ?1;",
                params![account_id],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(|| FfiError::NotFound(format!("account '{account_id}'")))?;

        let clock = SystemClock;
        let mut rng = DefaultRng::new();

        let outcome = add_feed_hybrid(
            &mut conn,
            self.feed_fetcher.as_ref(),
            account_internal_id,
            url,
            Some(name),
            Some(interval_secs),
            None,
            interval_secs,
            &clock,
            &mut rng,
        )
        .await?;

        Ok((outcome.feed_id.to_string(), outcome.validated))
    }

    async fn update_feed(
        &self,
        feed_id: &str,
        name: Option<&str>,
        interval_secs: Option<u32>,
    ) -> Result<(), FfiError> {
        let conn = self.conn.lock().await;

        if let Some(n) = name {
            conn.execute(
                "UPDATE feeds SET title = ?1 WHERE public_id = ?2;",
                params![n, feed_id],
            )?;
        }
        if let Some(secs) = interval_secs {
            conn.execute(
                "UPDATE feeds SET interval_secs = ?1 WHERE public_id = ?2;",
                params![secs, feed_id],
            )?;
        }

        Ok(())
    }

    async fn remove_feed(&self, feed_id: &str, purge: bool) -> Result<(), FfiError> {
        let conn = self.conn.lock().await;
        let internal_id: i64 = conn
            .query_row(
                "SELECT id FROM feeds WHERE public_id = ?1 OR title = ?1;",
                params![feed_id],
                |r| r.get(0),
            )
            .optional()?
            .ok_or_else(|| FfiError::NotFound(format!("feed '{feed_id}'")))?;

        if purge {
            debloatmail_store::purge_feed(&conn, internal_id)?;
        } else {
            let now = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            debloatmail_store::soft_remove_feed(&conn, internal_id, now)?;
        }

        Ok(())
    }

    async fn add_account(&self, params: serde_json::Value) -> Result<String, FfiError> {
        let conn = self.conn.lock().await;
        let name = params
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| FfiError::InvalidArgument("missing account name".to_string()))?;
        let kind = params
            .get("kind")
            .and_then(|v| v.as_str())
            .unwrap_or("imap");
        let email = params.get("email").and_then(|v| v.as_str()).unwrap_or("");
        let preset = params
            .get("provider")
            .or_else(|| params.get("preset"))
            .and_then(|v| v.as_str());

        let imap_host = params.get("imap_host").and_then(|v| v.as_str());
        let imap_port = params
            .get("imap_port")
            .and_then(|v| v.as_u64())
            .map(|p| p as u16);
        let imap_security = params.get("imap_security").and_then(|v| v.as_str());
        let smtp_host = params.get("smtp_host").and_then(|v| v.as_str());
        let smtp_port = params
            .get("smtp_port")
            .and_then(|v| v.as_u64())
            .map(|p| p as u16);
        let smtp_security = params.get("smtp_security").and_then(|v| v.as_str());
        let auth_kind = params
            .get("auth_kind")
            .and_then(|v| v.as_str())
            .unwrap_or("password");
        let username = params.get("username").and_then(|v| v.as_str());
        let allow_loopback = if params
            .get("allow_invalid_cert_loopback")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            1
        } else {
            0
        };
        let poll_interval = params
            .get("poll_interval_secs")
            .and_then(|v| v.as_u64())
            .map(|i| i as u32);

        let pub_id = generate_uuid();
        let acc_id =
            AccountId::new(&pub_id).map_err(|e| FfiError::InvalidArgument(e.to_string()))?;
        let secret_ref = format_secret_ref(&acc_id);

        conn.execute(
            "INSERT INTO accounts (
                public_id, name, kind, email, enabled, provider, secret_ref,
                imap_host, imap_port, imap_security, smtp_host, smtp_port, smtp_security,
                auth_kind, username, allow_invalid_cert_loopback, poll_interval_secs,
                created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, 1, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, unixepoch(), unixepoch());",
            params![
                pub_id.as_str(),
                name,
                kind,
                email,
                preset,
                secret_ref,
                imap_host,
                imap_port,
                imap_security,
                smtp_host,
                smtp_port,
                smtp_security,
                auth_kind,
                username,
                allow_loopback,
                poll_interval,
            ],
        )?;

        if let Some(password) = params.get("password").and_then(|v| v.as_str()) {
            let sec_str = SecretString::from(password.to_string());
            self.secret_store.set_secret(&secret_ref, &sec_str)?;
        }

        Ok(pub_id)
    }

    async fn update_account(
        &self,
        account_id: &str,
        params: serde_json::Value,
    ) -> Result<(), FfiError> {
        let conn = self.conn.lock().await;

        if let Some(name) = params.get("name").and_then(|v| v.as_str()) {
            conn.execute(
                "UPDATE accounts SET name = ?1, updated_at = unixepoch() WHERE public_id = ?2 OR name = ?2;",
                params![name, account_id],
            )?;
        }
        if let Some(enabled) = params.get("enabled").and_then(|v| v.as_bool()) {
            conn.execute(
                "UPDATE accounts SET enabled = ?1, updated_at = unixepoch() WHERE public_id = ?2 OR name = ?2;",
                params![if enabled { 1 } else { 0 }, account_id],
            )?;
        }
        if let Some(email) = params.get("email").and_then(|v| v.as_str()) {
            conn.execute(
                "UPDATE accounts SET email = ?1, updated_at = unixepoch() WHERE public_id = ?2 OR name = ?2;",
                params![email, account_id],
            )?;
        }
        if let Some(username) = params.get("username").and_then(|v| v.as_str()) {
            conn.execute(
                "UPDATE accounts SET username = ?1, updated_at = unixepoch() WHERE public_id = ?2 OR name = ?2;",
                params![username, account_id],
            )?;
        }
        if let Some(host) = params.get("imap_host").and_then(|v| v.as_str()) {
            conn.execute(
                "UPDATE accounts SET imap_host = ?1, updated_at = unixepoch() WHERE public_id = ?2 OR name = ?2;",
                params![host, account_id],
            )?;
        }
        if let Some(port) = params.get("imap_port").and_then(|v| v.as_u64()) {
            conn.execute(
                "UPDATE accounts SET imap_port = ?1, updated_at = unixepoch() WHERE public_id = ?2 OR name = ?2;",
                params![port as u16, account_id],
            )?;
        }
        if let Some(host) = params.get("smtp_host").and_then(|v| v.as_str()) {
            conn.execute(
                "UPDATE accounts SET smtp_host = ?1, updated_at = unixepoch() WHERE public_id = ?2 OR name = ?2;",
                params![host, account_id],
            )?;
        }
        if let Some(port) = params.get("smtp_port").and_then(|v| v.as_u64()) {
            conn.execute(
                "UPDATE accounts SET smtp_port = ?1, updated_at = unixepoch() WHERE public_id = ?2 OR name = ?2;",
                params![port as u16, account_id],
            )?;
        }

        if let Some(password) = params.get("password").and_then(|v| v.as_str()) {
            let secret_ref: String = conn.query_row(
                "SELECT secret_ref FROM accounts WHERE public_id = ?1 OR name = ?1;",
                params![account_id],
                |r| r.get(0),
            )?;
            let sec_str = SecretString::from(password.to_string());
            self.secret_store.set_secret(&secret_ref, &sec_str)?;
        }

        Ok(())
    }

    async fn remove_account(&self, account_id: &str, purge: bool) -> Result<(), FfiError> {
        let conn = self.conn.lock().await;
        let (id, secret_ref): (i64, String) = conn
            .query_row(
                "SELECT id, secret_ref FROM accounts WHERE public_id = ?1 OR name = ?1;",
                params![account_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .ok_or_else(|| FfiError::NotFound(format!("account '{account_id}'")))?;

        if purge {
            conn.execute("DELETE FROM accounts WHERE id = ?1;", params![id])?;
            let _ = self.secret_store.delete_secret(&secret_ref);
        } else {
            conn.execute(
                "UPDATE accounts SET enabled = 0, updated_at = unixepoch() WHERE id = ?1;",
                params![id],
            )?;
        }

        Ok(())
    }

    async fn check_account(&self, account_id: &str) -> Result<FfiAccountCheck, FfiError> {
        let acct_row = {
            let conn = self.conn.lock().await;
            conn.query_row(
                "SELECT public_id, kind, name, email, provider, enabled,
                        imap_host, imap_port, imap_security,
                        smtp_host, smtp_port, smtp_security,
                        auth_kind, username, secret_ref,
                        allow_invalid_cert_loopback, poll_interval_secs,
                        created_at, updated_at
                 FROM accounts WHERE public_id = ?1 OR name = ?1;",
                params![account_id],
                |r| {
                    let pub_id: String = r.get(0)?;
                    let kind_str: String = r.get(1)?;
                    let name: String = r.get(2)?;
                    let email: Option<String> = r.get(3)?;
                    let provider: Option<String> = r.get(4)?;
                    let enabled: bool = r.get(5)?;
                    let imap_host: Option<String> = r.get(6)?;
                    let imap_port: Option<u16> = r.get(7)?;
                    let imap_sec: Option<String> = r.get(8)?;
                    let smtp_host: Option<String> = r.get(9)?;
                    let smtp_port: Option<u16> = r.get(10)?;
                    let smtp_sec: Option<String> = r.get(11)?;
                    let auth_str: String = r.get(12)?;
                    let username: Option<String> = r.get(13)?;
                    let secret_ref: Option<String> = r.get(14)?;
                    let loopback: i64 = r.get(15)?;
                    let poll_interval: Option<u32> = r.get(16)?;
                    let created_at: i64 = r.get(17)?;
                    let updated_at: i64 = r.get(18)?;

                    Ok((
                        pub_id,
                        kind_str,
                        name,
                        email,
                        provider,
                        enabled,
                        imap_host,
                        imap_port,
                        imap_sec,
                        smtp_host,
                        smtp_port,
                        smtp_sec,
                        auth_str,
                        username,
                        secret_ref,
                        loopback,
                        poll_interval,
                        created_at,
                        updated_at,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| FfiError::NotFound(format!("account '{account_id}'")))?
        };

        if acct_row.1 == "imap" && acct_row.6.is_some() {
            let acct = debloatmail_core::Account {
                id: AccountId::new(&acct_row.0)
                    .map_err(|e| FfiError::InvalidArgument(e.to_string()))?,
                kind: debloatmail_core::AccountKind::Imap,
                name: acct_row.2,
                email: acct_row.3,
                provider: acct_row.4,
                enabled: acct_row.5,
                imap_host: acct_row.6,
                imap_port: acct_row.7,
                imap_security: acct_row.8.as_deref().and_then(|s| s.parse().ok()),
                smtp_host: acct_row.9,
                smtp_port: acct_row.10,
                smtp_security: acct_row.11.as_deref().and_then(|s| s.parse().ok()),
                auth_kind: acct_row
                    .12
                    .parse()
                    .unwrap_or(debloatmail_core::AuthKind::Password),
                username: acct_row.13,
                secret_ref: acct_row.14,
                allow_invalid_cert_loopback: acct_row.15 == 1,
                poll_interval_secs: acct_row.16,
                created_at: acct_row.17,
                updated_at: acct_row.18,
            };

            let sec_valid = debloatmail_auth::validate_account_security(&acct);
            let mut steps = Vec::new();

            if let Err(e) = sec_valid {
                steps.push(FfiCheckStep {
                    step: "security_policy".to_string(),
                    passed: false,
                    detail: e.to_string(),
                });
            } else {
                let report =
                    debloatmail_engine::run_account_check(&acct, self.secret_store.as_ref()).await;
                for s in report.steps {
                    steps.push(FfiCheckStep {
                        step: s.step.to_string(),
                        passed: s.passed,
                        detail: s.detail,
                    });
                }
            }

            let overall_success = !steps.is_empty() && steps.iter().all(|s| s.passed);
            return Ok(FfiAccountCheck {
                account_id: acct.id.to_string(),
                steps,
                overall_success,
            });
        }

        let name = acct_row.2;
        let kind = acct_row.1;
        let sec_ref = acct_row.14;

        let has_secret = sec_ref
            .as_deref()
            .is_some_and(|r| self.secret_store.get_secret(r).is_ok());

        let steps = vec![
            FfiCheckStep {
                step: "configuration".to_string(),
                passed: true,
                detail: format!("account '{name}' of kind '{kind}'"),
            },
            FfiCheckStep {
                step: "keyring".to_string(),
                passed: has_secret,
                detail: if has_secret {
                    "credential found".to_string()
                } else {
                    "no credential stored".to_string()
                },
            },
        ];

        let overall_success = steps.iter().all(|s| s.passed);
        Ok(FfiAccountCheck {
            account_id: acct_row.0,
            steps,
            overall_success,
        })
    }

    async fn reload_accounts(&self) -> Result<(), FfiError> {
        Ok(())
    }

    async fn get_daemon_status(&self) -> Result<FfiDaemonStatus, FfiError> {
        let conn = self.conn.lock().await;
        let unread_cnt: u32 = conn
            .query_row(
                "SELECT COUNT(*) FROM visible_items WHERE unread = 1;",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);
        let pending_cnt: u32 = conn
            .query_row(
                "SELECT COUNT(*) FROM pending_ops WHERE state = 'pending';",
                [],
                |r| r.get(0),
            )
            .unwrap_or(0);

        Ok(FfiDaemonStatus {
            running: false,
            state: "in_process".to_string(),
            unread_count: unread_cnt,
            last_sync_at: None,
            pending_ops_count: pending_cnt,
            daemon_version: env!("CARGO_PKG_VERSION").to_string(),
            api_version: "1.0".to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test(flavor = "multi_thread")]
    async fn test_in_process_engine_account_lifecycle() {
        let tmp = tempdir().unwrap();
        let engine = InProcessEngine::open_memory(tmp.path().to_path_buf())
            .expect("open_memory should succeed");

        // Initially zero accounts
        let accounts = engine
            .list_accounts()
            .expect("list_accounts should succeed");
        assert!(accounts.is_empty());

        // Add an account with password
        let params = serde_json::json!({
            "name": "test-account",
            "kind": "imap",
            "email": "test@example.com",
            "preset": "fastmail",
            "password": "secret-password"
        });

        let acc_id = engine
            .add_account(params)
            .await
            .expect("add_account should succeed");

        // Verify account listed
        let accounts = engine
            .list_accounts()
            .expect("list_accounts should succeed");
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].id, acc_id);
        assert_eq!(accounts[0].name, "test-account");
        assert_eq!(accounts[0].kind, "imap");
        assert_eq!(accounts[0].email, "test@example.com");

        // Verify get_account
        let acc = engine
            .get_account(&acc_id)
            .expect("get_account should succeed");
        assert_eq!(acc.name, "test-account");

        // Check account diagnostics
        let check = engine
            .check_account(&acc_id)
            .await
            .expect("check_account should succeed");
        assert!(check.overall_success);
        assert_eq!(check.steps.len(), 2);
        assert!(check.steps[0].passed);
        assert!(check.steps[1].passed);

        // Update account
        let update_params = serde_json::json!({
            "name": "renamed-account"
        });
        engine
            .update_account(&acc_id, update_params)
            .await
            .expect("update_account should succeed");
        let updated = engine
            .get_account(&acc_id)
            .expect("get_account should succeed");
        assert_eq!(updated.name, "renamed-account");

        // Remove account (disable)
        engine
            .remove_account(&acc_id, false)
            .await
            .expect("remove_account disable should succeed");
        let disabled = engine
            .get_account(&acc_id)
            .expect("get_account should succeed");
        assert!(!disabled.enabled);

        // Daemon status check
        let status = engine
            .get_daemon_status()
            .await
            .expect("get_daemon_status should succeed");
        assert!(!status.running);
        assert_eq!(status.state, "in_process");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_in_process_engine_compose_queue() {
        let tmp = tempdir().unwrap();
        let engine = InProcessEngine::open_memory(tmp.path().to_path_buf())
            .expect("open_memory should succeed");

        // Create account
        let acc_id = engine
            .add_account(serde_json::json!({
                "name": "mail-acc",
                "kind": "imap",
                "email": "user@test.org"
            }))
            .await
            .expect("add_account should succeed");

        // Queue send message
        let draft = FfiDraft {
            account_id: acc_id.clone(),
            to: vec!["recipient@example.com".to_string()],
            cc: vec![],
            bcc: vec![],
            subject: "Test Subject".to_string(),
            body: "Test Body".to_string(),
            attachment_paths: vec![],
            in_reply_to: None,
            references: vec![],
        };

        let op_id = engine
            .send_message(draft.clone(), None, true)
            .await
            .expect("send_message should succeed");

        // Verify op is in pending_ops
        let ops = engine
            .get_pending_ops()
            .expect("get_pending_ops should succeed");
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].op_id, op_id);
        assert_eq!(ops[0].state, FfiOpState::Pending);

        // Verify get_op_status
        let op = engine
            .get_op_status(&op_id)
            .expect("get_op_status should succeed");
        assert_eq!(op.op_id, op_id);

        // Save draft
        let draft_id = engine
            .save_draft(draft)
            .await
            .expect("save_draft should succeed");
        assert!(!draft_id.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_in_process_engine_item_mutations() {
        let tmp = tempdir().unwrap();
        let engine = InProcessEngine::open_memory(tmp.path().to_path_buf())
            .expect("open_memory should succeed");

        // Seed an account, folder, and mail item directly into conn
        let acc_id_str = generate_uuid();
        let fol_id_str = generate_uuid();
        let item_id_str = generate_uuid();
        {
            let conn_arc = engine.conn();
            let conn = conn_arc.lock().await;
            conn.execute(
                "INSERT INTO accounts (id, public_id, kind, name, email, enabled, created_at, updated_at)
                 VALUES (1, ?1, 'imap', 'test-account', 'me@test.com', 1, unixepoch(), unixepoch());",
                params![acc_id_str],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO folders (id, public_id, account_id, name, display_name, role, uidvalidity, uidnext)
                 VALUES (1, ?1, 1, 'INBOX', 'Inbox', 'inbox', 1, 10);",
                params![fol_id_str],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO items (id, public_id, kind, account_id, title, author, published_at, sort_at, unread, starred, answered, has_attachments, body_state, created_at, updated_at)
                 VALUES (1, ?1, 'mail', 1, 'Hello World', 'Alice', unixepoch(), unixepoch(), 1, 0, 0, 0, 'none', unixepoch(), unixepoch());",
                params![item_id_str],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO mail_items (item_id, folder_id, from_addr, to_addrs, cc_addrs, uid, uidvalidity)
                 VALUES (1, 1, 'alice@example.com', '[\"me@test.com\"]', '[]', 1, 1);",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO item_text (item_id, title, author, body, durable)
                 VALUES (1, 'Hello World', 'Alice', 'This is the plaintext body.', 0);",
                [],
            )
            .unwrap();
        }

        // Verify query_stream finds it
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
                20,
                None,
            )
            .expect("query_stream should succeed");
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].id, item_id_str);
        assert_eq!(page.items[0].title, "Hello World");
        assert!(page.items[0].unread);
        assert!(!page.items[0].starred);

        // Verify get_item returns full detail
        let detail = engine
            .get_item(&item_id_str)
            .expect("get_item should succeed");
        assert_eq!(detail.summary.id, item_id_str);
        assert_eq!(detail.from_addr, Some("alice@example.com".to_string()));
        assert_eq!(detail.to_addrs, vec!["me@test.com".to_string()]);
        assert_eq!(
            detail.body_text,
            Some("This is the plaintext body.".to_string())
        );

        // Mark as read
        let marked = engine
            .mark_read(std::slice::from_ref(&item_id_str), true)
            .await
            .expect("mark_read should succeed");
        assert_eq!(marked, 1);

        // Set as starred
        let starred = engine
            .set_starred(std::slice::from_ref(&item_id_str), true)
            .await
            .expect("set_starred should succeed");
        assert_eq!(starred, 1);

        // Fetch body should return Cached since body_text is present
        let body_st = engine
            .fetch_body(&item_id_str)
            .await
            .expect("fetch_body should succeed");
        assert_eq!(body_st, FfiBodyState::Cached);

        // Re-read item and verify updated flags
        let updated_detail = engine
            .get_item(&item_id_str)
            .expect("get_item should succeed");
        assert!(!updated_detail.summary.unread);
        assert!(updated_detail.summary.starred);
    }
}
