//! IMAP folder reconciliation, `SELECT` & `UIDVALIDITY` 7-step staged recovery,
//! windowed header sync (`2,000` UIDs/window), `CONDSTORE` / fallback flag sync,
//! and `UID SEARCH ALL` expunge reconciliation (`SYNC.md` §5.3–5.8, §11.3).

use std::collections::{HashMap, HashSet};
use std::num::{NonZeroU32, NonZeroU64};

use debloatmail_core::{FolderRole, PublicId, RoleSource, Timestamp};
use debloatmail_engine::io::AsyncByteStream;
use debloatmail_engine::{
    Clock, DiscoveredFolder, HEADER_FETCH_WINDOW_SIZE, ImapConnection, Rng, discover_folders,
    fetch_header_window,
};
use debloatmail_extract::header::{clean_msg_id, normalize_subject};
use debloatmail_extract::structure::derive_title;
use debloatmail_store::{
    NewAttachment, UpsertMailHeader, decrement_blob_ref, has_pending_flag_op,
    rebuild_account_threads, replace_provisional_attachments, set_meta, upsert_mail_header,
};
use io_imap::client::ImapClientAsync;
use io_imap::rfc3501::fetch::{ImapMessageFetch, ImapMessageFetchOptions};
use io_imap::rfc3501::list::ImapMailboxList;
use io_imap::rfc3501::search::{ImapMessageSearch, ImapMessageSearchOptions};
use io_imap::rfc3501::select::{
    ImapMailboxSelect, ImapMailboxSelectData, ImapMailboxSelectOptions,
};
use io_imap::types::body::{BodyStructure, SpecificFields};
use io_imap::types::command::FetchModifier;
use io_imap::types::core::{NString, Vec1};
use io_imap::types::envelope::{Address, Envelope};
use io_imap::types::fetch::{
    MacroOrMessageDataItemNames, MessageDataItem, MessageDataItemName,
};
use io_imap::types::flag::{Flag, FlagFetch};
use io_imap::types::mailbox::Mailbox;
use io_imap::types::search::SearchKey;
use io_imap::types::sequence::{SeqOrUid, Sequence, SequenceSet};
use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::SyncError;
use crate::id::mint_uuid_v7_from_clock;
use crate::scheduler::CancelToken;

/// Maximum number of recent UIDs scanned during fallback (non-`CONDSTORE`) flag sync (`SYNC.md` §5.6).
pub const FALLBACK_FLAG_SYNC_WINDOW_UIDS: u32 = 5_000;

/// Maximum number of attachments recorded from a single `BODYSTRUCTURE` (`MIME-AND-TEXT.md` §13).
const MAX_BODYSTRUCTURE_ATTACHMENTS: usize = 256;

/// Maximum MIME nesting depth walked in `BODYSTRUCTURE` (`MIME-AND-TEXT.md` §13).
const MAX_BODYSTRUCTURE_DEPTH: usize = 16;

/// Summary of `LIST` folder discovery and SQLite reconciliation (`SYNC.md` §5.3).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FolderReconcileReport {
    pub inserted: usize,
    pub updated: usize,
    pub unsubscribed_missing: usize,
}

/// Summary of a folder sync pass (`SYNC.md` §5.4–5.6).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FolderSyncReport {
    pub folder_id: i64,
    pub folder_public_id: String,
    pub uidvalidity_reset: bool,
    pub unpreserved_ids: usize,
    pub rebound_ops: usize,
    pub inserted_items: usize,
    pub updated_items: usize,
    pub expunged_items: usize,
    pub windows_committed: usize,
    pub preempted: bool,
}

/// Runs `LIST "" "*"` on `imap_conn`, maps folders via `discover_folders`, and reconciles
/// the `folders` table for `account_id` (`SYNC.md` §5.3, §5.8).
pub async fn reconcile_account_folders<S: AsyncByteStream>(
    conn: &mut Connection,
    imap_conn: &mut ImapConnection<S>,
    account_id: i64,
    clock: &dyn Clock,
    rng: &mut dyn Rng,
) -> Result<FolderReconcileReport, SyncError> {
    let listing = imap_conn.run(ImapMailboxList::new()).await?;
    let discovered = discover_folders(listing);
    reconcile_discovered_folders(conn, account_id, &discovered, clock, rng)
}

