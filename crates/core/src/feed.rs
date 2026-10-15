use crate::item::{AccountId, FeedId, Timestamp};

/// RSS/Atom feed subscription.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Feed {
    pub id: FeedId,
    pub account_id: AccountId,
    pub url: String,
    pub title: Option<String>,
    pub site_url: Option<String>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub interval_secs: Option<u32>,
    pub last_fetch_at: Option<Timestamp>,
    pub next_fetch_at: Option<Timestamp>,
    pub backoff_secs: u32,
    pub consecutive_failures: u32,
    pub last_error: Option<String>,
    pub removed_at: Option<Timestamp>,
}
