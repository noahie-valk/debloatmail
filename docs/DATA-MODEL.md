# Data Model

This document defines how debloatmail represents mail, feeds, and their **unified
stream** on disk and across the `ffi` boundary. It complements `architecture.md`:
that file explains *why* the system is shaped this way; this one specifies *what the
data looks like*.

The rules in `GEMINI.md` still apply. In particular: this schema lives behind
`crates/store`, is Qt-free, and `crates/core` knows the types but not the tables.

---

## 1. Principles

1. **One spine, two detail tables.** A single `items` table carries every field the
   unified UI needs. Mail-only and feed-only fields hang off it in `mail_items` and
   `feed_items`. This is what makes "one list view, one unread model, one search" a
   property of the schema rather than something the UI has to fake.
2. **Two identities per row.** An internal `id` (SQLite rowid) that never leaves the
   process, and a `public_id` (UUIDv7) that is stable across re-syncs and is the only
   id that crosses the ABI. Rationale in §3.
3. **Text is stored once.** Extracted plaintext lives in `item_text`, and the FTS5
   index is external-content over it. No second copy for search.
4. **Durability is asymmetric.** Feed content is often *not* re-fetchable (feeds
   rotate), so it is stored permanently. Mail bodies are always re-fetchable from
   IMAP, so they are an evictable cache. §9.
5. **The daemon writes; the UI reads.** WAL mode plus a busy timeout, not shared
   mutable state. §6.
6. **One unread model, full fidelity preserved.** `items.unread` / `items.starred`
   are the only unread state the UI ever sees. The complete IMAP flag set is kept
   alongside for round-tripping. §7.

---

## 2. Entity map

```
accounts ─┬─ folders ──────┐
          │                 │
          │            mail_items ──┐
          │                         ├── items ─┬── item_text ── item_text_fts
          └─ feeds ──────┐          │          │
                          │         │          ├── attachments ── blobs
                     feed_items ────┘          │
                                               └── threads
                                    pending_ops (account-scoped, references items)
```

- An **account** is a configured source of items. Two kinds: `imap` and `feed`.
  A `feed` account is a named group (e.g. "Tech", "News"); it holds no connection
  of its own. This keeps `items.account_id` `NOT NULL` and makes the two sources
  structurally symmetric — `folders` for mail, `feeds` for feeds.
- **`items`** is the spine. Every row is exactly one mail message *or* one feed
  article, and carries the fields the unified stream needs.
- **`threads`** is per-account (see §8 for why it is not global).

---

## 3. Identity

Three different "ids" appear, and conflating them causes real bugs.

| Identifier | Scope | Stable? | Purpose |
|---|---|---|---|
| `items.id` | internal | No | SQLite rowid. Fast joins, never exposed. |
| `items.public_id` | **ABI** | **Yes** | UUIDv7. The only id the UI, CLI, or D-Bus ever sees. |
| Natural key | per source | Semi | `(folder_id, uidvalidity, uid)` for mail; `(feed_id, guid)` for feeds. |

**Why `public_id` is mandatory.** A naive design exposes the rowid and it works — until
a UIDVALIDITY change forces a delete-and-refetch, at which point every cached rowid the
UI holds becomes wrong and points at a *different* message. The UI would silently
mis-render rather than error. A stable `public_id` decouples the UI's references from
the storage lifecycle.

**UUIDv7 specifically**, not v4: it is time-ordered, so it sorts by creation and is
kinder to B-tree inserts. Useful when debugging and when the CLI dumps items.

**The natural key is still stored** — it is what dedup and incremental sync actually
use. `public_id` is for external references; the natural key is for reconciliation.

For mail, identity is not the `Message-ID` header. The same message can appear in
multiple folders, and a message may lack a `Message-ID` entirely. `(folder, uid)` is
authoritative; `Message-ID` is a *secondary* key used for threading and cross-folder
dedup only.

---

## 4. The unified `Item` and `ContentStream`

### `Item`

The ABI-facing projection of an `items` row. This is what `crates/core` defines and
`ui/bridge` hands to QML.

