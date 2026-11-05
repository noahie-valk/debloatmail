//! Feed polling, conditional GET (`ETag` / `Last-Modified`), entry upsert by `(feed_id, guid)`,
//! failure backoff (`Retry-After`, `404`/`410`, `5xx`), and `Feeds.Add` hybrid 15-second
//! validation (`SYNC.md` §7.1–7.4, `DATA-MODEL.md` §11.2–11.3).

use std::future::Future;
use std::pin::Pin;
use std::str::FromStr;
use std::time::Duration;

use debloatmail_core::{BodyState, FeedId, ItemKind, Timestamp};
use debloatmail_engine::{Clock, Rng, feed_poll_backoff};
use debloatmail_extract::structure::{derive_excerpt, derive_title};
use debloatmail_extract::{CURRENT_EXTRACTOR_VERSION, extract_feed_text};
use debloatmail_feeds::{
    DEFAULT_FEED_TIMEOUT, FetchError, FetchResult, ParsedEntry, ParsedFeed,
    fetch_feed_with_timeout, parse_feed_document,
};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::SyncError;
use crate::id::mint_uuid_v7_from_clock;
use crate::scheduler::{SyncEvent, effective_feed_interval_secs};

/// Synchronous validation budget for `Feeds.Add` (`SYNC.md` §7.4).
pub const FEED_ADD_VALIDATION_TIMEOUT: Duration = Duration::from_secs(15);

/// Classification of hard failures during `Feeds.Add` (`SYNC.md` §7.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedHardFailureKind {
    /// HTTP `404 Not Found` or `410 Gone`.
    Gone,
    /// HTML page or unsupported `Content-Type`.
    NotAFeed,
    /// Malformed XML or unparseable RSS/Atom document.
    Malformed,
    /// Redirect rejected by the SSRF / scheme / hop limit hook (`SECURITY.md` §4).
    RedirectRejected,
}

/// Abstract feed HTTP fetcher so `crates/sync` can run deterministic zero-socket tests
/// while delegating live HTTP strictly to `crates/feeds`.
pub trait FeedFetcher: Send + Sync {
    fn fetch<'a>(
        &'a self,
        url: &'a str,
        etag: Option<&'a str>,
        last_modified: Option<&'a str>,
        timeout: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<FetchResult, FetchError>> + Send + 'a>>;
}

/// Production [`FeedFetcher`] backed by [`debloatmail_feeds::fetch_feed_with_timeout`].
#[derive(Debug, Clone, Copy, Default)]
pub struct ReqwestFeedFetcher;

impl FeedFetcher for ReqwestFeedFetcher {
    fn fetch<'a>(
        &'a self,
        url: &'a str,
        etag: Option<&'a str>,
        last_modified: Option<&'a str>,
        timeout: Duration,
    ) -> Pin<Box<dyn Future<Output = Result<FetchResult, FetchError>> + Send + 'a>> {
        Box::pin(fetch_feed_with_timeout(url, etag, last_modified, timeout))
    }
}

/// A feed row due for polling (`SYNC.md` §7.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DueFeedRow {
    pub id: i64,
    pub public_id: FeedId,
    pub account_id: i64,
    pub url: String,
    pub title: Option<String>,
    pub site_url: Option<String>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub interval_secs: Option<u32>,
    pub last_fetch_at: Option<Timestamp>,
    pub next_fetch_at: Option<Timestamp>,
    pub backoff_secs: u32,
    pub consecutive_failures: u32,
    pub last_error: Option<String>,
}

/// Result of polling a single feed (`SYNC.md` §7.1–7.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedPollReport {
    pub feed_id: FeedId,
    pub not_modified: bool,
    pub inserted_count: usize,
    pub updated_count: usize,
    pub permanent_redirect_url: Option<String>,
    pub next_fetch_at: Timestamp,
    pub error: Option<String>,
    pub event: Option<SyncEvent>,
}

