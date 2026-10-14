# debloatmailctl — Command Specification

The command-line interface to debloatmail. This document is the contract for the
binary: its surface, its output, its exit codes, and the seam it shares with the
daemon.

It is written to be built **first** — before any QML exists. It is the engine's
**first customer** (a daemon is not a reference implementation of anything, and the
daemon owns the protocol — `architecture.md` §9), and it is the regression suite for
the hard parts (authentication, IMAP state, MIME parsing, the unified model).

---

## 1. Purpose and scope

`debloatmailctl` is a **scriptable, stateless command interface** to a local-first mail
and feed store. It is not a TUI and has no event loop — every invocation runs, prints,
and exits, composable with pipes and scripts.

Three jobs, in priority order:

1. **Exercise the engine headlessly.** If a feature cannot be driven from here, it is
   not ready for a GUI.
2. **Be the power-user interface.** Sync, search, filter, and script across the unified
   stream without a window.
3. **Provide a stable test surface.** Every behavior is assertable against a fixture
   database with no network.

**Explicit non-goals:** no interactive full-screen UI (that is a future `debloatmail-tui`
front-end), no HTML rendering, no calendar or contacts.

### Relationship to the GUI

The GUI and the CLI are peers over the same store and the same daemon. Neither is
privileged. Anything the GUI can do, `debloatmailctl` can do; anything the CLI can do
should be expressible in QML over the `ffi` ABI. If they diverge, the ABI is wrong.

---

## 2. Design principles

| Principle | Consequence |
|---|---|
| **Reads are local and direct** | Listing and searching read SQLite directly (`SQLITE_OPEN_READ_ONLY`). No daemon round-trip, no network. The default sort is index-covered (`idx_items_date`, `DATA-MODEL.md` §5), so a page scan stays sub-10 ms at the §13 budgets. |
| **Writes go through the daemon** | Marking, moving, deleting, hiding, sending, sync, account and feed changes are D-Bus calls to `org.noahie.debloatmail.Daemon`, preserving the single-writer rule (`DATA-MODEL.md` §6). The CLI never opens the database for writing, even in standalone mode's *reads*. |
| **Never surprise a script** | Output format is stable and never depends on TTY state. Human output by default; machine formats opt-in. |
| **Offline is a first-class mode** | `--offline` forbids network access entirely and errors rather than silently fetching (exit 7). |
| **Plaintext only** | Bodies print as extracted text. No HTML, ever. |
| **Fail loudly, not cleverly** | An unknown search operator is an error, not a silently-dropped term. An ambiguous id prefix is an error, not a guess. |

**The read/write split is the central design decision.** It means `debloatmailctl list`
never touches the network or the daemon — so it works with the daemon stopped, in a
cron job, or over a slow link. Bodies that are not cached are the one exception: they
require a fetch, which is an explicit daemon call followed by a re-read (§10).

**Name resolution is client-side, ids are what crosses the wire.** The CLI reads the
database directly, so it resolves every human name (account, folder, feed, item short
id) to a `public_id` *before* calling the daemon. D-Bus methods take `public_id`
strings and nothing else (`debloatmaild.md` §3, §17 — name-taking methods would reopen
the injection surface the daemon refuses to own).

---

## 3. Invocation and global flags

```
debloatmailctl [GLOBAL OPTIONS] <COMMAND> [ARGS]
```

| Flag | Meaning |
|---|---|
| `-a, --account <NAME\|ID>` | Scope to one account. Resolved client-side. Default: all accounts (unified). |
| `-c, --config <PATH>` | Config file. Repeatable; multiple paths are deep-merged left-to-right, `:`-separated in one argument. Matches the pimalaya convention [^ec53a6]. |
| `--db <PATH>` | Override the database path. Primarily for tests. |
| `--offline` | Never touch the network. Any command that would need it exits 7 — including fetch-backed reads *and* `sync`/`feed add`/`feed refresh`/`account check`/`compose --now`. |
| `--no-daemon` | Do not use D-Bus; run write operations in-process. The CLI becomes a writer — takes the exclusive lock (§10). |
| `--format <human\|json\|jsonl\|plain>` | Output format. Default `human`. |
| `--color <auto\|always\|never>` | Default `auto`. `NO_COLOR` is honored; `--color always` overrides it. |
| `-q, --quiet` | Suppress non-essential output. Errors still print to stderr. |
| `-v, --verbose` | Increase logging verbosity. Repeatable (`-vv`, `-vvv`). |
| `--debug` | Log at debug level, including protocol traces with credentials redacted (`SECURITY.md` §7.3). |
| `--timeout <DURATION>` | Overall operation timeout, e.g. `30s`, `5m`. Default `60s`. **The default is contractual for `feed add`:** it is the deadline at which the daemon's internal feed validation times out and returns `validated: false` instead of failing the call (`debloatmaild.md` §8, §19). |
| `-y, --yes` | Assume yes for destructive confirmations. Required for destructive commands when stdin is not a TTY. |
| `--dry-run` | Print what would happen; change nothing. |
| `-h, --help` / `-V, --version` | Standard. |

**Ordering:** global flags may appear before or after the subcommand.

---

## 4. Paths and configuration

| Purpose | Path | XDG variable |
|---|---|---|
| App config (TOML) | `debloatmail/config.toml` | `$XDG_CONFIG_HOME` |
| **Database** (items, feed text, FTS) | `debloatmail/mail.db` (+ `-wal`, `-shm`) | `$XDG_DATA_HOME` |
| Blob store (raw MIME, attachments) | `debloatmail/blobs/<ab>/<hash>` | `$XDG_CACHE_HOME` |
| Logs, last-sync state, **the standalone lock file** | `debloatmail/` | `$XDG_STATE_HOME` |
| Secrets | Secret Service portal (keyring) | — |

**The database belongs in `DATA`, not `CACHE`.** This is not cosmetic. Feed article text
is *durable* data that cannot be re-fetched once the feed rotates (`DATA-MODEL.md` §9), so
the database must survive a cache wipe. Only evictable content — raw MIME and
attachments, both re-fetchable from IMAP — belongs in `CACHE`. Putting the DB in
`CACHE` would silently destroy the user's feed archive. The standalone lock file, in
contrast, is transient state: it is in `STATE`, not beside the database
(`SECURITY.md` §8.5).

### Accounts

Accounts are stored in the SQLite `accounts` table, not in the TOML file. The TOML file
holds application preferences only (poll intervals, cache cap, UI defaults).