/// Reconciles an already-discovered folder list against SQLite for `account_id` (`SYNC.md` §5.3, §5.8).
///
/// Rules enforced:
/// - Existing folders with `role_source = 'manual'` keep their `role` and `role_source` untouched.
/// - At most one folder per account may hold a given `FolderRole` (manual roles take precedence).
/// - Folders absent from `discovered` are marked `subscribed = 0` without deleting the row or its items.
/// - `\Noselect` and `\All` (`!sync_by_default`) folders are stored with `subscribed = 0` by default.
pub fn reconcile_discovered_folders(
    conn: &mut Connection,
    account_id: i64,
    discovered: &[DiscoveredFolder],
    clock: &dyn Clock,
    rng: &mut dyn Rng,
) -> Result<FolderReconcileReport, SyncError> {
    let tx = conn.transaction()?;

    #[derive(Debug)]
    struct ExistingFolder {
        id: i64,
        name: String,
        role: Option<String>,
        role_source: Option<String>,
        subscribed: bool,
    }

    let existing_rows: Vec<ExistingFolder> = {
        let mut stmt = tx.prepare(
            "SELECT id, name, role, role_source, subscribed
             FROM folders
             WHERE account_id = ?1",
        )?;
        let mapped = stmt.query_map(params![account_id], |row| {
            Ok(ExistingFolder {
                id: row.get(0)?,
                name: row.get(1)?,
                role: row.get(2)?,
                role_source: row.get(3)?,
                subscribed: row.get::<_, i64>(4)? != 0,
            })
        })?;
        mapped.collect::<Result<Vec<_>, _>>()?
    };

    let mut existing_by_name: HashMap<String, ExistingFolder> = HashMap::new();
    let mut claimed_roles: HashSet<String> = HashSet::new();

    for row in existing_rows {
        if row.role_source.as_deref() == Some(RoleSource::Manual.as_str())
            && let Some(r) = &row.role
        {
            claimed_roles.insert(r.clone());
        }
        existing_by_name.insert(row.name.clone(), row);
    }

    let mut seen_names: HashSet<String> = HashSet::with_capacity(discovered.len());
    let mut report = FolderReconcileReport::default();

    for folder in discovered {
        seen_names.insert(folder.name.clone());
        let delim_str = folder.delimiter.map(|c| c.to_string());

        if let Some(existing) = existing_by_name.get(&folder.name) {
            let is_manual = existing.role_source.as_deref() == Some(RoleSource::Manual.as_str());
            let (final_role, final_source) = if is_manual {
                (existing.role.clone(), existing.role_source.clone())
            } else if let (Some(role), Some(src)) = (folder.role, folder.role_source) {
                let role_str = role.as_str().to_string();
                if claimed_roles.insert(role_str.clone()) {
                    (Some(role_str), Some(src.as_str().to_string()))
                } else {
                    (None, None)
                }
            } else {
                (None, None)
            };

            let subscribed = if !folder.selectable {
                false
            } else if folder.sync_by_default {
                true
            } else {
                existing.subscribed && is_manual
            };

            tx.execute(
                "UPDATE folders
                 SET display_name = ?1,
                     delimiter = ?2,
                     role = ?3,
                     role_source = ?4,
                     subscribed = ?5
                 WHERE id = ?6",
                params![
                    folder.display_name,
                    delim_str,
                    final_role,
                    final_source,
                    i64::from(subscribed),
                    existing.id,
                ],
            )?;
            report.updated += 1;
        } else {
            let (final_role, final_source) =
                if let (Some(role), Some(src)) = (folder.role, folder.role_source) {
                    let role_str = role.as_str().to_string();
                    if claimed_roles.insert(role_str.clone()) {
                        (Some(role_str), Some(src.as_str().to_string()))
                    } else {
                        (None, None)
                    }
                } else {
                    (None, None)
                };

            let public_id = mint_uuid_v7_from_clock(clock, rng);
            let subscribed = folder.selectable && folder.sync_by_default;

            tx.execute(
                "INSERT INTO folders (
                    public_id, account_id, name, display_name, delimiter,
                    role, role_source, uidvalidity, uidnext, highestmodseq,
                    subscribed, last_sync_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, 0, 0, ?8, NULL)",
                params![
                    public_id.as_str(),
                    account_id,
                    folder.name,
                    folder.display_name,
                    delim_str,
                    final_role,
                    final_source,
                    i64::from(subscribed),
                ],
            )?;
            report.inserted += 1;
        }
    }

    for existing in existing_by_name.values() {
        if !seen_names.contains(&existing.name) && existing.subscribed {
            tx.execute(
                "UPDATE folders SET subscribed = 0 WHERE id = ?1",
                params![existing.id],
            )?;
            report.unsubscribed_missing += 1;
        }
    }

    tx.commit()?;
    Ok(report)
}

/// Converts a folder name into an `io_imap` [`Mailbox`].
pub fn parse_imap_mailbox(name: &str) -> Result<Mailbox<'static>, SyncError> {
    if name.eq_ignore_ascii_case("INBOX") {
        Ok(Mailbox::Inbox)
    } else {
        Mailbox::try_from(name.to_string()).map_err(|e| {
            SyncError::InvalidState(format!("invalid IMAP mailbox name '{name}': {e}"))
        })
    }
}

/// Issues `SELECT <mailbox>` on `imap_conn` and returns [`ImapMailboxSelectData`].
pub async fn select_folder<S: AsyncByteStream>(
    imap_conn: &mut ImapConnection<S>,
    folder_name: &str,
) -> Result<ImapMailboxSelectData, SyncError> {
    let mailbox = parse_imap_mailbox(folder_name)?;
    let data = imap_conn
        .run(ImapMailboxSelect::new(
            mailbox,
            ImapMailboxSelectOptions::default(),
        ))
        .await
        .map_err(debloatmail_engine::EngineError::from)?;
    Ok(data)
}

/// Synchronizes a single folder (`SELECT`, `UIDVALIDITY` check, 7-step staged recovery or
/// incremental windowed header + flag + expunge sync, and thread rebuild) (`SYNC.md` §5.4–5.6).
#[allow(clippy::too_many_arguments)]
pub async fn sync_folder<S: AsyncByteStream, F>(
    conn: &mut Connection,
    imap_conn: &mut ImapConnection<S>,
    folder_id: i64,
    full_sync: bool,
    cancel_token: &CancelToken,
    mut should_preempt: F,
    clock: &dyn Clock,
    rng: &mut dyn Rng,
) -> Result<FolderSyncReport, SyncError>
where
    F: FnMut() -> bool,
{
    let folder = load_folder_state(conn, folder_id)?;

    // Skip \Noselect or \All folders unless explicitly forced via full_sync
    if !folder.subscribed && !full_sync {
        return Ok(FolderSyncReport {
            folder_id,
            folder_public_id: folder.public_id,
            ..Default::default()
        });
    }
    if folder.role.as_deref() == Some(FolderRole::All.as_str()) && !full_sync {
        return Ok(FolderSyncReport {
            folder_id,
            folder_public_id: folder.public_id,
            ..Default::default()
        });
    }

    let select_data = select_folder(imap_conn, &folder.name).await?;
    let server_uidvalidity = select_data.uid_validity.map(NonZeroU32::get).unwrap_or(1);
    let server_uidnext = select_data.uid_next.map(NonZeroU32::get).unwrap_or(1);

    let uidvalidity_mismatch =
        folder.uidvalidity != 0 && server_uidvalidity != folder.uidvalidity;
    let uidnext_regressed = folder.uidnext != 0 && server_uidnext < folder.uidnext;

    let report = if full_sync || uidvalidity_mismatch || uidnext_regressed {
        run_uidvalidity_staged_recovery(
            conn,
            imap_conn,
            &folder,
            &select_data,
            cancel_token,
            &mut should_preempt,
            clock,
            rng,
        )
        .await?
    } else {
        let first_sync = folder.uidvalidity == 0;
        if first_sync {
            conn.execute(
                "UPDATE folders SET uidvalidity = ?1 WHERE id = ?2",
                params![i64::from(server_uidvalidity), folder.id],
            )?;
        }

        let mut rep = sync_folder_headers_windowed(
            conn,
            imap_conn,
            &folder,
            server_uidvalidity,
            server_uidnext,
            & HashMap::new(),
            cancel_token,
            &mut should_preempt,
            clock,
            rng,
        )
        .await?;

        if !rep.preempted {
            let expunged = sync_folder_flags_and_expunges(
                conn,
                imap_conn,
                &folder,
                &select_data,
                first_sync,
                clock.now_secs(),
            )
            .await?;
            rep.expunged_items = expunged;
        }
        rep
    };

    {
        let tx = conn.transaction()?;
        rebuild_account_threads(&tx, folder.account_id)?;
        tx.commit()?;
    }

    Ok(report)
}

