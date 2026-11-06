//! Direct SQLite read implementation for `EngineReads`.
//!
//! Both `InProcessEngine` and `DbusEngine` share this read path, fulfilling the
//! architectural invariant that GUI and CLI directly read the local database via
//! WAL without serializing large item sets across D-Bus or duplicating queries.

use std::path::Path;
use std::str::FromStr;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::DateTime;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use debloatmail_core::{
    AccountId, Cursor, FeedId, FolderId, Item, PublicId, RoleSource, SortKey, StreamSource,
    StreamSpec, Timestamp,
};
use debloatmail_store::blobs::blob_path;
use debloatmail_store::headers::{query_stream, row_to_item};
use debloatmail_store::search::search_items;

use crate::error::FfiError;
use crate::types::*;

#[derive(Serialize, Deserialize)]
struct WireCursor {
    sort: String,
    date: Option<i64>,
    sort_at: Option<i64>,
    text: Option<String>,
    id: String,
}

pub fn encode_cursor(c: &Cursor) -> String {
    let wire = WireCursor {
        sort: c.sort.as_str().to_string(),
        date: c.date,
        sort_at: c.sort_at,
        text: c.text.clone(),
        id: c.id.to_string(),
    };
    let json = serde_json::to_string(&wire).unwrap_or_default();
    URL_SAFE_NO_PAD.encode(json.as_bytes())
}

pub fn decode_cursor(token: &str) -> Result<Cursor, FfiError> {
    let bytes = URL_SAFE_NO_PAD
        .decode(token)
        .map_err(|e| FfiError::InvalidArgument(format!("invalid cursor base64: {e}")))?;
    let wire: WireCursor = serde_json::from_slice(&bytes)
        .map_err(|e| FfiError::InvalidArgument(format!("invalid cursor json: {e}")))?;
    let sort = SortKey::from_str(&wire.sort)
        .map_err(|e| FfiError::InvalidArgument(format!("invalid cursor sort key: {e}")))?;
    let id = PublicId::from_str(&wire.id)
        .map_err(|e| FfiError::InvalidArgument(format!("invalid cursor id: {e}")))?;

    Ok(Cursor {
        sort,
        date: wire.date,
        sort_at: wire.sort_at,
        text: wire.text,
        id,
    })
}

pub fn format_timestamp_opt(ts: Option<Timestamp>) -> String {
    match ts {
        Some(secs) => DateTime::from_timestamp(secs, 0)
            .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
            .unwrap_or_default(),
        None => String::new(),
    }
}

pub fn format_timestamp(ts: Timestamp) -> String {
    DateTime::from_timestamp(ts, 0)
        .map(|dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .unwrap_or_default()
}

pub fn parse_iso8601(s: &str) -> Result<i64, FfiError> {
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|dt| dt.timestamp())
        .or_else(|_| {
            chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S")
                .map(|dt| dt.and_utc().timestamp())
        })
        .or_else(|_| {
            chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
                .map(|d| d.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp())
        })
        .map_err(|e| FfiError::InvalidArgument(format!("invalid timestamp '{s}': {e}")))
}

pub fn item_to_summary(conn: &Connection, item: Item) -> Result<FfiItemSummary, FfiError> {
    let mut stmt = conn.prepare_cached(
        "SELECT a.name, f.public_id, f.name
         FROM items i
         JOIN accounts a ON a.id = i.account_id
         LEFT JOIN mail_items m ON m.item_id = i.id
         LEFT JOIN folders f ON f.id = m.folder_id
         WHERE i.public_id = ?1;",
    )?;

    let (acc_name, folder_id, folder_name): (String, Option<String>, Option<String>) = stmt
        .query_row(params![item.id.as_str()], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })
        .unwrap_or_else(|_| (String::new(), None, None));

    let short_id = if item.id.as_str().len() >= 8 {
        item.id.as_str()[..8].to_string()
    } else {
        item.id.to_string()
    };

    Ok(FfiItemSummary {
        id: item.id.to_string(),
        short_id,
        kind: item.kind.into(),
        account_id: item.account.to_string(),
        account_name: acc_name,
        folder_id,
        folder_name,
        title: item.title,
        author: item.author.unwrap_or_default(),
        published_at: format_timestamp_opt(item.date),
        sort_at: format_timestamp(item.sort_at),
        unread: item.unread,
        starred: item.starred,
        has_attachments: item.has_attachments,
        thread_id: item.thread.map(|t| t.to_string()),
        body_state: item.body_state.into(),
    })
}