Rationale: accounts carry per-account sync state (per-folder `uidnext`, `highestmodseq`,
feed `etag`, `backoff_secs`) that must live transactionally with the data, and the
`secret_ref` pointer must never be separated from the account it belongs to.
**Secrets are never in the TOML file** — only a keyring handle.

*Known tradeoff:* accounts are not human-editable in a text file, and not
version-controllable. Accepted for beta; revisit if it becomes friction.

---

## 5. Addressing items

Every item has a `public_id` (UUIDv7, `DATA-MODEL.md` §3). Users should never type a
UUID, so commands accept a **short id**: the first 8 hex characters, resolved as a
unique prefix against the database, git-style.

```
$ debloatmailctl list --limit 3
  3f2a9c1e  2026-10-07  Alice Chen        Re: release checklist
  7b0d44a2  2026-10-06  Rust Blog         Async cancellation in practice
  c19e88f5  2026-10-05  Bob Martinez      Invoice #2291

$ debloatmailctl read 3f2a          # unique prefix → resolved
$ debloatmailctl read 3f2a9c1e      # full short id
$ debloatmailctl read 018f3a2b-…    # full UUID also accepted
```

**Ambiguous prefixes error; they never guess** (exit 9, §7):

```
$ debloatmailctl read 3f
error: id prefix '3f' is ambiguous (3 matches: 3f2a9c1e, 3f8b1022, 3fcc4410)
```

This is why short ids are not sequential integers: the daemon inserts rows underneath
the user between commands, so a positional number would silently change meaning. A
content-derived prefix cannot.

Resolution happens client-side (the CLI reads the DB directly). The daemon never sees
an ambiguous form — short ids do not cross the wire; only resolved `public_id`s do,
which is also why the daemon's interface has no ambiguity error at all
(`debloatmaild.md` §15).

Commands taking multiple items accept them space-separated or from stdin:

```
$ debloatmailctl mark read 3f2a 7b0d c19e
$ debloatmailctl list --unread --format plain --print0 | debloatmailctl mark read -
```

The literal `-` means "read ids from stdin, one per line."

**Accounts, folders, and feeds are addressed by name** in CLI arguments (that is the
human surface — `accounts.name` is `UNIQUE`, `DATA-MODEL.md` §5); a full id is also
accepted. The same resolution rule applies: unique match or exit 3.

---

## 6. Output and formats

### `human` (default)

Right-aligned columns, ANSI color when a TTY. Designed for reading, not parsing —
**scripts must not parse this.** It may use display names freely.

### `json`

A single structured document. Stable key names, treated as an API.

```json
{
  "items": [
    {
      "id": "018f3a2b-7c41-7e2a-9d3f-1a2b3c4d5e6f",
      "short_id": "3f2a9c1e",
      "kind": "mail",
      "account": "018f3a9c-e4f2-7a1b-8c3d-9f0a1b2c3d4e",
      "account_name": "fastmail",
      "folder": "018f3aa1-…",            // null for feed items
      "title": "Re: release checklist",
      "author": "Alice Chen",
      "published_at": "2026-10-07T14:22:11Z",
      "sort_at": "2026-10-07T14:25:02Z",
      "unread": true,
      "starred": false,
      "has_attachments": true,
      "thread_id": "019f3a2b-7d52-7e1c-9a4b-2c3d4e5f6a7b",
      "body_state": "cached"
    }
  ],
  "cursor": "eyJ0IjoxNzYwMDEx…",
  "has_more": true
}
```

**Id field rules** — the point of the field is that a script can address the row it
was just printed:

