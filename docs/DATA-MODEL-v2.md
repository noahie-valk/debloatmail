# DATA-MODEL.md v2 — Patch

**Status:** proposed · **Supersedes:** §1, §3, §4, §5, §7, §8, §9, §10, §11, §14, §15 of `DATA-MODEL.md` (v1)

This patch makes the schema agree with `debloatmaild.md` and `debloatmailctl.md`. It
exists because two claims were made by the API specs that the v1 schema could not
satisfy:

1. "Every method that takes an id takes a `public_id` string, never a path"
   (`debloatmaild.md` §3) — but only `items` had a `public_id`.
2. `Items.Hide` and `Items.Delete` on feed items — but `items` had no suppression
   column, so a hidden article reappears on the next poll.

Both are one-line changes now and migrations later. Everything else here is a
consequence of fixing those two, plus the smaller contract gaps the audit found.

---

## 0. How to apply

No database has shipped, so this **folds into `0001_init.sql`** rather than becoming a
second migration. Set `PRAGMA user_version = 2` and treat §5 below as the whole of
`0001_init.sql`.

If a development database already exists, the equivalent is
`0002_public_ids_and_suppression.sql`, which is additive except for the
`item_text` eviction change in §9 — and that one only alters behaviour, not structure.

**Order of operations matters for exactly one thing:** adding `public_id` to
`accounts`, `folders`, `feeds`, and `threads` requires backfilling UUIDv7 values before
the `NOT NULL UNIQUE` constraint can hold. In a fresh `0001` there is nothing to
backfill. In an `0002` migration the pattern is: add nullable column → backfill in
Rust (SQLite has no UUID function) → recreate the table with the constraint. Do not
try to do it with `ALTER TABLE ADD COLUMN ... NOT NULL` — SQLite will reject it.

---

## 1. §1 Principles — two additions, one clarification

Keep principles 1–6. Add:

7. **Every ABI-visible row has a stable `public_id`.** Internal rowids never cross the
   process boundary. This applies to `items`, `accounts`, `folders`, `feeds`,
   `threads`, and `pending_ops` — not just `items`. §3.
8. **Suppression is a first-class state.** A row can exist and be deliberately hidden
   (`items.hidden_at`). Every stream query excludes hidden rows by construction, via a
   view, not by remembering to add a `WHERE` clause. §4.

**Clarify principle 3 ("text is stored once"):** it applies to the **body**. `title`
and `author` are deliberately duplicated between `items` (display) and `item_text`
(FTS5 columns), because FTS5 external-content requires every indexed column to exist in
the content table. v1 stated the principle more broadly than the schema implements.

---

## 2. §3 Identity — generalise from "items" to "every ABI-visible row"

Replace the identifier table with:

| Identifier | Scope | Stable? | Crosses ABI? | Purpose |
|---|---|---|---|---|
| `<table>.id` | internal | No | **Never** | SQLite rowid. Fast joins. |
| `<table>.public_id` | **ABI** | **Yes** | **Yes** | UUIDv7. The only id a client sees. |
| Natural key | per source | Semi | No | `(folder_id, uidvalidity, uid)`; `(feed_id, guid)`. Dedup and incremental sync. |

**Which tables carry a `public_id`:**

| Table | `public_id` | ABI handle |
|---|---|---|
| `items` | ✅ | `Item.id`, the CLI's short id |
| `accounts` | ✅ **(new)** | `AccountId` |
| `folders` | ✅ **(new)** | `FolderId` |
| `feeds` | ✅ **(new)** | `FeedId` |
| `threads` | ✅ **(new)** | `ThreadId` |
| `pending_ops` | ✅ **(new)** | `Compose.Send` → `op_id` |
| `attachments` | ❌ | Addressed by `(item_public_id, part_id)` — see below |
| `blobs` | ❌ | Internal; addressed by content hash |
| `item_text`, `mail_items`, `feed_items`, `drafts` | ❌ | Never addressed directly |