```rust
pub struct Item {
    pub id: PublicId,               // UUIDv7, stable
    pub kind: ItemKind,             // Mail | Feed
    pub account: AccountId,
    pub title: String,              // Subject, or article title
    pub author: Option<String>,     // From display name, or feed author
    pub excerpt: Option<String>,    // One-line snippet for the list
    pub date: Option<Timestamp>,    // Date header, or feed published
    pub unread: bool,
    pub starred: bool,
    pub has_attachments: bool,
    pub thread: Option<ThreadId>,
    pub body_state: BodyState,      // None | Pending | Cached | Failed
}

pub enum ItemKind { Mail, Feed }
pub enum BodyState { None, Pending, Cached, Failed }
```

Note what is *absent*: no `folder`, no `feed_url`, no `Message-ID`, no IMAP flags.
Those are source-specific and stay below the abstraction. The UI never branches on
`kind` for layout; if it needs to, the abstraction is leaking and should be extended
instead.

### `ContentStream`

A `ContentStream` is **not a table**. It is a named, parameterized query over `items`,
resolved to SQL by `crates/store`. Treating it as a spec rather than a materialized
table is deliberate: there is nothing to keep in sync, and every stream is
automatically consistent with the spine.

```rust
pub struct StreamSpec {
    pub source: StreamSource,
    pub unread_only: bool,
    pub starred_only: bool,
    pub since: Option<Timestamp>,
}

pub enum StreamSource {
    Unified,                 // inbox mail (all accounts) + all feed items
    Account(AccountId),
    Folder(FolderId),
    Feed(FeedId),
    Thread(ThreadId),
    Search(String),          // FTS5-backed
}
```

The UI pages over a stream with a cursor. Because streams are ordered by
`items.sort_at DESC` and `public_id` is time-ordered, a keyset cursor
`(sort_at, public_id)` is stable under concurrent daemon writes — offset-based paging
is not, and should not be used.

**The canonical "unified inbox" predicate:**

```sql
-- mail in a folder whose role is 'inbox', plus every feed item
kind = 'feed'
OR (kind = 'mail' AND folder_id IN (SELECT id FROM folders WHERE role = 'inbox'))
```

---

## 5. Schema (v1)

Applied by `crates/store/src/schema.rs` from `migrations/0001_init.sql`.

