use std::str::FromStr;

use crate::error::CoreError;
use crate::item::{AccountId, DraftId, OpId, PublicId, Timestamp};

/// Type of pending offline operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OpKind {
    Flag,
    Send,
    Move,
    Delete,
    Expunge,
}

impl OpKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Flag => "flag",
            Self::Send => "send",
            Self::Move => "move",
            Self::Delete => "delete",
            Self::Expunge => "expunge",
        }
    }
}

impl FromStr for OpKind {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "flag" => Ok(Self::Flag),
            "send" => Ok(Self::Send),
            "move" => Ok(Self::Move),
            "delete" => Ok(Self::Delete),
            "expunge" => Ok(Self::Expunge),
            other => Err(CoreError::InvalidToken {
                expected: "flag, send, move, delete, expunge",
                found: other.to_string(),
            }),
        }
    }
}

/// Execution state of a queued operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OpState {
    Pending,
    InFlight,
    Failed,
    Done,
}

impl OpState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InFlight => "in_flight",
            Self::Failed => "failed",
            Self::Done => "done",
        }
    }
}

impl FromStr for OpState {
    type Err = CoreError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(Self::Pending),
            "in_flight" => Ok(Self::InFlight),
            "failed" => Ok(Self::Failed),
            "done" => Ok(Self::Done),
            other => Err(CoreError::InvalidToken {
                expected: "pending, in_flight, failed, done",
                found: other.to_string(),
            }),
        }
    }
}

/// Queued offline mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingOp {
    pub id: OpId,
    pub account_id: AccountId,
    pub op: OpKind,
    pub item_id: Option<PublicId>,
    pub payload: String,
    pub state: OpState,
    pub attempts: u32,
    pub last_error: Option<String>,
    pub created_at: Timestamp,
    pub finished_at: Option<Timestamp>,
}

/// Local draft representation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Draft {
    pub id: DraftId,
    pub account_id: AccountId,
    pub to_addrs: Vec<String>,
    pub cc_addrs: Vec<String>,
    pub bcc_addrs: Vec<String>,
    pub subject: String,
    pub body: String,
    pub in_reply_to: Option<String>,
    pub references_hdr: Option<String>,
    pub attachments: Vec<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_op_tokens() {
        assert_eq!(OpKind::Flag.as_str(), "flag");
        assert_eq!(OpKind::Send.as_str(), "send");
        assert_eq!(OpKind::Move.as_str(), "move");
        assert_eq!(OpKind::Delete.as_str(), "delete");
        assert_eq!(OpKind::Expunge.as_str(), "expunge");

        assert_eq!(OpKind::from_str("flag").unwrap(), OpKind::Flag);
        assert_eq!(OpKind::from_str("send").unwrap(), OpKind::Send);
        assert_eq!(OpKind::from_str("move").unwrap(), OpKind::Move);
        assert_eq!(OpKind::from_str("delete").unwrap(), OpKind::Delete);
        assert_eq!(OpKind::from_str("expunge").unwrap(), OpKind::Expunge);
        assert!(OpKind::from_str("append").is_err());

        assert_eq!(OpState::Pending.as_str(), "pending");
        assert_eq!(OpState::InFlight.as_str(), "in_flight");
        assert_eq!(OpState::Failed.as_str(), "failed");
        assert_eq!(OpState::Done.as_str(), "done");

        assert_eq!(OpState::from_str("pending").unwrap(), OpState::Pending);
        assert_eq!(OpState::from_str("in_flight").unwrap(), OpState::InFlight);
        assert_eq!(OpState::from_str("failed").unwrap(), OpState::Failed);
        assert_eq!(OpState::from_str("done").unwrap(), OpState::Done);
        assert!(OpState::from_str("unknown").is_err());
    }
}
