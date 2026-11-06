//! On-demand `BODY.PEEK[]` message body extraction and `BODY.PEEK[<section_id>]`
//! attachment section fetching (`SYNC.md` §6.1–6.2).
//!
//! # Protocol Safety Invariant
//! All body and section fetches delegate to `fetch_body_peek` and `fetch_attachment_section`,
//! which always set `peek: true` (`BODY.PEEK[...]`) so fetching never marks a message `\Seen`.

use std::num::NonZeroU32;
use std::path::{Path, PathBuf};

use debloatmail_core::BodyState;
use debloatmail_engine::io::AsyncByteStream;
use debloatmail_engine::{
    Clock, ImapConnection, Rng, fetch_attachment_section, fetch_body_peek, validate_section_id,
};
use debloatmail_extract::{CURRENT_EXTRACTOR_VERSION, Limits, extract_message};
use debloatmail_store::{
    NewAttachment, blob_path, decrement_blob_ref, increment_blob_ref, put_blob,
    rebuild_account_threads, reconcile_extracted_attachments, touch_blob,
};
use rusqlite::{Connection, OptionalExtension, params};

use crate::SyncError;
use crate::id::mint_uuid_v7_from_clock;
use crate::mail::select_folder;

/// Result of fetching and extracting an email's body via `BODY.PEEK[]` (`SYNC.md` §6.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchedBodyOutcome {
    pub item_id: i64,
    pub raw_blob_hash: String,
    pub body: String,
    pub excerpt: Option<String>,
    pub body_state: BodyState,
    pub attachment_count: usize,
}

/// Result of fetching (or serving from cache) an attachment part via `BODY.PEEK[<section_id>]` (`SYNC.md` §6.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchedAttachmentOutcome {
    pub item_id: i64,
    pub section_id: String,
    pub blob_hash: String,
    pub path: PathBuf,
    pub from_cache: bool,
}

