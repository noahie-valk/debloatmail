//! Feed synchronization crate
//!
//! Provides functions to fetch, parse, and store RSS/Atom/JSON feeds.
//! The public API is intentionally small; the daemon (`debloatmaild`) will call
//! `poll_feed` for each enabled feed. All heavy lifting happens inside this crate
//! and the results are written through the existing `store` layer.

pub mod fetch;
pub mod parse;
pub mod poll;

pub use fetch::{fetch_feed, FetchError, FetchResult};
pub use parse::{parse_feed, ParsedEntry, ParseError};
pub use poll::poll_feed;
