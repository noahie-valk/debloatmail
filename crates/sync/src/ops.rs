//! Per-account `pending_ops` enqueue helpers and drain loop (`SYNC.md` §8.1–8.6,
//! `DATA-MODEL.md` §11.5–11.7).
//!
//! Enforces:
//! - Oldest-first (`created_at ASC, id ASC`) per-account execution.
//! - `state = 'in_flight'` persisted before any network I/O command (`SYNC.md` §8.2, §11.3).
//! - Connection-side failures leave `attempts` unchanged (`SYNC.md` §8.2, §9).
//! - Server-side rejections increment `attempts` and transition to `state = 'failed'` at `attempts >= 10`.
//! - Never issues plain `EXPUNGE` (`UID EXPUNGE` only when `UIDPLUS` is negotiated; `SYNC.md` §8.3).
//! - `send` op duplicate-delivery protection (`UID SEARCH HEADER Message-ID <id>` in `\Sent` after
//!   a `NoVerdict` attempt or mid-flight crash) and single-op `\Sent` `APPEND` (`SYNC.md` §8.4–8.5).

use std::num::NonZeroU32;
use std::str::FromStr;

use debloatmail_core::{FolderRole, OpId, OpKind, OpState};
use debloatmail_engine::io::AsyncByteStream;
use debloatmail_engine::smtp::build_rfc5322_message;
use debloatmail_engine::{
    Clock, EngineError, FailureClass, ImapConnection, OutgoingEmail, Rng, SendOutcome,
    SmtpConnection, classify_imap_error, send_email, uid_expunge_messages,
};
use io_imap::client::ImapClientAsync;
use io_imap::rfc3501::copy::{ImapMessageCopy, ImapMessageCopyOptions};
use io_imap::rfc3501::search::{ImapMessageSearch, ImapMessageSearchOptions};
use io_imap::rfc3501::store::{ImapMessageStoreOptions, ImapMessageStoreSilent};
use io_imap::rfc4315::appenduid::{ImapAppendUid, ImapAppendUidOptions};
use io_imap::rfc6851::r#move::{ImapMessageMove, ImapMessageMoveOptions};
use io_imap::types::core::{AString, Atom, Literal, Vec1};
use io_imap::types::extensions::binary::LiteralOrLiteral8;
use io_imap::types::flag::{Flag, StoreType};
use io_imap::types::search::SearchKey;
use io_imap::types::sequence::{SeqOrUid, Sequence, SequenceSet};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::SyncError;
use crate::id::mint_uuid_v7_from_clock;
use crate::mail::{parse_imap_mailbox, select_folder};
use crate::scheduler::SyncEvent;

/// Maximum server-side attempts before a `pending_ops` row transitions to `failed` (`SYNC.md` §8.2, §8.6).
pub const MAX_OP_SERVER_ATTEMPTS: u32 = 10;

fn default_true() -> bool {
    true
}

/// JSON payload for `op = 'flag'` (`SYNC.md` §8.3).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlagOpPayload {
    #[serde(default)]
    pub add_flags: Vec<String>,
    #[serde(default)]
    pub remove_flags: Vec<String>,
}

/// JSON payload for `op = 'move'` (`SYNC.md` §8.3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MoveOpPayload {
    /// Target folder `public_id`, role token (`"trash"`, `"archive"`), or raw IMAP mailbox name.
    pub target_folder: String,
}

/// JSON payload for `op = 'delete'` (`SYNC.md` §8.3).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeleteOpPayload {
    /// Whether to request immediate `UID EXPUNGE` when `UIDPLUS` is supported.
    #[serde(default)]
    pub expunge: bool,
}

/// JSON payload for `op = 'expunge'` (`SYNC.md` §8.3).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExpungeOpPayload {
    /// Optional explicit UIDs to expunge via `UID EXPUNGE`. If empty, uses the target item's UID.
    #[serde(default)]
    pub uids: Vec<u32>,
}

