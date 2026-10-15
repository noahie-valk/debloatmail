use rusqlite::{params, Connection};

use crate::schema::bump_revision;
use crate::StoreError;

/// Set the unread flag for an item (by internal id).
pub fn set_unread(conn: &Connection, item_id: i64, unread: bool) -> Result<(), StoreError> {
    conn.execute(
        "UPDATE items SET unread = ?1 WHERE id = ?2;",
        params![if unread { 1 } else { 0 }, item_id],
    )?;
    bump_revision(conn)?;
    Ok(())
}

/// Set the starred flag for an item.
pub fn set_starred(conn: &Connection, item_id: i64, starred: bool) -> Result<(), StoreError> {
    conn.execute(
        "UPDATE items SET starred = ?1 WHERE id = ?2;",
        params![if starred { 1 } else { 0 }, item_id],
    )?;
    bump_revision(conn)?;
    Ok(())
}

/// Set the answered flag for an item.
pub fn set_answered(conn: &Connection, item_id: i64, answered: bool) -> Result<(), StoreError> {
    conn.execute(
        "UPDATE items SET answered = ?1 WHERE id = ?2;",
        params![if answered { 1 } else { 0 }, item_id],
    )?;
    bump_revision(conn)?;
    Ok(())
}

/// Set or clear suppression (hidden_at) on an item.
pub fn set_hidden(
    conn: &Connection,
    item_id: i64,
    hidden_at: Option<i64>,
) -> Result<(), StoreError> {
    conn.execute(
        "UPDATE items SET hidden_at = ?1 WHERE id = ?2;",
        params![hidden_at, item_id],
    )?;
    bump_revision(conn)?;
    Ok(())
}

/// Queue a flag operation in pending_ops when modifying a mail item.
pub fn queue_flag_op(
    conn: &Connection,
    op_public_id: &str,
    account_id: i64,
    item_id: i64,
    payload_json: &str,
    now: i64,
) -> Result<i64, StoreError> {
    conn.execute(
        "INSERT INTO pending_ops (
            public_id, account_id, op, item_id, payload, state, attempts, created_at
        ) VALUES (
            ?1, ?2, 'flag', ?3, ?4, 'pending', 0, ?5
        );",
        params![op_public_id, account_id, item_id, payload_json, now],
    )?;
    let op_id = conn.last_insert_rowid();
    bump_revision(conn)?;
    Ok(op_id)
}
