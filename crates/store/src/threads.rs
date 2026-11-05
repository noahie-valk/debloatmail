//! Local per-account conversation threading (`DATA-MODEL.md` §4.4, §5, §8; `SYNC.md` §3.1).
//!
//! Groups mail items within a single account using JWZ reference graph resolution
//! (`message_id`, `in_reply_to`, `references_hdr`) and falls back to non-empty `subject_norm`
//! within the same account when reference headers are missing.
//! Recomputes `threads.(last_activity, item_count, unread_count)` strictly over
//! `visible_items` (`hidden_at IS NULL`).

use std::collections::{BTreeMap, HashMap};

use rusqlite::{Connection, OptionalExtension, params};

use crate::StoreError;
use crate::schema::bump_revision;

#[derive(Debug, Clone)]
struct MailThreadRow {
    item_id: i64,
    title: String,
    sort_at: i64,
    unread: bool,
    hidden: bool,
    existing_thread_id: Option<i64>,
    message_id: Option<String>,
    in_reply_to: Option<String>,
    references_hdr: Option<String>,
    subject_norm: Option<String>,
}

/// Disjoint-set union-find over string keys (`Message-ID` tokens and synthetic item keys).
struct DisjointSet {
    parent: Vec<usize>,
    key_to_idx: HashMap<String, usize>,
    idx_to_key: Vec<String>,
}

impl DisjointSet {
    fn new() -> Self {
        Self {
            parent: Vec::new(),
            key_to_idx: HashMap::new(),
            idx_to_key: Vec::new(),
        }
    }

    fn intern(&mut self, key: &str) -> usize {
        if let Some(&idx) = self.key_to_idx.get(key) {
            return idx;
        }
        let idx = self.parent.len();
        self.parent.push(idx);
        self.key_to_idx.insert(key.to_string(), idx);
        self.idx_to_key.push(key.to_string());
        idx
    }

    fn find(&mut self, mut i: usize) -> usize {
        while self.parent[i] != i {
            let p = self.parent[i];
            self.parent[i] = self.parent[p];
            i = p;
        }
        i
    }

    /// Unions `child` into `root_pref`, keeping `root_pref`'s representative as the root.
    fn union_prefer_first(&mut self, root_pref: usize, child: usize) {
        let r1 = self.find(root_pref);
        let r2 = self.find(child);
        if r1 != r2 {
            self.parent[r2] = r1;
        }
    }
}