**Attachments are the deliberate exception.** An attachment has no stable identity of
its own — it is a *part of a message*, and `part_id` (the MIME part path, e.g. `"1.2"`)
is stable for the life of that message. `FetchAttachment(item_id, part_id)` already
addresses it this way, so a separate `public_id` would be an unused column. Record this
as intentional, or someone will "fix" it later.

**Why UUIDv7, restated:** time-ordered, so it sorts by creation, is kind to B-tree
inserts, and — new here — lets the CLI's 8-hex short id remain stable and collision-free
across all six tables, since the prefixes are drawn from a single time-ordered space.

---

## 3. §4 The unified `Item` — three fields added

v1's `Item` could not express ordering, the `\Answered` state that §7 promised the UI
would show, or the short id the CLI prints.

```rust
pub struct Item {
    pub id: PublicId,               // items.public_id (UUIDv7)
    pub kind: ItemKind,             // Mail | Feed
    pub account: AccountId,         // accounts.public_id
    pub title: String,              // never empty — see §3.1
    pub author: Option<String>,
    pub excerpt: Option<String>,    // derived at text-insert time — see §3.2
    pub date: Option<Timestamp>,    // published_at: Date header / feed published
    pub sort_at: Timestamp,         // ← NEW. The ordering key. See §3.3
    pub unread: bool,
    pub starred: bool,
    pub answered: bool,             // ← NEW. Mail only; false for feeds.
    pub has_attachments: bool,
    pub thread: Option<ThreadId>,   // threads.public_id
    pub body_state: BodyState,      // None | Pending | Cached | Failed
}

pub struct Thread {
    pub id: ThreadId,               // threads.public_id
    pub account: AccountId,
    pub subject: String,            // from subject_norm, un-normalised for display
    pub last_activity: Timestamp,
    pub item_count: u32,
    pub unread_count: u32,
}

pub struct Attachment {
    pub item: PublicId,             // the owning item
    pub part_id: String,            // the address within the item
    pub filename: Option<String>,
    pub mime_type: String,
    pub size_bytes: Option<u64>,
    pub is_pdf: bool,
    pub cached: bool,               // blob_hash IS NOT NULL
}
```

### 3.1 `title` is never empty

- Mail: `Subject` header, or `"(no subject)"`.
- Feed: entry title, else the link, else `"(untitled)"`.

A `String`, not an `Option<String>`, because the list view needs a row label and
`None` would force every renderer to invent a fallback. The DB column stays nullable;
the projection is not.

### 3.2 `excerpt` derivation

First 200 characters of the first non-empty paragraph of `item_text.body`,
whitespace-collapsed, trailing punctuation preserved. `None` when the body is empty or
unevicted. Computed in `store` when the text is written — not at read time, so the list
query stays a single indexed scan.

### 3.3 `sort_at` — the ordering key, and what `--sort` means

`sort_at` was already in the schema and already half the cursor, but it was absent from
`Item`, so a JSON consumer could not reproduce the ordering. Worse, `--sort date` and
`--sort received` were undefined. Pin them:

| Kind | `published_at` (display) | `sort_at` (ordering) |
|---|---|---|
| Mail | `Date` header | `INTERNALDATE` (received) |
| Feed | entry `published` | entry `published`, else `updated`, else fetch time |

`sort_at` is `NOT NULL`; every fallback chain ends at `created_at`, so it can never be
null and the cursor is never ambiguous.

CLI mapping:

| `--sort` | Order by |
|---|---|
| `date` (default) | `published_at DESC NULLS LAST`, then `sort_at DESC` |
| `received` | `sort_at DESC` |
| `author` | `author COLLATE NOCASE ASC` |
| `subject` | `title COLLATE NOCASE ASC` |

`date` is the default because it is what a user means by "newest", and for feeds the
two are usually identical.

### 3.4 Cursor

```rust
pub struct Cursor { pub sort_at: Timestamp, pub id: PublicId }
```

Encoded as `base64url(JSON)`, treated as **opaque** by clients. Keyset only; offsets are
never used (the daemon writes while a client reads).

---

## 4. §4 ContentStream — corrected predicate and a view

### The v1 predicate was wrong twice

```sql
-- v1, WRONG
kind = 'feed'
OR (kind = 'mail' AND folder_id IN (SELECT id FROM folders WHERE role = 'inbox'))
```

