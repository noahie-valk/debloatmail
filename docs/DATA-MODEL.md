# Data Model

**Status: authoritative.**

This document defines how debloatmail represents mail, feeds, and their **unified
stream** on disk and across the `ffi` boundary. It complements `architecture.md`: that
file explains *why* the system is shaped this way; this one specifies *what the data
looks like*.

The rules in `GEMINI.md` still apply. In particular: this schema lives behind
`crates/store`, is Qt-free, and `crates/core` knows the types but not the tables.

---

## 0. Authority and how to apply

**Scope of authority.** This file owns the schema, identity, the `Item` projection and
related ABI types, the state and flag model, eviction and blob lifecycle, and the enum
token mapping. Where another document disagrees about any of these, **this file wins**.
It does not own D-Bus signatures (`debloatmaild.md`), the sync engine (`SYNC.md`), the
extraction pipeline (`MIME-AND-TEXT.md`), the CLI surface (`debloatmailctl.md`), or the
UI (`UI.md`).

**Applying the schema.** No database has shipped, so all of v1, v2, and this
revision's corrections are folded into `0001_init.sql`: §5 is the whole of that file,
and it sets `PRAGMA user_version = 1` (§6, §12).

If a development database already exists, the equivalent is an additive
`0002_revision3.sql`: every structural change in this revision is additive except
three table rebuilds — tightening `mail_items` to
`UNIQUE (folder_id, uidvalidity, uid)`, renaming `attachments.part_id` to
`section_id`, and changing `pending_ops.item_id` from `ON DELETE CASCADE` to
`ON DELETE SET NULL` — and one index replacement (`idx_feeds_next` gains a new filter
column). The §9.2 eviction change alters only behaviour, not structure.

**Order of operations matters for the rebuilds**. In a fresh `0001` there is nothing to backfill and no rebuild. In a
`0002` migration the pattern for each rebuilt table is: add the new column as
nullable → backfill/transform in Rust (SQLite has no UUID function and no in-place
constraint change) → recreate the table with the final constraint. Do not try to do it
with `ALTER TABLE ADD COLUMN … NOT NULL` — SQLite will reject it.

---

## 1. Principles

1. **One spine, two detail tables.** A single `items` table carries every field the
   unified UI needs. Mail-only and feed-only fields hang off it in `mail_items` and
   `feed_items`. This is what makes "one list view, one unread model, one search" a
   property of the schema rather than something the UI has to fake.
2. **Two identities per row.** An internal `id` (SQLite rowid) that never leaves the
   process, and a `public_id` (UUIDv7) that is stable across re-syncs and is the only
   id that crosses the ABI. Rationale in §3.
3. **The body is stored once.** Extracted plaintext lives in `item_text`, and the FTS5
   index is external-content over it. No second copy for search. *(Clarification: this
   applies to the **body**. `title` and `author` are deliberately duplicated between
   `items`, for display, and `item_text`, as FTS5 columns, because FTS5 external-content
   requires every indexed column to exist in the content table.)*
4. **Durability is asymmetric.** Feed content is often *not* re-fetchable (feeds
   rotate), so it is stored permanently. Mail bodies are always re-fetchable from
   IMAP, so they are an evictable cache. §9.
5. **The daemon writes; the UI reads.** WAL mode plus a busy timeout, not shared
   mutable state. §6.
6. **One unread model, full fidelity preserved.** `items.unread` / `items.starred` /
   `items.answered` are the only flag state the UI ever sees. Custom IMAP keywords are
   kept alongside for round-tripping. §7.
7. **Every ABI-visible row has a stable `public_id`.** Internal rowids never cross the
   process boundary. This applies to `items`, `accounts`, `folders`, `feeds`,
   `threads`, `pending_ops`, and `drafts` — not just `items`. §3.
8. **Suppression is a first-class state.** A row can exist and be deliberately hidden
   (`items.hidden_at`). Every stream query excludes hidden rows by construction, via a
   view, not by remembering to add a `WHERE` clause. §4.4.

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

pending_ops   (account-scoped, references items)
drafts        (account-scoped, local-only; NOT on the spine)
visible_items (view over items + mail_items + feed_items; hidden rows excluded)
meta          (single row per counter; global `revision`, §5)
```

- An **account** is a configured source of items. Two kinds: `imap` and `feed`.
  A `feed` account is a named group (e.g. "Tech", "News"); it holds no connection
  of its own. This keeps `items.account_id` `NOT NULL` and makes the two sources
  structurally symmetric — `folders` for mail, `feeds` for feeds.
- **`items`** is the spine. Every row is exactly one mail message *or* one feed
  article, and carries the fields the unified stream needs.
- **`threads`** is per-account (see §8 for why it is not global).
- **`drafts`** are local-only and deliberately not items (§11.5).
- **`meta`** is a key/value table holding the global `revision` counter that the
  daemon protocol publishes (`debloatmaild.md` §16, `UI.md` §6.4). It is not an
  item and is not ABI-addressable (§14).

---

## 3. Identity

Several different "ids" appear, and conflating them causes real bugs.

| Identifier | Scope | Stable? | Crosses ABI? | Purpose |
|---|---|---|---|---|
| `<table>.id` | internal | No | **Never** | SQLite rowid. Fast joins. |
| `<table>.public_id` | **ABI** | **Yes** | **Yes** | UUIDv7. The only id a client sees. |
| Natural key | per source | Semi | No | `(folder_id, uidvalidity, uid)` for mail; `(feed_id, guid)` for feeds. Dedup and incremental sync. |

**Which tables carry a `public_id`:**

| Table | `public_id` | ABI handle |
|---|---|---|
| `items` | ✅ | `Item.id`, the CLI's short id |
| `accounts` | ✅ | `AccountId` |
| `folders` | ✅ | `FolderId` |
| `feeds` | ✅ | `FeedId` |
| `threads` | ✅ | `ThreadId` |
| `drafts` | ✅ | `DraftId` |
| `pending_ops` | ✅ | `Compose.Send` → `op_id` |
| `attachments` | ❌ | Addressed by `(item_public_id, section_id)` — see below |
| `blobs` | ❌ | Internal; addressed by content hash |
| `item_text`, `mail_items`, `feed_items` | ❌ | Never addressed directly |

**Why `public_id` is mandatory.** A naive design exposes the rowid and it works — until
a UIDVALIDITY change forces a delete-and-refetch, at which point every cached rowid the
UI holds becomes wrong and points at a *different* message. The UI would silently
mis-render rather than error. A stable `public_id` decouples the UI's references from
the storage lifecycle.

**Attachments are the deliberate exception.** An attachment has no stable identity of
its own — it is a *part of a message*, and `section_id` (the RFC 3501 §6.4.5 IMAP
section number, e.g. `"1.2"`) is stable for the life of that message.
`FetchAttachment(item_id, section_id)` already addresses it this way, so a separate
`public_id` would be an unused column. `MIME-AND-TEXT.md` §5.1 is the authority on what
the value looks like (our own structural walk assigns the numbers; that walk is the
authority); the name `section_id` is kept in sync with that document. This is
intentional; do not "fix" it.

**UUIDv7 specifically**, not v4: it is time-ordered, so it sorts by creation and is
kinder to B-tree inserts. Useful when debugging and when the CLI dumps items. It also
lets the CLI's 8-hex short id remain stable and collision-free across all seven tables,
since the prefixes are drawn from a single time-ordered space.

**The natural key is still stored** — it is what dedup and incremental sync actually
use. `public_id` is for external references; the natural key is for reconciliation.

For mail, identity is not the `Message-ID` header. The same message can appear in
multiple folders, and a message may lack a `Message-ID` entirely. `(folder, uid)` is
authoritative within a UIDVALIDITY epoch; `Message-ID` is a *secondary* key used for
threading, cross-folder dedup, and — since §11.1 — `public_id` preservation across a
UIDVALIDITY change.

---

## 4. The unified `Item` and `ContentStream`

### 4.1 `Item` and its companions

The ABI-facing projection of an `items` row. This is what `crates/core` defines and
`ui/bridge` hands to QML.

```rust
pub struct Item {
    pub id: PublicId,               // items.public_id (UUIDv7)
    pub kind: ItemKind,             // Mail | Feed
    pub account: AccountId,         // accounts.public_id
    pub title: String,              // never empty — see §4.2.1
    pub author: Option<String>,
    pub excerpt: Option<String>,    // derived at text-insert time — see §4.2.2
    pub date: Option<Timestamp>,    // published_at: Date header / feed published
    pub sort_at: Timestamp,         // the ordering key — see §4.2.3
    pub unread: bool,
    pub starred: bool,
    pub answered: bool,             // mail only; false for feeds
    pub has_attachments: bool,
    pub thread: Option<ThreadId>,   // threads.public_id
    pub body_state: BodyState,      // None | Pending | Cached | Failed
}