#[derive(Debug, Clone)]
pub(crate) struct FolderRowState {
    pub id: i64,
    pub public_id: String,
    pub account_id: i64,
    pub name: String,
    pub role: Option<String>,
    pub uidvalidity: u32,
    pub uidnext: u32,
    pub highestmodseq: u64,
    pub subscribed: bool,
}

pub(crate) fn load_folder_state(
    conn: &Connection,
    folder_id: i64,
) -> Result<FolderRowState, SyncError> {
    conn.query_row(
        "SELECT id, public_id, account_id, name, role, uidvalidity, uidnext, highestmodseq, subscribed
         FROM folders
         WHERE id = ?1",
        params![folder_id],
        |row| {
            Ok(FolderRowState {
                id: row.get(0)?,
                public_id: row.get(1)?,
                account_id: row.get(2)?,
                name: row.get(3)?,
                role: row.get(4)?,
                uidvalidity: row.get::<_, i64>(5)?.max(0) as u32,
                uidnext: row.get::<_, i64>(6)?.max(0) as u32,
                highestmodseq: row.get::<_, i64>(7)?.max(0) as u64,
                subscribed: row.get::<_, i64>(8)? != 0,
            })
        },
    )
    .optional()?
    .ok_or_else(|| SyncError::InvalidState(format!("folder {folder_id} not found")))
}

/// Executes the 7-step `UIDVALIDITY` staged recovery algorithm (`SYNC.md` §5.4).
///
/// 1. Detect mismatch (`server_uidvalidity != stored_uidvalidity`, `UIDNEXT` regression, or `--full`).
/// 2. Build `Message-ID -> public_id` preservation map from existing rows in `folder_id`.
/// 3. Snapshot queued `pending_ops` (`pending` / `in_flight`) referencing items in `folder_id`.
/// 4. Delete stale `items` in `folder_id` in one transaction (`pending_ops.item_id` -> `NULL`).
/// 5. Refetch headers in 2,000-UID windows, reusing `public_id`s where `Message-ID` matches.
/// 6. Rebind snapshotted `pending_ops` to the new `items.id` rows where `Message-ID` reappeared.
/// 7. Advance `folders.(uidvalidity, uidnext, highestmodseq, last_sync_at)` in the final commit.
#[allow(clippy::too_many_arguments)]
pub async fn run_uidvalidity_staged_recovery<S: AsyncByteStream, F>(
    conn: &mut Connection,
    imap_conn: &mut ImapConnection<S>,
    folder: &FolderRowState,
    select_data: &ImapMailboxSelectData,
    cancel_token: &CancelToken,
    mut should_preempt: F,
    clock: &dyn Clock,
    rng: &mut dyn Rng,
) -> Result<FolderSyncReport, SyncError>
where
    F: FnMut() -> bool,
{
    let server_uidvalidity = select_data.uid_validity.map(NonZeroU32::get).unwrap_or(1);
    let server_uidnext = select_data.uid_next.map(NonZeroU32::get).unwrap_or(1);
    let server_modseq = select_data.highest_mod_seq.unwrap_or(0);

    // Step 2 & 3 & 4: Build Message-ID preservation map, snapshot pending_ops, and delete stale items.
    let (preserved_id_map, snapshotted_ops) = {
        let tx = conn.transaction()?;

        // Step 2: Message-ID -> public_id map
        let mut msg_id_map: HashMap<String, PublicId> = HashMap::new();
        {
            let mut stmt = tx.prepare(
                "SELECT m.message_id, i.public_id
                 FROM mail_items m
                 JOIN items i ON i.id = m.item_id
                 WHERE m.folder_id = ?1
                   AND m.message_id IS NOT NULL
                   AND m.message_id != ''",
            )?;
            let rows = stmt.query_map(params![folder.id], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            for pair in rows {
                let (mid, pid_str) = pair?;
                if let Ok(pid) = PublicId::new(pid_str) {
                    msg_id_map.entry(mid).or_insert(pid);
                }
            }
        }

        // Step 3: Snapshot active pending_ops referencing items in this folder
        let mut op_snapshots: Vec<(i64, Option<String>)> = Vec::new();
        {
            let mut stmt = tx.prepare(
                "SELECT p.id, m.message_id
                 FROM pending_ops p
                 JOIN mail_items m ON m.item_id = p.item_id
                 WHERE m.folder_id = ?1
                   AND p.state IN ('pending', 'in_flight')",
            )?;
            let rows = stmt.query_map(params![folder.id], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?))
            })?;
            for row in rows {
                op_snapshots.push(row?);
            }
        }

        // Decrement blob refcounts for raw_blob and attachments of items in this folder before deleting
        decrement_folder_item_blobs(&tx, folder.id)?;

        // Step 4: Delete all items in this folder (ON DELETE SET NULL sets pending_ops.item_id = NULL)
        tx.execute(
            "DELETE FROM items
             WHERE id IN (SELECT item_id FROM mail_items WHERE folder_id = ?1)",
            params![folder.id],
        )?;

        // Reset folder watermarks for staged rebuild
        tx.execute(
            "UPDATE folders
             SET uidvalidity = ?1, uidnext = 0, highestmodseq = 0
             WHERE id = ?2",
            params![i64::from(server_uidvalidity), folder.id],
        )?;

        tx.commit()?;
        (msg_id_map, op_snapshots)
    };

    // Step 5: Refetch headers in 2,000-UID windows
    let reset_folder_state = FolderRowState {
        uidvalidity: server_uidvalidity,
        uidnext: 0,
        highestmodseq: 0,
        ..folder.clone()
    };

    let mut report = sync_folder_headers_windowed(
        conn,
        imap_conn,
        &reset_folder_state,
        server_uidvalidity,
        server_uidnext,
        &preserved_id_map,
        cancel_token,
        &mut should_preempt,
        clock,
        rng,
    )
    .await?;
    report.uidvalidity_reset = true;

    // Step 6 & 7: Rebind snapshotted pending_ops and finalize folder watermarks
    {
        let tx = conn.transaction()?;
        let mut rebound = 0usize;

        for (op_id, maybe_msg_id) in &snapshotted_ops {
            if let Some(msg_id) = maybe_msg_id
                && !msg_id.is_empty()
            {
                let new_item_id: Option<i64> = tx
                    .query_row(
                        "SELECT item_id FROM mail_items
                         WHERE folder_id = ?1 AND message_id = ?2
                         ORDER BY uid DESC
                         LIMIT 1",
                        params![folder.id, msg_id],
                        |row| row.get(0),
                    )
                    .optional()?;

                if let Some(new_id) = new_item_id {
                    tx.execute(
                        "UPDATE pending_ops SET item_id = ?1 WHERE id = ?2",
                        params![new_id, op_id],
                    )?;
                    rebound += 1;
                }
            }
        }

        if !report.preempted {
            let now = clock.now_secs();
            tx.execute(
                "UPDATE folders
                 SET uidvalidity = ?1,
                     uidnext = ?2,
                     highestmodseq = ?3,
                     last_sync_at = ?4
                 WHERE id = ?5",
                params![
                    i64::from(server_uidvalidity),
                    i64::from(server_uidnext),
                    server_modseq as i64,
                    now,
                    folder.id,
                ],
            )?;
        }

        let meta_key = format!("uidvalidity_unpreserved:{}", folder.public_id);
        set_meta(&tx, &meta_key, &report.unpreserved_ids.to_string())?;

        tx.commit()?;
        report.rebound_ops = rebound;
    }

    Ok(report)
}

