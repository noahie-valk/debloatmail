use rusqlite::{params, Connection, Row};
use std::str::FromStr;

use debloatmail_core::{
    AccountId, BodyState, Cursor, Item, ItemKind, PublicId, SortKey, StreamSource, StreamSpec,
    ThreadId, Timestamp,
};

use crate::schema::bump_revision;
use crate::StoreError;

/// Input data for recording a mail header during sync.
pub struct NewMailHeader<'a> {
    pub public_id: &'a str,
    pub account_id: i64,
    pub folder_id: i64,
    pub uid: u32,
    pub uidvalidity: u32,
    pub title: Option<&'a str>,
    pub author: Option<&'a str>,
    pub published_at: Option<Timestamp>,
    pub sort_at: Timestamp,
    pub unread: bool,
    pub starred: bool,
    pub answered: bool,
    pub has_attachments: bool,
    pub message_id: Option<&'a str>,
    pub in_reply_to: Option<&'a str>,
    pub references_hdr: Option<&'a str>,
    pub subject_norm: Option<&'a str>,
    pub from_addr: Option<&'a str>,
    pub to_addrs_json: Option<&'a str>,
    pub cc_addrs_json: Option<&'a str>,
    pub reply_to: Option<&'a str>,
    pub size_bytes: Option<u64>,
    pub flags_local: &'a str,
    pub created_at: Timestamp,
}

/// Insert a newly fetched email header, creating items, mail_items, and item_text rows.
pub fn insert_mail_header(conn: &Connection, h: &NewMailHeader) -> Result<i64, StoreError> {
    let title_display = match h.title {
        Some(t) if !t.trim().is_empty() => t,
        _ => "(no subject)",
    };

    conn.execute(
        "INSERT INTO items (
            public_id, kind, account_id, title, author, excerpt, published_at,
            sort_at, unread, starred, answered, hidden_at, thread_id,
            body_state, has_attachments, created_at, updated_at
        ) VALUES (
            ?1, 'mail', ?2, ?3, ?4, NULL, ?5,
            ?6, ?7, ?8, ?9, NULL, NULL,
            'none', ?10, ?11, ?11
        );",
        params![
            h.public_id,
            h.account_id,
            title_display,
            h.author,
            h.published_at,
            h.sort_at,
            if h.unread { 1 } else { 0 },
            if h.starred { 1 } else { 0 },
            if h.answered { 1 } else { 0 },
            if h.has_attachments { 1 } else { 0 },
            h.created_at,
        ],
    )?;

    let item_id = conn.last_insert_rowid();

    conn.execute(
        "INSERT INTO mail_items (
            item_id, folder_id, uid, uidvalidity, message_id, in_reply_to,
            references_hdr, subject_norm, from_addr, to_addrs, cc_addrs,
            reply_to, internal_date, size_bytes, raw_blob, flags_local
        ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6,
            ?7, ?8, ?9, ?10, ?11,
            ?12, ?13, ?14, NULL, ?15
        );",
        params![
            item_id,
            h.folder_id,
            h.uid,
            h.uidvalidity,
            h.message_id,
            h.in_reply_to,
            h.references_hdr,
            h.subject_norm,
            h.from_addr,
            h.to_addrs_json.unwrap_or("[]"),
            h.cc_addrs_json.unwrap_or("[]"),
            h.reply_to,
            h.sort_at,
            h.size_bytes,
            h.flags_local,
        ],
    )?;

    // Extracted text row exists from header sync (body empty, durable = 0)
    conn.execute(
        "INSERT INTO item_text (item_id, title, author, body, durable, extractor_version)
         VALUES (?1, ?2, ?3, '', 0, 1);",
        params![item_id, title_display, h.author.unwrap_or("")],
    )?;

    bump_revision(conn)?;
    Ok(item_id)
}

/// Evict body text for a mail item (empties body, sets body_state = 'none', keeps title/author).
pub fn evict_mail_body(conn: &Connection, item_id: i64) -> Result<(), StoreError> {
    conn.execute(
        "UPDATE item_text SET body = '' WHERE item_id = ?1 AND durable = 0;",
        params![item_id],
    )?;
    conn.execute(
        "UPDATE items SET body_state = 'none' WHERE id = ?1;",
        params![item_id],
    )?;
    bump_revision(conn)?;
    Ok(())
}

/// Map a SQL row from visible_items (with joined account and thread public_ids) to Item.
pub fn row_to_item(row: &Row) -> Result<Item, StoreError> {
    let public_id_str: String = row.get("public_id")?;
    let kind_str: String = row.get("kind")?;
    let account_pub_str: String = row.get("account_public_id")?;
    let title: String = row
        .get::<_, Option<String>>("title")?
        .unwrap_or_else(|| "(untitled)".to_string());
    let author: Option<String> = row.get("author")?;
    let excerpt: Option<String> = row.get("excerpt")?;
    let date: Option<Timestamp> = row.get("published_at")?;
    let sort_at: Timestamp = row.get("sort_at")?;
    let unread: i64 = row.get("unread")?;
    let starred: i64 = row.get("starred")?;
    let answered: i64 = row.get("answered")?;
    let has_attachments: i64 = row.get("has_attachments")?;
    let thread_pub_str: Option<String> = row.get("thread_public_id")?;
    let body_state_str: String = row.get("body_state")?;

    let id = PublicId::new(public_id_str)?;
    let kind = ItemKind::from_str(&kind_str)?;
    let account = AccountId::new(account_pub_str)?;
    let thread = thread_pub_str.map(ThreadId::new).transpose()?;
    let body_state = BodyState::from_str(&body_state_str)?;

    Ok(Item {
        id,
        kind,
        account,
        title,
        author,
        excerpt,
        date,
        sort_at,
        unread: unread != 0,
        starred: starred != 0,
        answered: answered != 0,
        has_attachments: has_attachments != 0,
        thread,
        body_state,
    })
}