/// Outcome of `Feeds.Add` hybrid validation (`SYNC.md` §7.4, `DATA-MODEL.md` §11.3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddFeedOutcome {
    pub row_id: i64,
    pub feed_id: FeedId,
    /// `true` when validated within the 15s budget; `false` when inserted transiently on timeout.
    pub validated: bool,
    pub inserted_items: usize,
}

/// Outcome of the background continuation for a `validated: false` feed (`SYNC.md` §7.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PendingValidationOutcome {
    /// Background fetch succeeded; metadata and `last_fetch_at` populated.
    Validated {
        feed_id: FeedId,
        inserted_items: usize,
    },
    /// Background fetch encountered a hard failure; transient feed row deleted.
    DeletedHardFailure {
        feed_id: FeedId,
        kind: FeedHardFailureKind,
        message: String,
    },
    /// Background fetch timed out or hit a transient network error; left on §7.3 poll schedule.
    DeferredToSchedule { feed_id: FeedId },
}

/// Queries all active (`removed_at IS NULL`) feeds due at or before `now` (`SYNC.md` §7.3).
///
/// # Invariant (`SYNC.md` §7.3)
/// Never filters on `last_error` — `last_error` is display metadata, never a `WHERE` filter.
pub fn query_due_feeds(conn: &Connection, now: Timestamp) -> Result<Vec<DueFeedRow>, SyncError> {
    let mut stmt = conn.prepare(
        "SELECT id, public_id, account_id, url, title, site_url, etag, last_modified,
                interval_secs, last_fetch_at, next_fetch_at, backoff_secs,
                consecutive_failures, last_error
         FROM feeds
         WHERE removed_at IS NULL
           AND (next_fetch_at IS NULL OR next_fetch_at <= ?1)
         ORDER BY COALESCE(next_fetch_at, 0) ASC, id ASC",
    )?;

    let rows = stmt.query_map(params![now], map_due_feed_row)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

fn load_feed_row(conn: &Connection, feed_row_id: i64) -> Result<DueFeedRow, SyncError> {
    conn.query_row(
        "SELECT id, public_id, account_id, url, title, site_url, etag, last_modified,
                interval_secs, last_fetch_at, next_fetch_at, backoff_secs,
                consecutive_failures, last_error
         FROM feeds
         WHERE id = ?1 AND removed_at IS NULL",
        params![feed_row_id],
        map_due_feed_row,
    )
    .optional()?
    .ok_or_else(|| SyncError::InvalidState(format!("active feed {feed_row_id} not found")))
}

fn map_due_feed_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<DueFeedRow> {
    let pid_str: String = row.get(1)?;
    let feed_id = FeedId::from_str(&pid_str).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(1, rusqlite::types::Type::Text, Box::new(e))
    })?;
    Ok(DueFeedRow {
        id: row.get(0)?,
        public_id: feed_id,
        account_id: row.get(2)?,
        url: row.get(3)?,
        title: row.get(4)?,
        site_url: row.get(5)?,
        etag: row.get(6)?,
        last_modified: row.get(7)?,
        interval_secs: row.get::<_, Option<i64>>(8)?.map(|v| v.max(0) as u32),
        last_fetch_at: row.get(9)?,
        next_fetch_at: row.get(10)?,
        backoff_secs: row.get::<_, i64>(11)?.max(0) as u32,
        consecutive_failures: row.get::<_, i64>(12)?.max(0) as u32,
        last_error: row.get(13)?,
    })
}