/// Fetches missing headers in 2,000-UID windows, committing each window and its `folders.uidnext`
/// watermark in the same SQLite transaction (`SYNC.md` §5.5, §11.3).
///
/// Checks `cancel_token` and `should_preempt` between windows so interactive requests preempt
/// background header sync cleanly at window boundaries without losing committed progress.
#[allow(clippy::too_many_arguments)]
pub async fn sync_folder_headers_windowed<S: AsyncByteStream, F>(
    conn: &mut Connection,
    imap_conn: &mut ImapConnection<S>,
    folder: &FolderRowState,
    server_uidvalidity: u32,
    server_uidnext: u32,
    preserved_id_map: &HashMap<String, PublicId>,
    cancel_token: &CancelToken,
    mut should_preempt: F,
    clock: &dyn Clock,
    rng: &mut dyn Rng,
) -> Result<FolderSyncReport, SyncError>
where
    F: FnMut() -> bool,
{
    let mut report = FolderSyncReport {
        folder_id: folder.id,
        folder_public_id: folder.public_id.clone(),
        ..Default::default()
    };

    let start_uid = if folder.uidnext == 0 {
        1
    } else {
        folder.uidnext
    };
    let max_uid = server_uidnext.saturating_sub(1);

    if max_uid < start_uid {
        return Ok(report);
    }

    let mut current_start = start_uid;
    let mut used_preserved_ids: HashSet<String> = HashSet::new();

    while current_start <= max_uid {
        if report.windows_committed > 0 && (cancel_token.is_cancelled() || should_preempt()) {
            report.preempted = true;
            return Ok(report);
        }

        let window_end = current_start
            .saturating_add(HEADER_FETCH_WINDOW_SIZE - 1)
            .min(max_uid);

        let fetched = fetch_header_window(imap_conn, current_start, Some(window_end)).await?;
        let now = clock.now_secs();

        {
            let tx = conn.transaction()?;

            for (_seq, items) in fetched {
                if let Some(parsed) = parse_fetched_header_items(items.as_ref(), now) {
                    let chosen_public_id = if let Some(mid) = &parsed.message_id
                        && let Some(preserved) = preserved_id_map.get(mid)
                        && used_preserved_ids.insert(preserved.as_str().to_string())
                    {
                        preserved.clone()
                    } else {
                        if !preserved_id_map.is_empty() {
                            report.unpreserved_ids += 1;
                        }
                        mint_uuid_v7_from_clock(clock, rng)
                    };

                    let flags_json = serde_json::to_string(&parsed.custom_keywords)?;
                    let upsert_input = UpsertMailHeader {
                        public_id: chosen_public_id,
                        account_id: folder.account_id,
                        folder_id: folder.id,
                        uidvalidity: server_uidvalidity,
                        uid: parsed.uid,
                        message_id: parsed.message_id,
                        in_reply_to: parsed.in_reply_to,
                        references_hdr: None,
                        from_addr: parsed.from_addr,
                        to_addrs: parsed.to_addrs,
                        cc_addrs: parsed.cc_addrs,
                        reply_to: parsed.reply_to,
                        subject_norm: parsed.subject_norm,
                        title: parsed.title,
                        author: parsed.author,
                        published_at: parsed.published_at,
                        sort_at: parsed.sort_at,
                        size_bytes: parsed.size_bytes,
                        unread: parsed.unread,
                        starred: parsed.starred,
                        answered: parsed.answered,
                        has_attachments: !parsed.attachments.is_empty(),
                        flags_local_json: flags_json,
                        now,
                    };

                    let outcome = upsert_mail_header(&tx, &upsert_input)?;
                    if !parsed.attachments.is_empty() {
                        replace_provisional_attachments(
                            &tx,
                            outcome.item_id,
                            &parsed.attachments,
                        )?;
                    }

                    if outcome.inserted {
                        report.inserted_items += 1;
                    } else {
                        report.updated_items += 1;
                    }
                }
            }

            // Advance folders.uidnext in the SAME transaction as the window's items (SYNC.md §11.3)
            let next_watermark = if window_end == max_uid {
                server_uidnext
            } else {
                window_end.saturating_add(1)
            };
            tx.execute(
                "UPDATE folders
                 SET uidvalidity = ?1, uidnext = ?2
                 WHERE id = ?3",
                params![
                    i64::from(server_uidvalidity),
                    i64::from(next_watermark),
                    folder.id,
                ],
            )?;

            tx.commit()?;
        }

        report.windows_committed += 1;
        if window_end == u32::MAX {
            break;
        }
        current_start = window_end + 1;
    }

    Ok(report)
}

