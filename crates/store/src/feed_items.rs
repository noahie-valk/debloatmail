use rusqlite::{params, Connection, OptionalExtension};

use crate::schema::bump_revision;
use crate::StoreError;

/// Input for upserting a feed article.
pub struct UpsertFeedItem<'a> {
    pub feed_id: i64,
    pub account_id: i64,
    pub public_id: &'a str,
    pub guid: Option<&'a str>,
    pub link: Option<&'a str>,
    pub title: Option<&'a str>,
    pub author: Option<&'a str>,
    pub published_at: Option<i64>,
    pub sort_at: i64,
    pub body: &'a str,
    pub now: i64,
}

/// Derive an excerpt from the body text (first 200 chars of first non-empty paragraph).
pub fn derive_excerpt(body: &str) -> Option<String> {
    for para in body.split("\n\n") {
        let trimmed = para.trim();
        if !trimmed.is_empty() {
            let collapsed: String = trimmed.split_whitespace().collect::<Vec<_>>().join(" ");
            let mut chars = collapsed.chars();
            let excerpt: String = chars.by_ref().take(200).collect();
            return Some(excerpt);
        }
    }
    None
}

/// Derive feed item title according to §4.2.1: entry title, else link, else "(untitled)".
pub fn derive_title(title: Option<&str>, link: Option<&str>) -> String {
    if let Some(t) = title {
        let trimmed = t.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    if let Some(l) = link {
        let trimmed = l.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    "(untitled)".to_string()
}

/// Upsert a feed article according to §11.2 and §11.4:
/// Content columns are updated, but unread, starred, hidden_at, body_state, thread_id, and created_at are preserved.
pub fn upsert_feed_item(conn: &Connection, item: &UpsertFeedItem) -> Result<i64, StoreError> {
    let title = derive_title(item.title, item.link);
    let excerpt = derive_excerpt(item.body);

    // Look for existing feed item with same (feed_id, guid)
    let existing_item_id: Option<i64> = if let Some(guid) = item.guid {
        conn.query_row(
            "SELECT item_id FROM feed_items WHERE feed_id = ?1 AND guid = ?2;",
            params![item.feed_id, guid],
            |row| row.get(0),
        )
        .optional()?
    } else {
        None
    };

    if let Some(item_id) = existing_item_id {
        // Update content columns only (preserve unread, starred, hidden_at, etc.)
        conn.execute(
            "UPDATE items SET
                title = ?1,
                author = ?2,
                excerpt = ?3,
                published_at = ?4,
                sort_at = ?5,
                updated_at = ?6
             WHERE id = ?7;",
            params![
                title,
                item.author,
                excerpt,
                item.published_at,
                item.sort_at,
                item.now,
                item_id,
            ],
        )?;

        conn.execute(
            "UPDATE feed_items SET link = ?1, author = ?2 WHERE item_id = ?3;",
            params![item.link, item.author, item_id],
        )?;

        conn.execute(
            "UPDATE item_text SET title = ?1, author = ?2, body = ?3 WHERE item_id = ?4;",
            params![title, item.author.unwrap_or(""), item.body, item_id],
        )?;

        bump_revision(conn)?;
        Ok(item_id)
    } else {
        // Fresh insert: body_state is 'cached', durable = 1
        conn.execute(
            "INSERT INTO items (
                public_id, kind, account_id, title, author, excerpt, published_at,
                sort_at, unread, starred, answered, hidden_at, thread_id,
                body_state, has_attachments, created_at, updated_at
            ) VALUES (
                ?1, 'feed', ?2, ?3, ?4, ?5, ?6,
                ?7, 1, 0, 0, NULL, NULL,
                'cached', 0, ?8, ?8
            );",
            params![
                item.public_id,
                item.account_id,
                title,
                item.author,
                excerpt,
                item.published_at,
                item.sort_at,
                item.now,
            ],
        )?;

        let item_id = conn.last_insert_rowid();

        conn.execute(
            "INSERT INTO feed_items (item_id, feed_id, guid, link, author)
             VALUES (?1, ?2, ?3, ?4, ?5);",
            params![item_id, item.feed_id, item.guid, item.link, item.author],
        )?;

        conn.execute(
            "INSERT INTO item_text (item_id, title, author, body, durable, extractor_version)
             VALUES (?1, ?2, ?3, ?4, 1, 1);",
            params![item_id, title, item.author.unwrap_or(""), item.body],
        )?;

        bump_revision(conn)?;
        Ok(item_id)
    }
}

/// Soft-remove a feed: sets removed_at so polling stops, but retaining articles in streams/search.
pub fn soft_remove_feed(conn: &Connection, feed_id: i64, now: i64) -> Result<(), StoreError> {
    conn.execute(
        "UPDATE feeds SET removed_at = ?1 WHERE id = ?2;",
        params![now, feed_id],
    )?;
    bump_revision(conn)?;
    Ok(())
}

/// Purge a feed: delete the feed row, along with feed_items, spine items, and item_text.
pub fn purge_feed(conn: &Connection, feed_id: i64) -> Result<(), StoreError> {
    // Find all spine item_ids for this feed
    let mut stmt = conn.prepare("SELECT item_id FROM feed_items WHERE feed_id = ?1;")?;
    let item_ids: Vec<i64> = stmt
        .query_map(params![feed_id], |row| row.get(0))?
        .collect::<Result<Vec<_>, _>>()?;

    for id in item_ids {
        conn.execute("DELETE FROM items WHERE id = ?1;", params![id])?;
    }
    conn.execute("DELETE FROM feeds WHERE id = ?1;", params![feed_id])?;

    bump_revision(conn)?;
    Ok(())
}
