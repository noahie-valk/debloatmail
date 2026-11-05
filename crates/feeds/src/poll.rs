use super::fetch::{FetchError, fetch_feed};
use super::parse::{ParseError, parse_feed_document};
use debloatmail_store::{StoreError, UpsertFeedItem, upsert_feed_item};
use rusqlite::{Connection, params};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, thiserror::Error)]
pub enum PollError {
    #[error("Fetch error: {0}")]
    Fetch(#[from] FetchError),
    #[error("Parse error: {0}")]
    Parse(#[from] ParseError),
    #[error("Database error: {0}")]
    Store(#[from] StoreError),
}

/// Poll a single feed identified by `feed_id`.
///
/// This function:
/// 1. Loads the feed URL and conditional headers from the DB.
/// 2. Performs an HTTP fetch.
/// 3. If the response is `304 Not Modified`, nothing is changed.
/// 4. Otherwise parses the feed and upserts each entry in-place via the store layer
///    (preserving `unread`, `starred`, and `hidden_at` per `SYNC.md` §7.2).
pub async fn poll_feed(conn: &Connection, feed_id: i64) -> Result<(), PollError> {
    let mut stmt = conn
        .prepare("SELECT url, etag, last_modified, account_id FROM feeds WHERE id = ?1")
        .map_err(StoreError::Sqlite)?;
    let row = stmt
        .query_row(params![feed_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })
        .map_err(StoreError::Sqlite)?;
    let (url, etag_opt, lm_opt, account_id) = row;

    let fetch_res = fetch_feed(&url, etag_opt.as_deref(), lm_opt.as_deref()).await?;

    // 304 – nothing changed, just return.
    if fetch_res.status == 304 {
        return Ok(());
    }

    let parsed = parse_feed_document(&fetch_res.body)?;

    // Current timestamp for `now` and `sort_at` fallback.
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;

    if let Some(ref perm_url) = fetch_res.permanent_redirect_url {
        conn.execute(
            "UPDATE feeds SET url = ?1 WHERE id = ?2",
            params![perm_url, feed_id],
        )
        .map_err(StoreError::Sqlite)?;
    }

    conn.execute(
        "UPDATE feeds SET
            title = COALESCE(?1, title),
            site_url = COALESCE(?2, site_url),
            etag = COALESCE(?3, etag),
            last_modified = COALESCE(?4, last_modified),
            last_fetch_at = ?5,
            consecutive_failures = 0,
            backoff_secs = 0,
            last_error = NULL
         WHERE id = ?6",
        params![
            parsed.title,
            parsed.site_url,
            fetch_res.etag,
            fetch_res.last_modified,
            now,
            feed_id,
        ],
    )
    .map_err(StoreError::Sqlite)?;

    // Upsert each entry in-place without deleting existing rows (`SYNC.md` §7.2).
    for entry in parsed.entries {
        let effective_guid = entry.guid.as_deref().or(entry.link.as_deref());
        let public_id = uuid::Uuid::new_v4().to_string();
        let sort_at = entry
            .published_at
            .or(entry.updated_at)
            .unwrap_or(now);

        let upsert = UpsertFeedItem {
            feed_id,
            account_id,
            public_id: &public_id,
            guid: effective_guid,
            link: entry.link.as_deref(),
            title: entry.title.as_deref(),
            author: entry.author.as_deref(),
            published_at: entry.published_at,
            sort_at,
            body: &entry.body,
            now,
        };
        upsert_feed_item(conn, &upsert)?;
    }

    Ok(())
}