#[allow(clippy::too_many_arguments)]
pub fn query_stream_fn(
    conn: &Connection,
    account_id: Option<&str>,
    folder_id: Option<&str>,
    feed_id: Option<&str>,
    _kind_filter: Option<&str>,
    unread_only: bool,
    starred_only: bool,
    since: Option<&str>,
    _before: Option<&str>,
    sort: SortKey,
    limit: u32,
    cursor: Option<&str>,
) -> Result<FfiPage<FfiItemSummary>, FfiError> {
    let source = if let Some(fid) = feed_id {
        let f = FeedId::new(fid).map_err(|e| FfiError::InvalidArgument(e.to_string()))?;
        StreamSource::Feed(f)
    } else if let Some(fol_id) = folder_id {
        let fol = FolderId::new(fol_id).map_err(|e| FfiError::InvalidArgument(e.to_string()))?;
        StreamSource::Folder(fol)
    } else if let Some(acc_id) = account_id {
        let acc = AccountId::new(acc_id).map_err(|e| FfiError::InvalidArgument(e.to_string()))?;
        StreamSource::Account(acc)
    } else {
        StreamSource::Unified
    };

    let since_ts = match since {
        Some(s) => Some(parse_iso8601(s)?),
        None => None,
    };

    let spec = StreamSpec {
        source,
        sort,
        unread_only,
        starred_only,
        since: since_ts,
    };

    let parsed_cursor = match cursor {
        Some(tok) => Some(decode_cursor(tok)?),
        None => None,
    };

    let query_limit = limit.saturating_add(1);
    let items = query_stream(conn, &spec, parsed_cursor.as_ref(), query_limit)?;

    let has_more = items.len() > limit as usize;
    let mut page_items = items;
    if has_more {
        page_items.truncate(limit as usize);
    }

    let next_cursor = if has_more {
        page_items.last().map(|item| {
            encode_cursor(&Cursor {
                sort,
                date: item.date,
                sort_at: Some(item.sort_at),
                text: match sort {
                    SortKey::Author => item.author.clone(),
                    SortKey::Subject => None,
                    _ => None,
                },
                id: item.id.clone(),
            })
        })
    } else {
        None
    };

    let mut summaries = Vec::with_capacity(page_items.len());
    for it in page_items {
        summaries.push(item_to_summary(conn, it)?);
    }

    Ok(FfiPage {
        items: summaries,
        next_cursor,
        has_more,
    })
}

pub fn query_threads_fn(
    conn: &Connection,
    account_id: Option<&str>,
    _sort: SortKey,
    limit: u32,
    _cursor: Option<&str>,
) -> Result<FfiPage<FfiThreadSummary>, FfiError> {
    let mut stmt = conn.prepare(
        "SELECT t.public_id, a.public_id, t.display_subject,
                (SELECT COUNT(*) FROM items i WHERE i.thread_id = t.id AND i.hidden_at IS NULL) AS reply_count,
                (SELECT COUNT(*) FROM items i WHERE i.thread_id = t.id AND i.unread = 1 AND i.hidden_at IS NULL) AS unread_count,
                t.last_activity
         FROM threads t
         JOIN accounts a ON a.id = t.account_id
         WHERE (?1 IS NULL OR a.public_id = ?1)
         ORDER BY t.last_activity DESC
         LIMIT ?2;",
    )?;

    let rows = stmt.query_map(params![account_id, limit], |r| {
        let last_act: i64 = r.get(5)?;
        Ok(FfiThreadSummary {
            id: r.get(0)?,
            account_id: r.get(1)?,
            display_subject: r.get(2)?,
            reply_count: r.get(3)?,
            unread_count: r.get(4)?,
            last_activity: format_timestamp(last_act),
            participants: Vec::new(),
        })
    })?;

    let mut threads = Vec::new();
    for t in rows {
        threads.push(t?);
    }

    Ok(FfiPage {
        items: threads,
        next_cursor: None,
        has_more: false,
    })
}

pub fn search_items_fn(
    conn: &Connection,
    query: &str,
    _account_id: Option<&str>,
    _sort: SortKey,
    limit: u32,
    _cursor: Option<&str>,
) -> Result<FfiPage<FfiItemSummary>, FfiError> {
    let items = search_items(conn, query, limit)?;

    let mut summaries = Vec::with_capacity(items.len());
    for it in items {
        summaries.push(item_to_summary(conn, it)?);
    }

    Ok(FfiPage {
        items: summaries,
        next_cursor: None,
        has_more: false,
    })
}