/// Fetches the raw message via `UID FETCH <uid> (BODY.PEEK[])`, stores the raw MIME blob,
/// runs `debloatmail-extract`, populates `item_text`, refines `attachments`, and updates
/// `items.body_state` (`SYNC.md` §6.1).
pub async fn fetch_item_body<S: AsyncByteStream>(
    conn: &mut Connection,
    imap_conn: &mut ImapConnection<S>,
    cache_dir: &Path,
    item_id: i64,
    clock: &dyn Clock,
    rng: &mut dyn Rng,
) -> Result<FetchedBodyOutcome, SyncError> {
    #[derive(Debug)]
    struct ItemMailTarget {
        account_id: i64,
        folder_name: String,
        uid: u32,
        existing_raw_blob: Option<String>,
        title: String,
        author: Option<String>,
    }

    let target: ItemMailTarget = conn
        .query_row(
            "SELECT i.account_id, f.name, m.uid, m.raw_blob, i.title, i.author
             FROM items i
             JOIN mail_items m ON m.item_id = i.id
             JOIN folders f ON f.id = m.folder_id
             WHERE i.id = ?1",
            params![item_id],
            |row| {
                Ok(ItemMailTarget {
                    account_id: row.get(0)?,
                    folder_name: row.get(1)?,
                    uid: row.get::<_, i64>(2)?.max(0) as u32,
                    existing_raw_blob: row.get(3)?,
                    title: row.get(4)?,
                    author: row.get(5)?,
                })
            },
        )
        .optional()?
        .ok_or_else(|| SyncError::InvalidState(format!("mail item {item_id} not found")))?;

    let uid_nz = NonZeroU32::new(target.uid)
        .ok_or_else(|| SyncError::InvalidState(format!("mail item {item_id} has invalid UID 0")))?;

    // Transition body_state -> 'pending' before network I/O (SYNC.md §6.1)
    conn.execute(
        "UPDATE items SET body_state = 'pending', updated_at = ?1 WHERE id = ?2",
        params![clock.now_secs(), item_id],
    )?;

    let raw_bytes = match async {
        select_folder(imap_conn, &target.folder_name).await?;
        Ok::<Vec<u8>, SyncError>(fetch_body_peek(imap_conn, uid_nz).await?)
    }
    .await
    {
        Ok(bytes) => bytes,
        Err(err) => {
            let _ = conn.execute(
                "UPDATE items SET body_state = 'failed', updated_at = ?1 WHERE id = ?2",
                params![clock.now_secs(), item_id],
            );
            return Err(err);
        }
    };

    let extracted = match extract_message(&raw_bytes, &Limits::default()) {
        Ok(ext) => ext,
        Err(err) => {
            let _ = conn.execute(
                "UPDATE items SET body_state = 'failed', updated_at = ?1 WHERE id = ?2",
                params![clock.now_secs(), item_id],
            );
            return Err(SyncError::Extract(err));
        }
    };

    let authoritative_attachments: Vec<NewAttachment<'_>> = extracted
        .attachments
        .iter()
        .map(|att| NewAttachment {
            section_id: &att.section_id,
            filename: att.filename.as_deref(),
            mime_type: &att.mime_type,
            size_bytes: att.size_bytes,
            content_id: att.content_id.as_deref(),
            is_pdf: att.is_pdf,
        })
        .collect();

    let now = clock.now_secs();
    let references_hdr = if extracted.references.is_empty() {
        None
    } else {
        Some(extracted.references.join(" "))
    };

    let tx = conn.transaction()?;

    let raw_blob_hash = put_blob(&tx, cache_dir, &raw_bytes, now)?;

    // Update raw_blob reference counting
    if target.existing_raw_blob.as_deref() != Some(raw_blob_hash.as_str()) {
        increment_blob_ref(&tx, &raw_blob_hash, now)?;
        if let Some(old_hash) = &target.existing_raw_blob {
            decrement_blob_ref(&tx, old_hash, now)?;
        }
    } else {
        let _ = touch_blob(&tx, cache_dir, &raw_blob_hash, now)?;
    }

    let text_title = if extracted.title != "(no subject)" {
        &extracted.title
    } else {
        &target.title
    };
    let text_author = extracted.author.as_deref().or(target.author.as_deref());

    tx.execute(
        "INSERT INTO item_text (item_id, title, author, body, durable, extractor_version)
         VALUES (?1, ?2, ?3, ?4, 0, ?5)
         ON CONFLICT(item_id) DO UPDATE SET
             title = excluded.title,
             author = excluded.author,
             body = excluded.body,
             extractor_version = excluded.extractor_version",
        params![
            item_id,
            text_title,
            text_author.unwrap_or(""),
            extracted.body,
            i64::from(CURRENT_EXTRACTOR_VERSION),
        ],
    )?;

    tx.execute(
        "UPDATE mail_items
         SET raw_blob = ?1,
             in_reply_to = COALESCE(?2, in_reply_to),
             references_hdr = COALESCE(?3, references_hdr)
         WHERE item_id = ?4",
        params![
            raw_blob_hash,
            extracted.in_reply_to,
            references_hdr,
            item_id,
        ],
    )?;

    reconcile_extracted_attachments(&tx, item_id, &authoritative_attachments, now)?;

    tx.execute(
        "UPDATE items
         SET excerpt = ?1,
             body_state = ?2,
             has_attachments = ?3,
             updated_at = ?4
         WHERE id = ?5",
        params![
            extracted.excerpt,
            extracted.body_state.as_str(),
            i64::from(!authoritative_attachments.is_empty()),
            now,
            item_id,
        ],
    )?;

    if references_hdr.is_some() || extracted.in_reply_to.is_some() {
        rebuild_account_threads(&tx, target.account_id, || {
            mint_uuid_v7_from_clock(clock, rng).to_string()
        })?;
    }

    tx.commit()?;

    Ok(FetchedBodyOutcome {
        item_id,
        raw_blob_hash,
        body: extracted.body,
        excerpt: extracted.excerpt,
        body_state: extracted.body_state,
        attachment_count: authoritative_attachments.len(),
    })
}

