-- ================= meta: global counters =================
CREATE TABLE meta (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);
INSERT INTO meta (key, value) VALUES ('revision', '0');

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

PRAGMA user_version = 1;
