use std::fmt;
use std::ops::Deref;
use std::str::FromStr;

use crate::error::CoreError;

/// Unix timestamp in seconds (UTC).
pub type Timestamp = i64;

/// Stable public identifier crossing the ABI boundary (UUIDv7 string).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PublicId(String);

impl PublicId {
    /// Validate and construct a new PublicId.
    pub fn new(s: impl Into<String>) -> Result<Self, CoreError> {
        let s = s.into();
        if !Self::is_valid_uuid(&s) {
            return Err(CoreError::InvalidId(s));
        }
        Ok(Self(s))
    }

    /// Return the public ID as a string slice.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Internal helper validating 36-char ASCII UUID structure (xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx).
    fn is_valid_uuid(s: &str) -> bool {
        if s.len() != 36 {
            return false;
        }
        let b = s.as_bytes();
        for (i, &byte) in b.iter().enumerate() {
            if i == 8 || i == 13 || i == 18 || i == 23 {
                if byte != b'-' {
                    return false;
                }
            } else if !byte.is_ascii_hexdigit() {
                return false;
            }
        }
        true
    }
}

impl Deref for PublicId {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl fmt::Display for PublicId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl FromStr for PublicId {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::new(s)
    }
}

/// Macro for distinct newtypes wrapping PublicId.
macro_rules! define_id_wrapper {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(pub PublicId);

        impl $name {
            pub fn new(s: impl Into<String>) -> Result<Self, CoreError> {
                PublicId::new(s).map(Self)
            }

            pub fn as_str(&self) -> &str {
                self.0.as_str()
            }
        }

        impl Deref for $name {
            type Target = str;

            fn deref(&self) -> &Self::Target {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }

        impl FromStr for $name {
            type Err = CoreError;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                PublicId::from_str(s).map(Self)
            }
        }
    };
}

define_id_wrapper!(AccountId);
define_id_wrapper!(FolderId);
define_id_wrapper!(FeedId);
define_id_wrapper!(ThreadId);
define_id_wrapper!(DraftId);
define_id_wrapper!(OpId);

/// Source kind of an Item in the unified stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ItemKind {
    Mail,
    Feed,
}

impl ItemKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Mail => "mail",
            Self::Feed => "feed",
        }
    }
}

impl FromStr for ItemKind {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "mail" => Ok(Self::Mail),
            "feed" => Ok(Self::Feed),
            other => Err(CoreError::InvalidToken {
                expected: "mail, feed",
                found: other.to_string(),
            }),
        }
    }
}

/// Availability state of an item's body text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BodyState {
    None,
    Pending,
    Cached,
    Failed,
}

impl BodyState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Pending => "pending",
            Self::Cached => "cached",
            Self::Failed => "failed",
        }
    }
}

impl FromStr for BodyState {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "none" => Ok(Self::None),
            "pending" => Ok(Self::Pending),
            "cached" => Ok(Self::Cached),
            "failed" => Ok(Self::Failed),
            other => Err(CoreError::InvalidToken {
                expected: "none, pending, cached, failed",
                found: other.to_string(),
            }),
        }
    }
}

/// The unified item representing either an email message or a feed article.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    pub id: PublicId,
    pub kind: ItemKind,
    pub account: AccountId,
    pub title: String,
    pub author: Option<String>,
    pub excerpt: Option<String>,
    pub date: Option<Timestamp>,
    pub sort_at: Timestamp,
    pub unread: bool,
    pub starred: bool,
    pub answered: bool,
    pub has_attachments: bool,
    pub thread: Option<ThreadId>,
    pub body_state: BodyState,
}

/// Metadata describing an attachment part belonging to an item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    pub item: PublicId,
    pub section_id: String,
    pub filename: Option<String>,
    pub mime_type: String,
    pub size_bytes: Option<u64>,
    pub is_pdf: bool,
    pub cached: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_public_id() {
        let valid = "018f6e80-7c00-7218-971e-7f62b2512f7a";
        let id = PublicId::from_str(valid);
        assert!(id.is_ok());
        let id = id.unwrap();
        assert_eq!(id.as_str(), valid);
        assert_eq!(id.to_string(), valid);
    }

    #[test]
    fn test_invalid_public_id() {
        assert!(PublicId::from_str("not-a-uuid").is_err());
        assert!(PublicId::from_str("018f6e80-7c00-7218-971e-7f62b2512f7g").is_err());
        assert!(PublicId::from_str("018f6e807c007218971e7f62b2512f7a1234").is_err());
    }

    #[test]
    fn test_enum_tokens() {
        assert_eq!(ItemKind::Mail.as_str(), "mail");
        assert_eq!(ItemKind::Feed.as_str(), "feed");
        assert_eq!(ItemKind::from_str("mail").unwrap(), ItemKind::Mail);
        assert_eq!(ItemKind::from_str("feed").unwrap(), ItemKind::Feed);
        assert!(ItemKind::from_str("Mail").is_err());

        assert_eq!(BodyState::None.as_str(), "none");
        assert_eq!(BodyState::Pending.as_str(), "pending");
        assert_eq!(BodyState::Cached.as_str(), "cached");
        assert_eq!(BodyState::Failed.as_str(), "failed");
        assert_eq!(BodyState::from_str("cached").unwrap(), BodyState::Cached);
        assert!(BodyState::from_str("unknown").is_err());
    }
}