/// Fetches a single attachment section via `UID FETCH <uid> (BODY.PEEK[<section_id>])`
/// or serves it from the local `BlobStore` if already cached (`SYNC.md` §6.2).
///
/// Always refreshes `blobs.last_used_at` on both cache hits and newly fetched sections.
pub async fn fetch_item_attachment<S: AsyncByteStream>(
    conn: &mut Connection,
    imap_conn: &mut ImapConnection<S>,
    cache_dir: &Path,
    item_id: i64,
    section_id: &str,
    clock: &dyn Clock,
) -> Result<FetchedAttachmentOutcome, SyncError> {
    // 1. Validate section_id format before touching DB or wire (SYNC.md §6.2)
    validate_section_id(section_id)?;

    #[derive(Debug)]
    struct AttachmentTarget {
        attachment_id: i64,
        blob_hash: Option<String>,
        is_pdf: bool,
        folder_name: String,
        uid: u32,
    }

    let target: AttachmentTarget = conn
        .query_row(
            "SELECT a.id, a.blob_hash, a.is_pdf, f.name, m.uid
             FROM attachments a
             JOIN mail_items m ON m.item_id = a.item_id
             JOIN folders f ON f.id = m.folder_id
             WHERE a.item_id = ?1 AND a.section_id = ?2",
            params![item_id, section_id],
            |row| {
                Ok(AttachmentTarget {
                    attachment_id: row.get(0)?,
                    blob_hash: row.get(1)?,
                    is_pdf: row.get::<_, i64>(2)? != 0,
                    folder_name: row.get(3)?,
                    uid: row.get::<_, i64>(4)?.max(0) as u32,
                })
            },
        )
        .optional()?
        .ok_or_else(|| {
            SyncError::InvalidState(format!(
                "attachment section '{section_id}' not found on item {item_id}"
            ))
        })?;

    let now = clock.now_secs();

    // 2. Check if already cached on disk
    if let Some(existing_hash) = &target.blob_hash
        && let Ok(path) = blob_path(cache_dir, existing_hash)
        && path.is_file()
    {
        let _ = touch_blob(conn, cache_dir, existing_hash, now)?;
        return Ok(FetchedAttachmentOutcome {
            item_id,
            section_id: section_id.to_string(),
            blob_hash: existing_hash.clone(),
            path,
            from_cache: true,
        });
    }

    // 3. Fetch section via BODY.PEEK[<section_id>]
    let uid_nz = NonZeroU32::new(target.uid)
        .ok_or_else(|| SyncError::InvalidState(format!("mail item {item_id} has invalid UID 0")))?;

    select_folder(imap_conn, &target.folder_name).await?;
    let bytes = fetch_attachment_section(imap_conn, uid_nz, section_id).await?;

    let is_pdf = target.is_pdf || bytes.starts_with(b"%PDF-");

    let tx = conn.transaction()?;
    let new_hash = put_blob(&tx, cache_dir, &bytes, now)?;
    let path = blob_path(cache_dir, &new_hash)?;

    if target.blob_hash.as_deref() != Some(new_hash.as_str()) {
        increment_blob_ref(&tx, &new_hash, now)?;
        if let Some(old_hash) = &target.blob_hash {
            decrement_blob_ref(&tx, old_hash, now)?;
        }
    } else {
        let _ = touch_blob(&tx, cache_dir, &new_hash, now)?;
    }

    tx.execute(
        "UPDATE attachments
         SET blob_hash = ?1,
             size_bytes = ?2,
             is_pdf = ?3
         WHERE id = ?4",
        params![
            new_hash,
            bytes.len() as i64,
            i64::from(is_pdf),
            target.attachment_id,
        ],
    )?;
    tx.commit()?;

    Ok(FetchedAttachmentOutcome {
        item_id,
        section_id: section_id.to_string(),
        blob_hash: new_hash,
        path,
        from_cache: false,
    })
}