/// Polls a single feed using `fetcher`, updating conditional GET validators, upserting entries
/// by `(feed_id, guid)`, or applying `SYNC.md` §7.3 failure backoff.
#[allow(clippy::too_many_arguments)]
pub async fn poll_feed_with_fetcher(
    conn: &mut Connection,
    fetcher: &dyn FeedFetcher,
    feed_row_id: i64,
    account_poll_interval_secs: Option<u32>,
    global_feed_interval_secs: u32,
    clock: &dyn Clock,
    rng: &mut dyn Rng,
) -> Result<FeedPollReport, SyncError> {
    let feed = load_feed_row(conn, feed_row_id)?;
    let effective_interval = effective_feed_interval_secs(
        feed.interval_secs,
        account_poll_interval_secs,
        global_feed_interval_secs,
    );

    let fetch_res = fetcher
        .fetch(
            &feed.url,
            feed.etag.as_deref(),
            feed.last_modified.as_deref(),
            DEFAULT_FEED_TIMEOUT,
        )
        .await;

    let now = clock.now_secs();

    match fetch_res {
        Ok(FetchResult::NotModified) => {
            let next_fetch_at = compute_next_success_fetch_at(now, effective_interval, rng);
            conn.execute(
                "UPDATE feeds
                 SET last_fetch_at = ?1,
                     next_fetch_at = ?2,
                     backoff_secs = 0,
                     consecutive_failures = 0,
                     last_error = NULL
                 WHERE id = ?3",
                params![now, next_fetch_at, feed.id],
            )?;

            // SYNC.md §7.1 & §13.2: 304 Not Modified emits NO FeedsUpdated signal.
            Ok(FeedPollReport {
                feed_id: feed.public_id,
                not_modified: true,
                inserted_count: 0,
                updated_count: 0,
                permanent_redirect_url: None,
                next_fetch_at,
                error: None,
                event: None,
            })
        }
        Ok(FetchResult::Fetched {
            bytes,
            etag,
            last_modified,
            permanent_redirect_url,
            ..
        }) => {
            let parsed = match parse_feed_document(&bytes) {
                Ok(p) => p,
                Err(parse_err) => {
                    return record_feed_poll_failure(
                        conn,
                        &feed,
                        &parse_err.to_string(),
                        None,
                        now,
                        rng,
                    );
                }
            };

            let next_fetch_at = compute_next_success_fetch_at(now, effective_interval, rng);
            let final_url = permanent_redirect_url
                .as_deref()
                .unwrap_or(&feed.url)
                .to_string();

            let tx = conn.transaction()?;
            let (inserted_count, updated_count) = upsert_parsed_feed_entries(
                &tx,
                feed.id,
                feed.account_id,
                &parsed,
                feed.site_url.as_deref(),
                now,
                clock,
                rng,
            )?;

            let new_etag = etag.or(feed.etag);
            let new_last_mod = last_modified.or(feed.last_modified);

            tx.execute(
                "UPDATE feeds
                 SET url = ?1,
                     title = COALESCE(?2, title),
                     site_url = COALESCE(?3, site_url),
                     etag = ?4,
                     last_modified = ?5,
                     last_fetch_at = ?6,
                     next_fetch_at = ?7,
                     backoff_secs = 0,
                     consecutive_failures = 0,
                     last_error = NULL
                 WHERE id = ?8",
                params![
                    final_url,
                    parsed.title,
                    parsed.site_url,
                    new_etag,
                    new_last_mod,
                    now,
                    next_fetch_at,
                    feed.id,
                ],
            )?;
            tx.commit()?;

            let event = if inserted_count > 0 {
                Some(SyncEvent::FeedsUpdated {
                    feed_id: feed.public_id.clone(),
                    new_count: inserted_count as u32,
                })
            } else {
                None
            };

            Ok(FeedPollReport {
                feed_id: feed.public_id,
                not_modified: false,
                inserted_count,
                updated_count,
                permanent_redirect_url,
                next_fetch_at,
                error: None,
                event,
            })
        }
        Err(fetch_err) => {
            let retry_after = match &fetch_err {
                FetchError::RateLimited {
                    retry_after_secs, ..
                } => *retry_after_secs,
                _ => None,
            };
            record_feed_poll_failure(
                conn,
                &feed,
                &fetch_err.to_string(),
                retry_after,
                now,
                rng,
            )
        }
    }
}