```sql
-- ---------- accounts ----------
CREATE TABLE accounts (
  id                INTEGER PRIMARY KEY,
  kind              TEXT    NOT NULL CHECK (kind IN ('imap','feed')),
  name              TEXT    NOT NULL,
  email             TEXT,
  provider          TEXT,                    -- 'fastmail'|'proton'|'generic'|NULL
  enabled           INTEGER NOT NULL DEFAULT 1,

  -- IMAP / SMTP (kind = 'imap')
  imap_host         TEXT,
  imap_port         INTEGER,
  imap_security     TEXT CHECK (imap_security IN ('tls','starttls','none')),
  smtp_host         TEXT,
  smtp_port         INTEGER,
  smtp_security     TEXT CHECK (smtp_security IN ('tls','starttls','none')),

  -- auth
  auth_kind         TEXT    NOT NULL DEFAULT 'password'
                            CHECK (auth_kind IN ('password','oauth2')),
  username          TEXT,
  secret_ref        TEXT,                    -- keyring handle; NEVER the secret

  -- Proton Bridge: loopback-scoped, never a global "ignore certs" switch
  allow_invalid_cert_loopback INTEGER NOT NULL DEFAULT 0,

  -- feeds (kind = 'feed')
  poll_interval_secs INTEGER,

  created_at        INTEGER NOT NULL,
  updated_at        INTEGER NOT NULL
);

-- ---------- folders (mail) ----------
CREATE TABLE folders (
  id            INTEGER PRIMARY KEY,
  account_id    INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  name          TEXT    NOT NULL,            -- full IMAP path, e.g. "INBOX.Sent"
  display_name  TEXT    NOT NULL,
  delimiter     TEXT,
  role          TEXT CHECK (role IN
                  ('inbox','sent','drafts','trash','archive','junk','all')),
  uidvalidity   INTEGER NOT NULL DEFAULT 0,
  uidnext       INTEGER NOT NULL DEFAULT 0,
  subscribed    INTEGER NOT NULL DEFAULT 1,
  last_sync_at  INTEGER,
  UNIQUE (account_id, name)
);

-- ---------- feeds (feed accounts) ----------
CREATE TABLE feeds (
  id             INTEGER PRIMARY KEY,
  account_id     INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  url            TEXT    NOT NULL,
  title          TEXT,
  site_url       TEXT,
  etag           TEXT,                       -- conditional GET
  last_modified  TEXT,                       -- conditional GET
  last_fetch_at  INTEGER,
  next_fetch_at  INTEGER,
  backoff_secs   INTEGER NOT NULL DEFAULT 0,
  last_error     TEXT,
  UNIQUE (account_id, url)
);

-- ---------- threads ----------
CREATE TABLE threads (
  id             INTEGER PRIMARY KEY,
  account_id     INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  root_msgid     TEXT,                       -- Message-ID of the root, if known
  subject_norm   TEXT,                       -- Re:/Fwd: stripped, for fallback grouping
  last_activity  INTEGER NOT NULL,           -- max(item.sort_at), drives sort
  item_count     INTEGER NOT NULL DEFAULT 0,
  unread_count   INTEGER NOT NULL DEFAULT 0,
  UNIQUE (account_id, root_msgid)
);

-- ---------- items: the spine ----------
CREATE TABLE items (
  id             INTEGER PRIMARY KEY,        -- internal only
  public_id      TEXT    NOT NULL UNIQUE,    -- UUIDv7; the ABI id
  kind           TEXT    NOT NULL CHECK (kind IN ('mail','feed')),
  account_id     INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,

  -- unified display fields
  title          TEXT,
  author         TEXT,
  excerpt        TEXT,
  published_at   INTEGER,                    -- Date header / feed published
  sort_at        INTEGER NOT NULL,           -- canonical sort key

  -- unified state (see §7)
  unread         INTEGER NOT NULL DEFAULT 1,
  starred        INTEGER NOT NULL DEFAULT 0,

  -- relationships
  thread_id      INTEGER REFERENCES threads(id) ON DELETE SET NULL,

  -- body availability
  body_state     TEXT    NOT NULL DEFAULT 'none'
                         CHECK (body_state IN ('none','pending','cached','failed')),
  has_attachments INTEGER NOT NULL DEFAULT 0,

  created_at     INTEGER NOT NULL,
  updated_at     INTEGER NOT NULL
);

-- ---------- mail-specific ----------
CREATE TABLE mail_items (
  item_id        INTEGER PRIMARY KEY REFERENCES items(id) ON DELETE CASCADE,
  folder_id      INTEGER NOT NULL REFERENCES folders(id) ON DELETE CASCADE,
  uid            INTEGER NOT NULL,
  uidvalidity    INTEGER NOT NULL,           -- value at fetch time; mismatch = stale
  message_id     TEXT,                       -- no angle brackets
  in_reply_to    TEXT,
  references_hdr TEXT,                       -- space-separated Message-IDs
  subject_norm   TEXT,
  to_addrs       TEXT,                       -- JSON array
  cc_addrs       TEXT,                       -- JSON array
  reply_to       TEXT,
  size_bytes     INTEGER,
  raw_blob       TEXT,                       -- blob hash of raw MIME, NULL if evicted
  flags_local    TEXT NOT NULL DEFAULT '',   -- full IMAP set we don't map (see §7)
  UNIQUE (folder_id, uid)
);

-- ---------- feed-specific ----------
CREATE TABLE feed_items (
  item_id        INTEGER PRIMARY KEY REFERENCES items(id) ON DELETE CASCADE,
  feed_id        INTEGER NOT NULL REFERENCES feeds(id) ON DELETE CASCADE,
  guid           TEXT,                       -- RSS <guid> / Atom <id>
  link           TEXT,
  author         TEXT,
  UNIQUE (feed_id, guid)
);

-- ---------- extracted text: one copy, drives display + search ----------
CREATE TABLE item_text (
  item_id  INTEGER PRIMARY KEY REFERENCES items(id) ON DELETE CASCADE,
  title    TEXT NOT NULL DEFAULT '',
  author   TEXT NOT NULL DEFAULT '',
  body     TEXT NOT NULL DEFAULT '',
  durable  INTEGER NOT NULL DEFAULT 0        -- 1 = never evict (see §9)
);

CREATE VIRTUAL TABLE item_text_fts USING fts5(
  title, author, body,
  content    = 'item_text',
  content_rowid = 'item_id',
  tokenize   = "unicode61 remove_diacritics 2",
  prefix     = '2 3 4'
);

CREATE TRIGGER item_text_ai AFTER INSERT ON item_text BEGIN
  INSERT INTO item_text_fts(rowid, title, author, body)
  VALUES (new.item_id, new.title, new.author, new.body);
END;

CREATE TRIGGER item_text_ad AFTER DELETE ON item_text BEGIN
  INSERT INTO item_text_fts(item_text_fts, rowid, title, author, body)
  VALUES ('delete', old.item_id, old.title, old.author, old.body);
END;

CREATE TRIGGER item_text_au AFTER UPDATE ON item_text BEGIN
  INSERT INTO item_text_fts(item_text_fts, rowid, title, author, body)
  VALUES ('delete', old.item_id, old.title, old.author, old.body);
  INSERT INTO item_text_fts(rowid, title, author, body)
  VALUES (new.item_id, new.title, new.author, new.body);
END;

-- ---------- attachments ----------
CREATE TABLE attachments (
  id           INTEGER PRIMARY KEY,
  item_id      INTEGER NOT NULL REFERENCES items(id) ON DELETE CASCADE,
  part_id      TEXT    NOT NULL,             -- IMAP MIME part path, e.g. "1.2"
  filename     TEXT,
  mime_type    TEXT    NOT NULL,
  size_bytes   INTEGER,
  content_id   TEXT,                         -- for inline parts
  is_pdf       INTEGER NOT NULL DEFAULT 0,
  blob_hash    TEXT,                         -- NULL until fetched
  UNIQUE (item_id, part_id)
);

-- ---------- blob store (raw MIME, fetched attachments) ----------
CREATE TABLE blobs (
  hash         TEXT PRIMARY KEY,             -- BLAKE3 of raw bytes
  size_bytes   INTEGER NOT NULL,
  refcount     INTEGER NOT NULL DEFAULT 1,
  created_at   INTEGER NOT NULL
);

-- ---------- offline operation queue ----------
CREATE TABLE pending_ops (
  id           INTEGER PRIMARY KEY,
  account_id   INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  op           TEXT    NOT NULL CHECK (op IN ('flag','send','move','delete','expunge')),
  item_id      INTEGER REFERENCES items(id) ON DELETE CASCADE,
  payload      TEXT    NOT NULL,             -- JSON
  created_at   INTEGER NOT NULL,
  attempts     INTEGER NOT NULL DEFAULT 0,
  last_error   TEXT
);

-- ---------- indexes ----------
CREATE INDEX idx_items_sort        ON items(sort_at DESC);
CREATE INDEX idx_items_acct_sort   ON items(account_id, sort_at DESC);
CREATE INDEX idx_items_thread      ON items(thread_id);
CREATE INDEX idx_items_unread      ON items(sort_at DESC) WHERE unread = 1;
CREATE INDEX idx_items_starred     ON items(sort_at DESC) WHERE starred = 1;
CREATE INDEX idx_mail_msgid        ON mail_items(message_id);
CREATE INDEX idx_mail_subjectnorm  ON mail_items(subject_norm);
CREATE INDEX idx_feeditems_feed    ON feed_items(feed_id);
CREATE INDEX idx_feeds_next        ON feeds(next_fetch_at) WHERE last_error IS NULL;
CREATE INDEX idx_attach_item       ON attachments(item_id);
CREATE INDEX idx_pending_acct      ON pending_ops(account_id, created_at);
```

