# Glossary

The project's controlled vocabulary: what each term means, where it is defined, and —
where two documents use the same word differently — which meaning is authoritative.

Every document in this set asserts a distinction in passing and then relies on it.
`architecture.md` §8 says "add it to the stream, not to one source." `SYNC.md` §4.1 says
"jobs are not ops." `DATA-MODEL.md` §9 says "durability is asymmetric."
`MIME-AND-TEXT.md` §5.1 says `section_id` "is the IMAP section number, not an array
index." Each of those is a one-line rule guarding a class of bug, and none of them is
collected anywhere. A developer who reads "stream" in `architecture.md` and "stream" in
`DATA-MODEL.md` will assume they are the same thing, and they will be wrong in a way
that compiles.

**Scope of authority.** This file owns *terminology*. It does not own any mechanism: the
D-Bus signatures are `debloatmaild.md`, the schema is `DATA-MODEL.md` revision 3, the
engine is `SYNC.md`, the parser is `MIME-AND-TEXT.md`, the client is `UI.md`, the
threat model is `SECURITY.md`, the CLI is `debloatmailctl.md`. Where this file and
another disagree about **what a word means**, this file wins. Where they disagree about
**how something works**, the cited document wins, and §7 records the conflict rather
than papering over it.

---

## 1. How to use this glossary

Three rules, in order of authority:

1. **A term in `code` font is a literal identifier** — a column, a method, a token, a
   type. `hidden_at` is the column; "suppression" is the concept it implements. The
   reverse also holds: when a document writes `Pending` (Rust) and `pending` (SQL/wire),
   they are the same enum value in two cases (§2.1).
2. **Prefer the definition's exact word.** If this glossary says the thing on disk is a
   *blob* and the thing that points at it is a *blob hash*, then "the attachment's blob"
   is ambiguous and "the attachment's `blob_hash`" is not.
3. **When in doubt, the distinction is the point.** Nearly every entry below exists
   because two nearby concepts are easy to conflate and expensive to conflate.

---

## 2. The naming rules

### 2.1 One token per enum value, in two cases

Three conventions existed with no stated mapping. `DATA-MODEL.md` §14 pins one:

| Layer | Case | Example |
|---|---|---|
| Rust type / variant | `PascalCase` | `BodyState::Cached`, `ItemKind::Mail` |
| SQL `CHECK` value **and** D-Bus wire token | `lowercase snake_case` | `'cached'`, `'mail'` |

**The SQL value and the wire token are the same string.** Conversion happens in exactly
two places — `store` (SQL ↔ Rust) and `ffi` (Rust ↔ wire) — and there is no third
convention. No token is ever free-form.

The full mapping (`DATA-MODEL.md` §14):

| Enum | Rust | Token |
|---|---|---|
| `ItemKind` | `Mail`, `Feed` | `mail`, `feed` |
| `BodyState` | `None`, `Pending`, `Cached`, `Failed` | `none`, `pending`, `cached`, `failed` |
| `AccountKind` | `Imap`, `Feed` | `imap`, `feed` |
| `FolderRole` | `Inbox`, `Sent`, `Drafts`, `Trash`, `Archive`, `Junk`, `All` | `inbox`, `sent`, `drafts`, `trash`, `archive`, `junk`, `all` |
| `OpKind` | `Flag`, `Send`, `Move`, `Delete`, `Expunge` | `flag`, `send`, `move`, `delete`, `expunge` |
| `OpState` | `Pending`, `InFlight`, `Failed`, `Done` | `pending`, `in_flight`, `failed`, `done` |
| `AuthKind` | `Password`, `OAuth2` | `password`, `oauth2` |
| `Security` | `Tls`, `StartTls`, `None` | `tls`, `starttls`, `none` |

### 2.2 The id vocabulary

Six things are called "an id" and they are not interchangeable
(`DATA-MODEL.md` §2, `debloatmailctl.md` §5, `debloatmaild.md` §6, §9):

| Name | What it is | Stable? | Crosses the ABI? |
|---|---|---|---|
| `<table>.id` | SQLite rowid | No | **Never** |
| `<table>.public_id` | UUIDv7 | **Yes** | **Yes** — the only id a client sees |
| **short id** | first 8 hex of a `public_id` | Derived | Yes (CLI-only convenience) |
| **natural key** | `(folder_id, uidvalidity, uid)` / `(feed_id, guid)` | Semi | No |
| `job_id` | `u`, per-run, in memory | **No** | Yes (transiently) |
| `op_id` | `pending_ops.public_id` | **Yes** | Yes (durable) |