`items` has no `folder_id` — it lives on `mail_items` — so this does not run. And it has
no `hidden_at` guard, which `Items.Hide` requires.

### The fix: one view, used by every stream

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
remember. This is the single change most likely to prevent a class of bug.

### The canonical unified-inbox predicate, corrected

```sql
SELECT * FROM visible_items
WHERE kind = 'feed'
   OR (kind = 'mail'
       AND folder_id IN (SELECT id FROM folders WHERE role = 'inbox'))
ORDER BY sort_at DESC, public_id DESC
LIMIT ?;
```

`StreamSpec` is unchanged in shape, but every variant now resolves against
`visible_items`. `StreamSource::Folder(folder_id)` is `WHERE folder_id = ?`;
`StreamSource::Thread(thread_id)` joins on `items.thread_id`.

**A consequence to state explicitly:** a thread's `last_activity` and `item_count`
(§8) are computed over **visible** items. A hidden feed article must not keep a thread
at the top of the list.

---

## 5. §5 Schema — full revised DDL

This block replaces v1 §5 entirely. Changed lines are marked `-- [v2]`.

```sql
-- ================= accounts =================
CREATE TABLE accounts (
  id                          INTEGER PRIMARY KEY,
  public_id                   TEXT    NOT NULL UNIQUE,   -- [v2] UUIDv7
  kind                        TEXT    NOT NULL CHECK (kind IN ('imap','feed')),
  name                        TEXT    NOT NULL,
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
  public_id     TEXT    NOT NULL UNIQUE,                 -- [v2]
  account_id    INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  name          TEXT    NOT NULL,                        -- full IMAP path, e.g. "INBOX.Sent"
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

-- ================= feeds =================
CREATE TABLE feeds (
  id             INTEGER PRIMARY KEY,
  public_id      TEXT    NOT NULL UNIQUE,                -- [v2]
  account_id     INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  url            TEXT    NOT NULL,
  title          TEXT,
  site_url       TEXT,
  etag           TEXT,                                   -- conditional GET
  last_modified  TEXT,                                   -- conditional GET
  interval_secs  INTEGER,                                -- [v2] per-feed override
  last_fetch_at  INTEGER,
  next_fetch_at  INTEGER,
  backoff_secs   INTEGER NOT NULL DEFAULT 0,
  last_error     TEXT,
  UNIQUE (account_id, url)
);

-- ================= threads =================
CREATE TABLE threads (
  id             INTEGER PRIMARY KEY,
  public_id      TEXT    NOT NULL UNIQUE,                -- [v2]
  account_id     INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  root_msgid     TEXT,                                   -- Message-ID of the root, if known
  subject_norm   TEXT,                                   -- Re:/Fwd: stripped
  display_subject TEXT,                                  -- [v2] un-normalised, for the UI
  last_activity  INTEGER NOT NULL,                       -- max(visible sort_at)
  item_count     INTEGER NOT NULL DEFAULT 0,             -- visible items only
  unread_count   INTEGER NOT NULL DEFAULT 0,             -- visible items only
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
  excerpt        TEXT,                                   -- [v2] derived, see §3.2
  published_at   INTEGER,                                -- Date header / feed published
  sort_at        INTEGER NOT NULL,                       -- canonical sort key

  -- unified state (see §7)
  unread         INTEGER NOT NULL DEFAULT 1,
  starred        INTEGER NOT NULL DEFAULT 0,
  answered       INTEGER NOT NULL DEFAULT 0,             -- [v2] projection of \Answered
  hidden_at      INTEGER,                                -- [v2] NULL = visible. Suppression.

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
  to_addrs       TEXT,                       -- JSON array
  cc_addrs       TEXT,                       -- JSON array
  reply_to       TEXT,
  internal_date  INTEGER,                    -- [v2] INTERNALDATE; source of sort_at
  size_bytes     INTEGER,
  raw_blob       TEXT,                       -- blob hash of raw MIME, NULL if evicted
  flags_local    TEXT NOT NULL DEFAULT '',   -- [v2] custom keywords ONLY (see §7)
  UNIQUE (folder_id, uid)
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

-- ================= drafts (NEW) =================
-- A draft is local-only: it has no UID and no server counterpart until it is sent.
-- It is therefore NOT an item on the spine (see §11).
CREATE TABLE drafts (
  id             INTEGER PRIMARY KEY,
  public_id      TEXT    NOT NULL UNIQUE,                -- [v2] UUIDv7
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

-- ================= extracted text =================
CREATE TABLE item_text (
  item_id  INTEGER PRIMARY KEY REFERENCES items(id) ON DELETE CASCADE,
  title    TEXT NOT NULL DEFAULT '',
  author   TEXT NOT NULL DEFAULT '',
  body     TEXT NOT NULL DEFAULT '',         -- [v2] emptied on eviction, row retained
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

-- ================= attachments =================
CREATE TABLE attachments (
  id           INTEGER PRIMARY KEY,
  item_id      INTEGER NOT NULL REFERENCES items(id) ON DELETE CASCADE,
  part_id      TEXT    NOT NULL,             -- MIME part path, e.g. "1.2"; the address
  filename     TEXT,
  mime_type    TEXT    NOT NULL,
  size_bytes   INTEGER,
  content_id   TEXT,                         -- for inline parts
  is_pdf       INTEGER NOT NULL DEFAULT 0,
  blob_hash    TEXT,                         -- NULL until fetched
  UNIQUE (item_id, part_id)
);

-- ================= blob store =================
CREATE TABLE blobs (
  hash         TEXT PRIMARY KEY,             -- BLAKE3 of raw bytes
  size_bytes   INTEGER NOT NULL,
  refcount     INTEGER NOT NULL DEFAULT 0,   -- [v2] non-NULL references to this blob
  last_used_at INTEGER NOT NULL,             -- [v2] GC grace window (see §9)
  created_at   INTEGER NOT NULL
);

-- ================= offline operation queue =================
CREATE TABLE pending_ops (
  id           INTEGER PRIMARY KEY,
  public_id    TEXT    NOT NULL UNIQUE,      -- [v2] the stable op_id
  account_id   INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
  op           TEXT    NOT NULL CHECK (op IN ('flag','send','move','delete','expunge')),
  item_id      INTEGER REFERENCES items(id) ON DELETE CASCADE,
  payload      TEXT    NOT NULL,             -- JSON
  state        TEXT    NOT NULL DEFAULT 'pending'      -- [v2]
                       CHECK (state IN ('pending','in_flight','failed','done')),
  attempts     INTEGER NOT NULL DEFAULT 0,
  last_error   TEXT,
  created_at   INTEGER NOT NULL,
  finished_at  INTEGER                       -- [v2] set on done/failed
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
CREATE INDEX idx_items_acct_sort   ON items(account_id, sort_at DESC);
CREATE INDEX idx_items_thread      ON items(thread_id);
CREATE INDEX idx_items_unread      ON items(sort_at DESC)
                                   WHERE unread = 1 AND hidden_at IS NULL;   -- [v2]
CREATE INDEX idx_items_starred     ON items(sort_at DESC)
                                   WHERE starred = 1 AND hidden_at IS NULL;  -- [v2]
CREATE INDEX idx_mail_msgid        ON mail_items(message_id);
CREATE INDEX idx_mail_subjectnorm  ON mail_items(subject_norm);
CREATE INDEX idx_mail_folder       ON mail_items(folder_id);                 -- [v2]
CREATE INDEX idx_feeditems_feed    ON feed_items(feed_id);
CREATE INDEX idx_feeds_next        ON feeds(next_fetch_at) WHERE last_error IS NULL;
CREATE INDEX idx_attach_item       ON attachments(item_id);
CREATE INDEX idx_pending_acct      ON pending_ops(account_id, created_at);
CREATE INDEX idx_pending_state     ON pending_ops(state, created_at);        -- [v2]
CREATE INDEX idx_drafts_acct       ON drafts(account_id, updated_at DESC);   -- [v2]
```