/// Self-sufficient JSON payload for `op = 'send'` (`DATA-MODEL.md` §11.5–11.6, `SYNC.md` §8.4–8.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SendOpPayload {
    pub from: String,
    pub to: Vec<String>,
    #[serde(default)]
    pub cc: Vec<String>,
    #[serde(default)]
    pub bcc: Vec<String>,
    pub subject: String,
    pub body: String,
    /// Stable RFC 5322 `Message-ID` minted once at queue time (with or without `<...>`).
    pub message_id: String,
    #[serde(default)]
    pub in_reply_to: Option<String>,
    #[serde(default)]
    pub references: Option<String>,
    #[serde(default = "default_true")]
    pub append_to_sent: bool,
    #[serde(default)]
    pub sent_folder: Option<String>,
    /// True when the op was queued with a reference to an original `item_id` (e.g. a reply).
    #[serde(default)]
    pub had_reply_item: bool,
    /// True after a mid-`DATA` `NoVerdict` or interrupted `in_flight` attempt (`SYNC.md` §8.4).
    #[serde(default)]
    pub needs_confirmation: bool,
}

/// Summary of draining `pending_ops` for an account.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DrainOpsReport {
    pub completed: usize,
    pub failed_permanently: usize,
    pub deferred_retry: usize,
    pub connection_aborted: bool,
    pub events: Vec<SyncEvent>,
}

/// Reads a `drafts` row, queues a self-sufficient `send` op in `pending_ops` with a stable
/// generated `Message-ID`, and deletes the `drafts` row in the same transaction
/// (`DATA-MODEL.md` §11.5, `SYNC.md` §8.1, §8.4).
pub fn queue_send_from_draft(
    conn: &mut Connection,
    draft_id: i64,
    reply_item_id: Option<i64>,
    append_to_sent: bool,
    clock: &dyn Clock,
    rng: &mut dyn Rng,
) -> Result<OpId, SyncError> {
    let tx = conn.transaction()?;

    #[derive(Debug)]
    struct DraftRecord {
        account_id: i64,
        from_email: String,
        to_json: String,
        cc_json: Option<String>,
        bcc_json: Option<String>,
        subject: String,
        body: String,
        in_reply_to: Option<String>,
        references_hdr: Option<String>,
    }

    let draft: DraftRecord = tx
        .query_row(
            "SELECT d.account_id,
                    COALESCE(a.email, a.username, 'user@localhost'),
                    d.to_addrs,
                    d.cc_addrs,
                    d.bcc_addrs,
                    d.subject,
                    d.body,
                    d.in_reply_to,
                    d.references_hdr
             FROM drafts d
             JOIN accounts a ON a.id = d.account_id
             WHERE d.id = ?1",
            params![draft_id],
            |row| {
                Ok(DraftRecord {
                    account_id: row.get(0)?,
                    from_email: row.get(1)?,
                    to_json: row.get(2)?,
                    cc_json: row.get(3)?,
                    bcc_json: row.get(4)?,
                    subject: row.get(5)?,
                    body: row.get(6)?,
                    in_reply_to: row.get(7)?,
                    references_hdr: row.get(8)?,
                })
            },
        )
        .optional()?
        .ok_or_else(|| SyncError::InvalidState(format!("draft {draft_id} not found")))?;

    let to: Vec<String> = serde_json::from_str(&draft.to_json).unwrap_or_default();
    let cc: Vec<String> = draft
        .cc_json
        .as_deref()
        .map(|s| serde_json::from_str(s).unwrap_or_default())
        .unwrap_or_default();
    let bcc: Vec<String> = draft
        .bcc_json
        .as_deref()
        .map(|s| serde_json::from_str(s).unwrap_or_default())
        .unwrap_or_default();

    let domain = draft
        .from_email
        .split_once('@')
        .map(|(_, d)| d.trim_matches('>').trim())
        .filter(|d| !d.is_empty())
        .unwrap_or("localhost");

    let msg_uuid = mint_uuid_v7_from_clock(clock, rng);
    let message_id = format!("<{msg_uuid}@{domain}>");

    let payload = SendOpPayload {
        from: draft.from_email,
        to,
        cc,
        bcc,
        subject: draft.subject,
        body: draft.body,
        message_id,
        in_reply_to: draft.in_reply_to,
        references: draft.references_hdr,
        append_to_sent,
        sent_folder: None,
        had_reply_item: reply_item_id.is_some(),
        needs_confirmation: false,
    };

    let payload_json = serde_json::to_string(&payload)?;
    let op_public_id = mint_uuid_v7_from_clock(clock, rng);
    let now = clock.now_secs();

    tx.execute(
        "INSERT INTO pending_ops (
            public_id, account_id, op, item_id, payload, state, attempts, last_error, created_at
         ) VALUES (?1, ?2, 'send', ?3, ?4, 'pending', 0, NULL, ?5)",
        params![
            op_public_id.as_str(),
            draft.account_id,
            reply_item_id,
            payload_json,
            now,
        ],
    )?;

    tx.execute("DELETE FROM drafts WHERE id = ?1", params![draft_id])?;
    tx.commit()?;

    Ok(OpId(op_public_id))
}