`PublicId` is a newtype over `String` in `core` with a validating `FromStr`. The
**seven** ABI-visible ids are typed distinctly (`AccountId(PublicId)`,
`FolderId(PublicId)`, `ItemId(PublicId)`, `FeedId(PublicId)`, `ThreadId(PublicId)`,
`OpId(PublicId)`, `DraftId(PublicId)`) so a folder id cannot be passed where an account
id is expected; the distinction is **erased at the wire**, where everything is a plain
string. (The old count of six predated the `drafts` table, `DATA-MODEL.md` §11.5.)
`section_id` is the exception to "typed PublicIds": part addressing is an integer-list
string (`"1.2"`), not a database identity, and it crosses the wire verbatim
(`debloatmaild.md` §6, §17).

**"Id" with no qualifier is a bug in a document.** Say which one.

---

## 3. Confusable pairs

The load-bearing table. Read this before writing code that touches either side.

| Pair | The distinction | The bug if conflated |
|---|---|---|
| **spine** vs **stream** | `items` is the spine (one row per message *or* article). A `ContentStream` is a *query* over it. | Materializing a stream into a table; two sources of truth. |
| **job** vs **op** | A job is in-memory, transient, `job_id` (`u`). An op is a `pending_ops` row, durable, `op_id` (`s`). | Persisting a `job_id`, or making `op_id` transient — a lost message. |
| **durable** vs **cache** | `item_text.durable = 1` (feed content) is never evicted. `durable = 0` (mail bodies) is evictable. | Getting it backwards deletes feed text forever. |
| **`items.published_at`** vs **`items.sort_at`** | Display date vs ordering key. Mail: `Date` header vs `INTERNALDATE`. | Sorting by the wrong one; a message in the wrong place. |
| **`Item.date`** vs **`Item.sort_at`** | The Rust field `date` *is* `published_at`; `sort_at` is a separate field. The CLI JSON emits **both**, named `published_at` and `sort_at` — the one-word field `date` is dead (§5.1). | A consumer assuming the JSON `published_at` is the order key, or a client paging with a stale cursor. |
| **`subject_norm`** vs **`display_subject`** vs **`title`** | Normalised (threading only) vs un-normalised (UI) vs the `Item` field (decoded `Subject`). | Overwriting one with the other breaks threading or shows `Re: Re: Re:`; building a reply subject on `subject_norm` produces lowercase subjects. |
| **`section_id`** vs **array index** | `section_id` *is* the RFC 3501 §6.4.5 section number (`"1.2"`), usable verbatim in `BODY.PEEK[section_id]`. It was `part_id`; the name that suggested an index is dead (§5.1). | Treating it as an index fails the moment a user clicks a nested attachment. |
| **`pending` (body)** vs **`pending` (op)** | `body_state` ∈ none/pending/cached/failed. `pending_ops.state` ∈ pending/in_flight/failed/done. | Two different enums, one word, two tables. |
| **`cached` (body)** vs **`cached` (attachment)** vs **cached articles** | `body_state='cached'` = extraction ran. `attachments.blob_hash IS NOT NULL` = the file was fetched. "Cached articles" = `feed_items` rows. | Treating a cached article as a fetched blob. |
| **`delete`** vs **`hide`** vs **`purge`** vs **`expunge`** | `delete` = mail items → trash (the `expunge` argument additionally removes server-side). `hide` = feed-item suppression (`hidden_at`). `purge` = the **argument** on `Feeds.Remove`/`Accounts.Remove`: drop the row and cascade. There is no free-standing purge verb. | Calling the wrong verb silently does nothing useful — and both are hard errors on the wrong kind (exit 2), so the bug is user-visible in seconds, not a data event. |
| **account `kind='feed'`** vs **a feed** | A `feed` account is a named *group* with no connection of its own. A `feed` row is a subscription inside it. | "Add a feed" means `Feeds.Add`, not `Accounts.Add(kind=feed)`. |
| **role** vs **kind** | `folders.role` ∈ inbox/sent/…. `items.kind` ∈ mail/feed. | Filtering items by a folder role. |
| **`excerpt`** vs **`snippet`** | `excerpt` is stored, derived once. `snippet()` is FTS5, computed per query. | A Search stream that returns the stored excerpt shows nothing about the match. |
| **`message_id`** vs **`Message-ID`** | The column drops the angle brackets. The header keeps them. | Double-wrapping; a dedup key that never matches. |
| **natural key** vs **`Message-ID`** | Identity is `(folder_id, uidvalidity, uid)`. `Message-ID` is a *secondary* key. | Dropping `uidvalidity` from the natural key rewrites public_ids after a reset; using `Message-ID` as identity breaks on messages lacking one. |
| **watch connection** vs **work connection** | The watch connection IDLEs and runs no other command. Everything else uses the second connection. | Sharing them blocks IDLE or tears it down per fetch. |
| **`--offline`** vs **`--no-daemon`** | `--offline` forbids network for this invocation. `--no-daemon` makes the CLI the writer (standalone mode, exclusive `flock`). | Orthogonal; conflating them breaks both. |
| **initial** vs **incremental** vs **`--full`** | Never-synced (`UID FETCH 1:*`) vs `UIDNEXT` advanced vs discard cached sync state. | `--full` must