The two partial indexes (`idx_items_unread`, `idx_items_starred`) matter: unread and
starred are the two most common filters and typically a small fraction of rows.

---

## 6. Pragmas and concurrency

```sql
PRAGMA journal_mode = WAL;       -- concurrent reader (UI) + writer (daemon)
PRAGMA synchronous  = NORMAL;    -- safe under WAL, far faster than FULL
PRAGMA foreign_keys = ON;        -- ON DELETE CASCADE is load-bearing above
PRAGMA busy_timeout = 5000;      -- ms; daemon and UI contend briefly
PRAGMA temp_store   = MEMORY;
```

**This is the schema's answer to the process split.** `debloatmaild` is the only
writer; `debloatmail` (UI) and `debloatmailctl` read. WAL lets readers proceed during a
write instead of blocking, and `busy_timeout` absorbs the brief exclusive moments
(checkpoint, migration).

Consequences worth stating explicitly:

- **The UI must never write the DB.** State changes (mark read, star, delete) go over
  D-Bus to the daemon, which writes and emits a signal. This keeps a single writer and
  avoids the UI and daemon fighting over the same rows.
- **A UI-visible change is not durable until the daemon commits.** The UI should apply
  the change optimistically and reconcile on the signal. If it writes to the DB
  directly "just this once," WAL's guarantees no longer cover the interaction.
