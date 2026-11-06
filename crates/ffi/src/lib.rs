//! `debloatmail-ffi` — Qt-free engine trait and transport boundary for debloatmail.

pub mod dbus;
pub mod error;
pub mod in_process;
pub mod lock;
pub mod reader;
pub mod traits;
pub mod types;

pub use dbus::DbusEngine;
pub use debloatmail_auth::{KeyringStore, SecretStore};
pub use error::FfiError;
pub use in_process::InProcessEngine;
pub use lock::StandaloneLock;
pub use traits::{Engine, EngineMutations, EngineReads};
pub use types::*;
