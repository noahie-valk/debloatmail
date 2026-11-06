//! The unified stream `list` command conforming to `docs/debloatmailctl.md` §8.

use chrono::{Duration, Utc};
use debloatmail_core::{Cursor, SortKey};
use debloatmail_ffi::reader::{
    decode_cursor, encode_cursor, item_to_summary, query_threads_fn,
};
use debloatmail_ffi::{FfiItemSummary, FfiPage};
use debloatmail_store::headers::row_to_item;
use rusqlite::Connection;

use crate::args::{Cli, ListArgs, SortOption};
use crate::error::CliError;
use crate::format::{format_item_page, format_thread_page};
use crate::resolve::{resolve_account, resolve_feed, resolve_folder};

/// Parse date bounds supporting ISO 8601 (`2026-10-07`, `2026-10-07T14:22:11Z`)
/// and relative shorthand (`7d`, `2w`, `24h`, `30m`).
pub fn parse_date_filter(s: &str) -> Result<i64, CliError> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Err(CliError::Usage("date filter cannot be empty".to_string()));
    }

    // Relative shorthand
    if let Some(num_str) = trimmed.strip_suffix('d') {
        if let Ok(n) = num_str.parse::<i64>() {
            return Ok((Utc::now() - Duration::days(n)).timestamp());
        }
    } else if let Some(num_str) = trimmed.strip_suffix('w') {
        if let Ok(n) = num_str.parse::<i64>() {
            return Ok((Utc::now() - Duration::weeks(n)).timestamp());
        }
    } else if let Some(num_str) = trimmed.strip_suffix('h') {
        if let Ok(n) = num_str.parse::<i64>() {
            return Ok((Utc::now() - Duration::hours(n)).timestamp());
        }
    } else if let Some(num_str) = trimmed.strip_suffix('m') {
        if let Ok(n) = num_str.parse::<i64>() {
            return Ok((Utc::now() - Duration::minutes(n)).timestamp());
        }
    } else if let Some(num_str) = trimmed.strip_suffix('y') {
        if let Ok(n) = num_str.parse::<i64>() {
            return Ok((Utc::now() - Duration::days(n * 365)).timestamp());
        }
    }

    // ISO 8601 / RFC 3339 formats
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(trimmed) {
        return Ok(dt.timestamp());
    }
    if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(trimmed, "%Y-%m-%d %H:%M:%S") {
        return Ok(dt.and_utc().timestamp());
    }
    if let Ok(d) = chrono::NaiveDate::parse_from_str(trimmed, "%Y-%m-%d") {
        if let Some(dt) = d.and_hms_opt(0, 0, 0) {
            return Ok(dt.and_utc().timestamp());
        }
    }

    Err(CliError::Usage(format!("invalid date format '{trimmed}'")))
}

pub fn sort_option_to_key(opt: SortOption) -> SortKey {
    match opt {
        SortOption::Date => SortKey::Date,
        SortOption::Received => SortKey::Received,
        SortOption::Author => SortKey::Author,
        SortOption::Subject => SortKey::Subject,
    }
}