fn record_feed_poll_failure(
    conn: &mut Connection,
    feed: &DueFeedRow,
    error_msg: &str,
    retry_after_secs: Option<u64>,
    now: Timestamp,
    rng: &mut dyn Rng,
) -> Result<FeedPollReport, SyncError> {
    let new_failures = feed.consecutive_failures.saturating_add(1);
    let backoff_dur = if let Some(ra) = retry_after_secs {
        Duration::from_secs(ra.max(1))
    } else {
        feed_poll_backoff(new_failures, rng)
    };
    let backoff_secs = backoff_dur.as_secs().min(u64::from(u32::MAX)) as u32;
    let next_fetch_at = now.saturating_add(i64::from(backoff_secs));

    conn.execute(
        "UPDATE feeds
         SET consecutive_failures = ?1,
             backoff_secs = ?2,
             next_fetch_at = ?3,
             last_error = ?4
         WHERE id = ?5",
        params![
            i64::from(new_failures),
            i64::from(backoff_secs),
            next_fetch_at,
            error_msg,
            feed.id,
        ],
    )?;

    Ok(FeedPollReport {
        feed_id: feed.public_id.clone(),
        not_modified: false,
        inserted_count: 0,
        updated_count: 0,
        permanent_redirect_url: None,
        next_fetch_at,
        error: Some(error_msg.to_string()),
        event: None,
    })
}

/// Implements `Feeds.Add` hybrid 15-second validation (`SYNC.md` §7.4, `DATA-MODEL.md` §11.3):
/// - **Fast path (< 15s, valid XML)**: inserts validated `feeds` row (`last_fetch_at = now`),
///   upserts entries, and returns `validated: true`.
/// - **Hard failure (`404`/`410`, HTML, malformed XML, rejected redirect)**: creates **no row**
///   and returns `Err(SyncError::HardFeedFailure { .. })`.
/// - **Timeout (`15s` budget exceeded)**: inserts transient `feeds` row (`last_fetch_at = NULL`),
///   schedules `next_fetch_at = now + effective_interval + jitter`, and returns `validated: false`.
#[allow(clippy::too_many_arguments)]
pub async fn add_feed_hybrid(
    conn: &mut Connection,
    fetcher: &dyn FeedFetcher,
    account_id: i64,
    url: &str,
    given_name: Option<&str>,
    interval_secs: Option<u32>,
    account_poll_interval_secs: Option<u32>,
    global_feed_interval_secs: u32,
    clock: &dyn Clock,
    rng: &mut dyn Rng,
) -> Result<AddFeedOutcome, SyncError> {
    let effective_interval = effective_feed_interval_secs(
        interval_secs,
        account_poll_interval_secs,
        global_feed_interval_secs,
    );

    let fetch_res = fetcher
        .fetch(url, None, None, FEED_ADD_VALIDATION_TIMEOUT)
        .await;
    let now = clock.now_secs();

    match fetch_res {
        Ok(FetchResult::Fetched {
            bytes,
            etag,
            last_modified,
            permanent_redirect_url,
            ..
        }) => {
            let parsed = parse_feed_document(&bytes).map_err(|e| SyncError::HardFeedFailure {
                kind: classify_parse_error(&bytes),
                message: e.to_string(),
            })?;

            let public_id = mint_uuid_v7_from_clock(clock, rng);
            let feed_id = FeedId(public_id.clone());
            let final_url = permanent_redirect_url.as_deref().unwrap_or(url);
            let title = given_name
                .map(str::to_string)
                .or_else(|| parsed.title.clone());
            let next_fetch_at = compute_next_success_fetch_at(now, effective_interval, rng);

            let tx = conn.transaction()?;
            tx.execute(
                "INSERT INTO feeds (
                    public_id, account_id, url, title, site_url, etag, last_modified,
                    interval_secs, last_fetch_at, next_fetch_at, backoff_secs,
                    consecutive_failures, last_error, removed_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 0, 0, NULL, NULL)",
                params![
                    public_id.as_str(),
                    account_id,
                    final_url,
                    title,
                    parsed.site_url,
                    etag,
                    last_modified,
                    interval_secs.map(i64::from),
                    now,
                    next_fetch_at,
                ],
            )?;
            let row_id = tx.last_insert_rowid();

            let (inserted_items, _) = upsert_parsed_feed_entries(
                &tx,
                row_id,
                account_id,
                &parsed,
                parsed.site_url.as_deref(),
                now,
                clock,
                rng,
            )?;

            tx.commit()?;

            Ok(AddFeedOutcome {
                row_id,
                feed_id,
                validated: true,
                inserted_items,
            })
        }
        Ok(FetchResult::NotModified) => Err(SyncError::HardFeedFailure {
            kind: FeedHardFailureKind::Malformed,
            message: "unexpected 304 Not Modified on unconditional initial feed fetch".to_string(),
        }),
        Err(FetchError::TimedOut) => {
            // SYNC.md §7.4: Timeout is not a verdict -> insert transient row with last_fetch_at = NULL
            let public_id = mint_uuid_v7_from_clock(clock, rng);
            let feed_id = FeedId(public_id.clone());
            let next_fetch_at = compute_next_success_fetch_at(now, effective_interval, rng);

            conn.execute(
                "INSERT INTO feeds (
                    public_id, account_id, url, title, site_url, etag, last_modified,
                    interval_secs, last_fetch_at, next_fetch_at, backoff_secs,
                    consecutive_failures, last_error, removed_at
                 ) VALUES (?1, ?2, ?3, ?4, NULL, NULL, NULL, ?5, NULL, ?6, 0, 0, NULL, NULL)",
                params![
                    public_id.as_str(),
                    account_id,
                    url,
                    given_name,
                    interval_secs.map(i64::from),
                    next_fetch_at,
                ],
            )?;
            let row_id = conn.last_insert_rowid();

            Ok(AddFeedOutcome {
                row_id,
                feed_id,
                validated: false,
                inserted_items: 0,
            })
        }
        Err(err) => {
            if let Some(kind) = classify_fetch_hard_failure(&err) {
                Err(SyncError::HardFeedFailure {
                    kind,
                    message: err.to_string(),
                })
            } else {
                Err(SyncError::Feeds(debloatmail_feeds::FeedError::Fetch(err)))
            }
        }
    }
}