/// Synchronizes flags (`CONDSTORE` `CHANGEDSINCE` or fallback 5,000-UID window) and expunged
/// messages (`QRESYNC` `VANISHED (EARLIER)` or `UID SEARCH ALL` diff) (`SYNC.md` §5.6).
pub async fn sync_folder_flags_and_expunges<S: AsyncByteStream>(
    conn: &mut Connection,
    imap_conn: &mut ImapConnection<S>,
    folder: &FolderRowState,
    select_data: &ImapMailboxSelectData,
    first_sync_after_reset: bool,
    now: Timestamp,
) -> Result<usize, SyncError> {
    let server_uidnext = select_data.uid_next.map(NonZeroU32::get).unwrap_or(1);
    let server_modseq = select_data.highest_mod_seq.unwrap_or(folder.highestmodseq);

    // 1. Flag sync (CONDSTORE vs fallback window)
    if server_uidnext > 1 {
        let use_condstore = imap_conn.capabilities.condstore
            && folder.highestmodseq > 0
            && !first_sync_after_reset;

        let flag_rows = if use_condstore {
            if let Some(modseq_nz) = NonZeroU64::new(folder.highestmodseq) {
                let seq = Sequence::Range(
                    SeqOrUid::Value(NonZeroU32::MIN),
                    SeqOrUid::Asterisk,
                );
                let items = MacroOrMessageDataItemNames::MessageDataItemNames(vec![
                    MessageDataItemName::Uid,
                    MessageDataItemName::Flags,
                ]);
                let opts = ImapMessageFetchOptions {
                    uid: true,
                    modifiers: vec![FetchModifier::ChangedSince(modseq_nz)],
                };
                Some(
                    imap_conn
                        .run(ImapMessageFetch::new(
                            SequenceSet(Vec1::from(seq)),
                            items,
                            opts,
                        ))
                        .await
                        .map_err(debloatmail_engine::EngineError::from)?,
                )
            } else {
                None
            }
        } else if !first_sync_after_reset {
            let max_uid = server_uidnext.saturating_sub(1);
            let low_uid = server_uidnext
                .saturating_sub(FALLBACK_FLAG_SYNC_WINDOW_UIDS)
                .max(1);
            if let (Some(low_nz), Some(high_nz)) =
                (NonZeroU32::new(low_uid), NonZeroU32::new(max_uid))
            {
                let seq = if low_nz == high_nz {
                    Sequence::Single(SeqOrUid::Value(low_nz))
                } else {
                    Sequence::Range(SeqOrUid::Value(low_nz), SeqOrUid::Value(high_nz))
                };
                let items = MacroOrMessageDataItemNames::MessageDataItemNames(vec![
                    MessageDataItemName::Uid,
                    MessageDataItemName::Flags,
                ]);
                let opts = ImapMessageFetchOptions {
                    uid: true,
                    modifiers: Vec::new(),
                };
                Some(
                    imap_conn
                        .run(ImapMessageFetch::new(
                            SequenceSet(Vec1::from(seq)),
                            items,
                            opts,
                        ))
                        .await
                        .map_err(debloatmail_engine::EngineError::from)?,
                )
            } else {
                None
            }
        } else {
            None
        };

        if let Some(rows) = flag_rows {
            let tx = conn.transaction()?;
            for (_seq, data_items) in rows {
                if let Some((uid, unread, starred, answered, custom_keywords)) =
                    extract_uid_and_flags(data_items.as_ref())
                {
                    let existing_item_id: Option<i64> = tx
                        .query_row(
                            "SELECT item_id FROM mail_items WHERE folder_id = ?1 AND uid = ?2",
                            params![folder.id, i64::from(uid)],
                            |row| row.get(0),
                        )
                        .optional()?;

                    if let Some(item_id) = existing_item_id
                        && !has_pending_flag_op(&tx, item_id)?
                    {
                        let flags_json = serde_json::to_string(&custom_keywords)?;
                        tx.execute(
                            "UPDATE items
                             SET unread = ?1, starred = ?2, answered = ?3, updated_at = ?4
                             WHERE id = ?5",
                            params![
                                i64::from(unread),
                                i64::from(starred),
                                i64::from(answered),
                                now,
                                item_id,
                            ],
                        )?;
                        tx.execute(
                            "UPDATE mail_items SET flags_local = ?1 WHERE item_id = ?2",
                            params![flags_json, item_id],
                        )?;
                    }
                }
            }
            tx.commit()?;
        }
    }

    // 2. Expunge detection (QRESYNC vanished_earlier or UID SEARCH ALL diff)
    let mut expunged_count = 0usize;
    if !select_data.vanished_earlier.is_empty() {
        let tx = conn.transaction()?;
        for uid_nz in &select_data.vanished_earlier {
            expunged_count += delete_local_item_by_uid(&tx, folder.id, uid_nz.get())?;
        }
        tx.commit()?;
    } else if !first_sync_after_reset {
        let local_uids: Vec<u32> = {
            let mut stmt = conn.prepare(
                "SELECT uid FROM mail_items WHERE folder_id = ?1 AND uid < ?2",
            )?;
            let rows = stmt.query_map(
                params![folder.id, i64::from(server_uidnext)],
                |row| Ok(row.get::<_, i64>(0)?.max(0) as u32),
            )?;
            rows.collect::<Result<Vec<_>, _>>()?
        };

        if !local_uids.is_empty() {
            let search_uids = imap_conn
                .run(ImapMessageSearch::new(
                    Vec1::from(SearchKey::All),
                    ImapMessageSearchOptions { uid: true },
                ))
                .await
                .map_err(debloatmail_engine::EngineError::from)?;

            let server_uid_set: HashSet<u32> =
                search_uids.into_iter().map(NonZeroU32::get).collect();

            let tx = conn.transaction()?;
            for local_uid in local_uids {
                if !server_uid_set.contains(&local_uid) {
                    expunged_count += delete_local_item_by_uid(&tx, folder.id, local_uid)?;
                }
            }
            tx.commit()?;
        }
    }

    // 3. Update folder highestmodseq and last_sync_at
    conn.execute(
        "UPDATE folders
         SET highestmodseq = ?1, last_sync_at = ?2
         WHERE id = ?3",
        params![server_modseq as i64, now, folder.id],
    )?;

    Ok(expunged_count)
}