- Every id field (`id`, `thread_id`, `account`, `folder`, cursors' contents) is a
  **full `public_id` UUIDv7**. Never a rowid, never a name (`DATA-MODEL.md` §3).
- Display names appear only in dedicated `*_name` fields, which are never valid input
  by that field's name.
- **Both dates are present**: `published_at` (the display date) and `sort_at` (the
  ordering key, `DATA-MODEL.md` §4.2.3), so a consumer can reproduce the exact
  ordering — including the default `date` sort, which orders by `published_at` first.
- Enum values use the wire tokens (`mail`/`feed`; `none`/`pending`/`cached`/`failed`
  for `body_state`) — the §14 convention of `DATA-MODEL.md`.

### `jsonl`

One object per line (the item object, no envelope). Preferred for large result sets and
streaming — the consumer can process items as they arrive rather than buffering a whole
document.

### `plain`

Tab-separated, no headers, no color, no truncation: `short_id`, `published_at`,
`author`, `title`. The `xargs` format. `--print0` instead prints `id` NUL-separated —
ids, so pipelines back into commands that take ids, without ever parsing a row.

**Timestamps are ISO 8601 UTC** in machine formats. Never local time, never a bare
epoch — formatting is the consumer's job.

**Machine formats never truncate and never color.** If `--format json` were affected by
terminal width, output would differ between an interactive run and a piped one, which
is exactly the class of bug that makes a tool untrustworthy in scripts.

---

## 7. Exit codes

| Code | Meaning |
|---|---|
| `0` | Success |
| `1` | Unexpected internal error |
| `2` | Usage error (unknown flag, bad arguments, unknown search operator, a feed id where a mail item is expected) |
| `3` | Not found (item, folder, feed, account, or op) |
| `4` | Authentication failed |
| `5` | Network or TLS failure |
| `6` | Daemon required but unavailable |
| `7` | Offline mode: data not cached, or the command requires the network |
| `8` | Lock held by another process (standalone mode) |
| `9` | Ambiguous id prefix |

Every non-zero exit writes a single-line message to stderr, prefixed `error:`. With
`--format json`, errors are emitted as `{"error": {"code": "...", "message": "..."}}` to
**stdout** and the exit code is still set, so a caller can use either mechanism.

`code` reuses the daemon's error names where the failure originated there —
`not_found`, `invalid_argument`, `not_authenticated`, `network_error`, `busy`,
`unsupported`, `queue_full`, `internal` (`debloatmaild.md` §15) — plus these
client-side codes the daemon can never raise: `usage`, `ambiguous_id`,
`offline_data`, `daemon_unavailable`, `lock_held`.

**What was removed and why:** the daemon spec's dead codes `AmbiguousId` and
`Offline` never appear above *as daemon errors*, because both conditions — short-id
resolution and `--offline` — are entirely client-side (the daemon never sees prefixes,
and `--offline` never reaches it). They now live exactly once, here, as client codes.

---

## 8. Command reference

### Synopsis

```
debloatmailctl account    list | add | update | remove | check | show
debloatmailctl folder     list | sync | stats
debloatmailctl feed       list | add | update | set-url | remove | refresh | show
debloatmailctl list                                    ★ the unified stream
debloatmailctl read       <id> [--headers] [--raw] [--mark-read] [--thread]
debloatmailctl search     <query> [list options…]
debloatmailctl mark       read | unread | starred | unstarred <ids…>
debloatmailctl move       <ids…> --to <FOLDER>
debloatmailctl copy       <ids…> --to <FOLDER>
debloatmailctl delete     <ids…> [--expunge]
debloatmailctl hide       <ids…>
debloatmailctl compose    [--to …] [--edit]
debloatmailctl reply      <id> [--all] [--quote]
debloatmailctl forward    <id> [--attach-original]
debloatmailctl attachment list | download | open
debloatmailctl sync       [--account] [--folder] [--feed] [--wait] [--full]
debloatmailctl daemon     status | stop | reload | logs
debloatmailctl config     path | show | validate | get | set
debloatmailctl doctor     [--json]
debloatmailctl generate   <completions|man|schema> [shell|dir]
debloatmailctl version
```

**Naming rule:** top-level verbs for the things done constantly (`list`, `read`,
`search`, `sync`, `compose`); noun groups for the rest (`account`, `folder`, `feed`,
`attachment`, `daemon`, `config`). Typing `debloatmailctl list` should never require
knowing the taxonomy.

**Two ways to stop seeing a feed item, and one command each.** `delete` is mail-only
(trash/expunge); `hide` is feed-only. The old overload — `delete` meaning "hide" on
feeds and "purge local row" via `--purge` on mail — is gone: delete's two kinds of
behavior had two different data-model meanings (`DATA-MODEL.md` §11.2 vs §7), and
`--purge`'s local-row-deletion has no daemon method behind it and no local write
privilege to do it. Overload removed, not documented.

---

### `list` — the unified stream

The headline command. Emails and feed articles in one ordered list.

```
debloatmailctl list [OPTIONS]
```

| Option | Meaning |
|---|---|
| `--mail` / `--feed` | Restrict to one kind. Default: both. |
| `--unread` / `--starred` | Filter by state. |
| `--folder <NAME\|ROLE>` | Mail folders only. Roles: `inbox`, `sent`, `drafts`, `trash`, `archive`, `junk`, `all`. A role is matched against `folders.role` across the `-a` scope. |
| `--feed <NAME>` | One feed. |
| `--since <DATE>` / `--before <DATE>` | Bounds on `published_at` (the display date), ISO 8601 or relative (`7d`, `2w`). |
| `--threads` | Collapse to one row per conversation (the row shows the thread's `display_subject`). |
| `--sort <date\|received\|author\|subject>` | Default `date`, per the ordering tuples of `DATA-MODEL.md` §4.2.3. |
| `-n, --limit <N>` | Default 25. `--all` for everything (beware). |
| `--cursor <TOKEN>` | Keyset cursor from prior output. |
| `--print0` | In `plain` format, print `id` NUL-separated instead of tab-separated columns (§6). |

Default view is the unified inbox: mail in `role = 'inbox'` folders across all
accounts, plus all feed items (`DATA-MODEL.md` §4.4's canonical predicate — which
excludes hidden rows). Read and unread both shown; `--unread` filters.

**Paging uses keyset cursors, never offsets.** The daemon writes while you read, so an
`OFFSET 50` on a shifting table skips and duplicates rows. The cursor is opaque
(base64url JSON, `DATA-MODEL.md` §4.3) and **encodes the sort key it was minted
under**; applying a `--sort author` cursor to a `--sort received` run is an error
(exit 2), never a silent re-order. Because each ordering tuple ends in the rowid
tiebreaker, paginated streams are stable under concurrent daemon writes.

```
$ debloatmailctl list --unread --limit 20
$ debloatmailctl list --feed --since 7d
$ debloatmailctl list --threads --sort received
```

---

### `read`

```
debloatmailctl read <id> [--headers] [--raw] [--mark-read] [--thread]
```

Prints the plaintext body to stdout. If the body is not cached, it is fetched on demand
through the daemon (§10), then re-read from SQLite and printed.

| Option | Meaning |
|---|---|
| `--headers` | Print headers before the body (including `From`, which is `mail_items.from_addr`; for feeds, the entry's author/link). |
| `--raw` | Mail only (feed items: exit 2). Print the raw MIME from the blob store. If the raw blob was evicted, the CLI performs the body-fetch first (which re-populates it, `SYNC.md` §6.1) and then prints. |
| `--mark-read` | Mark as read. **Off by default** (see below). |
| `--thread` | Print the whole conversation, oldest first (each part as `--headers` + body). |

**`read` does not mark as read by default.** This is a deliberate reversal of the
`mutt`/`himalaya` convention. In a scripting tool, `debloatmailctl read 3f2a | grep -q X`
silently clearing unread state is a data-loss-shaped surprise. Marking read is an
explicit intent and gets an explicit flag. The GUI marks read on display
(`UI.md` §8.5), which is correct there because display *is* the intent.

Under `--offline`, an uncached body is exit 7 — `read` never fetches.

---

### `search`

```
debloatmailctl search <query> [list options…]
```

Full-text search over extracted text for both kinds, backed by FTS5, and **entirely
local** — search never touches the network or the daemon. Accepts a Gmail-style filter
language (§9) that compiles to FTS5 plus SQL predicates.

```
$ debloatmailctl search "borrow checker" --unread
$ debloatmailctl search 'from:alice is:starred after:2026-01-01'
$ debloatmailctl search 'is:feed author:"rust blog" "async"'
$ debloatmailctl search 'invoice -draft' --format jsonl
```

`--explain` prints the generated SQL and FTS5 query instead of results. Search bugs are
otherwise near-impossible to diagnose, and this costs almost nothing to implement.

**One honest limit, stated:** header sync writes `item_text(title, author, body = '')`
(`DATA-MODEL.md` §11.1), so title and author search work for **every** message from
first sync — but body text is searchable only once the body has been fetched (and, for
mail, until eviction empties it, §9.2 of the data model). A user who has never opened a
message cannot search its body. This is inherited from the cache design
(`DATA-MODEL.md` §15, open question 5), not a CLI defect.

---

### `mark`

```
debloatmailctl mark read|unread|starred|unstarred <ids…>
```

`seen`/`unseen` are accepted aliases for `read`/`unread`. This is one verb over two
daemon methods: `read`/`unread` → `Items.MarkRead(ids, true|false)`,
`starred`/`unstarred` → `Items.SetStarred(ids, true|false)` (§10). For mail the daemon
also queues the IMAP `UID STORE`; for feed items the change is local-only, which is
invisible here (`DATA-MODEL.md` §7).

The daemon returns the count of items actually affected. The CLI prints it
(`marked 3`) and exits 3 if any id did not resolve — partial application is normal and
must be verifiable from the output.

```
$ debloatmailctl mark read 3f2a 7b0d
$ debloatmailctl list --unread --format plain --print0 | debloatmailctl mark read -
```

---

### `move`, `copy`, `delete`, `hide`

```
debloatmailctl move    <ids…> --to <FOLDER>
debloatmailctl copy    <ids…> --to <FOLDER>
debloatmailctl delete  <ids…> [--expunge]
debloatmailctl hide    <ids…>
```

- `move`/`copy` are mail-only; they error on feed items (exit 2) rather than silently
  doing nothing. `--to` takes a folder **name or role**; the CLI resolves it to the
  folder's `public_id` client-side (one query on `folders` within the `-a` scope;
  ambiguous or missing → exit 3) and the daemon receives the id only
  (`debloatmaild.md` §5, §17).
- `delete` on mail moves the items to `role = 'trash'`; `--expunge` additionally removes
  them from the server (the daemon's `expunge` argument). It returns the affected count.
- `hide` is feed-only (exit 2 on mail items): sets `hidden_at`, which drops the items
  from every stream — list, thread counts, and search — without deleting them, and
  which the feed upsert preserves on the next poll (`DATA-MODEL.md` §11.4). **Hidden
  items stay hidden**: the interface has no un-hide (see §13), so `hide` is confirmed
  like any other destructive command (`-y` when stdin is not a TTY).
- `--purge` has been removed from `delete` (§8 synopsis note): it is not expressed on
  the daemon interface and the CLI has no local write privilege.

Destructive commands (`delete --expunge`, `hide`, `remove --purge`, `set-url`) require
`-y` when stdin is not a TTY, and refuse otherwise (`SECURITY.md` §7.3). `--dry-run`
prints the resolved resolution — which ids, which folder, which account — and changes
nothing.

---

### `compose`, `reply`, `forward`

```
debloatmailctl compose [--to <ADDR>] [--cc <ADDR>] [--bcc <ADDR>]
                       [--subject <S>] [--body-stdin] [--attach FILE]…
                       [--draft <DRAFT_ID>] [--save-draft] [--now]
                       [--no-append-sent] [--account <N>] [--edit]
debloatmailctl reply   <id> [--all] [--quote] …
debloatmailctl forward <id> [--attach-original] …
```

Plaintext only. `--body-stdin` reads the body from stdin; `--edit` opens `$EDITOR`
(defaulting to `$VISUAL`, then `vi`) on a pre-filled template. `--attach FILE` requires
a regular, readable file — the CLI pre-checks this so a script fails before it queues,
and the daemon re-validates on its own rules (`debloatmaild.md` §17;
`SECURITY.md` §8.3).

**Sending is queued, not immediate.** `compose` calls `Compose.Send` with an inline
draft, which writes a `pending_ops` row and returns an `op_id` as soon as the message
is durably queued. Composing offline therefore works, and a slow SMTP server never
blocks a script. The CLI prints the `op_id` — it is stable across daemon restarts
(`pending_ops.public_id`), so a caller can check later with the daemon's `Queue.Get`.

| Option | Meaning |
|---|---|
| `--now` | Block until the op reaches a terminal state (polls `Queue.Get` under `--timeout`), then exit 0 on `done` / exit 1 on `failed`. |
| `--dry-run` | Print the assembled RFC 5322 message; queue nothing. |
| `--save-draft` | Save to the **local** `drafts` table and exit, printing the draft id (`Compose.SaveDraft`). Note the semantics: a draft saved here is a *local* draft — it does not appear in the server's `drafts` folder or on another device (`DATA-MODEL.md` §11.5). Users who need shared drafts stay in web mail or the GUI. |
| `--draft <ID>` | Send a previously saved draft: the daemon embeds it in the op payload, queues, and deletes the draft row — one transaction (`DATA-MODEL.md` §11.5). |
| `--no-append-sent` | Do not append a copy to the Sent folder. |

Default behavior appends a copy to `role = 'sent'` **as part of the same op**
(`DATA-MODEL.md` §11.7); the CLI reports success by the send, and prints the
`detail` string when the Sent copy failed to append — a sent message is never reported
as failed.

**`reply <id>`** (mail only — feed items have no `From`; exit 2): `To` is prefilled
with `reply_to` when set, otherwise the From address (`mail_items.from_addr`;
`DATA-MODEL.md` §4.1). The subject is prefixed on the item's **title** (the display
subject); `subject_norm` is grouping metadata, normalized for folding — it must never
be used to build a user-visible reply subject. `--all` adds `cc_addrs` minus the user's
own addresses; `--quote` quotes the body with `> ` (the attribution line format is a
config option, `SECURITY.md` §10). `reply` and `forward` assemble an inline draft and
call `Send` — they do not write a draft row.

**`forward <id>`**: subject prefixed `Fwd:`; the body is quoted.
`--attach-original` attaches the message's **raw MIME** (the stored blob, read
directly; if evicted, a `FetchBody` call re-populates it first) — a faithful forward,
not a re-encoding.

---

### `feed`

```
debloatmailctl feed list
debloatmailctl feed add <URL> [--account <N>] [--name <N>] [--interval <DUR>]
debloatmailctl feed update <NAME|ID> [--name <N>] [--interval <DUR>]
debloatmailctl feed set-url <NAME|ID> <NEW-URL>
debloatmailctl feed remove <NAME|ID> [--purge]
debloatmailctl feed refresh <NAME|ID> [--wait]
debloatmailctl feed show <NAME|ID>
```

`--interval` takes a duration (`6h`, `1d`) or `0`/`auto`, meaning "inherit the account
default" — the resolution ladder of `DATA-MODEL.md` §11.3. `feed update` maps one-to-one
to `Feeds.Update`, which accepts **`name` and `interval_secs` only**; there is no
`url` key, by design.

**`feed add` names its feed account.** `feeds.account_id` is `NOT NULL`
(`DATA-MODEL.md` §5), so `--account <N>` (a `kind = 'feed'` account — the feed *group*)
is resolved client-side and passed as the first argument to `Feeds.Add`. When exactly
one feed account exists it is used implicitly; with several and no flag, exit 2 with
the list of choices.

**`feed add` fetches and validates immediately** (bounded by `--timeout`, §3), and
prints the result the daemon returns:

```
$ debloatmailctl feed add https://blog.rust-lang.org/feed.xml --account blogs
added feed 'Rust Blog' (20 items, 8 unread)
```

Two outcomes, exactly as `debloatmaild.md` §8 defines:

- **Hard validation failure** — 404, HTML served as a feed, malformed XML — fails loudly
  at add time (exit 5, message carries the HTTP status / parse error) and **no row is
  created**. A URL that 404s must not discover itself as an empty feed three syncs
  later.
- **Timeout** — the row is inserted and the call returns
  `validated: false`; the CLI exits 0 and prints
  `added feed '<unnamed>' — validation pending (slow server)`. Validation continues in
  the background; on success the daemon fills `title`/`site_url`/`etag`, and if it
  ultimately fails the row is deleted. The CLI is stateless and does not watch this;
  `doctor` surfaces feeds that are still unvalidated (§8 `doctor`).

**`feed set-url` is Remove + Add, made into one command.** This project's rule for
changing a feed's URL is not an in-place update (an in-place `url` key has no honest
failure mode for the timeout case — `debloatmaild.md` §8), so the command performs, in
order: (1) read the feed row (name, interval, account — the CLI can), (2)
`Feeds.Remove(id, purge=false)`, (3) `Feeds.Add(account, NEW-URL, name, interval)`.