/// Completes the background continuation of a `validated: false` (`last_fetch_at IS NULL`) feed
/// (`SYNC.md` §7.4):
/// - Valid XML -> fills `title`, `site_url`, `etag`, `last_modified`, `last_fetch_at = now`.
/// - Hard failure -> deletes the transient `feeds` row and any items.
/// - Another timeout / transient network error -> leaves the row on the normal `§7.3` poll schedule.
pub async fn complete_pending_feed_validation(
    conn: &mut Connection,
    fetcher: &dyn FeedFetcher,
    feed_row_id: i64,
    timeout: Duration,
    clock: &dyn Clock,
    rng: &mut dyn Rng,
) -> Result<PendingValidationOutcome, SyncError> {
    let feed = load_feed_row(conn, feed_row_id)?;
    let fetch_res = fetcher.fetch(&feed.url, None, None, timeout).await;
    let now = clock.now_secs();

    match fetch_res {
        Ok(FetchResult::Fetched {
            bytes,
            etag,
            last_modified,
            permanent_redirect_url,
            ..
        }) => match parse_feed_document(&bytes) {
            Ok(parsed) => {
                let final_url = permanent_redirect_url
                    .as_deref()
                    .unwrap_or(&feed.url)
                    .to_string();
                let tx = conn.transaction()?;
                let (inserted_items, _) = upsert_parsed_feed_entries(
                    &tx,
                    feed.id,
                    feed.account_id,
                    &parsed,
                    parsed.site_url.as_deref(),
                    now,
                    clock,
                    rng,
                )?;
                tx.execute(
                    "UPDATE feeds
                     SET url = ?1,
                         title = COALESCE(title, ?2),
                         site_url = COALESCE(?3, site_url),
                         etag = ?4,
                         last_modified = ?5,
                         last_fetch_at = ?6,
                         last_error = NULL
                     WHERE id = ?7",
                    params![
                        final_url,
                        parsed.title,
                        parsed.site_url,
                        etag,
                        last_modified,
                        now,
                        feed.id,
                    ],
                )?;
                tx.commit()?;

                Ok(PendingValidationOutcome::Validated {
                    feed_id: feed.public_id,
                    inserted_items,
                })
            }
            Err(parse_err) => {
                let kind = classify_parse_error(&bytes);
                hard_delete_feed_row(conn, feed.id)?;
                Ok(PendingValidationOutcome::DeletedHardFailure {
                    feed_id: feed.public_id,
                    kind,
                    message: parse_err.to_string(),
                })
            }
        },
        Ok(FetchResult::NotModified) => {
            hard_delete_feed_row(conn, feed.id)?;
            Ok(PendingValidationOutcome::DeletedHardFailure {
                feed_id: feed.public_id,
                kind: FeedHardFailureKind::Malformed,
                message: "unexpected 304 Not Modified on unvalidated feed".to_string(),
            })
        }
        Err(FetchError::TimedOut) => Ok(PendingValidationOutcome::DeferredToSchedule {
            feed_id: feed.public_id,
        }),
        Err(err) => {
            if let Some(kind) = classify_fetch_hard_failure(&err) {
                hard_delete_feed_row(conn, feed.id)?;
                Ok(PendingValidationOutcome::DeletedHardFailure {
                    feed_id: feed.public_id,
                    kind,
                    message: err.to_string(),
                })
            } else {
                Ok(PendingValidationOutcome::DeferredToSchedule {
                    feed_id: feed.public_id,
                })
            }
        }
    }
}