fn decrement_folder_item_blobs(tx: &Transaction<'_>, folder_id: i64) -> Result<(), SyncError> {
    let raw_blobs: Vec<String> = {
        let mut stmt = tx.prepare(
            "SELECT raw_blob FROM mail_items
             WHERE folder_id = ?1 AND raw_blob IS NOT NULL",
        )?;
        let rows = stmt.query_map(params![folder_id], |row| row.get::<_, String>(0))?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    for hash in raw_blobs {
        decrement_blob_ref(tx, &hash)?;
    }

    let att_blobs: Vec<String> = {
        let mut stmt = tx.prepare(
            "SELECT a.blob_hash
             FROM attachments a
             JOIN mail_items m ON m.item_id = a.item_id
             WHERE m.folder_id = ?1 AND a.blob_hash IS NOT NULL",
        )?;
        let rows = stmt.query_map(params![folder_id], |row| row.get::<_, String>(0))?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    for hash in att_blobs {
        decrement_blob_ref(tx, &hash)?;
    }
    Ok(())
}

fn delete_local_item_by_uid(
    tx: &Transaction<'_>,
    folder_id: i64,
    uid: u32,
) -> Result<usize, SyncError> {
    let row: Option<(i64, Option<String>)> = tx
        .query_row(
            "SELECT item_id, raw_blob FROM mail_items WHERE folder_id = ?1 AND uid = ?2",
            params![folder_id, i64::from(uid)],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;

    let Some((item_id, raw_blob)) = row else {
        return Ok(0);
    };

    if let Some(hash) = raw_blob {
        decrement_blob_ref(tx, &hash)?;
    }

    let att_blobs: Vec<String> = {
        let mut stmt = tx.prepare(
            "SELECT blob_hash FROM attachments WHERE item_id = ?1 AND blob_hash IS NOT NULL",
        )?;
        let rows = stmt.query_map(params![item_id], |r| r.get::<_, String>(0))?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    for hash in att_blobs {
        decrement_blob_ref(tx, &hash)?;
    }

    let deleted = tx.execute("DELETE FROM items WHERE id = ?1", params![item_id])?;
    Ok(deleted)
}

#[derive(Debug)]
struct ParsedHeaderMetadata {
    uid: u32,
    unread: bool,
    starred: bool,
    answered: bool,
    custom_keywords: Vec<String>,
    size_bytes: Option<u64>,
    sort_at: Timestamp,
    published_at: Option<Timestamp>,
    title: String,
    subject_norm: String,
    author: Option<String>,
    from_addr: Option<String>,
    to_addrs: Option<String>,
    cc_addrs: Option<String>,
    reply_to: Option<String>,
    message_id: Option<String>,
    in_reply_to: Option<String>,
    attachments: Vec<NewAttachment>,
}

fn extract_uid_and_flags(
    items: &[MessageDataItem<'_>],
) -> Option<(u32, bool, bool, bool, Vec<String>)> {
    let mut uid: Option<u32> = None;
    let mut seen = false;
    let mut flagged = false;
    let mut answered = false;
    let mut custom_keywords = Vec::new();
    let mut has_flags = false;

    for item in items {
        match item {
            MessageDataItem::Uid(u) => uid = Some(u.get()),
            MessageDataItem::Flags(flags) => {
                has_flags = true;
                for ff in flags {
                    if let FlagFetch::Flag(flag) = ff {
                        match flag {
                            Flag::Seen => seen = true,
                            Flag::Flagged => flagged = true,
                            Flag::Answered => answered = true,
                            Flag::Deleted | Flag::Draft => {}
                            Flag::Keyword(atom) => {
                                custom_keywords.push(atom.as_ref().to_string());
                            }
                            Flag::Extension(ext) => {
                                custom_keywords.push(format!("\\{}", ext.as_ref()));
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    let uid = uid?;
    if !has_flags {
        return None;
    }
    custom_keywords.sort();
    custom_keywords.dedup();
    Some((uid, !seen, flagged, answered, custom_keywords))
}

fn parse_fetched_header_items(
    items: &[MessageDataItem<'_>],
    fallback_now: Timestamp,
) -> Option<ParsedHeaderMetadata> {
    let mut uid: Option<u32> = None;
    let mut seen = false;
    let mut flagged = false;
    let mut answered = false;
    let mut custom_keywords = Vec::new();
    let mut internal_date: Option<Timestamp> = None;
    let mut size_bytes: Option<u64> = None;
    let mut envelope: Option<&Envelope<'_>> = None;
    let mut bodystructure: Option<&BodyStructure<'_>> = None;

    for item in items {
        match item {
            MessageDataItem::Uid(u) => uid = Some(u.get()),
            MessageDataItem::Flags(flags) => {
                for ff in flags {
                    if let FlagFetch::Flag(flag) = ff {
                        match flag {
                            Flag::Seen => seen = true,
                            Flag::Flagged => flagged = true,
                            Flag::Answered => answered = true,
                            Flag::Deleted | Flag::Draft => {}
                            Flag::Keyword(atom) => {
                                custom_keywords.push(atom.as_ref().to_string());
                            }
                            Flag::Extension(ext) => {
                                custom_keywords.push(format!("\\{}", ext.as_ref()));
                            }
                        }
                    }
                }
            }
            MessageDataItem::InternalDate(dt) => {
                internal_date = Some(dt.as_ref().timestamp());
            }
            MessageDataItem::Rfc822Size(sz) => {
                size_bytes = Some(u64::from(*sz));
            }
            MessageDataItem::Envelope(env) => {
                envelope = Some(env);
            }
            MessageDataItem::BodyStructure(bs) | MessageDataItem::Body(bs) => {
                bodystructure = Some(bs);
            }
            _ => {}
        }
    }

    let uid = uid?;
    custom_keywords.sort();
    custom_keywords.dedup();

    let (
        title,
        subject_norm,
        author,
        from_addr,
        to_addrs,
        cc_addrs,
        reply_to,
        message_id,
        in_reply_to,
        published_at,
    ) = if let Some(env) = envelope {
        parse_envelope_fields(env)
    } else {
        (
            "(no subject)".to_string(),
            String::new(),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
    };

    let sort_at = internal_date.or(published_at).unwrap_or(fallback_now);
    let attachments = bodystructure
        .map(extract_provisional_attachments_from_bodystructure)
        .unwrap_or_default();

    Some(ParsedHeaderMetadata {
        uid,
        unread: !seen,
        starred: flagged,
        answered,
        custom_keywords,
        size_bytes,
        sort_at,
        published_at,
        title,
        subject_norm,
        author,
        from_addr,
        to_addrs,
        cc_addrs,
        reply_to,
        message_id,
        in_reply_to,
        attachments,
    })
}

#[allow(clippy::type_complexity)]
fn parse_envelope_fields(
    env: &Envelope<'_>,
) -> (
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<Timestamp>,
) {
    let raw_subject = nstring_to_string(&env.subject);
    let decoded_subject = raw_subject
        .as_deref()
        .map(|s| decode_rfc2047_header_text("Subject", s));
    let title = derive_title(decoded_subject.as_deref());
    let subject_norm = normalize_subject(decoded_subject.as_deref());

    let (author, from_addr) = extract_author_and_from(&env.from);
    let to_addrs = format_address_list(&env.to);
    let cc_addrs = format_address_list(&env.cc);
    let reply_to = format_address_list(&env.reply_to);

    let message_id = nstring_to_string(&env.message_id)
        .map(|s| clean_msg_id(&s))
        .filter(|s| !s.is_empty());

    let in_reply_to = nstring_to_string(&env.in_reply_to)
        .and_then(|s| {
            s.split_whitespace()
                .next()
                .map(clean_msg_id)
                .filter(|id| !id.is_empty())
        });

    let published_at = nstring_to_string(&env.date)
        .and_then(|d| chrono::DateTime::parse_from_rfc2822(d.trim()).ok())
        .map(|dt| dt.timestamp());

    (
        title,
        subject_norm,
        author,
        from_addr,
        to_addrs,
        cc_addrs,
        reply_to,
        message_id,
        in_reply_to,
        published_at,
    )
}

fn nstring_to_string(ns: &NString<'_>) -> Option<String> {
    ns.0.as_ref()
        .map(|istr| String::from_utf8_lossy(istr.as_ref()).into_owned())
}

fn decode_rfc2047_header_text(header_name: &str, raw_val: &str) -> String {
    let clean: String = raw_val
        .chars()
        .filter(|&c| c != '\r' && c != '\n')
        .collect();
    if !clean.contains("=?") {
        return clean.trim().to_string();
    }
    // Use extract_message on a tiny synthetic header block so all RFC 2047 decoding stays in crates/extract.
    let synthetic = format!("{header_name}: {clean}\r\n\r\n");
    if header_name.eq_ignore_ascii_case("Subject")
        && let Ok(ext) =
            debloatmail_extract::extract_message(synthetic.as_bytes(), &debloatmail_extract::Limits::default())
        && ext.title != "(no subject)"
    {
        return ext.title;
    }
    clean.trim().to_string()
}

fn extract_author_and_from(addrs: &[Address<'_>]) -> (Option<String>, Option<String>) {
    for addr in addrs {
        let mailbox = nstring_to_string(&addr.mailbox)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let host = nstring_to_string(&addr.host)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let name = nstring_to_string(&addr.name)
            .map(|s| decode_rfc2047_header_text("From", &s))
            .filter(|s| !s.is_empty());

        let email = match (mailbox, host) {
            (Some(m), Some(h)) => Some(format!("{m}@{h}")),
            (Some(m), None) => Some(m),
            _ => None,
        };

        match (name, email) {
            (Some(n), Some(e)) => return (Some(n), Some(e)),
            (None, Some(e)) => return (Some(e.clone()), Some(e)),
            (Some(n), None) => return (Some(n), None),
            (None, None) => continue,
        }
    }
    (None, None)
}

fn format_address_list(addrs: &[Address<'_>]) -> Option<String> {
    let mut list = Vec::new();
    for addr in addrs {
        let mailbox = nstring_to_string(&addr.mailbox)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let host = nstring_to_string(&addr.host)
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        if let (Some(m), Some(h)) = (mailbox, host) {
            list.push(format!("{m}@{h}"));
        }
    }
    if list.is_empty() {
        None
    } else {
        Some(list.join(", "))
    }
}

/// Walks an IMAP `BODYSTRUCTURE` tree to assign RFC 3501 §6.4.5 `section_id`s and
/// seed provisional `attachments` rows (`SYNC.md` §5.5; `MIME-AND-TEXT.md` §6).
pub fn extract_provisional_attachments_from_bodystructure(
    bs: &BodyStructure<'_>,
) -> Vec<NewAttachment> {
    let mut attachments = Vec::new();
    let mut has_body_candidate = false;

    match bs {
        BodyStructure::Single { .. } => {
            walk_bodystructure_node(
                bs,
                "1",
                false,
                &mut has_body_candidate,
                &mut attachments,
                1,
            );
        }
        BodyStructure::Multi { .. } => {
            walk_bodystructure_node(
                bs,
                "",
                false,
                &mut has_body_candidate,
                &mut attachments,
                1,
            );
        }
    }

    attachments
}

fn walk_bodystructure_node(
    node: &BodyStructure<'_>,
    section_prefix: &str,
    in_alternative: bool,
    has_body_candidate: &mut bool,
    attachments: &mut Vec<NewAttachment>,
    depth: usize,
) {
    if depth > MAX_BODYSTRUCTURE_DEPTH || attachments.len() >= MAX_BODYSTRUCTURE_ATTACHMENTS {
        return;
    }

    match node {
        BodyStructure::Single {
            body,
            extension_data,
        } => {
            let (main_type, sub_type) = match &body.specific {
                SpecificFields::Basic { r#type, subtype } => (
                    String::from_utf8_lossy(r#type.as_ref()).to_ascii_lowercase(),
                    String::from_utf8_lossy(subtype.as_ref()).to_ascii_lowercase(),
                ),
                SpecificFields::Text { subtype, .. } => (
                    "text".to_string(),
                    String::from_utf8_lossy(subtype.as_ref()).to_ascii_lowercase(),
                ),
                SpecificFields::Message { .. } => ("message".to_string(), "rfc822".to_string()),
            };

            let mut disp_is_attachment = false;
            let mut filename: Option<String> = None;

            if let Some(ext) = extension_data
                && let Some(disp_wrapper) = &ext.tail
                && let Some((disp_type, disp_params)) = &disp_wrapper.disposition
            {
                let dt = String::from_utf8_lossy(disp_type.as_ref()).to_ascii_lowercase();
                if dt == "attachment" {
                    disp_is_attachment = true;
                }
                for (k, v) in disp_params {
                    let key = String::from_utf8_lossy(k.as_ref()).to_ascii_lowercase();
                    if (key == "filename" || key == "filename*") && filename.is_none() {
                        let val = String::from_utf8_lossy(v.as_ref()).trim().to_string();
                        if !val.is_empty() {
                            filename = Some(val);
                        }
                    }
                }
            }

            if filename.is_none() {
                for (k, v) in &body.basic.parameter_list {
                    let key = String::from_utf8_lossy(k.as_ref()).to_ascii_lowercase();
                    if key == "name" || key == "name*" {
                        let val = String::from_utf8_lossy(v.as_ref()).trim().to_string();
                        if !val.is_empty() {
                            filename = Some(val);
                            break;
                        }
                    }
                }
            }

            let is_text_body_type =
                main_type == "text" && (sub_type == "plain" || sub_type == "html");
            let is_attachment = if disp_is_attachment || filename.is_some() {
                true
            } else if main_type == "message"
                || (main_type == "text" && sub_type == "calendar")
            {
                true
            } else if is_text_body_type {
                if in_alternative {
                    *has_body_candidate = true;
                    false
                } else if !*has_body_candidate {
                    *has_body_candidate = true;
                    false
                } else {
                    true
                }
            } else {
                true
            };

            if is_attachment && attachments.len() < MAX_BODYSTRUCTURE_ATTACHMENTS {
                let mime_type = format!("{main_type}/{sub_type}");
                let is_pdf = mime_type == "application/pdf";
                let content_id = nstring_to_string(&body.basic.id)
                    .map(|cid| cid.trim().trim_matches(['<', '>']).to_string())
                    .filter(|s| !s.is_empty());

                let section_id = if section_prefix.is_empty() {
                    "1".to_string()
                } else {
                    section_prefix.to_string()
                };

                attachments.push(NewAttachment {
                    section_id,
                    filename,
                    mime_type,
                    size_bytes: Some(u64::from(body.basic.size)),
                    content_id,
                    is_pdf,
                });
            }
        }
        BodyStructure::Multi {
            bodies, subtype, ..
        } => {
            let sub = String::from_utf8_lossy(subtype.as_ref()).to_ascii_lowercase();
            if sub == "encrypted" {
                return;
            }
            let is_alt = sub == "alternative" || sub == "report";
            let is_signed = sub == "signed";

            for (idx, child) in bodies.as_ref().iter().enumerate() {
                if is_signed && idx > 0 {
                    // Discard detached signature part per MIME-AND-TEXT.md §6.2
                    break;
                }
                let child_section = if section_prefix.is_empty() {
                    format!("{}", idx + 1)
                } else {
                    format!("{}.{}", section_prefix, idx + 1)
                };
                walk_bodystructure_node(
                    child,
                    &child_section,
                    in_alternative || is_alt,
                    has_body_candidate,
                    attachments,
                    depth + 1,
                );
            }
        }
    }
}
