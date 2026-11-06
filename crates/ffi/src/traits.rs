//! Qt-free engine traits defining the boundary for UI and client consumers.

use async_trait::async_trait;
use debloatmail_core::SortKey;
use std::path::PathBuf;

use crate::error::FfiError;
use crate::types::*;

/// Read operations over the unified mail and feed store.
pub trait EngineReads: Send + Sync {
    /// Queries the unified content stream with optional filters, ordering, and keyset cursor.
    #[allow(clippy::too_many_arguments)]
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
    ) -> Result<FfiPage<FfiItemSummary>, FfiError>;

    /// Queries conversation threads for an account or across all accounts.
    fn query_threads(
        &self,
        account_id: Option<&str>,
        sort: SortKey,
        limit: u32,
        cursor: Option<&str>,
    ) -> Result<FfiPage<FfiThreadSummary>, FfiError>;

    /// Retrieves detailed content for a single item by its public UUIDv7 string.
    fn get_item(&self, public_id: &str) -> Result<FfiItemDetail, FfiError>;

    /// Searches items using full-text search (FTS5) and metadata matching.
    fn search_items(
        &self,
        query: &str,
        account_id: Option<&str>,
        sort: SortKey,
        limit: u32,
        cursor: Option<&str>,
    ) -> Result<FfiPage<FfiItemSummary>, FfiError>;

    /// Lists all configured accounts.
    fn list_accounts(&self) -> Result<Vec<FfiAccount>, FfiError>;

    /// Retrieves a single account by public ID or name.
    fn get_account(&self, public_id: &str) -> Result<FfiAccount, FfiError>;

    /// Lists folders, optionally scoped to a single account.
    fn list_folders(&self, account_id: Option<&str>) -> Result<Vec<FfiFolder>, FfiError>;

    /// Lists feeds, optionally scoped to a single feed account.
    fn list_feeds(&self, account_id: Option<&str>) -> Result<Vec<FfiFeed>, FfiError>;

    /// Retrieves a single feed by public ID.
    fn get_feed(&self, public_id: &str) -> Result<FfiFeed, FfiError>;

    /// Lists all currently pending operations in the queue.
    fn get_pending_ops(&self) -> Result<Vec<FfiOpStatus>, FfiError>;

    /// Gets status of a single operation by op ID.
    fn get_op_status(&self, op_id: &str) -> Result<FfiOpStatus, FfiError>;
}

/// Mutation and execution operations modifying state or dispatching protocol jobs.
#[async_trait]
pub trait EngineMutations: Send + Sync {
    /// Marks items as read or unread. Returns number of items affected.
    async fn mark_read(&self, ids: &[String], read: bool) -> Result<u32, FfiError>;

    /// Marks items as starred or unstarred. Returns number of items affected.
    async fn set_starred(&self, ids: &[String], starred: bool) -> Result<u32, FfiError>;

    /// Moves mail items to a target folder. Returns number of items moved.
    async fn move_items(&self, ids: &[String], target_folder_id: &str) -> Result<u32, FfiError>;

    /// Copies mail items to a target folder. Returns number of items copied.
    async fn copy_items(&self, ids: &[String], target_folder_id: &str) -> Result<u32, FfiError>;

    /// Deletes mail items (moves to trash or expunges). Returns number of items deleted.
    async fn delete_items(&self, ids: &[String], expunge: bool) -> Result<u32, FfiError>;

    /// Hides feed items from all streams. Returns number of items hidden.
    async fn hide_items(&self, ids: &[String]) -> Result<u32, FfiError>;

    /// Enqueues a message send operation from draft. Returns operation ID.
    async fn send_message(
        &self,
        draft: FfiDraft,
        draft_id: Option<String>,
        append_to_sent: bool,
    ) -> Result<String, FfiError>;

    /// Saves a local draft. Returns draft ID.
    async fn save_draft(&self, draft: FfiDraft) -> Result<String, FfiError>;

    /// Fetches uncached body text on demand. Returns updated body state.
    async fn fetch_body(&self, item_id: &str) -> Result<FfiBodyState, FfiError>;

    /// Fetches an attachment section on demand. Returns path to cached blob.
    async fn fetch_attachment(&self, item_id: &str, section_id: &str) -> Result<PathBuf, FfiError>;

    /// Triggers a synchronization job for the specified scope. Returns job ID.
    async fn trigger_sync(
        &self,
        account_id: Option<String>,
        folder_id: Option<String>,
        feed_id: Option<String>,
        full: bool,
    ) -> Result<String, FfiError>;

    /// Refreshes a single feed. Returns job ID.
    async fn refresh_feed(&self, feed_id: &str) -> Result<String, FfiError>;

    /// Adds and validates a new feed subscription. Returns `(feed_id, validated)`.
    async fn add_feed(
        &self,
        account_id: &str,
        url: &str,
        name: &str,
        interval_secs: u32,
    ) -> Result<(String, bool), FfiError>;

    /// Updates feed metadata or poll interval.
    async fn update_feed(
        &self,
        feed_id: &str,
        name: Option<&str>,
        interval_secs: Option<u32>,
    ) -> Result<(), FfiError>;

    /// Removes a feed subscription (soft-remove if `purge=false`, hard purge if `purge=true`).
    async fn remove_feed(&self, feed_id: &str, purge: bool) -> Result<(), FfiError>;

    /// Adds a new mail or feed account. Returns account ID.
    async fn add_account(&self, params: serde_json::Value) -> Result<String, FfiError>;

    /// Updates existing account configuration.
    async fn update_account(
        &self,
        account_id: &str,
        params: serde_json::Value,
    ) -> Result<(), FfiError>;

    /// Removes or disables an account.
    async fn remove_account(&self, account_id: &str, purge: bool) -> Result<(), FfiError>;

    /// Executes connectivity, authentication, and TLS diagnostics for an account.
    async fn check_account(&self, account_id: &str) -> Result<FfiAccountCheck, FfiError>;

    /// Reloads account configurations and resets idle connections.
    async fn reload_accounts(&self) -> Result<(), FfiError>;

    /// Queries the background daemon status.
    async fn get_daemon_status(&self) -> Result<FfiDaemonStatus, FfiError>;
}

/// Unified engine facade providing both read and mutation interfaces.
pub trait Engine: EngineReads + EngineMutations {}
impl<T: EngineReads + EngineMutations> Engine for T {}