fn hard_delete_feed_row(conn: &mut Connection, feed_row_id: i64) -> Result<(), SyncError> {
    let tx = conn.transaction()?;
    tx.execute(
        "DELETE FROM items WHERE id IN (SELECT item_id FROM feed_items WHERE feed_id = ?1)",
        params![feed_row_id],
    )?;
    tx.execute("DELETE FROM feeds WHERE id = ?1", params![feed_row_id])?;
    tx.commit()?;
    Ok(())
}

fn classify_fetch_hard_failure(err: &FetchError) -> Option<FeedHardFailureKind> {
    match err {
        FetchError::Gone(_) => Some(FeedHardFailureKind::Gone),
        FetchError::InvalidContentType(_) => Some(FeedHardFailureKind::NotAFeed),
        FetchError::Redirect(_) => Some(FeedHardFailureKind::RedirectRejected),
        FetchError::TooLarge { .. } => Some(FeedHardFailureKind::Malformed),
        FetchError::HttpStatus(code) if (400..500).contains(code) && *code != 429 => {
            Some(FeedHardFailureKind::Gone)
        }
        _ => None,
    }
}

fn classify_parse_error(bytes: &[u8]) -> FeedHardFailureKind {
    let prefix = String::from_utf8_lossy(&bytes[..bytes.len().min(512)]).to_ascii_lowercase();
    if prefix.contains("<!doctype html") || prefix.contains("<html") {
        FeedHardFailureKind::NotAFeed
    } else {
        FeedHardFailureKind::Malformed
    }
}

fn compute_next_success_fetch_at(
    now: Timestamp,
    effective_interval_secs: u32,
    rng: &mut dyn Rng,
) -> Timestamp {
    let base = f64::from(effective_interval_secs.max(60));
    let sample = rng.next_f64().clamp(0.0, 1.0);
    // ±10% uniform jitter (`SYNC.md` §4.1)
    let factor = 0.9 + (sample * 0.2);
    let delta = (base * factor).round() as i64;
    now.saturating_add(delta.max(1))
}

/// Derives the stable `(feed_id, guid)` key for a [`ParsedEntry`] (`SYNC.md` §7.2):
/// 1. `<guid>` / `<id>` if non-empty.
/// 2. `<link>` (`entry.url`) if non-empty.
/// 3. BLAKE3 hex hash of `title + "\n" + content/summary`.
pub fn derive_entry_guid(entry: &ParsedEntry) -> String {
    let trimmed_guid = entry.guid.trim();
    if !trimmed_guid.is_empty() {
        return trimmed_guid.to_string();
    }
    if let Some(url) = &entry.url {
        let trimmed_url = url.trim();
        if !trimmed_url.is_empty() {
            return trimmed_url.to_string();
        }
    }
    let title = entry.title.as_deref().unwrap_or("");
    let body = entry
        .content
        .as_deref()
        .or(entry.summary.as_deref())
        .unwrap_or("");
    let mut hasher = blake3::Hasher::new();
    hasher.update(title.as_bytes());
    hasher.update(b"\n");
    hasher.update(body.as_bytes());
    hasher.finalize().to_hex().to_string()
}