The consequences are stated, because they are the contract:

- It is **not atomic**, and both the halves are individually correct. If the new URL
  hard-fails validation, the old feed is soft-removed — **its articles are retained**
  (still in streams and search, no longer polled) — and the new URL created nothing.
  The command prints its recovery line: re-add the new URL when the server is fixed,
  or re-add the old URL to bring the old feed back — the resurrection form of
  `Feeds.Add` (§13, addendum A).
- The `purge=false` removal is deliberate: purge would delete the feed's articles, and
  feed content is **not re-fetchable** (`DATA-MODEL.md` §9). A URL change must never
  destroy an archive.

**`feed remove`** without `--purge` sets `removed_at` (soft-remove, `DATA-MODEL.md`
§11.2): polling stops, articles are retained in streams and search. `--purge` deletes
the feed row and, in the same transaction, the articles' `feed_items` and spine rows —
unrecoverable, hence `-y`.

`feed refresh` returns a job id; `--wait` loops on `Sync.Finished` for that job
(§10). `feed show` prints id, account, URL, title, effective interval (per the ladder),
last/next fetch, last error, and the unread item count — reads only.

---

### `account`

```
debloatmailctl account list [--json]
debloatmailctl account add [--kind imap|feed] [--name <N>] [--wizard]
                           [--email …] [--provider …] [--username …] [--auth-kind …]
                           [--imap-host … --imap-port … --imap-security …]
                           [--smtp-host … --smtp-port … --smtp-security …]
                           [--secret-stdin | --secret-command <CMD>]
debloatmailctl account update <NAME> [--name <N>] [--enable] [--disable]
                              [--secret-stdin | --secret-command <CMD>]
                              [<same connection flags as add>]
debloatmailctl account remove <NAME> [--purge]
debloatmailctl account check [<NAME>]      # validate config + connectivity
debloatmailctl account show <NAME>
```

