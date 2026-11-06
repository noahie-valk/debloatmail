//! Content-addressed on-disk blob management, refcount reconciliation,
//! and LRU body/attachment eviction (`DATA-MODEL.md` §5, §9.1–9.3; `SECURITY.md` §8.1–8.5).

use std::fs::{self, DirBuilder};
use std::io::Write;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

use debloatmail_core::Timestamp;
use rusqlite::{Connection, OptionalExtension, params};

use crate::StoreError;
use crate::schema::bump_revision;
use crate::secure_file::secure_create;

/// One-hour grace window before a zero-refcount blob may be garbage-collected (`DATA-MODEL.md` §9.3).
pub const BLOB_GC_GRACE_WINDOW_SECS: i64 = 3600;

/// Discrepancy between `blobs.refcount` and actual non-NULL references in `mail_items.raw_blob`
/// + `attachments.blob_hash` (`DATA-MODEL.md` §5, §16).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobRefcountDiscrepancy {
    pub hash: String,
    pub recorded_refcount: i64,
    pub actual_refcount: i64,
}

/// Validates that `hash` is a canonical 64-character lowercase hexadecimal BLAKE3 digest.
pub fn validate_blob_hash(hash: &str) -> Result<(), StoreError> {
    if hash.len() != 64
        || !hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(StoreError::InvalidBlobHash(hash.to_string()));
    }
    Ok(())
}

/// Computes the canonical 64-char hex BLAKE3 digest of `data`.
pub fn compute_blob_hash(data: &[u8]) -> String {
    blake3::hash(data).to_hex().to_string()
}

/// Resolves the content-addressed path `$XDG_CACHE_HOME/debloatmail/blobs/<hash[0:2]>/<hash>`
/// (`DATA-MODEL.md` §9.1).
///
/// Validates `hash` as 64 lowercase hex characters so path traversal is impossible by construction.
pub fn blob_path(cache_dir: &Path, hash: &str) -> Result<PathBuf, StoreError> {
    validate_blob_hash(hash)?;
    let prefix = &hash[0..2];
    Ok(cache_dir.join("blobs").join(prefix).join(hash))
}

/// Writes `data` into the content-addressed blob store on disk with mode `0o600`
/// (`O_CREAT | O_EXCL | O_NOFOLLOW`) and upserts the metadata row in `blobs` (`DATA-MODEL.md` §5, §9.1).
///
/// Note: Does **not** increment `refcount`; callers must call [`increment_blob_ref`] in the same
/// transaction that sets `mail_items.raw_blob` or `attachments.blob_hash` (`DATA-MODEL.md` §5).
pub fn put_blob(
    conn: &Connection,
    cache_dir: &Path,
    data: &[u8],
    now: Timestamp,
) -> Result<String, StoreError> {
    let hash = compute_blob_hash(data);
    let path = blob_path(cache_dir, &hash)?;

    if let Some(parent) = path.parent() {
        let mut builder = DirBuilder::new();
        builder.recursive(true).mode(0o700);
        builder.create(parent)?;
    }

    if !path.exists() {
        let mut file = secure_create(&path)?;
        file.write_all(data)?;
        file.sync_all()?;
    }

    let size_bytes = data.len() as i64;
    conn.execute(
        "INSERT INTO blobs (hash, size_bytes, refcount, last_used_at, created_at)
         VALUES (?1, ?2, 0, ?3, ?3)
         ON CONFLICT(hash) DO UPDATE SET
             size_bytes = excluded.size_bytes,
             last_used_at = excluded.last_used_at;",
        params![hash, size_bytes, now],
    )?;

    Ok(hash)
}

/// Reads the raw bytes of a content-addressed blob from disk.
pub fn read_blob(cache_dir: &Path, hash: &str) -> Result<Vec<u8>, StoreError> {
    let path = blob_path(cache_dir, hash)?;
    let bytes = fs::read(path)?;
    Ok(bytes)
}

/// Increments `blobs.refcount` and updates `last_used_at` inside the same transaction
/// as a new `mail_items.raw_blob` or `attachments.blob_hash` reference (`DATA-MODEL.md` §5).
pub fn increment_blob_ref(conn: &Connection, hash: &str, now: Timestamp) -> Result<(), StoreError> {
    validate_blob_hash(hash)?;
    let changed = conn.execute(
        "UPDATE blobs SET refcount = refcount + 1, last_used_at = ?1 WHERE hash = ?2;",
        params![now, hash],
    )?;
    if changed == 0 {
        return Err(StoreError::NotFound(format!("blob {hash}")));
    }
    Ok(())
}