/// Rebuilds all conversation threads for `account_id` (`DATA-MODEL.md` §8).
///
/// - Feed items always keep `thread_id = NULL`.
/// - Mail items are grouped by reference chain (`References` + `In-Reply-To` + `Message-ID`),
///   then merged by matching non-empty `subject_norm` within the same account when references
///   do not connect them.
/// - `last_activity`, `item_count`, and `unread_count` are computed strictly over
///   **visible** items (`hidden_at IS NULL`, `DATA-MODEL.md` §4.4, §8).
/// - Returns the number of active threads in the account after rebuild.
pub fn rebuild_account_threads<F>(
    conn: &Connection,
    account_id: i64,
    mut mint_public_id: F,
) -> Result<usize, StoreError>
where
    F: FnMut() -> String,
{
    // Ensure feed items in this account never hold a thread_id (`DATA-MODEL.md` §8).
    conn.execute(
        "UPDATE items SET thread_id = NULL WHERE account_id = ?1 AND kind = 'feed' AND thread_id IS NOT NULL;",
        params![account_id],
    )?;

    let mut stmt = conn.prepare(
        "SELECT i.id,
                COALESCE(i.title, '(no subject)'),
                i.sort_at,
                i.unread,
                i.hidden_at IS NOT NULL AS is_hidden,
                i.thread_id,
                m.message_id,
                m.in_reply_to,
                m.references_hdr,
                m.subject_norm
         FROM items i
         JOIN mail_items m ON m.item_id = i.id
         WHERE i.account_id = ?1 AND i.kind = 'mail'
         ORDER BY i.sort_at ASC, i.id ASC;",
    )?;

    let rows: Vec<MailThreadRow> = stmt
        .query_map(params![account_id], |r| {
            Ok(MailThreadRow {
                item_id: r.get(0)?,
                title: r.get(1)?,
                sort_at: r.get(2)?,
                unread: r.get::<_, i64>(3)? != 0,
                hidden: r.get::<_, bool>(4)?,
                existing_thread_id: r.get(5)?,
                message_id: r.get::<_, Option<String>>(6)?.filter(|s| !s.trim().is_empty()),
                in_reply_to: r.get::<_, Option<String>>(7)?.filter(|s| !s.trim().is_empty()),
                references_hdr: r.get::<_, Option<String>>(8)?.filter(|s| !s.trim().is_empty()),
                subject_norm: r.get::<_, Option<String>>(9)?.filter(|s| !s.trim().is_empty()),
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    if rows.is_empty() {
        conn.execute(
            "DELETE FROM threads WHERE account_id = ?1;",
            params![account_id],
        )?;
        bump_revision(conn)?;
        return Ok(0);
    }

    let mut dsu = DisjointSet::new();
    let mut item_node_indices = Vec::with_capacity(rows.len());

    // Step 1: Link each item's Message-ID, References chain, and In-Reply-To in chronological order.
    for row in &rows {
        let own_key = row
            .message_id
            .clone()
            .unwrap_or_else(|| format!("__item:{}", row.item_id));
        let own_idx = dsu.intern(&own_key);

        let mut chain: Vec<usize> = Vec::new();
        if let Some(refs) = &row.references_hdr {
            for token in refs.split_whitespace() {
                let cleaned = token.trim().trim_matches(['<', '>']).trim();
                if !cleaned.is_empty() {
                    chain.push(dsu.intern(cleaned));
                }
            }
        }
        if let Some(irt) = &row.in_reply_to {
            let cleaned = irt.trim().trim_matches(['<', '>']).trim();
            if !cleaned.is_empty() {
                let irt_idx = dsu.intern(cleaned);
                if !chain.contains(&irt_idx) {
                    chain.push(irt_idx);
                }
            }
        }
        if !chain.contains(&own_idx) {
            chain.push(own_idx);
        }

        if let Some(&first_idx) = chain.first() {
            for &other_idx in chain.iter().skip(1) {
                dsu.union_prefer_first(first_idx, other_idx);
            }
        }

        item_node_indices.push(own_idx);
    }

    // Step 2: Subject fallback grouping (`DATA-MODEL.md` §8):
    // When an item has no references (`in_reply_to IS NULL` and `references_hdr IS NULL`)
    // or when two clusters share a non-empty `subject_norm`, merge them under the earliest cluster root.
    let mut subject_to_root: HashMap<String, usize> = HashMap::new();
    for (idx, row) in rows.iter().enumerate() {
        if let Some(subj) = &row.subject_norm {
            let node_idx = item_node_indices[idx];
            let current_root = dsu.find(node_idx);
            if let Some(&existing_root) = subject_to_root.get(subj) {
                // Only fallback-merge by subject if this message lacked explicit references,
                // or if it is already in the same reference component.
                let has_explicit_refs = row.in_reply_to.is_some() || row.references_hdr.is_some();
                if !has_explicit_refs {
                    dsu.union_prefer_first(existing_root, current_root);
                }
            } else {
                subject_to_root.insert(subj.clone(), current_root);
            }
        }
    }

    // Group items by their resolved DSU root index in deterministic order.
    let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for (row_idx, &node_idx) in item_node_indices.iter().enumerate() {
        let root_idx = dsu.find(node_idx);
        groups.entry(root_idx).or_default().push(row_idx);
    }

    let mut kept_thread_ids: Vec<i64> = Vec::new();

    for (root_idx, member_indices) in groups {
        let root_key = &dsu.idx_to_key[root_idx];
        let root_msgid: Option<String> = if root_key.starts_with("__item:") {
            rows[member_indices[0]].message_id.clone()
        } else {
            Some(root_key.clone())
        };

        let first_row = &rows[member_indices[0]];
        let subject_norm = member_indices
            .iter()
            .find_map(|&i| rows[i].subject_norm.clone());
        let display_subject = first_row.title.clone();

        // Compute stats over visible items only (`hidden_at IS NULL`, `DATA-MODEL.md` §4.4, §8).
        let mut visible_count = 0i64;
        let mut unread_count = 0i64;
        let mut last_activity: Option<i64> = None;

        for &i in &member_indices {
            let r = &rows[i];
            if !r.hidden {
                visible_count += 1;
                if r.unread {
                    unread_count += 1;
                }
                last_activity = Some(match last_activity {
                    Some(prev) => prev.max(r.sort_at),
                    None => r.sort_at,
                });
            }
        }

        // If all items in this thread are hidden, fall back to max(sort_at) for the NOT NULL column
        // while keeping `item_count = 0` and `unread_count = 0`.
        let effective_last_activity = last_activity.unwrap_or_else(|| {
            member_indices
                .iter()
                .map(|&i| rows[i].sort_at)
                .max()
                .unwrap_or(0)
        });

        // Look up an existing thread row by (account_id, root_msgid) or by existing_thread_id.
        let existing_thread_id: Option<i64> = if let Some(ref msgid) = root_msgid {
            conn.query_row(
                "SELECT id FROM threads WHERE account_id = ?1 AND root_msgid = ?2;",
                params![account_id, msgid],
                |r| r.get(0),
            )
            .optional()?
        } else {
            None
        }
        .or_else(|| {
            member_indices
                .iter()
                .find_map(|&i| rows[i].existing_thread_id)
                .filter(|tid| !kept_thread_ids.contains(tid))
        });

        let thread_id = if let Some(tid) = existing_thread_id {
            conn.execute(
                "UPDATE threads SET
                    root_msgid = COALESCE(?1, root_msgid),
                    subject_norm = ?2,
                    display_subject = ?3,
                    last_activity = ?4,
                    item_count = ?5,
                    unread_count = ?6
                 WHERE id = ?7;",
                params![
                    root_msgid,
                    subject_norm,
                    display_subject,
                    effective_last_activity,
                    visible_count,
                    unread_count,
                    tid,
                ],
            )?;
            tid
        } else {
            let pub_id = mint_public_id();
            conn.execute(
                "INSERT INTO threads (
                    public_id, account_id, root_msgid, subject_norm, display_subject,
                    last_activity, item_count, unread_count
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8);",
                params![
                    pub_id,
                    account_id,
                    root_msgid,
                    subject_norm,
                    display_subject,
                    effective_last_activity,
                    visible_count,
                    unread_count,
                ],
            )?;
            conn.last_insert_rowid()
        };

        kept_thread_ids.push(thread_id);

        for &i in &member_indices {
            let item_id = rows[i].item_id;
            conn.execute(
                "UPDATE items SET thread_id = ?1 WHERE id = ?2;",
                params![thread_id, item_id],
            )?;
        }
    }

    // Remove stale threads in this account that no longer have any items.
    conn.execute(
        "DELETE FROM threads
         WHERE account_id = ?1
           AND NOT EXISTS (SELECT 1 FROM items i WHERE i.thread_id = threads.id);",
        params![account_id],
    )?;

    bump_revision(conn)?;
    Ok(kept_thread_ids.len())
}