- **`debloatmailctl` is read-only except for explicitly mutating commands**, which go
  through the same daemon path.

---

## 7. The flag model

This is where mail and feeds genuinely differ, and the schema contains the difference
rather than leaking it into the UI.

| `items` field | IMAP flag | Direction | Notes |
|---|---|---|---|
| `unread = 0` | `\Seen` | bidirectional | The only unread bit the UI sees |
| `starred = 1` | `\Flagged` | bidirectional | |
| — | `\Answered` | server → local | Informational; shown as "replied" |
| — | `\Draft` | derived | `folder.role = 'drafts'`, not a per-item flag |
| — | `\Deleted` | transient | We move/expunge; never persisted as state |
| — | custom keywords | server → local | Preserved verbatim in `flags_local` |

**`items.unread` and `items.starred` are the single source of truth for the UI**, for
both kinds. For mail they are *projections* of server flags; for feeds they are
*authoritative and local*.

`mail_items.flags_local` holds the IMAP flags we do not surface — `\Answered` and any
custom keywords — as a comma-separated set. This is what allows a round-trip without
losing server state. It is **not** a second unread model; `\Seen` is deliberately not
duplicated there.

**Writing flags back:**

1. User action sets `items.unread`/`items.starred` and inserts a `pending_ops` row
   (`op = 'flag'`).
2. The daemon drains the queue, issues `UID STORE`, and deletes the op on success.
3. Server-side changes arriving via `IDLE` (`FETCH FLAGS`) update `items` directly.

Steps 1 and 3 can race. The rule: **a pending op wins over an incoming flag update for
the same item until it is drained.** Without this, marking a message read while an
`IDLE` fetch reports it unread would flicker.

**Feeds have no push path.** `pending_ops` never contains a feed op, and feed
`unread`/`starred` are local-only by construction.

---

## 8. Threading

Threads are computed **locally, per account**, using the JWZ algorithm (the one
Thunderbird uses), from `mail_items.in_reply_to` and `references_hdr`, with
`subject_norm` as the fallback when references are missing or broken.

**Why per account, not global:** `Message-ID` is not globally unique in practice
(cross-posted mailing lists, resent messages, broken clients), and merging threads
across accounts would fuse unrelated conversations. `UNIQUE (account_id, root_msgid)`
enforces the boundary.

**Why not server-side `THREAD`:** RFC 5256 `THREAD` is optional and widely
unimplemented; relying on it would mean two threading code paths. Computing locally
also makes feeds threadable later if desired.

**Maintenance:** `threads.item_count`, `unread_count`, and `last_activity` are
denormalized for the thread list view. They are recomputed for affected threads at the
end of each sync, not maintained incrementally on every item change — incremental
updates here are a classic source of drift.

**Feed items** get `thread_id = NULL`. The UI renders them as single-item threads.

---

## 9. Body text, blobs, and eviction

Two storage tiers, with an intentional asymmetry:

| | Mail | Feeds |
|---|---|---|
| Extracted text (`item_text.body`) | Cache, **evictable** | **Durable** |
| Raw content (blobs on disk) | Cache, evictable | Not applicable (text is enough) |
| Re-fetchable? | Yes, always, from IMAP | **Often no** — feeds rotate |
| `item_text.durable` | `0` | `1` |

**Why this asymmetry is the correct default.** A feed carries only its most recent N
items. If you evict a feed article's text, it is frequently gone forever — the feed no
longer serves it. Mail is the opposite: the body is always retrievable by UID. So feed
content is treated as primary data and mail bodies as a cache. Getting this backwards
produces silent, permanent data loss that no user would forgive.