#[allow(clippy::too_many_arguments)]
fn upsert_parsed_feed_entries(
    tx: &Transaction<'_>,
    feed_row_id: i64,
    account_id: i64,
    parsed: &ParsedFeed,
    fallback_site_url: Option<&str>,
    now: Timestamp,
    clock: &dyn Clock,
    rng: &mut dyn Rng,
) -> Result<(usize, usize), SyncError> {
    let site_url = parsed.site_url.as_deref().or(fallback_site_url);
    let mut inserted = 0usize;
    let mut updated = 0usize;

    for entry in &parsed.entries {
        let guid = derive_entry_guid(entry);
        let body_text = extract_feed_text(
            entry.content.as_deref(),
            entry.summary.as_deref(),
            site_url,
        );
        let title = match entry.title.as_deref() {
            Some(t) if !t.trim().is_empty() => derive_title(Some(t)),
            _ => "Untitled".to_string(),
        };
        let excerpt = derive_excerpt(&body_text, 200);
        let published_at = entry.published_at.or(entry.updated_at);
        let sort_at = published_at.unwrap_or(now);

        let existing_item_id: Option<i64> = tx
            .query_row(
                "SELECT item_id FROM feed_items WHERE feed_id = ?1 AND guid = ?2",
                params![feed_row_id, guid],
                |r| r.get(0),
            )
            .optional()?;

        if let Some(item_id) = existing_item_id {
            // SYNC.md §7.2: Update title, author, excerpt, url, and item_text.body;
            // NEVER clobber unread, starred, hidden_at, body_state, thread_id, or created_at!
            tx.execute(
                "UPDATE items
                 SET title = ?1,
                     author = ?2,
                     excerpt = ?3,
                     published_at = COALESCE(?4, published_at),
                     updated_at = ?5
                 WHERE id = ?6",
                params![title, entry.author, excerpt, published_at, now, item_id],
            )?;
            tx.execute(
                "UPDATE feed_items SET url = ?1 WHERE item_id = ?2",
                params![entry.url, item_id],
            )?;
            tx.execute(
                "INSERT INTO item_text (item_id, title, author, body, extractor_version)
                 VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT(item_id) DO UPDATE SET
                     title = excluded.title,
                     author = excluded.author,
                     body = excluded.body,
                     extractor_version = excluded.extractor_version",
                params![
                    item_id,
                    title,
                    entry.author,
                    body_text,
                    i64::from(CURRENT_EXTRACTOR_VERSION),
                ],
            )?;
            updated += 1;
        } else {
            let public_id = mint_uuid_v7_from_clock(clock, rng);
            tx.execute(
                "INSERT INTO items (
                    public_id, kind, account_id, thread_id, title, author, excerpt,
                    published_at, sort_at, unread, starred, answered, has_attachments,
                    body_state, hidden_at, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, NULL, ?4, ?5, ?6, ?7, ?8, 1, 0, 0, 0, ?9, NULL, ?10, ?10)",
                params![
                    public_id.as_str(),
                    ItemKind::Feed.as_str(),
                    account_id,
                    title,
                    entry.author,
                    excerpt,
                    published_at,
                    sort_at,
                    BodyState::Cached.as_str(),
                    now,
                ],
            )?;
            let item_id = tx.last_insert_rowid();

            tx.execute(
                "INSERT INTO feed_items (item_id, feed_id, guid, url)
                 VALUES (?1, ?2, ?3, ?4)",
                params![item_id, feed_row_id, guid, entry.url],
            )?;
            tx.execute(
                "INSERT INTO item_text (item_id, title, author, body, extractor_version)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    item_id,
                    title,
                    entry.author,
                    body_text,
                    i64::from(CURRENT_EXTRACTOR_VERSION),
                ],
            )?;
            inserted += 1;
        }
    }

    Ok((inserted, updated))
}