pub enum ItemKind { Mail, Feed }
pub enum BodyState { None, Pending, Cached, Failed }

pub struct Thread {
    pub id: ThreadId,               // threads.public_id
    pub account: AccountId,
    pub subject: String,            // threads.display_subject (un-normalised, for display)
    pub last_activity: Timestamp,
    pub item_count: u32,
    pub unread_count: u32,
}

pub struct Attachment {
    pub item: PublicId,             // the owning item
    pub section_id: String,         // RFC 3501 §6.4.5 section number; the address (§3)
    pub filename: Option<String>,
    pub mime_type: String,
    pub size_bytes: Option<u64>,
    pub is_pdf: bool,
    pub cached: bool                // blob_hash IS NOT NULL
}
```

Note what is *absent* from `Item`: no `folder`, no `feed_url`, no `Message-ID`, no IMAP
flags. Those are source-specific and stay below the abstraction. The UI never branches
on `kind` for layout; if it needs to, the abstraction is leaking and should be extended
instead.

`from` is likewise absent from `Item`: it is mail-only, and it is a *detail* field, not
a list field. It is backed by `mail_items.from_addr` (§5) and surfaces exactly where it
matters — the detail projection (`UI.md` §8.4, `ItemDetail.from`), reply prefill
(`UI.md` §11.4, `to = reply_to else from`), and the CLI's `from:` search operator
(§10) — and nowhere else. Feeds have no `from`. A message with no parseable `From`
header has `from_addr = NULL` (see `MIME-AND-TEXT.md` §4.4 for how `author` degrades in
that case; `from_addr` simply has no value).

### 4.2 Derived-field rules

#### 4.2.1 `title` is never empty

- Mail: `Subject` header, or `"(no subject)"`.
- Feed: entry title, else the link, else `"(untitled)"`.

A `String`, not an `Option<String>`, because the list view needs a row label and
`None` would force every renderer to invent a fallback. The DB column stays nullable;
the projection is not.

#### 4.2.2 `excerpt` derivation

First 200 characters of the first non-empty paragraph of `item_text.body`,
whitespace-collapsed, trailing punctuation preserved. `None` when the body is empty or
evicted. Computed in `store` when the text is written — not at read time, so the list
query stays a single indexed scan.

#### 4.2.3 `sort_at` — the ordering key, and what `--sort` means

`sort_at` is the ordering key and is part of `Item`, so a JSON consumer can reproduce
the ordering. The two date fields are defined as:

| Kind | `published_at` (display) | `sort_at` (ordering) |
|---|---|---|
| Mail | `Date` header | `INTERNALDATE` (received) |
| Feed | entry `published` | entry `published`, else `updated`, else fetch time |

`sort_at` is `NOT NULL`; every fallback chain ends at `created_at`, so it can never be
null and the cursor is never ambiguous.

The sort options shared by the CLI, the D-Bus streams, and the UI:

| Sort key | Order by |
|---|---|
| `date` (default) | `published_at DESC NULLS LAST, sort_at DESC, id DESC` |
| `received` | `sort_at DESC, id DESC` |
| `author` | `author COLLATE NOCASE ASC, sort_at DESC, id DESC` |
| `subject` | `title COLLATE NOCASE ASC, sort_at DESC, id DESC` |

Every tuple ends in `id DESC`, so each ordering is **total** — that is what keeps the
keyset cursor (§4.3) unambiguous under concurrent daemon writes. In SQLite `NULL`
orders smallest, so a plain `published_at DESC` *is* `NULLS LAST`; the two forms are
the same query. The exact composite keyset predicate per sort key is
`crates/store`'s business; this table pins the contract. `idx_items_date`
(published_at, sort_at) covers the default sort, and `id` (the rowid) needs no
index as the final tiebreaker.

`date` is the default because it is what a user means by "newest", and for feeds the
two dates are usually identical.

### 4.3 Cursor

```rust
pub enum SortKey { Date, Received, Author, Subject }

