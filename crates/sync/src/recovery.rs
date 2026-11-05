//! Startup crash recovery and 7-day terminal `pending_ops` retention purge
//! (`SYNC.md` §8.6, §11.1; `DATA-MODEL.md` §11.6).

use debloatmail_core::Timestamp;
use debloatmail_store::bump_revision;
use rusqlite::{Connection, params};

use crate::SyncError;

/// Terminal `pending_ops` (`state IN ('done', 'failed')`) are retained for 7 days
/// before housekeeping purges them (`SYNC.md` §8.6, `DATA-MODEL.md` §11.6).
pub const TERMINAL_OP_RETENTION_SECS: i64 = 7 * 86_400;

/// Summary of actions taken during [`run_startup_recovery`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StartupRecoveryReport {
    /// Count of `items` rows whose `body_state = 'pending'` was reset to `'none'`.
    pub reset_pending_bodies: usize,
    /// Count of `folders` rows whose future `last_sync_at` was clamped to `now`.
    pub clamped_future_folder_syncs: usize,
    /// Count of `in_flight` `send` ops marked with `needs_confirmation = true` so their first
    /// post-crash retry searches the Sent folder by `Message-ID` before resending (`SYNC.md` §8.4, §11.1).
    pub marked_in_flight_sends_for_confirmation: usize,
    /// Count of `in_flight` non-send ops retained in `in_flight` with `attempts` unchanged (`SYNC.md` §11.1).
    pub in_flight_non_send_ops_retained: usize,
    /// Count of terminal (`done` / `failed`) ops older than 7 days that were purged.
    pub purged_terminal_ops: usize,
}

/// Runs the startup crash-recovery sweep defined in `SYNC.md` §11.1 and §8.6:
/// 1. Resets `items.body_state = 'pending'` to `'none'`.
/// 2. Keeps `pending_ops.state = 'in_flight'` as `'in_flight'` with `attempts` unchanged;
///    for `op = 'send'`, sets `"needs_confirmation": true` in the JSON payload so the drain
///    runs the §8.4 `Message-ID` confirmation path first instead of resending blindly.
/// 3. Clamps any `folders.last_sync_at > now` back to `now`.
/// 4. Purges terminal `pending_ops` (`done` / `failed`) finished more than 7 days ago.
pub fn run_startup_recovery(
    conn: &Connection,
    now: Timestamp,
) -> Result<StartupRecoveryReport, SyncError> {
    let reset_pending_bodies = conn.execute(
        "UPDATE items SET body_state = 'none', updated_at = ?1 WHERE body_state = 'pending';",
        params![now],
    )?;

    let clamped_future_folder_syncs = conn.execute(
        "UPDATE folders SET last_sync_at = ?1 WHERE last_sync_at > ?1;",
        params![now],
    )?;

    let in_flight_non_send: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pending_ops WHERE state = 'in_flight' AND op != 'send';",
        [],
        |r| r.get(0),
    )?;

    let mut stmt = conn.prepare(
        "SELECT id, payload FROM pending_ops WHERE state = 'in_flight' AND op = 'send' ORDER BY id;",
    )?;
    let in_flight_sends: Vec<(i64, String)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;

    let mut marked_sends = 0usize;
    for (op_id, payload_str) in in_flight_sends {
        let mut val: serde_json::Value = serde_json::from_str(&payload_str)
            .unwrap_or_else(|_| serde_json::json!({}));
        if let Some(obj) = val.as_object_mut() {
            obj.insert(
                "needs_confirmation".to_string(),
                serde_json::Value::Bool(true),
            );
            let updated_payload = serde_json::to_string(&val)?;
            conn.execute(
                "UPDATE pending_ops SET payload = ?1 WHERE id = ?2;",
                params![updated_payload, op_id],
            )?;
            marked_sends += 1;
        }
    }

    let purged_terminal_ops = purge_expired_terminal_ops(conn, now)?;

    if reset_pending_bodies > 0
        || clamped_future_folder_syncs > 0
        || marked_sends > 0
        || purged_terminal_ops > 0
    {
        bump_revision(conn)?;
    }

    Ok(StartupRecoveryReport {
        reset_pending_bodies,
        clamped_future_folder_syncs,
        marked_in_flight_sends_for_confirmation: marked_sends,
        in_flight_non_send_ops_retained: in_flight_non_send.max(0) as usize,
        purged_terminal_ops,
    })
}

/// Purges terminal `pending_ops` (`state IN ('done', 'failed')`) whose `finished_at` timestamp
/// is at least [`TERMINAL_OP_RETENTION_SECS`] (7 days) older than `now` (`SYNC.md` §8.6).
pub fn purge_expired_terminal_ops(conn: &Connection, now: Timestamp) -> Result<usize, SyncError> {
    let cutoff = now.saturating_sub(TERMINAL_OP_RETENTION_SECS);
    let deleted = conn.execute(
        "DELETE FROM pending_ops
         WHERE state IN ('done', 'failed')
           AND finished_at IS NOT NULL
           AND finished_at <= ?1;",
        params![cutoff],
    )?;
    if deleted > 0 {
        bump_revision(conn)?;
    }
    Ok(deleted)
}