`account add` without a connection block runs the **guided wizard**. This should be
built on `pimalaya-cli`'s IMAP/SMTP account wizards rather than hand-rolled
[^321fbf#72-79] — they already handle per-protocol settings collection. `Add` persists
configuration and returns; it does **not** verify connectivity — `account check` is the
separate step, and an account can be created fully offline.

**Secrets are never accepted on the command line** (they leak into shell history and
`ps`, `SECURITY.md` §9.3). Passwords are read from a prompt, from `--secret-stdin`, or
from `--secret-command <CMD>` whose stdout is the secret. The secret travels exactly
once, as a method argument to the daemon, and never again — no property returns it, no
command prints it, `doctor` reports "resolves" not the value.

```
$ debloatmailctl account add --kind imap --name fastmail --email me@fastmail.com \
    --imap-host imap.fastmail.com --imap-port 993 --imap-security tls \
    --smtp-host smtp.fastmail.com --smtp-port 465 --smtp-security tls \
    --secret-command 'pass show fastmail/app-password'
```

Proton is a preset over the same machinery, with a loopback-only certificate exception:

```
$ debloatmailctl account add --kind imap --name proton --provider proton
# presets host 127.0.0.1:1143 (STARTTLS), smtp 127.0.0.1:1025,
# and sets allow_invalid_cert_loopback = 1
```

**`account update`** maps to `Accounts.Update` (present-keys-only): rename (`--name`;
the unique constraint makes a collision a loud failure), enable/disable, change
connection settings, and **credential rotation** — `--secret-stdin` /
`--secret-command` replaces the keyring entry in transaction with the `secret_ref`
repoint, and the account's live connection is invalidated
(`debloatmaild.md` §7). Without this, an expired Bridge password or app password would
stop all syncs with the only fix being "delete and re-add the account."

**`account remove`** without `--purge` **disables** the account (`enabled = 0`; data
retained; `account update --enable` restores it) — addendum B pins this as the daemon's
`Remove(purge=false)` semantics (§13). `--purge` deletes the account row and cascades
(folders, items, feeds for feed accounts): unrecoverable, hence `-y`.

**`account check` is the diagnostic:** DNS, TCP, TLS (the loopback certificate
exception, when in use, is reported **explicitly** — never silently tolerated),
authentication, IMAP `CAPABILITY` (`Capabilities` recorded on the account object,
`debloatmaild.md` §12), SMTP `EHLO`, folder discovery. Each is reported pass/fail with
a one-line detail; a failed check is a **successful call** (exit 0 with failures
shown — the question was "is it reachable?"), exiting 4 when *all* of network/auth
fail. This is the single most useful command for support.

`account show` prints the saved configuration (secrets as `•••` + "in keyring"),
enabled state, negotiated capabilities, last sync time, and last error — reads only,
plus the account object's properties.

---

### `folder`

```
debloatmailctl folder list [--account <N>]
debloatmailctl folder sync <NAME> [--full]
debloatmailctl folder stats [--account <N>]
```

`folder list` prints name, role (and how it was derived — `role_source`),
uidvalidity, per-folder item and unread counts. Names shown are the full IMAP paths;
the id column (in `json`) is the folder `public_id` — what `--to` will resolve to.

`folder sync <NAME> [--full]` triggers `Sync(scope = {account, folder}, full)`.
`--full` forces a complete refetch, discarding the header cache for that folder. It is
the documented recovery path for a corrupted or desynchronized folder, and per
`DATA-MODEL.md` §11.1 it **preserves `public_id`s by `Message-ID` match** (minting new
ids only where there is no Message-ID to match on) and **does not touch feed content**.

`folder stats` prints the per-folder table without counts' sync state (rows, unread,
last sync time).