pub fn get_item_fn(
    conn: &Connection,
    blobs_dir: &Path,
    public_id: &str,
) -> Result<FfiItemDetail, FfiError> {
    let mut stmt = conn.prepare(
        "SELECT v.*, a.public_id AS account_public_id, t.public_id AS thread_public_id
         FROM visible_items v
         JOIN accounts a ON a.id = v.account_id
         LEFT JOIN threads t ON t.id = v.thread_id
         WHERE v.public_id = ?1;",
    )?;

    let item = stmt
        .query_row(params![public_id], |r| {
            row_to_item(r).map_err(|e| {
                rusqlite::Error::FromSqlConversionFailure(
                    0,
                    rusqlite::types::Type::Text,
                    Box::new(e),
                )
            })
        })
        .optional()?
        .ok_or_else(|| FfiError::NotFound(public_id.to_string()))?;

    let summary = item_to_summary(conn, item)?;

    let mut mail_stmt = conn.prepare(
        "SELECT m.from_addr, m.to_addrs, m.cc_addrs, m.reply_to, txt.body, m.raw_blob
         FROM items i
         LEFT JOIN mail_items m ON m.item_id = i.id
         LEFT JOIN item_text txt ON txt.item_id = i.id
         WHERE i.public_id = ?1;",
    )?;

    type MailRow = (
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
        Option<String>,
    );
    let (from_addr, to_json, cc_json, reply_to, body_text, raw_blob): MailRow = mail_stmt
        .query_row(params![public_id], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
            ))
        })
        .optional()?
        .unwrap_or_default();

    let to_addrs: Vec<String> = to_json
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();
    let cc_addrs: Vec<String> = cc_json
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();

    let raw_blob_path = raw_blob.and_then(|hash| blob_path(blobs_dir, &hash).ok());

    let mut att_stmt = conn.prepare(
        "SELECT section_id, filename, mime_type, size_bytes, blob_hash
         FROM attachments
         WHERE item_id = (SELECT id FROM items WHERE public_id = ?1)
         ORDER BY id ASC;",
    )?;

    let att_rows = att_stmt.query_map(params![public_id], |r| {
        let hash: Option<String> = r.get(4)?;
        let b_path = hash.as_deref().and_then(|h| blob_path(blobs_dir, h).ok());
        Ok(FfiAttachmentInfo {
            section_id: r.get(0)?,
            filename: r.get(1)?,
            content_type: r.get(2)?,
            size_bytes: r.get::<_, Option<i64>>(3)?.map(|s| s as u64),
            blob_hash: hash,
            blob_path: b_path,
        })
    })?;

    let mut attachments = Vec::new();
    for att in att_rows {
        attachments.push(att?);
    }

    Ok(FfiItemDetail {
        summary,
        from_addr,
        to_addrs,
        cc_addrs,
        reply_to,
        body_text,
        raw_blob_path,
        attachments,
    })
}

pub fn list_accounts_fn(conn: &Connection) -> Result<Vec<FfiAccount>, FfiError> {
    let mut stmt = conn.prepare(
        "SELECT a.public_id, a.name, a.kind, a.email, a.enabled, a.provider,
                (SELECT MAX(f.last_sync_at) FROM folders f WHERE f.account_id = a.id) AS last_sync_at,
                (SELECT COUNT(*) FROM visible_items v WHERE v.account_id = a.id AND v.unread = 1) AS unread_cnt
         FROM accounts a
         ORDER BY a.id ASC;",
    )?;

    let rows = stmt.query_map([], |r| {
        let last_sync: Option<i64> = r.get(6)?;
        Ok(FfiAccount {
            id: r.get(0)?,
            name: r.get(1)?,
            kind: r.get(2)?,
            email: r.get(3)?,
            enabled: r.get::<_, i64>(4)? != 0,
            provider_preset: r.get(5)?,
            last_sync_at: last_sync.map(format_timestamp),
            unread_count: r.get(7)?,
        })
    })?;

    let mut accounts = Vec::new();
    for a in rows {
        accounts.push(a?);
    }
    Ok(accounts)
}

pub fn get_account_fn(conn: &Connection, public_id: &str) -> Result<FfiAccount, FfiError> {
    let accounts = list_accounts_fn(conn)?;
    accounts
        .into_iter()
        .find(|a| a.id == public_id || a.name == public_id)
        .ok_or_else(|| FfiError::NotFound(public_id.to_string()))
}