/// Execute a query over the visible_items stream according to the given StreamSpec.
pub fn query_stream(
    conn: &Connection,
    spec: &StreamSpec,
    cursor: Option<&Cursor>,
    limit: u32,
) -> Result<Vec<Item>, StoreError> {
    if let Some(c) = cursor {
        if c.sort != spec.sort {
            return Err(StoreError::InvalidCursor(format!(
                "cursor sort {:?} does not match stream sort {:?}",
                c.sort, spec.sort
            )));
        }
    }

    let mut query = String::from(
        "SELECT v.*, a.public_id AS account_public_id, t.public_id AS thread_public_id
         FROM visible_items v
         JOIN accounts a ON a.id = v.account_id
         LEFT JOIN threads t ON t.id = v.thread_id
         WHERE 1=1 ",
    );

    // Source filtering
    match &spec.source {
        StreamSource::Unified => {
            query.push_str(
                " AND (v.kind = 'feed' OR (v.kind = 'mail' AND v.folder_id IN (
                    SELECT id FROM folders WHERE role = 'inbox'
                 ))) ",
            );
        }
        StreamSource::Account(acc_id) => {
            query.push_str(&format!(
                " AND v.account_id = (SELECT id FROM accounts WHERE public_id = '{}') ",
                acc_id.as_str()
            ));
        }
        StreamSource::Folder(folder_id) => {
            query.push_str(&format!(
                " AND v.folder_id = (SELECT id FROM folders WHERE public_id = '{}') ",
                folder_id.as_str()
            ));
        }
        StreamSource::Feed(feed_id) => {
            query.push_str(&format!(
                " AND v.feed_id = (SELECT id FROM feeds WHERE public_id = '{}') ",
                feed_id.as_str()
            ));
        }
        StreamSource::Thread(thread_id) => {
            query.push_str(&format!(
                " AND v.thread_id = (SELECT id FROM threads WHERE public_id = '{}') ",
                thread_id.as_str()
            ));
        }
        StreamSource::Search(_) => {
            // Handled via search.rs
        }
    }

    // Flag / time filters
    if spec.unread_only {
        query.push_str(" AND v.unread = 1 ");
    }
    if spec.starred_only {
        query.push_str(" AND v.starred = 1 ");
    }
    if let Some(since) = spec.since {
        query.push_str(&format!(" AND v.sort_at >= {since} "));
    }

    // Cursor pagination (keyset based on internal id lookup for cursor.id)
    if let Some(c) = cursor {
        match c.sort {
            SortKey::Date => {
                if let Some(pub_at) = c.published_at {
                    query.push_str(&format!(
                        " AND (v.published_at < {pub_at}
                               OR (v.published_at = {pub_at} AND v.sort_at < {})
                               OR (v.published_at = {pub_at} AND v.sort_at = {} AND v.id < (SELECT id FROM items WHERE public_id = '{}'))) ",
                        c.sort_at.unwrap_or(0),
                        c.sort_at.unwrap_or(0),
                        c.id.as_str()
                    ));
                } else {
                    query.push_str(&format!(
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
                let s_at = c.sort_at.unwrap_or(0);
                query.push_str(&format!(
                    " AND (v.sort_at < {s_at} OR (v.sort_at = {s_at} AND v.id < (SELECT id FROM items WHERE public_id = '{}'))) ",
                    c.id.as_str()
                ));
            }
            SortKey::Author => {
                let author_text = c.text.as_deref().unwrap_or("");
                query.push_str(&format!(
                    " AND (v.author COLLATE NOCASE > '{author_text}'
                           OR (v.author COLLATE NOCASE = '{author_text}' AND (
                               v.sort_at < {} OR (v.sort_at = {} AND v.id < (SELECT id FROM items WHERE public_id = '{}'))
                           ))) ",
                    c.sort_at.unwrap_or(0),
                    c.sort_at.unwrap_or(0),
                    c.id.as_str()
                ));
            }
            SortKey::Subject => {
                let title_text = c.text.as_deref().unwrap_or("");
                query.push_str(&format!(
                    " AND (v.title COLLATE NOCASE > '{title_text}'
                           OR (v.title COLLATE NOCASE = '{title_text}' AND (
                               v.sort_at < {} OR (v.sort_at = {} AND v.id < (SELECT id FROM items WHERE public_id = '{}'))
                           ))) ",
                    c.sort_at.unwrap_or(0),
                    c.sort_at.unwrap_or(0),
                    c.id.as_str()
                ));
            }
        }
    }

    // Sort order
    match spec.sort {
        SortKey::Date => {
            query.push_str(" ORDER BY v.published_at DESC, v.sort_at DESC, v.id DESC ")
        }
        SortKey::Received => query.push_str(" ORDER BY v.sort_at DESC, v.id DESC "),
        SortKey::Author => {
            query.push_str(" ORDER BY v.author COLLATE NOCASE ASC, v.sort_at DESC, v.id DESC ")
        }
        SortKey::Subject => {
            query.push_str(" ORDER BY v.title COLLATE NOCASE ASC, v.sort_at DESC, v.id DESC ")
        }
    }

    query.push_str(&format!(" LIMIT {limit};"));

    let mut stmt = conn.prepare(&query)?;
    let item_iter = stmt.query_map([], |row| {
        row_to_item(row).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
    })?;

    let mut items = Vec::new();
    for it in item_iter {
        items.push(it?);
    }
    Ok(items)
}