pub fn execute_list(
    conn: &Connection,
    cli: &Cli,
    args: &ListArgs,
) -> Result<(), CliError> {
    let sort_key = sort_option_to_key(args.sort);

    // Keyset cursor validation
    let cursor_opt = match &args.cursor {
        Some(token) => {
            let decoded = decode_cursor(token)
                .map_err(|e| CliError::Usage(format!("invalid cursor: {e}")))?;
            if decoded.sort != sort_key {
                return Err(CliError::Usage(format!(
                    "cursor sort '{:?}' does not match requested sort '{:?}'",
                    decoded.sort, sort_key
                )));
            }
            Some(decoded)
        }
        None => None,
    };

    // Client-side account resolution
    let account_filter = match &cli.account {
        Some(acc_arg) => {
            let acc = resolve_account(conn, acc_arg)?;
            Some(acc.id)
        }
        None => None,
    };

    // Thread aggregation mode
    if args.threads {
        let limit = if args.all { 10_000 } else { args.limit.unwrap_or(25) };
        let thread_page = query_threads_fn(
            conn,
            account_filter.as_deref(),
            sort_key,
            limit,
            args.cursor.as_deref(),
        )?;
        return format_thread_page(&thread_page, cli.format, cli.color);
    }

    // Client-side folder and feed resolution
    let folder_id_opt = match &args.folder {
        Some(f_arg) => {
            let f = resolve_folder(conn, account_filter.as_deref(), f_arg)?;
            Some(f.id)
        }
        None => None,
    };

    let feed_id_opt = match &args.feed {
        Some(feed_name) if !feed_name.is_empty() => {
            let feed = resolve_feed(conn, account_filter.as_deref(), feed_name)?;
            Some(feed.id)
        }
        _ => None,
    };

    // Kind filter: --mail, --feed (without name), or both
    let kind_filter: Option<&str> = if args.mail {
        Some("mail")
    } else if args.feed.is_some() && feed_id_opt.is_none() {
        Some("feed")
    } else {
        None
    };

    // Since and before timestamps
    let since_ts = match &args.since {
        Some(s) => Some(parse_date_filter(s)?),
        None => None,
    };
    let before_ts = match &args.before {
        Some(b) => Some(parse_date_filter(b)?),
        None => None,
    };

    let limit = if args.all { 10_000 } else { args.limit.unwrap_or(25) };

    let page = query_items(
        conn,
        account_filter.as_deref(),
        folder_id_opt.as_deref(),
        feed_id_opt.as_deref(),
        kind_filter,
        args.unread,
        args.starred,
        since_ts,
        before_ts,
        sort_key,
        limit,
        cursor_opt.as_ref(),
    )?;

    format_item_page(&page, cli.format, cli.color, args.print0)
}