/// Drains all `pending` and `in_flight` ops for `account_id` in `created_at ASC, id ASC` order
/// (`SYNC.md` §8.1–8.6).
///
/// `smtp_conn` is optional so pure-IMAP op queues (`flag`, `move`, `delete`, `expunge`) can drain
/// without opening an SMTP session until a `send` op is encountered.
pub async fn drain_account_ops<S1, S2>(
    conn: &mut Connection,
    imap_conn: &mut ImapConnection<S1>,
    mut smtp_conn: Option<&mut SmtpConnection<S2>>,
    account_id: i64,
    clock: &dyn Clock,
) -> Result<DrainOpsReport, SyncError>
where
    S1: AsyncByteStream,
    S2: AsyncByteStream,
{
    #[derive(Debug)]
    struct QueuedOpRow {
        id: i64,
        public_id: String,
        op_str: String,
        item_id: Option<i64>,
        payload: String,
        state_str: String,
        attempts: u32,
    }

    let rows: Vec<QueuedOpRow> = {
        let mut stmt = conn.prepare(
            "SELECT id, public_id, op, item_id, payload, state, attempts
             FROM pending_ops
             WHERE account_id = ?1
               AND state IN ('pending', 'in_flight')
             ORDER BY created_at ASC, id ASC",
        )?;
        let mapped = stmt.query_map(params![account_id], |row| {
            Ok(QueuedOpRow {
                id: row.get(0)?,
                public_id: row.get(1)?,
                op_str: row.get(2)?,
                item_id: row.get(3)?,
                payload: row.get(4)?,
                state_str: row.get(5)?,
                attempts: row.get::<_, i64>(6)?.max(0) as u32,
            })
        })?;
        mapped.collect::<Result<Vec<_>, _>>()?
    };

    let mut report = DrainOpsReport::default();

    for row in rows {
        let op_id = OpId::from_str(&row.public_id)?;
        let op_kind = OpKind::from_str(&row.op_str)?;
        let initial_state = OpState::from_str(&row.state_str)?;

        // Step 1: Persist state = 'in_flight' BEFORE any network I/O (SYNC.md §8.2, §11.3)
        if initial_state != OpState::InFlight {
            conn.execute(
                "UPDATE pending_ops SET state = 'in_flight' WHERE id = ?1",
                params![row.id],
            )?;
        }

        // Step 2: Check orphaned item_id IS NULL (DATA-MODEL.md §11.7)
        if op_kind != OpKind::Send && row.item_id.is_none() {
            let reason = "target item no longer exists".to_string();
            let now = clock.now_secs();
            conn.execute(
                "UPDATE pending_ops
                 SET state = 'failed', last_error = ?1, finished_at = ?2
                 WHERE id = ?3",
                params![reason, now, row.id],
            )?;
            report.failed_permanently += 1;
            report.events.push(SyncEvent::OpFinished {
                op_id,
                success: false,
                error: Some(reason),
            });
            continue;
        }

        // Step 3: Execute the operation
        let outcome = match op_kind {
            OpKind::Flag => {
                execute_flag_op(conn, imap_conn, row.item_id, &row.payload).await
            }
            OpKind::Move => {
                execute_move_op(conn, imap_conn, account_id, row.item_id, &row.payload).await
            }
            OpKind::Delete => {
                execute_delete_op(conn, imap_conn, row.item_id, &row.payload).await
            }
            OpKind::Expunge => {
                execute_expunge_op(conn, imap_conn, row.item_id, &row.payload).await
            }
            OpKind::Send => {
                execute_send_op(
                    conn,
                    imap_conn,
                    smtp_conn.as_deref_mut(),
                    account_id,
                    row.id,
                    row.item_id,
                    &row.payload,
                    initial_state,
                )
                .await
            }
        };

        let now = clock.now_secs();
        match outcome {
            OpExecutionResult::Done { warning } => {
                conn.execute(
                    "UPDATE pending_ops
                     SET state = 'done', last_error = ?1, finished_at = ?2
                     WHERE id = ?3",
                    params![warning, now, row.id],
                )?;
                report.completed += 1;
                report.events.push(SyncEvent::OpFinished {
                    op_id,
                    success: true,
                    error: warning,
                });
            }
            OpExecutionResult::ServerError { message } => {
                let new_attempts = row.attempts.saturating_add(1);
                if new_attempts >= MAX_OP_SERVER_ATTEMPTS {
                    conn.execute(
                        "UPDATE pending_ops
                         SET state = 'failed',
                             attempts = ?1,
                             last_error = ?2,
                             finished_at = ?3
                         WHERE id = ?4",
                        params![i64::from(new_attempts), message, now, row.id],
                    )?;
                    report.failed_permanently += 1;
                    report.events.push(SyncEvent::OpFinished {
                        op_id,
                        success: false,
                        error: Some(message),
                    });
                } else {
                    conn.execute(
                        "UPDATE pending_ops
                         SET state = 'in_flight',
                             attempts = ?1,
                             last_error = ?2
                         WHERE id = ?3",
                        params![i64::from(new_attempts), message, row.id],
                    )?;
                    report.deferred_retry += 1;
                }
            }
            OpExecutionResult::ConnectionError { message } => {
                // Connection-side failure: attempts UNCHANGED, stay in_flight (SYNC.md §8.2, §9)
                conn.execute(
                    "UPDATE pending_ops
                     SET state = 'in_flight',
                         last_error = ?1
                     WHERE id = ?2",
                    params![message, row.id],
                )?;
                report.deferred_retry += 1;
                report.connection_aborted = true;
                break;
            }
            OpExecutionResult::NoVerdict { message } => {
                // Mid-DATA drop: attempts UNCHANGED, needs_confirmation persisted (SYNC.md §8.4)
                conn.execute(
                    "UPDATE pending_ops
                     SET state = 'in_flight',
                         last_error = ?1
                     WHERE id = ?2",
                    params![message, row.id],
                )?;
                report.deferred_retry += 1;
                report.connection_aborted = true;
                break;
            }
        }
    }

    Ok(report)
}

