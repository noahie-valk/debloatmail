use crate::item::{AccountId, ThreadId, Timestamp};

/// A conversation thread calculated per account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thread {
    pub id: ThreadId,
    pub account: AccountId,
    pub subject: String,
    pub last_activity: Timestamp,
    pub item_count: u32,
    pub unread_count: u32,
}
