use std::str::FromStr;

use crate::error::CoreError;
use crate::item::{AccountId, Timestamp};

/// Kind of account: IMAP email or Feed subscription group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AccountKind {
    Imap,
    Feed,
}

impl AccountKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Imap => "imap",
            Self::Feed => "feed",
        }
    }
}

impl FromStr for AccountKind {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "imap" => Ok(Self::Imap),
            "feed" => Ok(Self::Feed),
            other => Err(CoreError::InvalidToken {
                expected: "imap, feed",
                found: other.to_string(),
            }),
        }
    }
}

/// Authentication mechanism for the account.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AuthKind {
    Password,
    OAuth2,
}

impl AuthKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Password => "password",
            Self::OAuth2 => "oauth2",
        }
    }
}

impl FromStr for AuthKind {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "password" => Ok(Self::Password),
            "oauth2" => Ok(Self::OAuth2),
            other => Err(CoreError::InvalidToken {
                expected: "password, oauth2",
                found: other.to_string(),
            }),
        }
    }
}

/// Transport security mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Security {
    Tls,
    StartTls,
    None,
}

impl Security {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Tls => "tls",
            Self::StartTls => "starttls",
            Self::None => "none",
        }
    }
}

impl FromStr for Security {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "tls" => Ok(Self::Tls),
            "starttls" => Ok(Self::StartTls),
            "none" => Ok(Self::None),
            other => Err(CoreError::InvalidToken {
                expected: "tls, starttls, none",
                found: other.to_string(),
            }),
        }
    }
}

/// Configured account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    pub id: AccountId,
    pub kind: AccountKind,
    pub name: String,
    pub email: Option<String>,
    pub provider: Option<String>,
    pub enabled: bool,
    pub imap_host: Option<String>,
    pub imap_port: Option<u16>,
    pub imap_security: Option<Security>,
    pub smtp_host: Option<String>,
    pub smtp_port: Option<u16>,
    pub smtp_security: Option<Security>,
    pub auth_kind: AuthKind,
    pub username: Option<String>,
    pub secret_ref: Option<String>,
    pub allow_invalid_cert_loopback: bool,
    pub poll_interval_secs: Option<u32>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_account_tokens() {
        assert_eq!(AccountKind::Imap.as_str(), "imap");
        assert_eq!(AccountKind::Feed.as_str(), "feed");
        assert_eq!(AccountKind::from_str("imap").unwrap(), AccountKind::Imap);
        assert_eq!(AccountKind::from_str("feed").unwrap(), AccountKind::Feed);

        assert_eq!(AuthKind::Password.as_str(), "password");
        assert_eq!(AuthKind::OAuth2.as_str(), "oauth2");
        assert_eq!(AuthKind::from_str("password").unwrap(), AuthKind::Password);
        assert_eq!(AuthKind::from_str("oauth2").unwrap(), AuthKind::OAuth2);

        assert_eq!(Security::Tls.as_str(), "tls");
        assert_eq!(Security::StartTls.as_str(), "starttls");
        assert_eq!(Security::None.as_str(), "none");
        assert_eq!(Security::from_str("tls").unwrap(), Security::Tls);
        assert_eq!(Security::from_str("starttls").unwrap(), Security::StartTls);
        assert_eq!(Security::from_str("none").unwrap(), Security::None);
    }
}