#[allow(clippy::too_many_arguments)]
fn query_items(
    conn: &Connection,
    account_id: Option<&str>,
    folder_id: Option<&str>,
    feed_id: Option<&str>,
    kind_filter: Option<&str>,
    unread_only: bool,
    starred_only: bool,
    since: Option<i64>,
    before: Option<i64>,
    sort: SortKey,
    limit: u32,
    cursor: Option<&Cursor>,
) -> Result<FfiPage<FfiItemSummary>, CliError> {
    let mut sql = String::from(
        "SELECT v.*, a.public_id AS account_public_id, t.public_id AS thread_public_id
         FROM visible_items v
         JOIN accounts a ON a.id = v.account_id
         LEFT JOIN threads t ON t.id = v.thread_id
         WHERE 1=1 ",
    );

    // Source predicate
    if let Some(fid) = feed_id {
        sql.push_str(&format!(
            " AND v.feed_id = (SELECT id FROM feeds WHERE public_id = '{fid}') "
        ));
    } else if let Some(fol_id) = folder_id {
        sql.push_str(&format!(
            " AND v.folder_id = (SELECT id FROM folders WHERE public_id = '{fol_id}') "
        ));
    } else if let Some(acc_id) = account_id {
        sql.push_str(&format!(
            " AND v.account_id = (SELECT id FROM accounts WHERE public_id = '{acc_id}') "
        ));
    } else {
        // Default unified inbox predicate: mail in inbox role + all feeds
        sql.push_str(
            " AND (v.kind = 'feed' OR (v.kind = 'mail' AND v.folder_id IN (
                SELECT id FROM folders WHERE role = 'inbox'
             ))) ",
        );
    }

    if let Some(kind) = kind_filter {
        sql.push_str(&format!(" AND v.kind = '{kind}' "));
    }

    if unread_only {
        sql.push_str(" AND v.unread = 1 ");
    }
    if starred_only {
        sql.push_str(" AND v.starred = 1 ");
    }
    if let Some(since_val) = since {
        sql.push_str(&format!(" AND v.sort_at >= {since_val} "));
    }
    if let Some(before_val) = before {
        sql.push_str(&format!(" AND v.sort_at <= {before_val} "));
    }

    // Cursor pagination (keyset)
    if let Some(c) = cursor {
        match c.sort {
            SortKey::Date => {
                if let Some(date) = c.date {
                    sql.push_str(&format!(
                        " AND (v.published_at < {date}
                               OR (v.published_at = {date} AND v.sort_at < {})
                               OR (v.published_at = {date} AND v.sort_at = {} AND v.id < (SELECT id FROM items WHERE public_id = '{}'))) ",
                        c.sort_at.unwrap_or(0),
                        c.sort_at.unwrap_or(0),
                        c.id.as_str()
                    ));
                } else {
                    sql.push_str(&format!(
                        " AND (v.published_at IS NULL AND (
                            v.sort_at < {}
                            OR (v.sort_at = {} AND v.id < (SELECT id FROM items WHERE public_id = '{}'))
                        )) ",
                        c.sort_at.unwrap_or(0),
                        c.sort_at.unwrap_or(0),
                        c.id.as_str()
                    ));
                }
            }
            SortKey::Received => {
                sql.push_str(&format!(
                    " AND (v.sort_at < {} OR (v.sort_at = {} AND v.id < (SELECT id FROM items WHERE public_id = '{}'))) ",
                    c.sort_at.unwrap_or(0),
                    c.sort_at.unwrap_or(0),
                    c.id.as_str()
                ));
            }
            SortKey::Author => {
                let author_val = c.text.as_deref().unwrap_or("").replace('\'', "''");
                sql.push_str(&format!(
                    " AND (v.author > '{author_val}'
                           OR (v.author = '{author_val}' AND v.sort_at < {})
                           OR (v.author = '{author_val}' AND v.sort_at = {} AND v.id < (SELECT id FROM items WHERE public_id = '{}'))) ",
                    c.sort_at.unwrap_or(0),
                    c.sort_at.unwrap_or(0),
                    c.id.as_str()
                ));
            }
            SortKey::Subject => {
                let title_val = c.text.as_deref().unwrap_or("").replace('\'', "''");
                sql.push_str(&format!(
                    " AND (v.title > '{title_val}'
                           OR (v.title = '{title_val}' AND v.sort_at < {})
                           OR (v.title = '{title_val}' AND v.sort_at = {} AND v.id < (SELECT id FROM items WHERE public_id = '{}'))) ",
                    c.sort_at.unwrap_or(0),
                    c.sort_at.unwrap_or(0),
                    c.id.as_str()
                ));
            }
        }
    }

    // ORDER BY
    match sort {
        SortKey::Date => sql.push_str(" ORDER BY v.published_at DESC, v.sort_at DESC, v.id DESC "),
        SortKey::Received => sql.push_str(" ORDER BY v.sort_at DESC, v.id DESC "),
        SortKey::Author => sql.push_str(" ORDER BY v.author ASC, v.sort_at DESC, v.id DESC "),
        SortKey::Subject => sql.push_str(" ORDER BY v.title ASC, v.sort_at DESC, v.id DESC "),
    }

    let fetch_limit = limit.saturating_add(1);
    sql.push_str(&format!(" LIMIT {fetch_limit};"));

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        row_to_item(row).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
    })?;

    let mut raw_items = Vec::new();
    for r in rows {
        raw_items.push(r?);
    }

    let has_more = raw_items.len() > limit as usize;
    if has_more {
        raw_items.truncate(limit as usize);
    }

    let next_cursor = if has_more {
        raw_items.last().map(|item| {
            encode_cursor(&Cursor {
                sort,
                date: item.date,
                sort_at: Some(item.sort_at),
                text: match sort {
                    SortKey::Author => item.author.clone(),
                    SortKey::Subject => Some(item.title.clone()),
                    _ => None,
                },
                id: item.id.clone(),
            })
        })
    } else {
        None
    };

    let mut summaries = Vec::with_capacity(raw_items.len());
    for it in raw_items {
        summaries.push(item_to_summary(conn, it)?);
    }

    Ok(FfiPage {
        items: summaries,
        next_cursor,
        has_more,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_date_filter_relative() {
        let ts_7d = parse_date_filter("7d").unwrap();
        let expected_approx = (Utc::now() - Duration::days(7)).timestamp();
        assert!((ts_7d - expected_approx).abs() < 5);

        let ts_2w = parse_date_filter("2w").unwrap();
        let expected_2w = (Utc::now() - Duration::weeks(2)).timestamp();
        assert!((ts_2w - expected_2w).abs() < 5);
    }

    #[test]
    fn test_parse_date_filter_iso() {
        let ts = parse_date_filter("2026-10-07").unwrap();
        let dt = chrono::DateTime::from_timestamp(ts, 0).unwrap();
        assert_eq!(dt.format("%Y-%m-%d").to_string(), "2026-10-07");
    }

    #[test]
    fn test_parse_date_filter_invalid() {
        assert!(parse_date_filter("not-a-date").is_err());
    }
}