#[derive(Debug)]
enum OpExecutionResult {
    Done { warning: Option<String> },
    ServerError { message: String },
    ConnectionError { message: String },
    NoVerdict { message: String },
}

impl From<EngineError> for OpExecutionResult {
    fn from(err: EngineError) -> Self {
        match classify_imap_error(&err) {
            FailureClass::TransientNetwork | FailureClass::AuthFailure => {
                OpExecutionResult::ConnectionError {
                    message: err.to_string(),
                }
            }
            _ => OpExecutionResult::ServerError {
                message: err.to_string(),
            },
        }
    }
}

struct MailItemLocation {
    folder_id: i64,
    folder_name: String,
    uid: NonZeroU32,
}

fn load_item_location(
    conn: &Connection,
    item_id: Option<i64>,
) -> Result<Option<MailItemLocation>, SyncError> {
    let Some(id) = item_id else {
        return Ok(None);
    };
    let row: Option<(i64, String, u32)> = conn
        .query_row(
            "SELECT m.folder_id, f.name, m.uid
             FROM mail_items m
             JOIN folders f ON f.id = m.folder_id
             WHERE m.item_id = ?1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)?.max(0) as u32)),
        )
        .optional()?;

    let Some((folder_id, folder_name, uid_raw)) = row else {
        return Ok(None);
    };
    let Some(uid) = NonZeroU32::new(uid_raw) else {
        return Ok(None);
    };
    Ok(Some(MailItemLocation {
        folder_id,
        folder_name,
        uid,
    }))
}

fn parse_imap_flags(names: &[String]) -> Vec<Flag<'static>> {
    let mut out = Vec::new();
    for raw in names {
        let trimmed = raw.trim();
        match trimmed.to_ascii_lowercase().as_str() {
            "\\seen" | "seen" => out.push(Flag::Seen),
            "\\flagged" | "flagged" | "starred" => out.push(Flag::Flagged),
            "\\answered" | "answered" => out.push(Flag::Answered),
            "\\deleted" | "deleted" => out.push(Flag::Deleted),
            "\\draft" | "draft" => out.push(Flag::Draft),
            _ => {
                let clean = trimmed.trim_start_matches('\\');
                if let Ok(atom) = Atom::try_from(clean.to_string()) {
                    out.push(Flag::Keyword(atom));
                }
            }
        }
    }
    out
}

