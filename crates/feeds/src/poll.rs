use rusqlite::{params, Connection};
use crate::store::{upsert_feed_item, UpsertFeedItem, StoreError};
use super::fetch::{fetch_feed, FetchError, FetchResult};
use super::parse::{parse_feed, ParsedEntry};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, thiserror::Error)]
pub enum PollError {
    #[error("Fetch error: {0}")]
    Fetch(#[from] FetchError),
    #[error("Parse error: {0}")]
    Parse(#[from] super::parse::ParseError),
    #[error("Database error: {0}")]
    Store(#[from] StoreError),
}

/// Poll a single feed identified by `feed_id`.
///
/// This function:
/// 1. Loads the feed URL and conditional headers from the DB.
/// 2. Performs an HTTP fetch.
/// 3. If the response is `304 Not Modified`, nothing is changed.
/// 4. Otherwise parses the feed, deletes any existing items for the feed,
///    and upserts each entry via the store layer.
pub fn poll_feed(conn: &Connection, feed_id: i64) -> Result<(), PollError> {
    // Load feed metadata (url, etag, last_modified, account_id).
    let mut stmt = conn.prepare(
        "SELECT url, etag, last_modified, account_id FROM feeds WHERE id = ?1",
    )?;
    let row = stmt.query_row(params![feed_id], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, Option<String>>(2)?,
            r.get::<_, i64>(3)?,
        ))
    })?;
    let (url, etag_opt, lm_opt, account_id) = row;

    let fetch_res = fetch_feed(&url, etag_opt.as_deref(), lm_opt.as_deref())?;

    // 304 – nothing changed, just return.
    if fetch_res.status == 304 {
        return Ok(());
    }

    // For 200, parse the feed.
    let entries = parse_feed(&fetch_res.body)?;

    // Delete existing feed items (both feed_items and the associated items rows).
    // First collect item ids for the feed.
    let mut del_stmt = conn.prepare("SELECT item_id FROM feed_items WHERE feed_id = ?1")?;
    let item_ids: Vec<i64> = del_stmt
        .query_map(params![feed_id], |r| r.get(0))?
        .collect::<Result<Vec<_>, _>>()?;
    for id in item_ids {
        conn.execute("DELETE FROM items WHERE id = ?1", params![id])?;
    }
    conn.execute("DELETE FROM feed_items WHERE feed_id = ?1", params![feed_id])?;

    // Current timestamp for `now` and `sort_at`.
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time went backwards")
        .as_secs() as i64;

    // Upsert each entry.
    for entry in entries {
        // Derive a public_id – use GUID if present, else link, else a UUID placeholder.
        let public_id = entry
            .guid
            .as_ref()
            .or(entry.link.as_ref())
            .cloned()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

        let upsert = UpsertFeedItem {
            feed_id,
            account_id,
            public_id: &public_id,
            guid: entry.guid.as_deref(),
            link: entry.link.as_deref(),
            title: entry.title.as_deref(),
            author: entry.author.as_deref(),
            published_at: entry.published_at,
            sort_at: entry.published_at.unwrap_or(now),
            body: &entry.body,
            now,
        };
        // Store – upsert_feed_item returns the item_id which we ignore.
        upsert_feed_item(conn, &upsert)?;
    }

    Ok(())
}