pub fn list_folders_fn(
    conn: &Connection,
    account_id: Option<&str>,
) -> Result<Vec<FfiFolder>, FfiError> {
    let mut stmt = conn.prepare(
        "SELECT f.public_id, a.public_id, f.name, f.role, f.role_source,
                (SELECT COUNT(*) FROM visible_items v WHERE v.folder_id = f.id) AS cnt,
                (SELECT COUNT(*) FROM visible_items v WHERE v.folder_id = f.id AND v.unread = 1) AS unread_cnt
         FROM folders f
         JOIN accounts a ON a.id = f.account_id
         WHERE (?1 IS NULL OR a.public_id = ?1 OR a.name = ?1)
         ORDER BY f.name ASC;",
    )?;

    let rows = stmt.query_map(params![account_id], |r| {
        let src_str: Option<String> = r.get(4)?;
        let role_source = src_str
            .as_deref()
            .and_then(|s| RoleSource::from_str(s).ok())
            .map(FfiRoleSource::from);

        Ok(FfiFolder {
            id: r.get(0)?,
            account_id: r.get(1)?,
            name: r.get(2)?,
            role: r.get(3)?,
            role_source,
            item_count: r.get(5)?,
            unread_count: r.get(6)?,
        })
    })?;

    let mut folders = Vec::new();
    for f in rows {
        folders.push(f?);
    }
    Ok(folders)
}

pub fn list_feeds_fn(
    conn: &Connection,
    account_id: Option<&str>,
) -> Result<Vec<FfiFeed>, FfiError> {
    let mut stmt = conn.prepare(
        "SELECT f.public_id, a.public_id, f.url, f.title, f.interval_secs,
                f.last_fetch_at, f.next_fetch_at,
                (SELECT COUNT(*) FROM visible_items v WHERE v.feed_id = f.id AND v.unread = 1) AS unread_cnt
         FROM feeds f
         JOIN accounts a ON a.id = f.account_id
         WHERE f.removed_at IS NULL AND (?1 IS NULL OR a.public_id = ?1)
         ORDER BY f.id ASC;",
    )?;

    let rows = stmt.query_map(params![account_id], |r| {
        let url: String = r.get(2)?;
        let title: Option<String> = r.get(3)?;
        let name = title.clone().unwrap_or_else(|| url.clone());
        let last_fetch: Option<i64> = r.get(5)?;
        let next_fetch: Option<i64> = r.get(6)?;
        Ok(FfiFeed {
            id: r.get(0)?,
            account_id: r.get(1)?,
            url,
            name,
            title,
            interval_secs: r.get::<_, Option<u32>>(4)?.unwrap_or(0),
            last_fetch_at: last_fetch.map(format_timestamp),
            next_fetch_at: next_fetch.map(format_timestamp),
            unread_count: r.get(7)?,
        })
    })?;

    let mut feeds = Vec::new();
    for f in rows {
        feeds.push(f?);
    }
    Ok(feeds)
}

pub fn get_feed_fn(conn: &Connection, public_id: &str) -> Result<FfiFeed, FfiError> {
    let feeds = list_feeds_fn(conn, None)?;
    feeds
        .into_iter()
        .find(|f| f.id == public_id || f.name == public_id || f.url == public_id)
        .ok_or_else(|| FfiError::NotFound(public_id.to_string()))
}

pub fn get_pending_ops_fn(conn: &Connection) -> Result<Vec<FfiOpStatus>, FfiError> {
    let mut stmt = conn.prepare(
        "SELECT public_id, state, attempts, last_error
         FROM pending_ops
         ORDER BY id ASC;",
    )?;

    let rows = stmt.query_map([], |r| {
        let state_str: String = r.get(1)?;
        let op_state = match state_str.as_str() {
            "in_flight" => FfiOpState::InFlight,
            "done" => FfiOpState::Done,
            "failed" => FfiOpState::Failed,
            _ => FfiOpState::Pending,
        };
        Ok(FfiOpStatus {
            op_id: r.get(0)?,
            state: op_state,
            attempts: r.get(2)?,
            last_error: r.get(3)?,
        })
    })?;

    let mut ops = Vec::new();
    for o in rows {
        ops.push(o?);
    }
    Ok(ops)
}

pub fn get_op_status_fn(conn: &Connection, op_id: &str) -> Result<FfiOpStatus, FfiError> {
    let ops = get_pending_ops_fn(conn)?;
    ops.into_iter()
        .find(|o| o.op_id == op_id)
        .ok_or_else(|| FfiError::NotFound(op_id.to_string()))
}