**`idx_mail_folder` is new and load-bearing.** The corrected unified predicate filters
on `folder_id`, which v1 never indexed because it never joined on it.

**Blob `refcount` semantics (new definition).** `refcount` is the number of non-NULL
references to that hash from `mail_items.raw_blob` and `attachments.blob_hash`. It is
maintained **in application code inside the same transaction as the reference change**,
not by triggers — a trigger spanning two tables is harder to audit than a `store`
function. `debloatmailctl doctor` gains a reconciliation check: recompute the true count
and compare, because this is the kind of counter that drifts.

---

## 6. §6 Pragmas — one addition

Unchanged, plus:

```sql
PRAGMA user_version = 2;         -- schema version, checked at startup
```

`doctor` already reports the schema version; it should now refuse to run against a
database whose `user_version` is *newer* than the binary, rather than silently
mis-reading it.

---

## 7. §7 The flag model — `\Answered` gets a home, `flags_local` narrows

v1 said `\Answered` is "shown as replied", but `Item` had no field for it, so nothing
could show it. And `flags_local` was described as holding `\Answered` *and* custom
keywords, which duplicates the new column.

Revised table:

| `items` field | IMAP flag | Direction | Notes |
|---|---|---|---|
| `unread = 0` | `\Seen` | bidirectional | The only unread bit the UI sees |
| `starred = 1` | `\Flagged` | bidirectional | |
| `answered = 1` | `\Answered` | bidirectional **(changed)** | Set on reply; shown as "replied" |
| — | `\Draft` | derived | `folder.role = 'drafts'`, not a per-item flag |
| — | `\Deleted` | transient | We move/expunge; never persisted |
| — | custom keywords | server → local | Preserved verbatim in `flags_local` |

