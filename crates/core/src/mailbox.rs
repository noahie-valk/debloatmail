use std::str::FromStr;

use crate::error::CoreError;
use crate::item::{AccountId, FolderId, Timestamp};

/// Standard role for a mail folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FolderRole {
    Inbox,
    Sent,
    Drafts,
    Trash,
    Archive,
    Junk,
    All,
}

impl FolderRole {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Inbox => "inbox",
            Self::Sent => "sent",
            Self::Drafts => "drafts",
            Self::Trash => "trash",
            Self::Archive => "archive",
            Self::Junk => "junk",
            Self::All => "all",
        }
    }
}

impl FromStr for FolderRole {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "inbox" => Ok(Self::Inbox),
            "sent" => Ok(Self::Sent),
            "drafts" => Ok(Self::Drafts),
            "trash" => Ok(Self::Trash),
            "archive" => Ok(Self::Archive),
            "junk" => Ok(Self::Junk),
            "all" => Ok(Self::All),
            other => Err(CoreError::InvalidToken {
                expected: "inbox, sent, drafts, trash, archive, junk, all",
                found: other.to_string(),
            }),
        }
    }
}

/// How the folder's role was determined.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RoleSource {
    SpecialUse,
    Name,
    Manual,
}

impl RoleSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::SpecialUse => "special-use",
            Self::Name => "name",
            Self::Manual => "manual",
        }
    }
}

impl FromStr for RoleSource {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "special-use" => Ok(Self::SpecialUse),
            "name" => Ok(Self::Name),
            "manual" => Ok(Self::Manual),
            other => Err(CoreError::InvalidToken {
                expected: "special-use, name, manual",
                found: other.to_string(),
            }),
        }
    }
}

/// Mailbox / folder in an IMAP account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folder {
    pub id: FolderId,
    pub account_id: AccountId,
    pub name: String,
    pub display_name: String,
    pub delimiter: Option<String>,
    pub role: Option<FolderRole>,
    pub role_source: Option<RoleSource>,
    pub uidvalidity: u32,
    pub uidnext: u32,
    pub highestmodseq: u64,
    pub subscribed: bool,
    pub last_sync_at: Option<Timestamp>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_folder_role_tokens() {
        assert_eq!(FolderRole::Inbox.as_str(), "inbox");
        assert_eq!(FolderRole::Sent.as_str(), "sent");
        assert_eq!(FolderRole::Drafts.as_str(), "drafts");
        assert_eq!(FolderRole::Trash.as_str(), "trash");
        assert_eq!(FolderRole::Archive.as_str(), "archive");
        assert_eq!(FolderRole::Junk.as_str(), "junk");
        assert_eq!(FolderRole::All.as_str(), "all");

        assert_eq!(FolderRole::from_str("inbox").unwrap(), FolderRole::Inbox);
        assert_eq!(FolderRole::from_str("all").unwrap(), FolderRole::All);
        assert!(FolderRole::from_str("invalid").is_err());
    }

    #[test]
    fn test_role_source_tokens() {
        assert_eq!(RoleSource::SpecialUse.as_str(), "special-use");
        assert_eq!(RoleSource::Name.as_str(), "name");
        assert_eq!(RoleSource::Manual.as_str(), "manual");

        assert_eq!(
            RoleSource::from_str("special-use").unwrap(),
            RoleSource::SpecialUse
        );
        assert_eq!(RoleSource::from_str("name").unwrap(), RoleSource::Name);
        assert_eq!(RoleSource::from_str("manual").unwrap(), RoleSource::Manual);
        assert!(RoleSource::from_str("unknown").is_err());
    }
}