---

### `sync`

```
debloatmailctl sync [--account <N>] [--folder <N>] [--feed <N>] [--wait] [--full]
```

Triggers `Sync(scope, full)` on the daemon — a scope dict with `account`/`folder`/`feed`
keys resolved client-side — and returns immediately with the job id, unless `--wait`
is given. With the daemon running, the job runs there; `--wait` is a **client-side**
loop on the `Sync.Finished` signal for that job id (`debloatmaild.md` §6 — the
interface has no blocking `wait` argument and none is wanted: the D-Bus call returns at
once, and the same loop is what a GUI progress bar will poll). A second `sync` for a
scope already syncing returns the **existing** job id, so a double invocation is
idempotent, not an error.

`--full` means "ignore all cached sync state and refetch" for mail; it is the global
form of `folder sync --full`, and — unlike `--full` on any single folder — it still
**does not touch feed content** (`DATA-MODEL.md` §13: feeds are the non-disposable
category). Feed refresh is `feed refresh`.

Without the daemon, runs the sync in-process (taking the standalone lock, §10).

---

### `daemon`

```
debloatmailctl daemon status [--json]
debloatmailctl daemon stop
debloatmailctl daemon reload
debloatmailctl daemon logs [-f]
```

`daemon status` reports: running/not, per-account state (`Account.State`,
`LastError`, `UnreadCount`), global `State` (idle/syncing/degraded), `LastSyncAt`,
`PendingOps` with `op_id`s (from `Queue.Pending`), `LastSyncAt`, `DaemonVersion`,
`ApiVersion` — all read from `org.noahie.debloatmail.Status` (`Properties.GetAll`,
one round trip) and the per-account objects (`GetManagedObjects`). It exits **0** when
the daemon is not running (a status query about a stopped daemon is a valid answer);
the exit-6 rule applies to commands that *require* it. This is the first thing to check
when sync is not happening.

`daemon stop` stops the systemd user unit (`debloatmaild.service` — primary, per
`debloatmaild.md` §21; on non-systemd sessions the command prints the unit's service
name and exits 6 with instructions). `daemon reload` calls `Accounts.Reload`
(re-read config, reconnect accounts) and reports whether anything changed. `daemon logs`
wraps `journalctl --user -u debloatmaild -n 100` (or `-f`) — again the systemd-first
consequence of §21's activation design.

---

### `doctor`

```
debloatmailctl doctor [--json]
```

End-to-end health check, and the second thing to run when something is wrong. Reading
only — every check below is a database read, a keyring resolution, or a
daemon-mediated diagnostic. It requires `-y` for nothing and changes nothing:

- config parses and validates
- binary matches the database: `PRAGMA user_version` is at or below this binary's
  (a newer DB is refused — do not open a schema you cannot read, exit 1); schema
  migration completeness; `PRAGMA integrity_check`; WAL state
- keyring reachable; every `secret_ref` resolves (reported as "resolves", never as a
  value)
- each account: connectivity, auth, folder list, TLS chain — the Proton loopback
  certificate exception reported explicitly
- **blob refcount reconciliation**: recompute true `refcount` from
  `mail_items.raw_blob` + `attachments.blob_hash` and compare to `blobs.refcount`
  (the counter is maintained in application code, `DATA-MODEL.md` §5 — this is the
  place it is checked)
- FTS5 index consistency (`item_text` row count vs. index; repair is the single
  `rebuild` statement, printed, not executed)
- **UIDVALIDITY residue**: how many mail rows were minted fresh at the last
  UIDVALIDITY reset because no Message-ID match could preserve them
  (`DATA-MODEL.md` §11.1) — a surprise number here means the server reset and you now
  know
- **operation queue**: any `failed` ops (with `last_error`), any `pending`/`in_flight`
  ops older than a day
- **unvalidated feeds**: feeds with no fetch ever (`last_fetch_at IS NULL`) past their
  add — the tail of `feed add`'s `validated: false` path
- cache and blob sizes against the configured cap (`DATA-MODEL.md` §13), with the
  per-category breakdown
- daemon reachable and its `DaemonVersion`/`ApiVersion` match this binary

---

### `generate`

```
debloatmailctl generate completions <bash|zsh|fish|elvish|powershell> [--dir <D>]
debloatmailctl generate man [--dir <D>]
debloatmailctl generate schema [--dir <D>]
```