**`flags_local` now holds custom keywords only** (`$Junk`, `$NotJunk`, user labels).
`\Seen`, `\Flagged`, and `\Answered` are projections with their own columns and are
reconstructed on write-back; duplicating them in `flags_local` was a second unread
model in disguise, which principle 6 forbids.

**`\Answered` becomes bidirectional** because replying is a user action that should be
reflected to the server: `Compose.Send` with `in_reply_to` set queues a `flag` op adding
`\Answered` to the original item.

The pending-op-wins rule is unchanged and still applies to all three projections.

---

## 8. §8 Threading — `public_id` and visible-only counts

Add to the threading section:

- Threads carry a `public_id` (§5); `ThreadId` is that value.
- `display_subject` is stored alongside `subject_norm` so the thread list can show
  "Re: release checklist" rather than the normalised form used for grouping.
- `item_count`, `unread_count`, and `last_activity` are computed over **`visible_items`
  only**. A hidden article must not hold a thread at the top of the list.
- Recomputation is still end-of-sync, not incremental.

---

## 9. §9 Eviction — two changes that fix real bugs

### 9.1 Eviction empties the body; it does not delete the row

v1: "Evicting text sets `items.body_state = 'none'` and **deletes the `item_text` row**."

That silently removed the item from search. For a mail client whose bodies are an
evictable cache, that means **old mail becomes unsearchable over time** — a behaviour no
user would predict and none of the documents state.

Revised:

```sql
-- eviction of a mail body:
UPDATE item_text SET body = '' WHERE item_id = ?;   -- triggers keep FTS in sync
UPDATE items     SET body_state = 'none' WHERE id = ?;
```

`title` and `author` are retained, so subject and sender stay searchable forever; only
the body text is dropped. The row is deleted only on explicit purge (`delete --purge`).

**State this limitation explicitly in §10:** full-text search covers **cached bodies
only**. A mail item whose body has been evicted matches on title and author but not on
body content, until it is re-fetched. This is a deliberate consequence of the thin
cache, and it must be documented rather than discovered.

### 9.2 Blob GC uses a grace window, not a pin

v1 / `debloatmaild.md` §6: `FetchAttachment` "pins the blob's `refcount` on return" — but
no method releases it, so every fetched attachment would leak until the end of time.

Revised: **remove the pin entirely.** `FetchAttachment` updates `blobs.last_used_at` and
returns the path. GC rules:

- Eligible: `refcount = 0` **and** `last_used_at < now - 3600`.
- Never eligible: `refcount > 0`, or used within the last hour.

