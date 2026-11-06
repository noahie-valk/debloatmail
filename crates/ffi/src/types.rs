//! Transfer and presentation data structures for `debloatmail-ffi`.

use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;

use debloatmail_core::{BodyState, ItemKind, OpState, RoleSource};
use serde::{Deserialize, Serialize};

use crate::error::FfiError;

/// Type of item: email message or syndicated feed entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FfiItemKind {
    Mail,
    Feed,
}

impl FfiItemKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Mail => "mail",
            Self::Feed => "feed",
        }
    }
}

impl fmt::Display for FfiItemKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl From<ItemKind> for FfiItemKind {
    fn from(kind: ItemKind) -> Self {
        match kind {
            ItemKind::Mail => Self::Mail,
            ItemKind::Feed => Self::Feed,
        }
    }
}

impl From<FfiItemKind> for ItemKind {
    fn from(kind: FfiItemKind) -> Self {
        match kind {
            FfiItemKind::Mail => Self::Mail,
            FfiItemKind::Feed => Self::Feed,
        }
    }
}

impl FromStr for FfiItemKind {
    type Err = FfiError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "mail" => Ok(Self::Mail),
            "feed" => Ok(Self::Feed),
            other => Err(FfiError::InvalidArgument(format!(
                "invalid item kind '{other}', expected 'mail' or 'feed'"
            ))),
        }
    }
}

/// Cache status of an item's body.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FfiBodyState {
    None,
    Pending,
    Cached,
    Failed,
}

impl FfiBodyState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Pending => "pending",
            Self::Cached => "cached",
            Self::Failed => "failed",
        }
    }
}

impl fmt::Display for FfiBodyState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl From<BodyState> for FfiBodyState {
    fn from(state: BodyState) -> Self {
        match state {
            BodyState::None => Self::None,
            BodyState::Pending => Self::Pending,
            BodyState::Cached => Self::Cached,
            BodyState::Failed => Self::Failed,
        }
    }
}

impl From<FfiBodyState> for BodyState {
    fn from(state: FfiBodyState) -> Self {
        match state {
            FfiBodyState::None => Self::None,
            FfiBodyState::Pending => Self::Pending,
            FfiBodyState::Cached => Self::Cached,
            FfiBodyState::Failed => Self::Failed,
        }
    }
}

/// Provenance of a mailbox folder's assigned special role.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FfiRoleSource {
    SpecialUse,
    Name,
    Manual,
}

impl FfiRoleSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::SpecialUse => "special_use",
            Self::Name => "name",
            Self::Manual => "manual",
        }
    }
}

impl fmt::Display for FfiRoleSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl FromStr for FfiRoleSource {
    type Err = crate::error::FfiError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "special-use" | "special_use" => Ok(Self::SpecialUse),
            "name" => Ok(Self::Name),
            "manual" => Ok(Self::Manual),
            other => Err(crate::error::FfiError::InvalidArgument(format!(
                "invalid role source: {other}"
            ))),
        }
    }
}

impl From<RoleSource> for FfiRoleSource {
    fn from(source: RoleSource) -> Self {
        match source {
            RoleSource::SpecialUse => Self::SpecialUse,
            RoleSource::Name => Self::Name,
            RoleSource::Manual => Self::Manual,
        }
    }
}

impl From<FfiRoleSource> for RoleSource {
    fn from(source: FfiRoleSource) -> Self {
        match source {
            FfiRoleSource::SpecialUse => Self::SpecialUse,
            FfiRoleSource::Name => Self::Name,
            FfiRoleSource::Manual => Self::Manual,
        }
    }
}

/// Execution state of a queued operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FfiOpState {
    Pending,
    InFlight,
    Done,
    Failed,
}

impl FfiOpState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InFlight => "in_flight",
            Self::Done => "done",
            Self::Failed => "failed",
        }
    }
}

impl fmt::Display for FfiOpState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl From<OpState> for FfiOpState {
    fn from(state: OpState) -> Self {
        match state {
            OpState::Pending => Self::Pending,
            OpState::InFlight => Self::InFlight,
            OpState::Done => Self::Done,
            OpState::Failed => Self::Failed,
        }
    }
}

impl From<FfiOpState> for OpState {
    fn from(state: FfiOpState) -> Self {
        match state {
            FfiOpState::Pending => Self::Pending,
            FfiOpState::InFlight => Self::InFlight,
            FfiOpState::Done => Self::Done,
            FfiOpState::Failed => Self::Failed,
        }
    }
}