/// Decrements `blobs.refcount` (clamped at `0`) and updates `last_used_at = now` so the
/// 1-hour grace window begins when the reference is released (`DATA-MODEL.md` §5, §9.3).
pub fn decrement_blob_ref(conn: &Connection, hash: &str, now: Timestamp) -> Result<(), StoreError> {
    validate_blob_hash(hash)?;
    conn.execute(
        "UPDATE blobs
         SET refcount = CASE WHEN refcount > 0 THEN refcount - 1 ELSE 0 END,
             last_used_at = ?1
         WHERE hash = ?2;",
        params![now, hash],
    )?;
    Ok(())
}

/// Updates `blobs.last_used_at = now` to protect the blob under the 1-hour grace window
/// (`DATA-MODEL.md` §9.3) and returns its on-disk path.
pub fn touch_blob(
    conn: &Connection,
    cache_dir: &Path,
    hash: &str,
    now: Timestamp,
) -> Result<PathBuf, StoreError> {
    let path = blob_path(cache_dir, hash)?;
    let updated = conn.execute(
        "UPDATE blobs SET last_used_at = ?1 WHERE hash = ?2;",
        params![now, hash],
    )?;
    if updated == 0 {
        return Err(StoreError::NotFound(format!("blob {hash}")));
    }
    Ok(path)
}

/// Garbage-collects unreferenced blobs whose 1-hour grace window has expired:
/// `refcount <= 0 AND last_used_at < now - 3600` (`DATA-MODEL.md` §9.3).
///
/// Unlinks the on-disk files and deletes the `blobs` rows. Returns the number of blobs collected.
pub fn gc_blobs(conn: &Connection, cache_dir: &Path, now: Timestamp) -> Result<usize, StoreError> {
    let cutoff = now.saturating_sub(BLOB_GC_GRACE_WINDOW_SECS);
    let mut stmt = conn.prepare(
        "SELECT hash FROM blobs WHERE refcount <= 0 AND last_used_at < ?1 ORDER BY hash;",
    )?;
    let hashes: Vec<String> = stmt
        .query_map(params![cutoff], |r| r.get(0))?
        .collect::<Result<Vec<_>, _>>()?;

    if hashes.is_empty() {
        return Ok(0);
    }

    for hash in &hashes {
        if let Ok(path) = blob_path(cache_dir, hash) {
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(StoreError::Io(e)),
            }
        }
        conn.execute("DELETE FROM blobs WHERE hash = ?1;", params![hash])?;
    }

    bump_revision(conn)?;
    Ok(hashes.len())
}

/// Reconciles `blobs.refcount` against the actual count of non-NULL references in
/// `mail_items.raw_blob` and `attachments.blob_hash` (`DATA-MODEL.md` §5, §16).
///
/// When `repair` is `true`, updates any mismatched `blobs.refcount` values to the true count.
pub fn reconcile_blob_refcounts(
    conn: &Connection,
    repair: bool,
) -> Result<Vec<BlobRefcountDiscrepancy>, StoreError> {
    let mut stmt = conn.prepare(
        "SELECT b.hash,
                b.refcount,
                (SELECT COUNT(*) FROM mail_items m WHERE m.raw_blob = b.hash) +
                (SELECT COUNT(*) FROM attachments a WHERE a.blob_hash = b.hash) AS actual_refcount
         FROM blobs b
         ORDER BY b.hash;",
    )?;

    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, i64>(2)?,
        ))
    })?;

    let mut discrepancies = Vec::new();
    for row in rows {
        let (hash, recorded, actual) = row?;
        if recorded != actual {
            if repair {
                conn.execute(
                    "UPDATE blobs SET refcount = ?1 WHERE hash = ?2;",
                    params![actual, hash],
                )?;
            }
            discrepancies.push(BlobRefcountDiscrepancy {
                hash,
                recorded_refcount: recorded,
                actual_refcount: actual,
            });
        }
    }

    if repair && !discrepancies.is_empty() {
        bump_revision(conn)?;
    }

    Ok(discrepancies)
}