async fn execute_flag_op<S: AsyncByteStream>(
    conn: &mut Connection,
    imap_conn: &mut ImapConnection<S>,
    item_id: Option<i64>,
    payload_json: &str,
) -> OpExecutionResult {
    let payload: FlagOpPayload = match serde_json::from_str(payload_json) {
        Ok(p) => p,
        Err(e) => {
            return OpExecutionResult::ServerError {
                message: format!("invalid flag op payload: {e}"),
            };
        }
    };

    let loc = match load_item_location(conn, item_id) {
        Ok(Some(l)) => l,
        Ok(None) => {
            return OpExecutionResult::ServerError {
                message: "target item no longer exists".to_string(),
            };
        }
        Err(e) => {
            return OpExecutionResult::ServerError {
                message: e.to_string(),
            };
        }
    };

    if let Err(e) = select_folder(imap_conn, &loc.folder_name).await {
        return classify_sync_err(e);
    }

    let seq = SequenceSet(Vec1::from(Sequence::Single(SeqOrUid::Value(loc.uid))));
    let add_flags = parse_imap_flags(&payload.add_flags);
    if !add_flags.is_empty() {
        let res = imap_conn
            .run(ImapMessageStoreSilent::new(
                seq.clone(),
                StoreType::Add,
                add_flags,
                ImapMessageStoreOptions { uid: true },
            ))
            .await
            .map_err(EngineError::from);
        if let Err(err) = res {
            return OpExecutionResult::from(err);
        }
    }

    let remove_flags = parse_imap_flags(&payload.remove_flags);
    if !remove_flags.is_empty() {
        let res = imap_conn
            .run(ImapMessageStoreSilent::new(
                seq,
                StoreType::Remove,
                remove_flags,
                ImapMessageStoreOptions { uid: true },
            ))
            .await
            .map_err(EngineError::from);
        if let Err(err) = res {
            return OpExecutionResult::from(err);
        }
    }

    OpExecutionResult::Done { warning: None }
}