/// High-level summary of an item (mail message or feed entry).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FfiItemSummary {
    pub id: String,
    pub short_id: String,
    pub kind: FfiItemKind,
    pub account_id: String,
    pub account_name: String,
    pub folder_id: Option<String>,
    pub folder_name: Option<String>,
    pub title: String,
    pub author: String,
    pub published_at: String,
    pub sort_at: String,
    pub unread: bool,
    pub starred: bool,
    pub has_attachments: bool,
    pub thread_id: Option<String>,
    pub body_state: FfiBodyState,
}

/// Metadata describing an attachment extracted or provisionally discovered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FfiAttachmentInfo {
    pub section_id: String,
    pub filename: Option<String>,
    pub content_type: String,
    pub size_bytes: Option<u64>,
    pub blob_hash: Option<String>,
    pub blob_path: Option<PathBuf>,
}

/// Comprehensive detail of a single item including body text and recipients.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FfiItemDetail {
    pub summary: FfiItemSummary,
    pub from_addr: Option<String>,
    pub to_addrs: Vec<String>,
    pub cc_addrs: Vec<String>,
    pub reply_to: Option<String>,
    pub body_text: Option<String>,
    pub raw_blob_path: Option<PathBuf>,
    pub attachments: Vec<FfiAttachmentInfo>,
}

/// Summary of a conversation thread.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FfiThreadSummary {
    pub id: String,
    pub account_id: String,
    pub display_subject: String,
    pub reply_count: u32,
    pub unread_count: u32,
    pub last_activity: String,
    pub participants: Vec<String>,
}

/// Keyset-paginated page of results.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FfiPage<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<String>,
    pub has_more: bool,
}

/// Representation of an account (IMAP or Feed).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FfiAccount {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub email: String,
    pub enabled: bool,
    pub provider_preset: Option<String>,
    pub last_sync_at: Option<String>,
    pub unread_count: u32,
}

/// Representation of a mailbox folder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FfiFolder {
    pub id: String,
    pub account_id: String,
    pub name: String,
    pub role: Option<String>,
    pub role_source: Option<FfiRoleSource>,
    pub item_count: u32,
    pub unread_count: u32,
}

/// Representation of a syndicated feed subscription.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FfiFeed {
    pub id: String,
    pub account_id: String,
    pub url: String,
    pub name: String,
    pub title: Option<String>,
    pub interval_secs: u32,
    pub last_fetch_at: Option<String>,
    pub next_fetch_at: Option<String>,
    pub unread_count: u32,
}

/// Draft parameters for message composition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FfiDraft {
    pub account_id: String,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
    pub subject: String,
    pub body: String,
    pub attachment_paths: Vec<PathBuf>,
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
}

/// Status of a queued operation in the pending_ops queue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FfiOpStatus {
    pub op_id: String,
    pub state: FfiOpState,
    pub attempts: u32,
    pub last_error: Option<String>,
}

/// Single step result from an account diagnostic check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FfiCheckStep {
    pub step: String,
    pub passed: bool,
    pub detail: String,
}

/// Overall diagnostic result for an account check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FfiAccountCheck {
    pub account_id: String,
    pub steps: Vec<FfiCheckStep>,
    pub overall_success: bool,
}

/// Status of the background daemon process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FfiDaemonStatus {
    pub running: bool,
    pub state: String,
    pub unread_count: u32,
    pub last_sync_at: Option<String>,
    pub pending_ops_count: u32,
    pub daemon_version: String,
    pub api_version: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_types_serde_roundtrip() {
        let summary = FfiItemSummary {
            id: "018f3a2b-7c41-7e2a-9d3f-1a2b3c4d5e6f".to_string(),
            short_id: "018f3a2b".to_string(),
            kind: FfiItemKind::Mail,
            account_id: "018f3a9c-e4f2-7a1b-8c3d-9f0a1b2c3d4e".to_string(),
            account_name: "fastmail".to_string(),
            folder_id: Some("018f3aa1-0000-7000-8000-000000000001".to_string()),
            folder_name: Some("INBOX".to_string()),
            title: "Re: release checklist".to_string(),
            author: "Alice Chen".to_string(),
            published_at: "2026-10-07T14:22:11Z".to_string(),
            sort_at: "2026-10-07T14:25:02Z".to_string(),
            unread: true,
            starred: false,
            has_attachments: true,
            thread_id: Some("019f3a2b-7d52-7e1c-9a4b-2c3d4e5f6a7b".to_string()),
            body_state: FfiBodyState::Cached,
        };

        let json = serde_json::to_string(&summary).unwrap();
        assert!(json.contains("\"kind\":\"mail\""));
        assert!(json.contains("\"body_state\":\"cached\""));

        let deserialized: FfiItemSummary = serde_json::from_str(&json).unwrap();
        assert_eq!(summary, deserialized);
    }
}