**Blob layout.** Raw MIME and fetched attachments live as files, not BLOBs:

```
$XDG_CACHE_HOME/debloatmail/blobs/<hash[0:2]>/<hash>
```

`blobs` tracks size and refcount for GC. Keeping large binary payloads out of SQLite
keeps the DB compact and avoids bloating WAL checkpoints.

**Eviction policy** (LRU, triggered by a size cap):

- Eligible: `item_text` rows with `durable = 0`, and `blobs` with `refcount = 0`.
- Never eligible: `durable = 1`, or any row with a pending op, or starred items.
- Evicting text sets `items.body_state = 'none'` and deletes the `item_text` row
  (the trigger removes it from FTS). The header remains, so the list view is unaffected.

**Attachment fetching is always on demand.** `attachments.blob_hash` is `NULL` until
the user opens the file. This is what keeps "sync" from meaning "download everything."

---

## 10. Search

FTS5 over `item_text`, external-content, with `unicode61 remove_diacritics 2`. One
index, both kinds, which is the whole point — a user should not have to know whether
"rust borrow checker" appears in an email or a blog post.

**Why external-content rather than contentless:** contentless FTS5 (`content=''`) saves
space but cannot return the original text, which means `snippet()` and `highlight()`
do not work. Search results need highlighted snippets, so the text must be reachable
from the index. The triggers in §5 keep the two in sync automatically.

**Why `remove_diacritics 2`:** correct for accented Latin text, which plain `remove_diacritics 1`
handles incorrectly for several languages. Cheap to get right at schema-creation time,
impossible to change later without a full reindex.

**Prefix indexes (`prefix = '2 3 4'`)** make as-you-type prefix search fast without a
`LIKE` scan.

**Query construction.** User input is never interpolated raw. Build a query string
where terms are quoted and combined with `AND`, then optionally a trailing `*` for
prefix matching. Column filters (`title:`, `author:`) map to FTS5 column syntax. This
is `crates/store/src/search.rs`'s entire job, and it needs adversarial testing — FTS5
query syntax is its own small language, and malformed input must produce zero results,
not an error.

**Language.** FTS5's default tokenizer does not segment Chinese/Japanese/Korean. If CJK
support is wanted, it requires the `icu` tokenizer or trigram indexing — a deliberate
future decision, not an oversight. Note it now so it is not mistaken for a bug later.

---

## 11. Sync state machine

The schema stores enough state to make sync resumable and idempotent.

### Mail

| Phase | Trigger | Reads/writes |
|---|---|---|
| Discover | account add, `LIST` | `folders` — names, delimiter, SPECIAL-USE role |
| Validate | each sync | compare `folders.uidvalidity` to server |
| Initial | folder never synced | `UID FETCH 1:* (ENVELOPE FLAGS)` → `items` + `mail_items` |
| Incremental | `uidnext` advanced | `UID FETCH <uidnext>:*` |
| Watch | idle | `IDLE`, then `FETCH FLAGS` for changed UIDs |
| Bodies | on demand | `UID FETCH <uid> BODY[]` → `item_text` + blob |

**UIDVALIDITY change invalidates the folder.** When the server's `UIDVALIDITY` differs
from `folders.uidvalidity`, every cached UID in that folder is meaningless. The correct
response is: **preserve `public_id`s, delete and refetch the mail rows.** This is
exactly the case §3 exists for — the UI's references survive, and only the storage-layer
identity is rebuilt. A naive implementation that deletes and reinserts rows *with new
ids* is the bug that motivated `public_id`.

**`mail_items.uidvalidity` is stored per row**, so a stale row is detectable even
before the folder-level sweep runs.

### Feeds

| Phase | Reads/writes |
|---|---|
| Fetch | conditional GET using `feeds.etag` / `last_modified` |
| Parse | `feed-rs` |
| Upsert | match on `(feed_id, guid)`; update in place, preserving `unread`/`starred` |
| Schedule | set `next_fetch_at`, honoring `backoff_secs` on error |

**Upsert must not reset read state.** Re-fetching a feed whose items were already read
must leave `unread = 0`. Matching on `guid` (falling back to `link`, then a content
hash) and updating only content fields is what prevents the "everything is unread
again" failure.