pub struct Cursor {
    pub sort: SortKey,                    // the ordering this cursor belongs to
    pub date: Option<Timestamp>,          // the `Date` key
    pub sort_at: Option<Timestamp>,       // the `Date` and `Received` keys
    pub text: Option<String>,             // `Author` → author, `Subject` → title
    pub id: PublicId,                     // final tiebreaker, always present
}
```

Encoded as `base64url(JSON)`, treated as **opaque** by clients. Keyset only; offsets
are never used (the daemon writes while a client reads). The cursor encodes **the
active sort key and the last row's values for that key's ordering columns** (§4.2.3),
so each sort pages consistently and a cursor cannot be applied to the wrong ordering
silently: a cursor from an `--sort author` stream is not valid on an `--sort received`
stream, and the daemon rejects the mismatch rather than reordering. Unused fields are
`None`.

Because the ordering tuples are total (§4.2.3), the keyset is stable under concurrent
daemon writes: rows inserted between pages do not cause skips or duplicates.

### 4.4 `ContentStream`

A `ContentStream` is **not a table**. It is a named, parameterized query over the
`visible_items` view (below), resolved to SQL by `crates/store`. Treating it as a spec
rather than a materialized table is deliberate: there is nothing to keep in sync, and
every stream is automatically consistent with the spine.

```rust
pub struct StreamSpec {
    pub source: StreamSource,
    pub sort: SortKey,              // §4.2.3; default `Date`
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

The UI pages over a stream with a cursor (§4.3). Offset-based paging is not stable
under concurrent daemon writes and must not be used. A stream's ordering is
`spec.sort` (default `Date`), and a cursor's `sort` must match it.

**One view, used by every stream.**

```sql
CREATE VIEW visible_items AS
SELECT
  i.*,
  m.folder_id AS folder_id,
  f.feed_id   AS feed_id
FROM items i
LEFT JOIN mail_items m ON m.item_id = i.id
LEFT JOIN feed_items f ON f.item_id = i.id
WHERE i.hidden_at IS NULL;
```

Every stream query selects from `visible_items`, never from `items`. Suppression and the
source-specific joins become properties of the view instead of clauses each query must
remember. Every `StreamSource` variant resolves against it: `Folder(folder_id)` is
`WHERE folder_id = ?`; `Thread(thread_id)` joins on `items.thread_id`.

**The canonical unified-inbox predicate** (the default `date` tuple of §4.2.3, in
SQL — in SQLite `NULL` orders smallest, so `published_at DESC` is NULLS LAST):

```sql
SELECT * FROM visible_items
WHERE kind = 'feed'
   OR (kind = 'mail'
       AND folder_id IN (SELECT id FROM folders WHERE role = 'inbox'))
ORDER BY published_at DESC, sort_at DESC, id DESC
LIMIT ?;
```

**A consequence to state explicitly:** a thread's `last_activity`, `item_count`, and
`unread_count` (§8) are computed over **visible** items. A hidden feed article must not
keep a thread at the top of the list.

---

## 5. Schema (revision 3)

This block is the whole of `crates/store/src/migrations/0001_init.sql` (§0, §12).

```sql
-- ================= meta: global counters =================
CREATE TABLE meta (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
INSERT INTO meta (key, value) VALUES ('revision', '0');
-- `store` increments `revision` by 1 inside every committed write transaction,
-- so the counter is committed with the data it describes and survives daemon
-- restarts. Readers use it per debloatmaild.md §16 / UI.md §6.4.

-- ================= accounts =================
CREATE TABLE accounts (
  id                          INTEGER PRIMARY KEY,
  public_id                   TEXT    NOT NULL UNIQUE,   -- UUIDv7
  kind                        TEXT    NOT NULL CHECK (kind IN ('imap','feed')),
  name                        TEXT    NOT NULL UNIQUE,   -- the CLI addresses accounts by name
  email                       TEXT,
  provider                    TEXT,                      -- 'fastmail'|'proton'|'generic'|NULL
  enabled                     INTEGER NOT NULL DEFAULT 1,

  -- IMAP / SMTP (kind = 'imap')
  imap_host                   TEXT,
  imap_port                   INTEGER,
  imap_security               TEXT CHECK (imap_security IN ('tls','starttls','none')),
  smtp_host                   TEXT,
  smtp_port                   INTEGER,
  smtp_security               TEXT CHECK (smtp_security IN ('tls','starttls','none')),

  -- auth
  auth_kind                   TEXT    NOT NULL DEFAULT 'password'
                                      CHECK (auth_kind IN ('password','oauth2')),
  username                    TEXT,
  secret_ref                  TEXT,                      -- keyring handle; NEVER the secret

  -- Proton Bridge: loopback-scoped, never a global "ignore certs" switch
  allow_invalid_cert_loopback INTEGER NOT NULL DEFAULT 0,

  -- feeds (kind = 'feed'): default poll interval for this account's feeds
  poll_interval_secs          INTEGER,

  created_at                  INTEGER NOT NULL,
  updated_at                  INTEGER NOT NULL
);

-- ================= folders (mail) =================
CREATE TABLE folders (
  id            INTEGER PRIMARY KEY,
  public_id     TEXT    NOT NULL UNIQUE,
  account_id    INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  name          TEXT    NOT NULL,                        -- full IMAP path, e.g. "INBOX.Sent"
  display_name  TEXT    NOT NULL,
  delimiter     TEXT,
  role          TEXT CHECK (role IN
                  ('inbox','sent','drafts','trash','archive','junk','all')),
  role_source   TEXT CHECK (role_source IN ('special-use','name','manual')),
                                                    -- how `role` was set (SYNC.md)
  uidvalidity   INTEGER NOT NULL DEFAULT 0,
  uidnext       INTEGER NOT NULL DEFAULT 0,
  highestmodseq INTEGER NOT NULL DEFAULT 0,              -- CONDSTORE CHANGEDSINCE cursor (SYNC.md §5.6)
  subscribed    INTEGER NOT NULL DEFAULT 1,
  last_sync_at  INTEGER,
  UNIQUE (account_id, name)
);

-- ================= feeds =================
CREATE TABLE feeds (
  id             INTEGER PRIMARY KEY,
  public_id      TEXT    NOT NULL UNIQUE,
  account_id     INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  url            TEXT    NOT NULL,
  title          TEXT,
  site_url       TEXT,
  etag           TEXT,                                   -- conditional GET
  last_modified  TEXT,                                   -- conditional GET
  interval_secs  INTEGER,                                -- per-feed override (§11.3)
  last_fetch_at  INTEGER,
  next_fetch_at  INTEGER,
  backoff_secs   INTEGER NOT NULL DEFAULT 0,
  consecutive_failures INTEGER NOT NULL DEFAULT 0,       -- error escalation (SYNC.md §7.3)
  last_error     TEXT,
  removed_at     INTEGER,                                -- soft-remove; NULL = active (§11.2)
  UNIQUE (account_id, url)
);

-- ================= threads =================
CREATE TABLE threads (
  id              INTEGER PRIMARY KEY,
  public_id       TEXT    NOT NULL UNIQUE,
  account_id      INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  root_msgid      TEXT,                                  -- Message-ID of the root, if known
  subject_norm    TEXT,                                  -- Re:/Fwd: stripped, for fallback grouping
  display_subject TEXT,                                  -- un-normalised, for the UI
  last_activity   INTEGER NOT NULL,                      -- max(visible sort_at)
  item_count      INTEGER NOT NULL DEFAULT 0,            -- visible items only
  unread_count    INTEGER NOT NULL DEFAULT 0,            -- visible items only
  UNIQUE (account_id, root_msgid)
);

-- ================= items: the spine =================
CREATE TABLE items (
  id             INTEGER PRIMARY KEY,                    -- internal only
  public_id      TEXT    NOT NULL UNIQUE,                -- UUIDv7; the ABI id
  kind           TEXT    NOT NULL CHECK (kind IN ('mail','feed')),
  account_id     INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,

  -- unified display fields
  title          TEXT,
  author         TEXT,
  excerpt        TEXT,                                   -- derived, see §4.2.2
  published_at   INTEGER,                                -- Date header / feed published
  sort_at        INTEGER NOT NULL,                       -- canonical sort key

  -- unified state (see §7)
  unread         INTEGER NOT NULL DEFAULT 1,
  starred        INTEGER NOT NULL DEFAULT 0,
  answered       INTEGER NOT NULL DEFAULT 0,             -- projection of \Answered
  hidden_at      INTEGER,                                -- NULL = visible. Suppression.

  -- relationships
  thread_id      INTEGER REFERENCES threads(id) ON DELETE SET NULL,

  -- body availability
  body_state     TEXT    NOT NULL DEFAULT 'none'
                         CHECK (body_state IN ('none','pending','cached','failed')),
  has_attachments INTEGER NOT NULL DEFAULT 0,

  created_at     INTEGER NOT NULL,
  updated_at     INTEGER NOT NULL
);

-- ================= mail-specific =================
CREATE TABLE mail_items (
  item_id        INTEGER PRIMARY KEY REFERENCES items(id) ON DELETE CASCADE,
  folder_id      INTEGER NOT NULL REFERENCES folders(id) ON DELETE CASCADE,
  uid            INTEGER NOT NULL,
  uidvalidity    INTEGER NOT NULL,           -- value at fetch time; mismatch = stale
  message_id     TEXT,                       -- no angle brackets
  in_reply_to    TEXT,
  references_hdr TEXT,                       -- space-separated Message-IDs
  subject_norm   TEXT,
  from_addr      TEXT,                       -- From address; no display name, no angle brackets
  to_addrs       TEXT,                       -- JSON array
  cc_addrs       TEXT,                       -- JSON array
  reply_to       TEXT,
  internal_date  INTEGER,                    -- INTERNALDATE; source of sort_at
  size_bytes     INTEGER,
  raw_blob       TEXT,                       -- blob hash of raw MIME, NULL if evicted
  flags_local    TEXT NOT NULL DEFAULT '',   -- custom keywords ONLY (see §7)
  UNIQUE (folder_id, uidvalidity, uid)
);

-- ================= feed-specific =================
CREATE TABLE feed_items (
  item_id        INTEGER PRIMARY KEY REFERENCES items(id) ON DELETE CASCADE,
  feed_id        INTEGER NOT NULL REFERENCES feeds(id) ON DELETE CASCADE,
  guid           TEXT,                       -- RSS <guid> / Atom <id>
  link           TEXT,
  author         TEXT,
  UNIQUE (feed_id, guid)
);

-- ================= drafts =================
-- A draft is local-only: it has no UID and no server counterpart until it is sent.
-- It is therefore NOT an item on the spine (see §11.5).
CREATE TABLE drafts (
  id             INTEGER PRIMARY KEY,
  public_id      TEXT    NOT NULL UNIQUE,                -- UUIDv7
  account_id     INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  to_addrs       TEXT    NOT NULL DEFAULT '[]',          -- JSON array
  cc_addrs       TEXT    NOT NULL DEFAULT '[]',          -- JSON array
  bcc_addrs      TEXT    NOT NULL DEFAULT '[]',          -- JSON array
  subject        TEXT    NOT NULL DEFAULT '',
  body           TEXT    NOT NULL DEFAULT '',
  in_reply_to    TEXT,
  references_hdr TEXT,
  attachments    TEXT    NOT NULL DEFAULT '[]',          -- JSON array of absolute paths
  created_at     INTEGER NOT NULL,
  updated_at     INTEGER NOT NULL
);

-- ================= extracted text: one body copy, drives display + search =================
-- The row exists from header sync (title/author set, body = '') — §11.1.
CREATE TABLE item_text (
  item_id           INTEGER PRIMARY KEY REFERENCES items(id) ON DELETE CASCADE,
  title             TEXT NOT NULL DEFAULT '',
  author            TEXT NOT NULL DEFAULT '',
  body              TEXT NOT NULL DEFAULT '',         -- emptied on eviction, row retained (§9.2)
  durable           INTEGER NOT NULL DEFAULT 0,       -- 1 = never evict (see §9)
  extractor_version INTEGER NOT NULL DEFAULT 1        -- re-extract pass: rows != current (MIME-AND-TEXT.md §18)
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

-- ================= attachments =================
CREATE TABLE attachments (
  id           INTEGER PRIMARY KEY,
  item_id      INTEGER NOT NULL REFERENCES items(id) ON DELETE CASCADE,
  section_id   TEXT    NOT NULL,             -- RFC 3501 §6.4.5 section number, e.g. "1.2"; the address
  filename     TEXT,
  mime_type    TEXT    NOT NULL,
  size_bytes   INTEGER,
  content_id   TEXT,                         -- for inline parts
  is_pdf       INTEGER NOT NULL DEFAULT 0,
  blob_hash    TEXT,                         -- NULL until fetched
  UNIQUE (item_id, section_id)
);

-- ================= blob store (raw MIME, fetched attachments) =================
CREATE TABLE blobs (
  hash         TEXT PRIMARY KEY,             -- BLAKE3 of raw bytes
  size_bytes   INTEGER NOT NULL,
  refcount     INTEGER NOT NULL DEFAULT 0,   -- non-NULL references to this blob
  last_used_at INTEGER NOT NULL,             -- GC grace window (see §9.3)
  created_at   INTEGER NOT NULL
);

-- ================= offline operation queue =================
CREATE TABLE pending_ops (
  id           INTEGER PRIMARY KEY,
  public_id    TEXT    NOT NULL UNIQUE,      -- the stable op_id
  account_id   INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  op           TEXT    NOT NULL CHECK (op IN ('flag','send','move','delete','expunge')),
  item_id      INTEGER REFERENCES items(id) ON DELETE SET NULL,
  payload      TEXT    NOT NULL,             -- JSON
  state        TEXT    NOT NULL DEFAULT 'pending'
                       CHECK (state IN ('pending','in_flight','failed','done')),
  attempts     INTEGER NOT NULL DEFAULT 0,
  last_error   TEXT,
  created_at   INTEGER NOT NULL,
  finished_at  INTEGER                       -- set on done/failed
);

-- ================= the stream surface =================
CREATE VIEW visible_items AS
SELECT i.*, m.folder_id AS folder_id, f.feed_id AS feed_id
FROM items i
LEFT JOIN mail_items m ON m.item_id = i.id
LEFT JOIN feed_items f ON f.item_id = i.id
WHERE i.hidden_at IS NULL;

-- ================= indexes =================
CREATE INDEX idx_items_sort        ON items(sort_at DESC);
CREATE INDEX idx_items_date        ON items(published_at DESC, sort_at DESC);
                                     -- default `date` sort (§4.2.3); `id` is the rowid tiebreaker
CREATE INDEX idx_items_acct_sort   ON items(account_id, sort_at DESC);
CREATE INDEX idx_items_thread      ON items(thread_id);
CREATE INDEX idx_items_unread      ON items(sort_at DESC)
                                   WHERE unread = 1 AND hidden_at IS NULL;
CREATE INDEX idx_items_starred     ON items(sort_at DESC)
                                   WHERE starred = 1 AND hidden_at IS NULL;
CREATE INDEX idx_mail_msgid        ON mail_items(message_id);
CREATE INDEX idx_mail_from         ON mail_items(from_addr);   -- the `from:` operator (§10)
CREATE INDEX idx_mail_subjectnorm  ON mail_items(subject_norm);
CREATE INDEX idx_mail_folder       ON mail_items(folder_id);
CREATE INDEX idx_feeditems_feed    ON feed_items(feed_id);
CREATE INDEX idx_feeds_next        ON feeds(next_fetch_at) WHERE removed_at IS NULL;
CREATE INDEX idx_attach_item       ON attachments(item_id);
CREATE INDEX idx_pending_acct      ON pending_ops(account_id, created_at);
CREATE INDEX idx_pending_state     ON pending_ops(state, created_at);
CREATE INDEX idx_drafts_acct       ON drafts(account_id, updated_at DESC);
```

**Notes on the schema.**

- The two partial indexes (`idx_items_unread`, `idx_items_starred`) matter: unread and
  starred are the two most common filters and typically a small fraction of rows. Both
  exclude hidden rows, matching `visible_items`.
- **`idx_mail_folder` is load-bearing.** The unified-inbox predicate (§4.4) filters on
  `folder_id`, which lives on `mail_items`.
- **Blob `refcount` semantics.** `refcount` is the number of non-NULL references to
  that hash from `mail_items.raw_blob` and `attachments.blob_hash`. It is maintained
  **in application code inside the same transaction as the reference change**, not by
  triggers — a trigger spanning two tables is harder to audit than a `store` function.
  `debloatmailctl doctor` runs a reconciliation check: recompute the true count and
  compare, because this is the kind of counter that drifts.
- **`meta.revision` is bumped inside the write transaction it belongs to.** The bump
  rides with the data, so a client that reads rows and `revision` in one transaction
  gets a consistent pairing, and a re-read in a fresh transaction can compare revisions
  per `debloatmaild.md` §16 and `UI.md` §6.4. The counter is in the database, not
  memory: it survives daemon restarts.
- **`accounts.name` is `UNIQUE`** because the CLI addresses accounts by name. In a
  `0002` migration against an existing dev database, deduplicate any legacy duplicate
  names before creating the unique index.
- **`idx_feeds_next` filters on `removed_at IS NULL`, not `last_error`.** Errored feeds
  are still retried (daily backoff, `SYNC.md` §7.3); the old filter excluded exactly
  them. Only soft-removed feeds drop out of scheduling.
- **`pending_ops.item_id` is `ON DELETE SET NULL`.** Delete-and-reinsert (and the
  UIDVALIDITY refetch, §11.1) must not cascade-delete queued user intent; the drain
  handles the orphaned ops explicitly (§11.6).

---

## 6. Pragmas and concurrency

```sql
PRAGMA journal_mode = WAL;       -- concurrent reader (UI) + writer (daemon)
PRAGMA synchronous  = NORMAL;    -- safe under WAL, far faster than FULL
PRAGMA foreign_keys = ON;        -- ON DELETE CASCADE is load-bearing above
PRAGMA busy_timeout = 5000;      -- ms; daemon and UI contend briefly
PRAGMA temp_store   = MEMORY;
PRAGMA user_version = 1;         -- 0001_init; each later migration increments (§12)
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
- **`user_version` is checked at startup.** `doctor` reports the schema version, and
  refuses to run against a database whose `user_version` is *newer* than the binary,
  rather than silently mis-reading it.

---

## 7. The flag model

This is where mail and feeds genuinely differ, and the schema contains the difference
rather than leaking it into the UI.

| `items` field | IMAP flag | Direction | Notes |
|---|---|---|---|
| `unread = 0` | `\Seen` | bidirectional | The only unread bit the UI sees |
| `starred = 1` | `\Flagged` | bidirectional | |
| `answered = 1` | `\Answered` | bidirectional | Set on reply; shown as "replied" |
| — | `\Draft` | derived | `folder.role = 'drafts'`, not a per-item flag |
| — | `\Deleted` | transient | We move/expunge; never persisted as state |
| — | custom keywords | server → local | Preserved verbatim in `flags_local` |

**`items.unread`, `items.starred`, and `items.answered` are the single source of truth
for the UI**, for both kinds. For mail they are *projections* of server flags; for feeds
they are *authoritative and local*.

**`mail_items.flags_local` holds custom keywords only** (`$Junk`, `$NotJunk`, user
labels), as a comma-separated set. `\Seen`, `\Flagged`, and `\Answered` are projections
with their own columns and are reconstructed on write-back; duplicating them in
`flags_local` would be a second unread model in disguise, which principle 6 forbids.

**`\Answered` is bidirectional** because replying is a user action that should be
reflected to the server: `Compose.Send` with `in_reply_to` set queues a `flag` op adding
`\Answered` to the original item.

**Writing flags back:**

1. User action sets `items.unread`/`starred`/`answered` and inserts a `pending_ops` row
   (`op = 'flag'`).
2. The daemon drains the queue and issues `UID STORE`. On success the op moves to a
   terminal state (§11.6).
3. Server-side changes arriving via `IDLE` (`FETCH FLAGS`) update `items` directly.

Steps 1 and 3 can race. The rule: **a pending op wins over an incoming flag update for
the same item until it is drained.** Without this, marking a message read while an
`IDLE` fetch reports it unread would flicker. The rule applies to all three projections.

**Feeds have no push path.** `pending_ops` never contains a feed op, and feed
`unread`/`starred` are local-only by construction.

---

## 8. Threading

Threads are computed **locally, per account**, using the JWZ algorithm (the one
Thunderbird uses), from `mail_items.in_reply_to` and `references_hdr`, with
`subject_norm` as the fallback when references are missing or broken.

- Threads carry a `public_id` (§3); `ThreadId` is that value.
- `display_subject` is stored alongside `subject_norm` so the thread list can show
  "Re: release checklist" rather than the normalised form used for grouping.

**Why per account, not global:** `Message-ID` is not globally unique in practice
(cross-posted mailing lists, resent messages, broken clients), and merging threads
across accounts would fuse unrelated conversations. `UNIQUE (account_id, root_msgid)`
enforces the boundary.

**Why not server-side `THREAD`:** RFC 5256 `THREAD` is optional and widely
unimplemented; relying on it would mean two threading code paths. Computing locally
also makes feeds threadable later if desired.

**Maintenance:** `threads.item_count`, `unread_count`, and `last_activity` are
denormalized for the thread list view and computed over **`visible_items` only** — a
hidden article must not hold a thread at the top of the list. They are recomputed for
affected threads at the end of each sync, not maintained incrementally on every item
change — incremental updates here are a classic source of drift.

**Feed items** get `thread_id = NULL`. The UI renders them as single-item threads.

---

## 9. Body text, blobs, and eviction

### 9.1 Two storage tiers

Two tiers, with an intentional asymmetry:

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

`blobs` tracks size, refcount, and last use for GC. Keeping large binary payloads out
of SQLite keeps the DB compact and avoids bloating WAL checkpoints.

**Attachment fetching is always on demand.** `attachments.blob_hash` is `NULL` until
the user opens the file. This is what keeps "sync" from meaning "download everything."

### 9.2 Eviction empties the body; it does not delete the row

Eviction policy (LRU, triggered by a size cap):

- Eligible: `item_text` rows with `durable = 0`.
- Never eligible: `durable = 1`, any row with a pending op, or starred items.

Evicting a mail body:

```sql
UPDATE item_text SET body = '' WHERE item_id = ?;   -- triggers keep FTS in sync
UPDATE items     SET body_state = 'none' WHERE id = ?;
```

`title` and `author` are retained, so subject and sender stay searchable; only the body
text is dropped. Because header sync writes the `item_text` row from the start (body
empty, §11.1), the row — and the FTS-indexed `title`/`author` columns — exist for
**every** message from first sync, not only for bodies that were once fetched. The
header remains, so the list view is unaffected. The `item_text` row is deleted only on
explicit purge (`delete --purge`).

Deleting the row on eviction would silently remove the item from search, making old
mail unsearchable over time — a behaviour no user would predict. **Full-text search of
the *body* therefore covers cached bodies only** (§10); `title` and `author` remain
searchable for every message.

### 9.3 Blob GC uses a grace window, not a pin

`FetchAttachment` updates `blobs.last_used_at` and returns the path. There is **no pin
and no release method.** GC rules:

- Eligible: `refcount = 0` **and** `last_used_at < now - 3600`.
- Never eligible: `refcount > 0`, or used within the last hour.

The one-hour window covers the gap between a caller receiving a path and opening the
file, which is milliseconds in practice. A pin with no release method would leak every
fetched attachment forever.

**Attachment blobs are bounded separately:** a size-capped LRU may
`UPDATE attachments SET blob_hash = NULL` on old items (which drops `refcount` and
makes the blob collectable), *except* for starred items and items with a pending op.

---

## 10. Search

FTS5 over `item_text`, external-content, with `unicode61 remove_diacritics 2`. One
index, both kinds, which is the whole point — a user should not have to know whether
"rust borrow checker" appears in an email or a blog post.

**Why external-content rather than contentless:** contentless FTS5 (`content=''`) saves
space but cannot return the original text, which means `snippet()` and `highlight()`
do not work. Search results need highlighted snippets, so the text must be reachable
from the index. The triggers in §5 keep the two in sync automatically.

**Why `remove_diacritics 2`:** correct for accented Latin text, which plain
`remove_diacritics 1` handles incorrectly for several languages. Cheap to get right at
schema-creation time, impossible to change later without a full reindex.

**Prefix indexes (`prefix = '2 3 4'`)** make as-you-type prefix search fast without a
`LIKE` scan.

**Query construction.** Search operators are a **closed set**:

| Operator | Meaning | Backed by |
|---|---|---|
| positional term | full-text term; terms are AND-combined | FTS5 over `item_text` |
| `title:` | match the title column | FTS5 column syntax |
| `author:` | match the author column | FTS5 column syntax |
| `from:` | mail only; structured address match, not full-text | `mail_items.from_addr` (indexed) |
| `is:mail`, `is:feed` | item-kind filter | the `items.kind` column |

`from:` and `is:*` are mapped to SQL by `store`, not passed to FTS5. User input is
never interpolated raw into the FTS5 part: build a query string where terms are quoted
and combined with `AND`, optionally a trailing `*` for prefix matching. This is
`crates/store/src/search.rs`'s entire job, and it needs adversarial testing — FTS5
query syntax is its own small language, and malformed input must produce zero results,
not an error. Downstream documents (`debloatmailctl.md` §8, `UI.md`) may use **only**
this set; this table *is* the operator list. In particular there is no `kind:`
operator — the kind filter is `is:mail` / `is:feed`.

**Body search covers cached bodies only.** Because mail bodies are evictable (§9.2), a
search for a phrase that appeared in an old mail body returns nothing once that body
has been evicted — but subject and author remain searchable for **every** message,
because the `item_text` row exists from header sync (§11.1). This is the price of the
thin cache, and it is the correct price: the alternative is keeping every body forever,
which is the thing this project exists not to do. `debloatmailctl doctor` reports the
ratio of `items` with `body_state = 'none'` to total, so a user can see how much of
their mail is currently unsearchable by body.

**Language.** FTS5's default tokenizer does not segment Chinese/Japanese/Korean. If CJK
support is wanted, it requires the `icu` tokenizer or trigram indexing — a deliberate
future decision, not an oversight. Note it now so it is not mistaken for a bug later.

---

## 11. Sync state machine

The schema stores enough state to make sync resumable and idempotent.

### 11.1 Mail

| Phase | Trigger | Reads/writes |
|---|---|---|
| Discover | account add, `LIST` | `folders` — names, delimiter, SPECIAL-USE role, recorded with `role_source = 'special-use'` (name-inferred roles record `'name'`; user-set roles, `'manual'`) |
| Validate | each sync | compare `folders.uidvalidity` to server |
| Initial | folder never synced | `UID FETCH 1:* (UID FLAGS INTERNALDATE RFC822.SIZE ENVELOPE BODYSTRUCTURE)` → `items` + `mail_items` + `item_text(title, author, body = '')` |
| Incremental | `uidnext` advanced | `UID FETCH <uidnext>:* (UID FLAGS INTERNALDATE RFC822.SIZE ENVELOPE BODYSTRUCTURE)` |
| Watch | idle | `IDLE`, then `FETCH FLAGS` for changed UIDs; with `CONDSTORE`, `CHANGEDSINCE <folders.highestmodseq>` — the monotonic cursor held per folder (`SYNC.md` §5.6) |
| Bodies | on demand | `UID FETCH <uid> BODY.PEEK[]` → `item_text` (body filled in) + blob |

Header fetches are metadata fetches and never set `\Seen`; all body and part fetches
use `BODY.PEEK[...]` (`SYNC.md` §5.5, §6.1). The header-time fetch of
`BODYSTRUCTURE` seeds `has_attachments` and the `attachments` rows provisionally;
`MIME-AND-TEXT.md` §5.1's walk over the fetched body is the authority on final
attachment rows (§14 of that document and `SYNC.md` §5.5 reconcile when the body
arrives).

**UIDVALIDITY change invalidates the folder.** When the server's `UIDVALIDITY` differs
from `folders.uidvalidity`, every cached UID in that folder is meaningless. The
correct response is: **preserve `public_id` where a `Message-ID` match identifies the
message, mint new ids where it cannot, and delete and refetch the mail rows.** This is
the case §3 exists for — the UI's references survive where they can, and only the
storage-layer identity is rebuilt. A naive implementation that deletes and reinserts
rows *with new ids* is the bug that motivated `public_id`.

**The limit (stated, per `SYNC.md` §5.4):** a message with no `Message-ID` **cannot**
have its `public_id` preserved. It gets a new one, and any UI reference to the old one
breaks. This is unavoidable — there is nothing to match on — and `debloatmailctl
doctor` reports how many rows could not be preserved, so it is known rather than
discovered.

**The schema makes the refetch safe where the old one was a data-loss path.** The
constraint is `UNIQUE (folder_id, uidvalidity, uid)`, so the refetch's upsert — which
matches on the same triple (`SYNC.md` §11.4, corrected) — cannot rebind an incoming
message to a stale row's `public_id` without an explicit `Message-ID` re-map. And
`pending_ops.item_id` is `ON DELETE SET NULL`, so purging the folder's stale rows does
not cascade-delete queued operations; the drain fails those ops with a clear error
(§11.6) instead of silently erasing user intent.

**`mail_items.uidvalidity` is stored per row**, so a stale row is detectable even
before the folder-level sweep runs.

### 11.2 Feeds

| Phase | Reads/writes |
|---|---|
| Fetch | conditional GET using `feeds.etag` / `last_modified` |
| Parse | `feed-rs` |
| Upsert | match on `(feed_id, guid)`; update in place, preserving state (§11.4) |
| Schedule | set `next_fetch_at`, honoring `backoff_secs` on error |

**Upsert must not reset read state.** Re-fetching a feed whose items were already read
must leave `unread = 0`. Matching on `guid` (falling back to `link`, then a content
hash) and updating only content fields is what prevents the "everything is unread
again" failure.

**Removal.** `Feeds.Remove` without purge sets `removed_at` (soft-removal): the
poller targets only feeds with `removed_at IS NULL`, so removal **stops polling but
retains the articles** — the feed's items stay in streams and in search
(`debloatmaild.md` §8 contract). The purge form deletes the `feeds` row and, in the
same transaction, the feed's `feed_items` rows and with them the spine `items` and
`item_text` rows of those articles: the FK cascade takes `feed_items` automatically,
but the spine rows must be deleted explicitly in the same transaction (there is no FK
from `items` back to `feed_items`), or they become visible items with no source.

### 11.3 Feed poll interval resolution

```
effective_interval = COALESCE(feeds.interval_secs,
                              accounts.poll_interval_secs,
                              global_default)
```

`Feeds.Add(account, url, name, interval_secs)` names the feed account (group) the feed
belongs to — `feeds.account_id` is `NOT NULL`, and `accounts.name` is `UNIQUE`, so the
CLI can resolve a name to the account — and writes `feeds.interval_secs`; when it is
`NULL` or `0`, the account default applies. (`debloatmaild.md` §8 gains the `account`
parameter in the same change.)

**Timeout vs. hard failure (`SYNC.md` §7.4).** A fetch that *times out* is not a
rejection: the row is inserted first and the call returns with `validated: false`,
then validation continues in the background — `title`, `site_url`, and `etag` are
filled in on success, and if validation ultimately fails the row is deleted and the
addition reported as failed. A *hard* validation failure — 404, non-feed content,
malformed XML — creates **no** row at all. The two policies do not conflict:
"fail loudly, don't create an empty feed" means no feed is ever left *permanently*
unvalidated; a `validated: false` row is transient by construction.

### 11.4 Feed upsert must preserve suppression, not just read state

`Items.Hide` sets `hidden_at`; the next poll matches on `(feed_id, guid)` and must
leave `hidden_at` alone, exactly as it leaves `unread`/`starred` alone.

**The upsert updates content columns only.** The full list of columns the upsert must
never touch:

`unread`, `starred`, `hidden_at`, `body_state`, `thread_id`, `created_at`.

State this as a list, because "don't reset read state" invites a partial fix that still
resets suppression.

### 11.5 Drafts

`compose --save-draft` has a home in the `drafts` table (§5) rather than being modelled
as an item on the spine, for one reason: **a draft has no UID.** `mail_items` requires
`(folder_id, uid, uidvalidity)` with `UNIQUE (folder_id, uidvalidity, uid)`, so a local
draft would need a synthetic UID and a fake folder — two lies in the schema to satisfy
a constraint that does not apply.

A draft is mail-only and local-only, so a separate table violates nothing. The
consequence is accepted and is the point of the decision: **a draft saved here is a
local draft — it does not appear in the server's `drafts` folder or on another
device's mail client.** Users who need server-side drafts keep writing in their web
mail; this client's drafts are its own working scratch space (the CLI's
`--save-draft` documents the same semantics).

Lifecycle:

| Step | Effect |
|---|---|
| `SaveDraft` | Insert into `drafts`; return `draft_id` |
| `Send(draft_id)` | Read the draft, queue a `send` op, delete the draft row |
| `DeleteDraft(draft_id)` | Delete the row |

`Compose.Send` accepts **either** a `draft_id` **or** an inline `a{sv}` draft, so the
GUI can send without a save round-trip.

### 11.6 Pending ops: state and retention

Ops carry a `state` and a `finished_at`, and terminal ops survive. `Compose.Send`
returns an `op_id` the caller may check later ("did my message go out?"); if the row
were deleted on success, the answer would always be "not found".

| State | Meaning |
|---|---|
| `pending` | Queued, not yet attempted |
| `in_flight` | Attempted at least once; may be retried |
| `failed` | Terminal failure after max attempts |
| `done` | Terminal success |

**Terminal rows (`done`, `failed`) are retained for 7 days**, then purged by a
housekeeping pass. `Queue.Pending()` returns `pending` + `in_flight` + `failed` (i.e.
"not done"). `Queue.Count` counts the same set. `Queue.Get(s op_id) -> a{sv}` queries a
specific op's state.

**An op whose item is gone.** The drain must treat `item_id IS NULL` (its item was
deleted — §5's `SET NULL`, or the item was purged in a UIDVALIDITY refetch) as data,
not corruption. Ops whose payload is self-sufficient (a `send` with an inline draft)
complete their self-sufficient part and record only the lost item-dependent part —
e.g. the `\Answered` back-flag (§7) — as a partial failure in `last_error`, applying
§11.7's "success means sent" rule. Ops that need the item to act on it (`flag`,
`move`, `delete` on it) are marked `failed` with `last_error` explaining that the item
no longer exists. Either way the op row survives and surfaces like any other terminal
op — user intent is never erased silently.

**One retry schedule, and offline is not an attempt.** Op retry timing is the formula
in `SYNC.md` §10.2 (base 1 minute, exponential with jitter, cap 6 hours, reset only on
success). The earlier table in `SYNC.md` §8.6 (1/5/15 min, then 1 h) contradicts
§10.2 and is corrected there; §10.2 is the single schedule this document cites. Same
for attempt counting: a failure where the server is simply unreachable (laptop
offline) must **not** increment `attempts` — the op stays `pending` at its §10.2
backoff — because an offline week must not burn the ten attempts that `failed` is
terminal for. `SYNC.md` §8.2/§9 are corrected to match when that document is
processed; the schema's meaning of `attempts` is "attempts made against a reachable,
authenticated server" from this revision forward.

### 11.7 Send + append is one op, and success means "sent"

`pending_ops.op` has no `append` variant, by design:

- `op = 'send'` with `payload.append_to_sent: bool` and `payload.sent_folder`.
- The op sends first, then appends.
- **Success is defined by the send.** If the append fails, the op still terminates as
  `done`, `last_error` records the append failure, and `Queue.Completed` carries
  `success = true` with a non-empty `detail` describing the missing Sent copy.

Rationale: reporting `success = false` after a successful send would imply the message
did not go out. That is the more dangerous error of the two — a user told a message
failed will send it again. The CHECK constraint needs no new op kind.

---

## 12. Migrations

- **Forward-only.** Numbered SQL files in `crates/store/src/migrations/`, applied in
  order, tracked by `PRAGMA user_version`. No down-migrations — the recovery path for
  a bad migration is a beta-phase schema bump or a cache rebuild, not a rollback.
- **Every migration is idempotent-safe under WAL** and runs inside a transaction.
- **`0001_init.sql` sets `user_version = 1`** (§0, §6). Each later migration
  `NNNN_*.sql` ends by setting `user_version` to its own number. Two conventions
  formerly conflated are now distinct: the *document* revision (v1, v2, this revision
  3) and the database `user_version` (1, 2, 3, …). The oddity of a fresh `0001` being
  stamped `user_version = 2` is resolved in favour of the migration numbering.
- **Schema changes are an "ask first" action** per `GEMINI.md`. Two specific hazards:
  adding a column to `items` touches the FTS-triggered `item_text` path indirectly, and
  changing `item_text` requires an FTS rebuild
  (`INSERT INTO item_text_fts(item_text_fts) VALUES('rebuild')`). The bundle of schema
  changes applied by *this* revision is the one decided in the issues work (new §16);
  any further change is ask-first again.
- **The cache is disposable; feeds are not.** A schema change that cannot migrate
  cleanly may drop and rebuild mail data, but **must not discard `feed_items` content**,
  for the reasons in §9.1. If a migration risks feed data, it needs an export path
  first.

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
| `accounts` | `store` | `AccountId` = `accounts.public_id` |
| `folders` | `store` | `FolderId` = `folders.public_id` |
| `feeds` | `store` | `FeedId` = `feeds.public_id` |
| `threads` | `store` → `core::Thread` | `ThreadId` = `threads.public_id` |
| `items` | `store` → `core::Item` | **Yes** — `Item` struct |
| `drafts` | `store` | `DraftId` = `drafts.public_id` |
| `pending_ops` | `store` / `sync` | `op_id` = `pending_ops.public_id` |
| `attachments` | `store` | `Attachment` metadata; addressed by `(item, section_id)` |
| `meta` | `store` | No — not an id; surfaced only as the protocol's `revision` (debloatmaild.md §16) |
| `mail_items`, `feed_items`, `item_text`, `blobs` | `store` | No — source detail stays below the abstraction |

**`crates/core` defines `Item`, `ItemKind`, `BodyState`, `PublicId`, `StreamSpec`,
`SortKey`, `Thread`, `Attachment`, `Cursor` — and knows nothing about SQLite.**
`crates/store` implements the mapping. This is what keeps invariant 1 (core has zero
dependencies) true while still letting the model own its types.

**`PublicId` is a newtype over `String`**, defined in `core`, with `FromStr`
validation. All ABI-visible ids are `PublicId` at the boundary and are typed distinctly
(`AccountId(PublicId)`, `FolderId(PublicId)`, `DraftId(PublicId)`, …) so a folder id
cannot be passed where an account id is expected. The D-Bus layer sees plain strings;
the type distinction lives in Rust and is erased at the wire.

**Across the ABI**, ids cross as strings, never as Rust references or rowids. UniFFI
generates the Kotlin/desktop bindings from these types. Because `Item` is a plain data
struct with no lifetimes, it maps cleanly to both QML and Kotlin — which is the whole
reason the `ffi` boundary can serve two UIs.

**Timestamps are Unix seconds, UTC, `i64`.** Not ISO strings, not local time.
Formatting is a UI concern; storing local time in the DB makes DST and timezone bugs
permanent.

**Enum token rule.** Pin one mapping:

| Enum | Rust | SQL / wire token |
|---|---|---|
| `ItemKind` | `Mail`, `Feed` | `mail`, `feed` |
| `BodyState` | `None`, `Pending`, `Cached`, `Failed` | `none`, `pending`, `cached`, `failed` |
| `AccountKind` | `Imap`, `Feed` | `imap`, `feed` |
| `FolderRole` | `Inbox`, `Sent`, `Drafts`, `Trash`, `Archive`, `Junk`, `All` | `inbox`, `sent`, `drafts`, `trash`, `archive`, `junk`, `all` |
| `SortKey` | `Date`, `Received`, `Author`, `Subject` | `date`, `received`, `author`, `subject` |
| `OpKind` | `Flag`, `Send`, `Move`, `Delete`, `Expunge` | `flag`, `send`, `move`, `delete`, `expunge` |
| `OpState` | `Pending`, `InFlight`, `Failed`, `Done` | `pending`, `in_flight`, `failed`, `done` |
| `AuthKind` | `Password`, `OAuth2` | `password`, `oauth2` |
| `Security` | `Tls`, `StartTls`, `None` | `tls`, `starttls`, `none` |

**Rule:** the SQL `CHECK` values and the wire tokens are the *same lowercase
snake_case strings*. Rust uses PascalCase. Conversion happens in exactly two places —
`store` (SQL ↔ Rust) and `ffi` (Rust ↔ wire). There is no third convention, and no
token is ever free-form.

---

## 15. Open questions

Unresolved, and to be decided before the corresponding code is written:

1. **Attachment size cap.** Is there a maximum attachment size worth fetching on
   demand, or does the user's click always win? Affects `attachments` and the LRU in
   §9.3.
2. **Feed full-content fetching.** For feeds that publish only summaries, do we fetch
   and extract the linked page (readability-style)? That is a significant scope
   addition and needs an explicit yes/no.
3. **CJK search tokenizer.** See §10 — `icu` vs. trigram vs. out of scope.
4. **Thread merge policy.** When a mail thread and a feed article share a subject, do
   they ever group? Current answer: never merge. Recorded as intentional.
5. **Search completeness vs. eviction.** §9.2 makes evicted mail bodies unsearchable
   by body. Is that acceptable, or should `search` transparently offer to
   fetch-and-search bodies on demand? Decide before the search UI is designed, because
   it changes what the search box can promise.
6. **Terminal op retention window.** 7 days (§11.6) is a guess. Surfacing is no longer
   the question — a `failed` op stays in `Queue.Pending()` and is inspectable via
   `Queue.Get` until the purge runs (§11.6) — but the window length itself is open.
7. **Retention for feed items.** Even durable content might need a "keep last N months"
   policy for very long-lived feed accounts.

---

## 16. Verification checklist

After applying, these must all hold. Each maps to a claim the API specs make.

- [ ] `SELECT` of every `public_id` in `accounts`, `folders`, `feeds`, `threads`,
      `items`, `drafts`, `pending_ops` is `NOT NULL` and `UNIQUE`.
- [ ] No D-Bus method in `debloatmaild.md` Appendix A takes an id that is not a
      `public_id` string (except `section_id`).
- [ ] The unified-inbox query runs and returns feed items plus inbox mail, and excludes
      rows with `hidden_at IS NOT NULL`.
- [ ] Hiding a feed item and re-polling its feed leaves `hidden_at` set.
- [ ] `Compose.Send` → `Queue.Get(op_id)` returns a terminal state after the message
      goes out.
- [ ] Evicting a mail body leaves `title`/`author` searchable and the item present in
      `visible_items`.
- [ ] `doctor`'s blob refcount reconciliation passes after a fetch/evict cycle.
- [ ] `doctor` refuses a database whose `user_version` is newer than the binary.
- [ ] No enum token appears in the database or on the wire in any case other than the
      lowercase snake_case form in §14.
- [ ] A fresh database opens with `PRAGMA user_version` = 1.
- [ ] `mail_items` enforces `UNIQUE (folder_id, uidvalidity, uid)`; a simulated
      UIDVALIDITY-change refetch rebinds no old `public_id` to a different message
      without a `Message-ID` match, and `doctor` reports the unpreservable count.
- [ ] Deleting an item that has queued ops leaves the ops with `item_id IS NULL`; the
      drain fails or partially-finalises them per §11.6 and drops none silently.
- [ ] `mail_items.from_addr` is populated at header sync; `from:` search and reply
      prefill (`to = reply_to else from`) work from the schema alone.
- [ ] Every mail item has an `item_text` row from header sync, including ones never
      opened; a subject search and an author search both hit them.
- [ ] `meta.revision` increments once per committed write transaction; a client that
      reads a stream and `revision` in one transaction gets a consistent snapshot; the
      counter survives a daemon restart.
- [ ] A cursor from each §4.2.3 sort keys its pages without skips or duplicates under
      concurrent daemon writes, and a cursor whose `sort` does not match the stream is
      rejected.
- [ ] A feed with `removed_at` set is not polled but still appears in streams and
      search; purging it deletes its `feed_items`, and via them its spine
      `items`/`item_text` rows; `idx_feeds_next` still covers an errored (not
      removed) feed.
- [ ] Every attachment is addressed by `(item_public_id, section_id)` — in the DDL, in
      the `Attachment` struct, and (after `debloatmaild.md` is updated) in the D-Bus
      signatures.

---