Writes to stdout by default, or into `--dir`. Use `pimalaya-cli`'s ready-made
completion/man/schema commands for this rather than reimplementing them — the crate
exposes exactly this positional-list-plus-`--dir` interface [^321fbf#72-79].
`generate schema` prints the current DDL (applied migrations), for scripts and for
diffing against the packaged `0001_init.sql`.

---

## 9. Search query language

A small, predictable filter language that compiles to FTS5 plus SQL. The operator set
is pinned here and in `DATA-MODEL.md` §10/§16; both documents must agree, and this
table is the CLI's.

| Form | Meaning |
|---|---|
| `word`, `"two words"` | Full-text over title, author, body |
| `-word`, `NOT word` | Exclude |
| `from:<addr>` | From address (`mail_items.from_addr`; substring match) — mail only. The canonical example from the data model. |
| `to:<addr>` | To/CC addresses (substring match) — mail only |
| `title:<text>` | Title field. *(Renamed from the draft's `subject:` when the operator set was pinned: `title` is the model's field (`DATA-MODEL.md` §4.1); `subject_norm` is for thread grouping and no operator reads it.)* |
| `author:<text>` | Author field (both kinds) |
| `is:unread`, `is:starred` | State filters |
| `is:mail`, `is:feed` | Kind filter. *(The early drafts' `kind:mail`/`kind:feed` form is dead — `is:feed` is the pinned token.)* |
| `has:attachment` | `has_attachments = 1` |
| `in:<folder>` | Mail folder by name or role |
| `account:<name>`, `feed:<name>` | Scope |
| `after:<date>`, `before:<date>` | Bounds on `published_at`; ISO 8601 or relative (`7d`, `2w`) |

**Unknown `key:` prefixes are an error, not a search term** (exit 2). `searh:foo`
returning zero results is a mystery; `error: unknown search operator 'searh:'` is a
five-second fix.

Term combination is `AND` by default. Bare terms also get prefix matching (`async*`) so
as-you-type-style queries work without the user typing wildcards.

Per `DATA-MODEL.md` §10/`SECURITY.md` §7.1, this parser needs **adversarial tests**:
FTS5's query syntax is its own small language, and everything it receives must be
*built* (parameterized, token-validated), never interpolated. Malformed input must
produce an operator error or zero results — never an SQL error or a panic.

---

## 10. The daemon seam

**Reads bypass the daemon. Writes and network operations go through it.**

```
┌────────────────────────────┐
│  debloatmailctl            │
│                            │
│  reads ────────────────────┼──► SQLite (direct, WAL, read-only; no daemon needed)
│                            │
│  writes / network ─────────┼──► D-Bus  org.noahie.debloatmail.Daemon
└────────────────────────────┘                │
                                              ▼
                                   debloatmaild (sole writer, owns I/O)
```

*(The earlier diagram pointed writes at `org.noahie.debloatmail` — that is the GUI's
activation name, `debloatmaild.md` §2. The two names are the whole argument of that
section; this diagram is now correct.)*

### Commands → methods (names only)

Signatures, argument order, and return shapes live in exactly one place:
`debloatmaild.md` and its Appendix A XML. This table is the **coverage** — every write
the CLI performs, mapped to the method it calls — and it deliberately repeats no
signature, because the three stale tables this doc has been corrected for all
lived in the duplicated-signature shape:

| CLI command | Daemon method / property |
|---|---|
| `mark read` / `mark unread` | `Items.MarkRead(ids, true/false)` |
| `mark starred` / `mark unstarred` | `Items.SetStarred(ids, true/false)` |
| `move` / `copy` | `Items.Move` / `Items.Copy` (folder `public_id`) |
| `delete` | `Items.Delete(ids, expunge)` |
| `hide` | `Items.Hide(ids)` |
| `compose` (+ `--now`) | `Compose.Send(inline draft, "", append_to_sent)` → `op_id`; `--now` polls `Queue.Get(op_id)` |
| `compose --save-draft` | `Compose.SaveDraft` → `draft_id` |
| `compose --draft` | `Compose.Send(…, draft_id, …)` → `op_id` (draft deleted by the daemon, one transaction) |
| `read` (uncached body / `--raw` after eviction) | `Sync.FetchBody(item_id)` → re-read SQLite |
| `attachment download` / `open` | `Sync.FetchAttachment(item_id, section_id)` → blob path |
| `sync`, `folder sync` | `Sync({account?, folder?}, full)` → `job_id` |
| `feed add` | `Feeds.Add(account_id, url, name, interval_secs)` → `result{…, validated}` |
| `feed update` | `Feeds.Update(id, {name?, interval_secs?})` |
| `feed set-url` | `Feeds.Remove(id, false)` + `Feeds.Add(...)` (§8) |
| `feed remove` | `Feeds.Remove(id, purge)` |
| `feed refresh` | `Sync.RefreshFeed(id)` → `job_id` (or `Feed.Refresh` on the feed object) |
| `account add` / `update` / `remove` | `Accounts.Add` / `Accounts.Update` / `Accounts.Remove` |
| `account check` | `Accounts.Check` → structured report (never raises for connectivity) |
| `daemon reload` | `Accounts.Reload` |
| `daemon status` | `Status` props via `Properties.GetAll`; `ObjectManager.GetManagedObjects`; `Queue.Pending` |
| `doctor` (queue section) | `Queue.Pending` / `Queue.Get` |

Signals, for reference — the CLI **does not subscribe to signals in normal operation**
(it is stateless; it needs none of them). The exceptions are the two loops: `--wait`
listens for `Sync.Finished(job_id)` for one job id, and `compose --now` merely polls
`Queue.Get` (even simpler). The signal names are, per `debloatmaild.md` §6/§7/§8/§10:
`Items.Changed`, `Sync.Progress`, `Sync.Finished`, `Accounts.StateChanged`,
`Feeds.Updated`, `Feeds.ValidationFailed`, `Queue.Completed`. The GUI subscribes; if a
CLI loop ever needs more than `Sync.Finished`, that is a design conversation, not a
silent subscription.

### The uncached-body flow

Fetching a body is the one read that needs the network, so it is deliberately two-step:

1. CLI calls `FetchBody(item_id)`. The daemon fetches over IMAP (`BODY.PEEK[]`, so this
   read does not set `\Seen` — reading a message in the CLI must not behave differently
   from reading it in the GUI), stores it, sets `body_state = 'cached'`, and returns
   the new state string — not the bytes.
2. CLI re-reads the row (or the blob path, for `--raw`) from SQLite and prints it.

This keeps **all** body reads on one code path (direct SQLite) and makes the daemon's
only read-side job "populate the cache." It also means `--offline` is trivially
implemented: skip step 1, and error with exit 7 if `body_state != 'cached'`.

### Standalone mode (`--no-daemon`)

Required for the week-one spike, before the daemon exists, and useful afterward for
debugging. In this mode the CLI performs writes and I/O **in-process** — the same
engine crates the daemon links — and **takes an exclusive `flock`** on
`$XDG_STATE_HOME/debloatmail/debloatmail-standalone.lock`. If the daemon holds it (or
another standalone CLI does), the command exits 8 with a clear message rather than
double-writing the same database.

The command surface is **identical** in both modes. Only the executor behind it changes
— an in-process `OpExecutor` versus a D-Bus `OpExecutor` implementing the same Rust
trait — which is what makes "the CLI runs the engine" a testing fact rather than a
hope: the CLI is the engine's first customer, and the engine is written to serve it.

---

## 11. The week-one spike

The smallest command set that proves the engine. Every line below must work before any
QML is written (this is the validation spike of `architecture.md` §15 — a gate, not a
phase; its job is to fail early).

```bash
# 1. Configure a Fastmail account (secrets from the keyring, never argv)
debloatmailctl account add --kind imap --name fastmail --provider fastmail \
    --email me@fastmail.com --secret-command 'pass show fastmail/app-password'

# 2. Validate before syncing
debloatmailctl account check fastmail

# 3. Initial header sync (standalone — no daemon yet)
debloatmailctl --no-daemon sync --wait --full

# 4. The unified list, from the local cache
debloatmailctl list --limit 20

# 5. On-demand body fetch + plaintext read
debloatmailctl read <id> --mark-read

# 6. Offline read (proves the cache is real)
debloatmailctl --offline read <id>

# 7. Queue and send a plaintext message
debloatmailctl compose --to someone@example.com --subject "test" --body-stdin <<< "hello"
debloatmailctl --no-daemon sync --wait      # drains the send queue

# 8. Add one feed under a feed account, then see mail and feeds in one list
debloatmailctl account add --kind feed --name blogs
debloatmailctl feed add https://blog.rust-lang.org/feed.xml --account blogs
debloatmailctl --no-daemon sync --wait
debloatmailctl list --limit 20              # ← both kinds, one stream

# 9. Search across both
debloatmailctl search 'rust after:2026-01-01'
```

**What this spike proves, in order:** config and keyring, IMAP auth and TLS, folder
discovery, UIDVALIDITY handling, header caching (and the header-time `item_text`
insert that makes search work on never-opened mail), the unified `Item` model,
on-demand MIME parsing, offline operation, SMTP send, the `pending_ops` queue, feed
fetch and validation, and FTS5 search across both kinds. That is the entire engine.

---

## 12. Testing

The CLI's testability is the main reason to build it first.

| Layer | Approach |
|---|---|
| Command behavior | `assert_cmd` + `predicates` against a seeded fixture database. |
| Output contract | **Golden files** for `--format json` — a change to the JSON shape fails the test. |
| Search | Table-driven: query string → expected ids, including malformed, adversarial, and all-unknown-operator inputs (exit 2, not panics). |
| Ids and cursors | Prefix resolution (unique, ambiguous → exit 9, no match → exit 3); a cursor minted under one sort applied to another → exit 2; paging stability against a database written to between pages. |
| MIME | Fixtures in `tests/fixtures/`: multipart, malformed, odd charsets, 8-bit, missing boundaries. |
| IMAP | Local GreenMail/Dovecot. **Never a live provider.** |
| Feeds | Recorded HTTP responses (conditional-GET 200/304/404/500, malformed XML, HTML-served-as-feed, and the slow-server timeout that yields `validated: false`). |
| Standalone mode | Lock contention → exit 8; identical surface in both modes, asserted by running the whole command matrix against a fixture DB under `--no-daemon`. |
| Exit codes | Explicitly asserted for every documented failure path, client-side and daemon-mapped. |

**Golden files for the JSON format are the load-bearing test.** The JSON output is a
published interface; without a golden test it will drift, and every downstream script
will break silently.

---

## 13. Dependencies

| Purpose | Crate | Notes |
|---|---|---|
| CLI scaffolding | **`pimalaya-cli`** | clap glue, completions/man/schema generation, printer/reporter, logging, prompts, spinner, **IMAP/SMTP account wizards** [^321fbf#72-79]. Config loading is *not* included [^321fbf#72-82]. |
| Argument parsing | `clap` (derive) | Via `pimalaya-cli` where possible |
| Engine | workspace crates | `debloatmail-core`, `-store`, `-engine`, `-feeds`, `-extract`, `-auth`, `-sync` — the CLI links the same crates `debloatmaild` links, which is why standalone mode is "the same engine," not a copy |
| D-Bus client | `zbus` | Blocking API is fine for a CLI (`debloatmaild.md` §19) |
| Config | `toml` + `serde` | Not covered by `pimalaya-cli` |
| Dates | `jiff` or `time` | ISO 8601 parsing and formatting |
| Terminal | `anstream`/`anstyle` | Color and TTY detection; respect `NO_COLOR` |
| Tables | via `pimalaya-cli` printer | Do not add `comfy-table` separately |
| Tests | `assert_cmd`, `predicates`, `insta` | Golden files via `insta` |

**Pin exact versions and re-verify each pimalaya crate's README before bumping.** The
crate statuses are hand-curated and `io-email`/`io-keyring` are traps
(`architecture.md` §5).

---

## 14. Naming decisions

| Decision | Chosen | Rejected | Reason |
|---|---|---|---|
| List command | `list` (top-level) | `envelope list` | The unified stream is the primary concept; no taxonomy lookup to run the most common command |
| Item ids | 8-hex prefix of UUIDv7, ambiguous → exit 9 | Sequential integers; full UUIDs | Survives daemon writes; typeable; git-style resolution is familiar |
| Wire ids | Full `public_id` in every machine field; names only in `*_name` | Names as ids | A script must be able to pipe output back in; rowids and names are both wrong as addresses |
| Marking | `mark read\|unread\|starred\|unstarred` | `flag add --flag seen` | `seen` is IMAP vocabulary; feeds have no flags |
| Reading state | Off by default, `--mark-read` | Auto-mark (mutt/himalaya) | Silent state change in a pipe is a scripting hazard |
| Feed vs mail removal | `delete` mail-only, `hide` feed-only | `delete` overloaded + `--purge` | One verb per data-model meaning; the overload's "purge" had no mechanism at all |
| Feed URL change | `feed set-url` = soft-Remove + Add | In-place `url` update | Locked decision (`debloatmaild.md` §8): no in-place failure mode for the timeout case; soft-remove retains the archive |
| Send | Queued by default | Immediate | Offline compose; never block a script on SMTP |
| Errors | Single-line stderr + documented exit codes | Verbose traces | Scriptable; `--debug` for the long form |
| Output default | `human`, always | Auto-JSON when piped | Auto-switching breaks `\| less` and makes output environment-dependent |
| Search syntax | Gmail-style `key:value`, `is:feed` form, `title:` field | FTS5 syntax passthrough; `kind:`; `subject:` | FTS5 syntax is a footgun; unknown operators error rather than silently dropping; tokens pinned with the data model |
| Folders on the wire | Name/role in args, `public_id` after client-side resolution | Names on the wire | One addressing scheme; no IMAP-path interpolation daemon-side (`debloatmaild.md` §17) |