/// Evicts oldest cached mail bodies (`durable = 0`) until total cached mail body bytes
/// are `<= max_total_bytes` (`DATA-MODEL.md` §9.2).
///
/// Never evicts:
/// - Feed items (`durable = 1`)
/// - Starred items (`starred = 1`)
/// - Items with an active (`pending` or `in_flight`) row in `pending_ops`
///
/// Empties `item_text.body = ''`, sets `items.body_state = 'none'` and `items.excerpt = NULL`,
/// and clears `mail_items.raw_blob = NULL` (decrementing `blobs.refcount`), while retaining
/// `item_text.title` and `item_text.author` so subject/author search continues to work.
pub fn evict_lru_mail_bodies(
    conn: &Connection,
    max_total_bytes: u64,
    now: Timestamp,
) -> Result<usize, StoreError> {
    let total_bytes: i64 = conn.query_row(
        "SELECT COALESCE(SUM(LENGTH(CAST(it.body AS BLOB))), 0)
         FROM item_text it
         JOIN items i ON i.id = it.item_id
         WHERE it.durable = 0 AND i.body_state = 'cached';",
        [],
        |r| r.get(0),
    )?;

    let mut current_bytes = total_bytes.max(0) as u64;
    if current_bytes <= max_total_bytes {
        return Ok(0);
    }

    let mut stmt = conn.prepare(
        "SELECT i.id,
                LENGTH(CAST(it.body AS BLOB)) AS body_bytes,
                m.raw_blob
         FROM items i
         JOIN item_text it ON it.item_id = i.id
         LEFT JOIN mail_items m ON m.item_id = i.id
         WHERE it.durable = 0
           AND i.body_state = 'cached'
           AND i.starred = 0
           AND NOT EXISTS (
               SELECT 1 FROM pending_ops po
               WHERE po.item_id = i.id
                 AND po.state IN ('pending', 'in_flight')
           )
         ORDER BY i.updated_at ASC, i.id ASC;",
    )?;

    let candidates: Vec<(i64, u64, Option<String>)> = stmt
        .query_map([], |r| {
            let id: i64 = r.get(0)?;
            let len: i64 = r.get(1)?;
            let raw_blob: Option<String> = r.get(2)?;
            Ok((id, len.max(0) as u64, raw_blob))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    let mut evicted_count = 0usize;
    for (item_id, body_bytes, raw_blob) in candidates {
        if current_bytes <= max_total_bytes {
            break;
        }

        conn.execute(
            "UPDATE item_text SET body = '' WHERE item_id = ?1 AND durable = 0;",
            params![item_id],
        )?;
        conn.execute(
            "UPDATE items SET body_state = 'none', excerpt = NULL, updated_at = ?1 WHERE id = ?2;",
            params![now, item_id],
        )?;

        if let Some(hash) = raw_blob {
            conn.execute(
                "UPDATE mail_items SET raw_blob = NULL WHERE item_id = ?1;",
                params![item_id],
            )?;
            decrement_blob_ref(conn, &hash, now)?;
        }

        current_bytes = current_bytes.saturating_sub(body_bytes);
        evicted_count += 1;
    }

    if evicted_count > 0 {
        bump_revision(conn)?;
    }

    Ok(evicted_count)
}

/// Evicts oldest cached attachment blob references (`attachments.blob_hash = NULL`)
/// until the total bytes of referenced attachment blobs are `<= max_total_bytes`
/// (`DATA-MODEL.md` §9.3).
///
/// Never evicts attachments belonging to starred items or items with active `pending_ops`.
pub fn evict_lru_attachment_blobs(
    conn: &Connection,
    max_total_bytes: u64,
    now: Timestamp,
) -> Result<usize, StoreError> {
    let total_bytes: i64 = conn.query_row(
        "SELECT COALESCE(SUM(b.size_bytes), 0)
         FROM attachments a
         JOIN blobs b ON b.hash = a.blob_hash
         WHERE a.blob_hash IS NOT NULL;",
        [],
        |r| r.get(0),
    )?;

    let mut current_bytes = total_bytes.max(0) as u64;
    if current_bytes <= max_total_bytes {
        return Ok(0);
    }

    let mut stmt = conn.prepare(
        "SELECT a.id, a.blob_hash, b.size_bytes
         FROM attachments a
         JOIN items i ON i.id = a.item_id
         JOIN blobs b ON b.hash = a.blob_hash
         WHERE a.blob_hash IS NOT NULL
           AND i.starred = 0
           AND NOT EXISTS (
               SELECT 1 FROM pending_ops po
               WHERE po.item_id = i.id
                 AND po.state IN ('pending', 'in_flight')
           )
         ORDER BY b.last_used_at ASC, a.id ASC;",
    )?;

    let candidates: Vec<(i64, String, u64)> = stmt
        .query_map([], |r| {
            let att_id: i64 = r.get(0)?;
            let hash: String = r.get(1)?;
            let sz: i64 = r.get(2)?;
            Ok((att_id, hash, sz.max(0) as u64))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    let mut evicted_count = 0usize;
    for (att_id, hash, sz) in candidates {
        if current_bytes <= max_total_bytes {
            break;
        }
        let still_set: Option<String> = conn
            .query_row(
                "SELECT blob_hash FROM attachments WHERE id = ?1;",
                params![att_id],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        if still_set.as_deref() == Some(hash.as_str()) {
            conn.execute(
                "UPDATE attachments SET blob_hash = NULL WHERE id = ?1;",
                params![att_id],
            )?;
            decrement_blob_ref(conn, &hash, now)?;
            current_bytes = current_bytes.saturating_sub(sz);
            evicted_count += 1;
        }
    }

    if evicted_count > 0 {
        bump_revision(conn)?;
    }

    Ok(evicted_count)
}
