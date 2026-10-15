//! Domain model and unified content stream types for debloatmail.
//!
//! # Invariants
//! `debloatmail-core` has strictly ZERO external dependencies (no tokio,
//! rusqlite, Qt, network, or filesystem).

pub mod account;
pub mod error;
pub mod feed;
pub mod item;
pub mod mailbox;
pub mod message;
pub mod stream;
pub mod thread;

// Re-export common types
pub use account::{Account, AccountKind, AuthKind, Security};
pub use error::CoreError;
pub use feed::Feed;
pub use item::{
    AccountId, Attachment, BodyState, DraftId, FeedId, FolderId, Item, ItemKind, OpId, PublicId,
    ThreadId, Timestamp,
};
pub use mailbox::{Folder, FolderRole, RoleSource};
pub use message::{Draft, OpKind, OpState, PendingOp};
pub use stream::{Cursor, SortKey, StreamSource, StreamSpec};
pub use thread::Thread;