fn resolve_target_folder_name(
    conn: &Connection,
    account_id: i64,
    target_spec: &str,
) -> Result<(i64, String), SyncError> {
    // Match by public_id, role, or name within the account
    let row: Option<(i64, String)> = conn
        .query_row(
            "SELECT id, name FROM folders
             WHERE account_id = ?1
               AND (public_id = ?2 OR LOWER(role) = LOWER(?2) OR name = ?2)
             ORDER BY CASE
                 WHEN public_id = ?2 THEN 0
                 WHEN LOWER(role) = LOWER(?2) THEN 1
                 ELSE 2
             END
             LIMIT 1",
            params![account_id, target_spec],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;

    row.ok_or_else(|| {
        SyncError::InvalidState(format!(
            "target folder '{target_spec}' not found on account {account_id}"
        ))
    })
}

async fn execute_move_op<S: AsyncByteStream>(
    conn: &mut Connection,
    imap_conn: &mut ImapConnection<S>,
    account_id: i64,
    item_id: Option<i64>,
    payload_json: &str,
) -> OpExecutionResult {
    let payload: MoveOpPayload = match serde_json::from_str(payload_json) {
        Ok(p) => p,
        Err(e) => {
            return OpExecutionResult::ServerError {
                message: format!("invalid move op payload: {e}"),
            };
        }
    };

    let loc = match load_item_location(conn, item_id) {
        Ok(Some(l)) => l,
        Ok(None) => {
            return OpExecutionResult::ServerError {
                message: "target item no longer exists".to_string(),
            };
        }
        Err(e) => {
            return OpExecutionResult::ServerError {
                message: e.to_string(),
            };
        }
    };

    let (target_folder_id, target_folder_name) =
        match resolve_target_folder_name(conn, account_id, &payload.target_folder) {
            Ok(pair) => pair,
            Err(e) => {
                return OpExecutionResult::ServerError {
                    message: e.to_string(),
                };
            }
        };

    if loc.folder_id == target_folder_id {
        return OpExecutionResult::Done { warning: None };
    }

    let target_mailbox = match parse_imap_mailbox(&target_folder_name) {
        Ok(m) => m,
        Err(e) => {
            return OpExecutionResult::ServerError {
                message: e.to_string(),
            };
        }
    };

    if let Err(e) = select_folder(imap_conn, &loc.folder_name).await {
        return classify_sync_err(e);
    }

    let seq = SequenceSet(Vec1::from(Sequence::Single(SeqOrUid::Value(loc.uid))));

    if imap_conn.capabilities.move_ext {
        let res = imap_conn
            .run(ImapMessageMove::new(
                seq,
                target_mailbox,
                ImapMessageMoveOptions { uid: true },
            ))
            .await
            .map_err(EngineError::from);

        if let Err(err) = res
            && !is_missing_uid_error(&err)
        {
            return OpExecutionResult::from(err);
        }
    } else {
        // Fallback: UID COPY + UID STORE +FLAGS.SILENT (\Deleted) + optional UID EXPUNGE (SYNC.md §8.3)
        let copy_res = imap_conn
            .run(ImapMessageCopy::new(
                seq.clone(),
                target_mailbox,
                ImapMessageCopyOptions { uid: true },
            ))
            .await
            .map_err(EngineError::from);

        if let Err(err) = copy_res {
            if !is_missing_uid_error(&err) {
                return OpExecutionResult::from(err);
            }
        } else {
            let store_res = imap_conn
                .run(ImapMessageStoreSilent::new(
                    seq.clone(),
                    StoreType::Add,
                    vec![Flag::Deleted],
                    ImapMessageStoreOptions { uid: true },
                ))
                .await
                .map_err(EngineError::from);
            if let Err(err) = store_res
                && !is_missing_uid_error(&err)
            {
                return OpExecutionResult::from(err);
            }

            if imap_conn.capabilities.uidplus
                && let Err(err) = uid_expunge_messages(imap_conn, seq).await
                && !is_missing_uid_error(&err)
            {
                return OpExecutionResult::from(err);
            }
        }
    }

    OpExecutionResult::Done { warning: None }
}

async fn execute_delete_op<S: AsyncByteStream>(
    conn: &mut Connection,
    imap_conn: &mut ImapConnection<S>,
    item_id: Option<i64>,
    payload_json: &str,
) -> OpExecutionResult {
    let payload: DeleteOpPayload = serde_json::from_str(payload_json).unwrap_or_default();

    let loc = match load_item_location(conn, item_id) {
        Ok(Some(l)) => l,
        Ok(None) => {
            return OpExecutionResult::ServerError {
                message: "target item no longer exists".to_string(),
            };
        }
        Err(e) => {
            return OpExecutionResult::ServerError {
                message: e.to_string(),
            };
        }
    };

    if let Err(e) = select_folder(imap_conn, &loc.folder_name).await {
        return classify_sync_err(e);
    }

    let seq = SequenceSet(Vec1::from(Sequence::Single(SeqOrUid::Value(loc.uid))));
    let store_res = imap_conn
        .run(ImapMessageStoreSilent::new(
            seq.clone(),
            StoreType::Add,
            vec![Flag::Deleted],
            ImapMessageStoreOptions { uid: true },
        ))
        .await
        .map_err(EngineError::from);

    if let Err(err) = store_res
        && !is_missing_uid_error(&err)
    {
        return OpExecutionResult::from(err);
    }

    let mut warning = None;
    if payload.expunge {
        if imap_conn.capabilities.uidplus {
            if let Err(err) = uid_expunge_messages(imap_conn, seq).await
                && !is_missing_uid_error(&err)
            {
                return OpExecutionResult::from(err);
            }
        } else {
            // SYNC.md §8.3: Never issue plain EXPUNGE when UIDPLUS is unavailable
            warning = Some(
                "server lacks UIDPLUS; message marked \\Deleted without plain EXPUNGE".to_string(),
            );
        }
    }

    OpExecutionResult::Done { warning }
}

async fn execute_expunge_op<S: AsyncByteStream>(
    conn: &mut Connection,
    imap_conn: &mut ImapConnection<S>,
    item_id: Option<i64>,
    payload_json: &str,
) -> OpExecutionResult {
    let payload: ExpungeOpPayload = serde_json::from_str(payload_json).unwrap_or_default();

    let loc = match load_item_location(conn, item_id) {
        Ok(Some(l)) => l,
        Ok(None) => {
            return OpExecutionResult::ServerError {
                message: "target item no longer exists".to_string(),
            };
        }
        Err(e) => {
            return OpExecutionResult::ServerError {
                message: e.to_string(),
            };
        }
    };

    if !imap_conn.capabilities.uidplus {
        return OpExecutionResult::ServerError {
            message: EngineError::UnsupportedCapability("UIDPLUS").to_string(),
        };
    }

    if let Err(e) = select_folder(imap_conn, &loc.folder_name).await {
        return classify_sync_err(e);
    }

    let mut uids_nz: Vec<Sequence> = Vec::new();
    if payload.uids.is_empty() {
        uids_nz.push(Sequence::Single(SeqOrUid::Value(loc.uid)));
    } else {
        for u in &payload.uids {
            if let Some(nz) = NonZeroU32::new(*u) {
                uids_nz.push(Sequence::Single(SeqOrUid::Value(nz)));
            }
        }
    }

    let Ok(vec1) = Vec1::try_from(uids_nz) else {
        return OpExecutionResult::Done { warning: None };
    };

    match uid_expunge_messages(imap_conn, SequenceSet(vec1)).await {
        Ok(_) => OpExecutionResult::Done { warning: None },
        Err(err) if is_missing_uid_error(&err) => OpExecutionResult::Done { warning: None },
        Err(err) => OpExecutionResult::from(err),
    }
}

#[allow(clippy::too_many_arguments)]
async fn execute_send_op<S1, S2>(
    conn: &mut Connection,
    imap_conn: &mut ImapConnection<S1>,
    smtp_conn: Option<&mut SmtpConnection<S2>>,
    account_id: i64,
    op_row_id: i64,
    item_id: Option<i64>,
    payload_json: &str,
    initial_state: OpState,
) -> OpExecutionResult
where
    S1: AsyncByteStream,
    S2: AsyncByteStream,
{
    let mut payload: SendOpPayload = match serde_json::from_str(payload_json) {
        Ok(p) => p,
        Err(e) => {
            return OpExecutionResult::ServerError {
                message: format!("invalid send op payload: {e}"),
            };
        }
    };

    let sent_folder_name = find_sent_folder_name(conn, account_id, payload.sent_folder.as_deref());

    // SYNC.md §8.4 Confirmation Path:
    // If the previous attempt ended in NoVerdict (or crashed while in_flight), search the Sent
    // folder for payload.message_id before resending.
    if (payload.needs_confirmation || initial_state == OpState::InFlight)
        && let Some(ref sent_name) = sent_folder_name
    {
        match check_message_id_in_sent_folder(imap_conn, sent_name, &payload.message_id).await {
            Ok(true) => {
                let mut note =
                    "confirmed via Sent folder Message-ID search after interrupted send".to_string();
                if payload.had_reply_item && item_id.is_none() {
                    note.push_str("; replied-to item no longer exists locally");
                }
                return OpExecutionResult::Done {
                    warning: Some(note),
                };
            }
            Ok(false) => {
                // Not found in Sent -> safe to proceed with fresh SMTP submission under the same Message-ID
            }
            Err(err) => {
                return classify_sync_err(err);
            }
        }
    }

    let Some(smtp) = smtp_conn else {
        return OpExecutionResult::ConnectionError {
            message: "SMTP connection not available for send op".to_string(),
        };
    };

    let email = OutgoingEmail {
        from: payload.from.clone(),
        to: payload.to.clone(),
        cc: payload.cc.clone(),
        bcc: payload.bcc.clone(),
        subject: payload.subject.clone(),
        message_id: payload.message_id.clone(),
        in_reply_to: payload.in_reply_to.clone(),
        references: payload.references.clone(),
        body: payload.body.clone(),
    };

    let send_outcome = match send_email(smtp, &email).await {
        Ok(o) => o,
        Err(err) => return OpExecutionResult::from(err),
    };

    match send_outcome {
        SendOutcome::Sent => {
            let mut warnings = Vec::new();
            if payload.had_reply_item && item_id.is_none() {
                warnings.push(
                    "replied-to item no longer exists locally; \\Answered flag not set".to_string(),
                );
            }

            // SYNC.md §8.5: Append to Sent folder if requested.
            // Success is defined by the SMTP send: if APPEND fails, the op is still Done with a warning!
            if payload.append_to_sent {
                if let Some(ref sent_name) = sent_folder_name {
                    if let Err(append_err) =
                        append_sent_copy(imap_conn, sent_name, &email).await
                    {
                        warnings.push(format!(
                            "message sent via SMTP, but IMAP APPEND to '{sent_name}' failed: {append_err}"
                        ));
                    }
                } else {
                    warnings.push(
                        "message sent via SMTP, but no Sent folder exists for IMAP APPEND"
                            .to_string(),
                    );
                }
            }

            let warning = if warnings.is_empty() {
                None
            } else {
                Some(warnings.join("; "))
            };
            OpExecutionResult::Done { warning }
        }
        SendOutcome::Rejected { message } => OpExecutionResult::ServerError { message },
        SendOutcome::ConnectionFailed { message } => OpExecutionResult::ConnectionError { message },
        SendOutcome::NoVerdict { message } => {
            // Mark needs_confirmation = true in payload so the next connection runs UID SEARCH HEADER Message-ID
            payload.needs_confirmation = true;
            if let Ok(updated_json) = serde_json::to_string(&payload) {
                let _ = conn.execute(
                    "UPDATE pending_ops SET payload = ?1 WHERE id = ?2",
                    params![updated_json, op_row_id],
                );
            }
            OpExecutionResult::NoVerdict { message }
        }
    }
}

fn find_sent_folder_name(
    conn: &Connection,
    account_id: i64,
    explicit_sent: Option<&str>,
) -> Option<String> {
    if let Some(spec) = explicit_sent
        && !spec.is_empty()
        && let Ok((_, name)) = resolve_target_folder_name(conn, account_id, spec)
    {
        return Some(name);
    }

    conn.query_row(
        "SELECT name FROM folders
         WHERE account_id = ?1 AND role = ?2
         LIMIT 1",
        params![account_id, FolderRole::Sent.as_str()],
        |r| r.get(0),
    )
    .optional()
    .ok()
    .flatten()
}

async fn check_message_id_in_sent_folder<S: AsyncByteStream>(
    imap_conn: &mut ImapConnection<S>,
    sent_folder_name: &str,
    message_id: &str,
) -> Result<bool, SyncError> {
    select_folder(imap_conn, sent_folder_name).await?;

    let header_key = AString::try_from("Message-ID").map_err(|e| {
        SyncError::InvalidState(format!("invalid Message-ID header key: {e}"))
    })?;
    let formatted_id = if message_id.starts_with('<') && message_id.ends_with('>') {
        message_id.to_string()
    } else {
        format!("<{}>", message_id.trim_matches(['<', '>']))
    };
    let header_val = AString::try_from(formatted_id).map_err(|e| {
        SyncError::InvalidState(format!("invalid Message-ID search value: {e}"))
    })?;

    let criteria = Vec1::from(SearchKey::Header(header_key, header_val));
    let matches = imap_conn
        .run(ImapMessageSearch::new(
            criteria,
            ImapMessageSearchOptions { uid: true },
        ))
        .await
        .map_err(EngineError::from)?;

    Ok(!matches.is_empty())
}

async fn append_sent_copy<S: AsyncByteStream>(
    imap_conn: &mut ImapConnection<S>,
    sent_folder_name: &str,
    email: &OutgoingEmail,
) -> Result<Option<(NonZeroU32, NonZeroU32)>, SyncError> {
    let mailbox = parse_imap_mailbox(sent_folder_name)?;
    let raw_bytes = build_rfc5322_message(email)?;
    let literal = Literal::try_from(raw_bytes).map_err(|e| {
        SyncError::InvalidState(format!("failed to construct IMAP literal for APPEND: {e}"))
    })?;
    let opts = ImapAppendUidOptions {
        flags: vec![Flag::Seen],
        date: None,
    };
    let append_uid = imap_conn
        .run(ImapAppendUid::new(
            mailbox,
            LiteralOrLiteral8::Literal(literal),
            opts,
        ))
        .await
        .map_err(EngineError::from)?;
    Ok(append_uid)
}

fn is_missing_uid_error(err: &EngineError) -> bool {
    let upper = err.to_string().to_ascii_uppercase();
    upper.contains("EXPUNGED") || upper.contains("NO SUCH MESSAGE") || upper.contains("NONEXISTENT")
}

fn classify_sync_err(err: SyncError) -> OpExecutionResult {
    match err {
        SyncError::Engine(eng) => OpExecutionResult::from(eng),
        other => OpExecutionResult::ServerError {
            message: other.to_string(),
        },
    }
}