The one-hour window covers the gap between a caller receiving a path and opening the
file, which is milliseconds in practice. This removes a leak *and* a missing D-Bus
method in one change. No `Release` method is needed.

**Attachment blobs are bounded separately:** a size-capped LRU may `UPDATE attachments
SET blob_hash = NULL` on old items (which drops `refcount` and makes the blob
collectable), *except* for starred items and items with a pending op.

---

## 10. §10 Search — one addition

Append to the search section:

**Search covers cached bodies only.** Because mail bodies are evictable (§9.1),
a search for a phrase that appeared in an old mail body returns nothing once that body
has been evicted, even though the message still matches on subject or sender. This is
the price of the thin cache, and it is the correct price: the alternative is keeping
every body forever, which is the thing this project exists not to do.

Add a `doctor` check: report the ratio of `items` with `body_state = 'none'` to total,
so a user can see how much of their mail is currently unsearchable by body.

Everything else in §10 is unchanged.

---

## 11. §11 Sync — feed interval resolution and the upsert guard

### Feed poll interval resolution (fixes the `Feeds.Add(interval_secs)` orphan)

```
effective_interval = COALESCE(feeds.interval_secs,
                              accounts.poll_interval_secs,
                              global_default)
```

`Feeds.Add(url, name, interval_secs)` writes `feeds.interval_secs`. When it is `NULL` or
`0`, the account default applies. v1 had nowhere for that argument to go.

### Feed upsert must preserve suppression, not just read state

v1's rule — "upsert must not reset read state" — is necessary but not sufficient.
`Items.Hide` sets `hidden_at`; the next poll matches on `(feed_id, guid)` and must
leave `hidden_at` alone, exactly as it leaves `unread`/`starred` alone.

**The upsert updates content columns only.** The full list of columns the upsert must
never touch:

`unread`, `starred`, `hidden_at`, `body_state`, `thread_id`, `created_at`.

State this as a list, because "don't reset read state" invites a partial fix that still
resets suppression.

---

## 12. §11 (new subsection) — Drafts

`compose --save-draft` had no home. Add the `drafts` table (§5) rather than modelling
drafts as items on the spine, for one reason: **a draft has no UID.** `mail_items`
requires `(folder_id, uid, uidvalidity)` with `UNIQUE (folder_id, uid)`, so a local draft
would need a synthetic UID and a fake folder — two lies in the schema to satisfy a
constraint that does not apply.

A draft is mail-only and local-only, so a separate table violates nothing.

Lifecycle:

| Step | Effect |
|---|---|
| `SaveDraft` | Insert into `drafts`; return `draft_id` |
| `Send(draft_id)` | Read the draft, queue a `send` op, delete the draft row |
| `DeleteDraft(draft_id)` | Delete the row |

`Compose.Send` accepts **either** a `draft_id` **or** an inline `a{sv}` draft, so the
GUI can send without a save round-trip.

---

## 13. §11 (new subsection) — Pending ops: state and the send/append composite

### 13.1 Ops need a state, and terminal ops must survive

v1 deleted the row on success. But `Compose.Send` returns an `op_id` the caller may
check later ("did my message go out?"), and `debloatmaild.md` §9 promises exactly that.
If the row is deleted on success, the answer is always "not found."

Add `state` and `finished_at` (§5):

| State | Meaning |
|---|---|
| `pending` | Queued, not yet attempted |
| `in_flight` | Attempted at least once; may be retried |
| `failed` | Terminal failure after max attempts |
| `done` | Terminal success |

**Terminal rows (`done`, `failed`) are retained for 7 days**, then purged by a
housekeeping pass. `Queue.Pending()` returns `pending` + `in_flight` + `failed` (i.e.
"not done"). `Queue.Count` counts the same set.

This requires a D-Bus addition: `Queue.Get(s op_id) -> a{sv}` to query a specific op's
state. Flagged in §16 as a cross-document correction.

### 13.2 Send + append is one op, and success means "sent"

v1's `pending_ops.op` had no `append` variant, and the API spec flagged the ambiguity.
Resolve it by **not adding one**:

- `op = 'send'` with `payload.append_to_sent: bool` and `payload.sent_folder`.
- The op sends first, then appends.
- **Success is defined by the send.** If the append fails, the op still terminates as
  `done`, `last_error` records the append failure, and `Queue.Completed` carries
  `success = true` with a non-empty `detail` describing the missing Sent copy.

Rationale: reporting `success = false` after a successful send would imply the message
did not go out. That is the more dangerous error of the two — a user told a message
failed will send it again. The CHECK constraint is unchanged, and no new op kind is
needed.

---

## 14. §14 Mapping to Rust and the ABI — updated table

| Table | Owning crate | ABI-visible? |
|---|---|---|
| `accounts` | `store` | `AccountId` = `accounts.public_id` |
| `folders` | `store` | `FolderId` = `folders.public_id` |
| `feeds` | `store` | `FeedId` = `feeds.public_id` |
| `threads` | `store` → `core::Thread` | `ThreadId` = `threads.public_id` |
| `items` | `store` → `core::Item` | **Yes** — `Item` struct |
| `drafts` | `store` | `DraftId` = `drafts.public_id` |
| `pending_ops` | `store` / `sync` | `op_id` = `pending_ops.public_id` |
| `attachments` | `store` | `Attachment` metadata; addressed by `(item, part_id)` |
| `mail_items`, `feed_items`, `item_text`, `blobs` | `store` | No |

**`PublicId` is a newtype over `String`**, defined in `core`, with `FromStr`
validation. All six ABI-visible ids are `PublicId` at the boundary and are typed
distinctly (`AccountId(PublicId)`, `FolderId(PublicId)`, …) so a folder id cannot be
passed where an account id is expected. The D-Bus layer sees plain strings; the type
distinction lives in Rust and is erased at the wire.

**Enum token rule (new).** Three conventions existed with no stated mapping. Pin one:

| Enum | Rust | SQL / wire token |
|---|---|---|
| `ItemKind` | `Mail`, `Feed` | `mail`, `feed` |
| `BodyState` | `None`, `Pending`, `Cached`, `Failed` | `none`, `pending`, `cached`, `failed` |
| `AccountKind` | `Imap`, `Feed` | `imap`, `feed` |
| `FolderRole` | `Inbox`, `Sent`, `Drafts`, `Trash`, `Archive`, `Junk`, `All` | `inbox`, … |
| `OpKind` | `Flag`, `Send`, `Move`, `Delete`, `Expunge` | `flag`, `send`, `move`, `delete`, `expunge` |
| `OpState` | `Pending`, `InFlight`, `Failed`, `Done` | `pending`, `in_flight`, `failed`, `done` |
| `AuthKind` | `Password`, `OAuth2` | `password`, `oauth2` |
| `Security` | `Tls`, `StartTls`, `None` | `tls`, `starttls`, `none` |

**Rule:** the SQL `CHECK` values and the wire tokens are the *same lowercase
snake_case strings*. Rust uses PascalCase. Conversion happens in exactly two places —
`store` (SQL ↔ Rust) and `ffi` (Rust ↔ wire). There is no third convention, and no
token is ever free-form.

---

## 15. §15 Open questions — updated

Resolved by this patch (remove from the list):

- ~~Feed interval parameter has nowhere to go~~ → `feeds.interval_secs` (§11).
- ~~Drafts have no home~~ → `drafts` table (§12).
- ~~`\Answered` has no field~~ → `items.answered` (§7).
- ~~Sent-append has no op~~ → composite `send` op (§13.2).

Still open, unchanged:

1. **Attachment size cap.** Unchanged; affects the LRU in §9.2.
2. **Feed full-content fetching.** Unchanged, still needs a yes/no.
3. **CJK search tokenizer.** Unchanged.
4. **Thread merge policy** (mail thread vs. feed article sharing a subject). Current
   answer: never merge. Recorded as intentional.

New:

5. **Search completeness vs. eviction.** §9.1 makes evicted mail bodies unsearchable by
   body. Is that acceptable, or should `search` transparently offer to fetch-and-search
   bodies on demand? Decide before the search UI is designed, because it changes what
   the search box can promise.