---

## 12. Migrations

- **Forward-only.** Numbered SQL files in `crates/store/src/migrations/`, applied in
  order, tracked by `PRAGMA user_version`. No down-migrations — the recovery path for
  a bad migration is a beta-phase schema bump or a cache rebuild, not a rollback.
- **Every migration is idempotent-safe under WAL** and runs inside a transaction.
- **Schema changes are an "ask first" action** per `GEMINI.md`. Two specific hazards:
  adding a column to `items` touches the FTS-triggered `item_text` path indirectly, and
  changing `item_text` requires an FTS rebuild (`INSERT INTO item_text_fts(item_text_fts) VALUES('rebuild')`).
- **The cache is disposable; feeds are not.** A schema change that cannot migrate
  cleanly may drop and rebuild mail data, but **must not discard `feed_items` content**,
  for the reasons in §9. If a migration risks feed data, it needs an export path first.

---

## 13. Size budget

The point of the thin cache is that it stays small. Rough per-user figures:

| Data | Typical | Heavy user |
|---|---|---|
| Headers (`items` + `mail_items`) | 50k rows ≈ 25 MB | 200k rows ≈ 100 MB |
| Cached body text (`item_text`, non-durable) | LRU-capped, ≈ 20 MB | LRU-capped, ≈ 50 MB |
| Feed content (`item_text`, durable) | 5k items ≈ 15 MB | 50k items ≈ 150 MB |
| FTS5 index | ≈ 30–40% of indexed text | — |
| Blobs (on disk) | LRU-capped | LRU-capped |
| **Total DB** | **≈ 60–90 MB** | **≈ 300–400 MB** |

Compare to Thunderbird, which mirrors full bodies and routinely occupies multiple GB.
The gap is the header-only cache plus body eviction. If the DB is growing without
bound, the eviction path is not running — that is the first thing to check.

Feed content is the one category that legitimately grows with time and is not capped.
That is a deliberate trade: correctness over size, because the content is not
recoverable.

---

## 14. Mapping to Rust and the ABI

| Table | Owning crate | ABI-visible? |
|---|---|---|
| `accounts`, `folders`, `feeds` | `store` | `AccountId`, `FolderId`, `FeedId` as opaque handles |
| `items` | `store` → `core::Item` | **Yes** — `Item` struct |
| `mail_items`, `feed_items` | `store` | No — source detail stays below the abstraction |
| `item_text`, FTS | `store` | No |
| `attachments` | `store` | `Attachment` metadata only |
| `threads` | `store` → `core::Thread` | `ThreadId` handle |
| `pending_ops` | `store` / `sync` | No |

**`crates/core` defines `Item`, `ItemKind`, `BodyState`, `PublicId`, `StreamSpec` — and
knows nothing about SQLite.** `crates/store` implements the mapping. This is what keeps
invariant 1 (core has zero dependencies) true while still letting the model own its
types.

**Across the ABI**, ids cross as opaque `u64`s or strings, never as Rust references.
UniFFI generates the Kotlin/desktop bindings from these types. Because `Item` is a
plain data struct with no lifetimes, it maps cleanly to both QML and Kotlin — which is
the whole reason the `ffi` boundary can serve two UIs.

**Timestamps are Unix seconds, UTC, `i64`.** Not ISO strings, not local time. Formatting
is a UI concern; storing local time in the DB makes DST and timezone bugs permanent.

---

## 15. Open questions

These are unresolved and should be decided before the corresponding code is written:

1. **Attachment size cap.** Is there a maximum attachment size worth fetching on
   demand, or does the user's click always win? Affects `attachments` and blob GC.
2. **Feed full-content fetching.** For feeds that publish only summaries, do we fetch
   and extract the linked page (readability-style)? That is a significant scope
   addition and needs an explicit yes/no.
3. **CJK search tokenizer.** See §10 — `icu` vs. trigram vs. out of scope.
4. **Thread merge policy.** When a mail thread and a feed article share a subject, do
   they ever group? Current answer: no, but it should be recorded as intentional.
5. **Retention for feed items.** Even durable content might need a "keep last N months"
   policy for very long-lived feed accounts.
