//! Feed synchronization crate
//!
//! Provides functions to fetch, parse, and store RSS/Atom/JSON feeds.
//! The public API is intentionally small; the daemon (`debloatmaild`) will call
//! `poll_feed` for each enabled feed. All heavy lifting happens inside this crate
//! and the results are written through the existing `store` layer.

pub mod fetch;
pub mod parse;
pub mod poll;

pub use fetch::{
    FetchError, FetchResult, MAX_FEED_BODY_BYTES, MAX_FEED_REDIRECT_HOPS, RedirectViolation,
    fetch_feed, fetch_feed_with_timeout, validate_redirect_target,
};
pub use parse::{ParseError, ParsedEntry, ParsedFeed, parse_feed, parse_feed_document};
pub use poll::{PollError, poll_feed};
