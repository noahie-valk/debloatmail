use std::str::FromStr;

use crate::error::CoreError;
use crate::item::{AccountId, FeedId, FolderId, PublicId, ThreadId, Timestamp};

/// Ordering column criteria for content streams.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SortKey {
    Date,
    Received,
    Author,
    Subject,
}

impl SortKey {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Date => "date",
            Self::Received => "received",
            Self::Author => "author",
            Self::Subject => "subject",
        }
    }
}

impl FromStr for SortKey {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "date" => Ok(Self::Date),
            "received" => Ok(Self::Received),
            "author" => Ok(Self::Author),
            "subject" => Ok(Self::Subject),
            other => Err(CoreError::InvalidToken {
                expected: "date, received, author, subject",
                found: other.to_string(),
            }),
        }
    }
}

/// Keyset pagination cursor tracking active sort key and last row values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cursor {
    pub sort: SortKey,
    pub date: Option<Timestamp>,
    pub sort_at: Option<Timestamp>,
    pub text: Option<String>,
    pub id: PublicId,
}

/// Target source filter for a content stream query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamSource {
    Unified,
    Account(AccountId),
    Folder(FolderId),
    Feed(FeedId),
    Thread(ThreadId),
    Search(String),
}

/// Parameterized query specification describing a content stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamSpec {
    pub source: StreamSource,
    pub sort: SortKey,
    pub unread_only: bool,
    pub starred_only: bool,
    pub since: Option<Timestamp>,
}

impl Default for StreamSpec {
    fn default() -> Self {
        Self {
            source: StreamSource::Unified,
            sort: SortKey::Date,
            unread_only: false,
            starred_only: false,
            since: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sort_key_tokens() {
        assert_eq!(SortKey::Date.as_str(), "date");
        assert_eq!(SortKey::Received.as_str(), "received");
        assert_eq!(SortKey::Author.as_str(), "author");
        assert_eq!(SortKey::Subject.as_str(), "subject");

        assert_eq!(SortKey::from_str("date").unwrap(), SortKey::Date);
        assert_eq!(SortKey::from_str("received").unwrap(), SortKey::Received);
        assert_eq!(SortKey::from_str("author").unwrap(), SortKey::Author);
        assert_eq!(SortKey::from_str("subject").unwrap(), SortKey::Subject);
        assert!(SortKey::from_str("other").is_err());
    }

    #[test]
    fn test_cursor_fields() {
        let id = PublicId::from_str("018f6e80-7c00-7218-971e-7f62b2512f7a").unwrap();
        let cursor = Cursor {
            sort: SortKey::Date,
            date: Some(1700000000),
            sort_at: Some(1700000000),
            text: None,
            id: id.clone(),
        };
        assert_eq!(cursor.sort, SortKey::Date);
        assert_eq!(cursor.date, Some(1700000000));
        assert_eq!(cursor.sort_at, Some(1700000000));
        assert_eq!(cursor.text, None);
        assert_eq!(cursor.id, id);
    }
}