6. **Terminal op retention window.** 7 days is a guess. What happens to a `failed` op
   that is never retried — does it surface anywhere, or expire silently?

---

## 16. Cross-document corrections this patch requires

These are edits to `debloatmaild.md` and `debloatmailctl.md`, not to this file. Listed
so they are not lost.

**`debloatmaild.md`:**

| § | Correction |
|---|---|
| §5, §6 | `Move`/`Copy` take a **folder `public_id`**, not a name. The CLI resolves names and roles to ids client-side (it can read the DB). One addressing scheme. |
| §7 | `Accounts.Add` config keys must include `username`, `auth_kind`, and `allow_invalid_cert_loopback` — all three are required to authenticate and are missing from the documented key list. |
| §8 | `Feeds.Add(interval_secs)` now lands in `feeds.interval_secs` (§11). |
| §9 | Remove "raises `Error.Unsupported`" for `SaveDraft` — drafts now have a home. |
| §10 | Add `Queue.Get(s op_id) -> a{sv}` (§13.1). |
| §11 | `Features` must not advertise `oauth` while OAuth is unimplemented (`architecture.md` §7). Advertise implemented capabilities only. |
| §15 | Cite the correct section for `ApiVersion` versioning; "(§15)" points at Errors. |
| §14 | `mailto:` is handled once — via `Application.Open`; `UI.Compose` is for internal pre-fill, not mailto routing. |

**`debloatmailctl.md`:**

| § | Correction |
|---|---|
| §6 | The JSON `date` field is `published_at`; add `sort_at` (or rename) so ordering is reproducible. `thread_id` is now a real UUIDv7 (§3). |
| §8 | The `search 'kind:feed …'` example uses an operator §9 does not define. Either define `kind:` or change the example to `is:feed`. |
| §8 | `delete --purge` on a mail item means "delete the local row"; on a feed item, `delete` already means "hide". Use `hide` explicitly or document the overload. |
| §10 | Method/signal names are stale: `Mark` → `MarkRead`/`SetStarred`; `Sync(scope, full, wait)` has no `wait`; signals are `Items.Changed`, `Sync.Progress`, `Sync.Finished`. |
| §10 | Writes go to `org.noahie.debloatmail.Daemon`. |
| §13 | The three gaps it raised are resolved here; the section becomes a pointer to this patch. |

**`architecture.md`:**

| § | Correction |
|---|---|
| §6 | "keeps the database at a few MB" contradicts this document's §13 (60–90 MB typical). Change to "tens of MB" or point at §13. |
| §9 | The diagram shows `debloatmailctl` touching only SQLite; it writes over D-Bus. |
| §9 | "The daemon is the reference implementation" contradicts `debloatmailctl.md` §1 and the build order in §15. Pick one — the CLI is built first, so the CLI is the reference implementation and the daemon shares its engine. |
| §12 | The App ID must match **two** D-Bus service names, not one. |
| §15 | Build order puts search last; the CLI spike requires search before any QML. Reconcile. |

---

## 17. Verification checklist

After applying, these must all hold. Each maps to a claim the API specs make.

- [ ] `SELECT` of every `public_id` in `accounts`, `folders`, `feeds`, `threads`,
      `items`, `pending_ops` is `NOT NULL` and `UNIQUE`.
- [ ] No D-Bus method in `debloatmaild.md` Appendix A takes an id that is not a
      `public_id` string (except `part_id`).
- [ ] The unified-inbox query runs and returns feed items plus inbox mail, and excludes
      rows with `hidden_at IS NOT NULL`.
- [ ] Hiding a feed item and re-polling its feed leaves `hidden_at` set.
- [ ] `Compose.Send` → `Queue.Get(op_id)` returns a terminal state after the message
      goes out.
- [ ] Evicting a mail body leaves `title`/`author` searchable and the item present in
      `visible_items`.
- [ ] `doctor`'s blob refcount reconciliation passes after a fetch/evict cycle.
- [ ] No enum token appears in the database or on the wire in any case other than the
      lowercase snake_case form in §14.
