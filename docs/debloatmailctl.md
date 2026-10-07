# debloatmailctl — Command Specification

The command-line interface to debloatmail. This document is the contract for the
binary: its surface, its output, its exit codes, and the seam it shares with the
daemon.

It is written to be built **first** — before any QML exists. It is the reference
implementation of the engine, and the regression suite for the hard parts
(authentication, IMAP state, MIME parsing, the unified model).

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
| **Reads are local and direct** | Listing and searching read SQLite directly. No daemon round-trip, no network, sub-10 ms. |
| **Writes go through the daemon** | Marking, moving, deleting, sending are D-Bus calls, preserving the single-writer rule (DATA-MODEL §6). |
| **Never surprise a script** | Output format is stable and never depends on TTY state. Human output by default; machine formats opt-in. |
| **Offline is a first-class mode** | `--offline` forbids network access entirely and errors rather than silently fetching. |
| **Plaintext only** | Bodies print as extracted text. No HTML, ever. |
| **Fail loudly, not cleverly** | An unknown search operator is an error, not a silently-dropped term. |

**The read/write split is the central design decision.** It means `debloatmailctl list`
never touches the network or the daemon — so it works with the daemon stopped, in a
cron job, or over a slow link. Bodies that are not cached are the one exception: they
require a fetch, which is an explicit daemon call followed by a re-read (§10).

---

## 3. Invocation and global flags

```
debloatmailctl [GLOBAL OPTIONS] <COMMAND> [ARGS]
```

| Flag | Meaning |
|---|---|
| `-a, --account <NAME\|ID>` | Scope to one account. Default: all accounts (unified). |
| `-c, --config <PATH>` | Config file. Repeatable; multiple paths are deep-merged left-to-right, `:`-separated in one argument. Matches the pimalaya convention [^ec53a6]. |
| `--db <PATH>` | Override the database path. Primarily for tests. |
| `--offline` | Never touch the network. Error if the requested data is not cached (exit 7). |
| `--no-daemon` | Do not use D-Bus; run operations in-process. Implies the CLI is the only writer — takes a lock (§10). |
| `--format <human\|json\|jsonl\|plain>` | Output format. Default `human`. |
| `--color <auto\|always\|never>` | Default `auto`. `NO_COLOR` is honored; `--color always` overrides it. |
| `-q, --quiet` | Suppress non-essential output. Errors still print to stderr. |
| `-v, --verbose` | Increase logging verbosity. Repeatable (`-vv`, `-vvv`). |
| `--debug` | Log at debug level, including protocol traces with credentials redacted. |
| `--timeout <DURATION>` | Overall operation timeout, e.g. `30s`, `5m`. Default `60s`. |
| `-y, --yes` | Assume yes for destructive confirmations. Required for non-interactive deletion. |
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
| Logs, last-sync state | `debloatmail/` | `$XDG_STATE_HOME` |
| Secrets | Secret Service portal (keyring) | — |

**The database belongs in `DATA`, not `CACHE`.** This is not cosmetic. Feed article text
is *durable* data that cannot be re-fetched once the feed rotates (DATA-MODEL §9), so
the database must survive a cache wipe. Only evictable content — raw MIME and
attachments, both re-fetchable from IMAP — belongs in `CACHE`. Putting the DB in
`CACHE` would silently destroy the user's feed archive.

### Accounts

Accounts are stored in the SQLite `accounts` table, not in the TOML file. The TOML file
holds application preferences only (poll intervals, cache cap, UI defaults).

Rationale: accounts carry per-account sync state (`uidnext`, `etag`, `backoff`) that
must live transactionally with the data, and the `secret_ref` pointer must never be
separated from the account it belongs to. **Secrets are never in the TOML file** — only
a keyring handle.

*Known tradeoff:* accounts are not human-editable in a text file, and not
version-controllable. Accepted for beta; revisit if it becomes friction.

---

## 5. Addressing items

Every item has a `public_id` (UUIDv7). Users should never type a UUID, so commands
accept a **short id**: the first 8 hex characters, resolved as a unique prefix against
the database, git-style.

```
$ debloatmailctl list --limit 3
  3f2a9c1e  2026-10-07  Alice Chen        Re: release checklist
  7b0d44a2  2026-10-06  Rust Blog         Async cancellation in practice
  c19e88f5  2026-10-05  Bob Martinez      Invoice #2291

$ debloatmailctl read 3f2a          # unique prefix → resolved
$ debloatmailctl read 3f2a9c1e      # full short id
$ debloatmailctl read 018f3a2b-...  # full UUID also accepted
```

**Ambiguous prefixes error; they never guess.**

```
$ debloatmailctl read 3f
error: id prefix '3f' is ambiguous (3 matches: 3f2a9c1e, 3f8b1022, 3fcc4410)
```

This is why short ids are not sequential integers: the daemon inserts rows underneath
the user between commands, so a positional number would silently change meaning. A
content-derived prefix cannot.

Commands taking multiple items accept them space-separated or from stdin:

```
$ debloatmailctl mark read 3f2a 7b0d c19e
$ debloatmailctl list --unread --format plain | debloatmailctl mark read -
```

The literal `-` means "read ids from stdin, one per line."

---

## 6. Output and formats

### `human` (default)

Right-aligned columns, truncated to terminal width, ANSI color when a TTY. Designed for
reading, not parsing — **scripts must not parse this.**

### `json`

A single structured document. Stable key names, treated as an API.

```json
{
  "items": [
    {
      "id": "018f3a2b-7c41-7e2a-9d3f-1a2b3c4d5e6f",
      "short_id": "3f2a9c1e",
      "kind": "mail",
      "account": "fastmail",
      "title": "Re: release checklist",
      "author": "Alice Chen",
      "date": "2026-10-07T14:22:11Z",
      "unread": true,
      "starred": false,
      "has_attachments": true,
      "thread_id": "019f3a2b-...",
      "body_state": "cached"
    }
  ],
  "cursor": "eyJ0IjoxNzYwMDEx...",
  "has_more": true
}
```

### `jsonl`

One object per line. Preferred for large result sets and streaming — the consumer can
process items as they arrive rather than buffering a whole document.

### `plain`

Tab-separated, no headers, no color, no truncation: `short_id`, `date`, `author`,
`title`. The `xargs` format. Combine with `--print0` for NUL-separated output.

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
| `2` | Usage error (unknown flag, bad arguments) — clap's convention |
| `3` | Not found (item, folder, feed, or account) |
| `4` | Authentication failed |
| `5` | Network or TLS failure |
| `6` | Daemon unavailable and required |
| `7` | Offline mode: data not cached |
| `8` | Lock held by another process (standalone mode) |

Every non-zero exit writes a single-line message to stderr, prefixed `error:`. With
`--format json`, errors are emitted as `{"error": {"code": "...", "message": "..."}}` to
**stdout** and the exit code is still set, so a caller can use either mechanism.

---

## 8. Command reference

### Synopsis

```
debloatmailctl account    list | add | remove | check | show
debloatmailctl folder     list | sync | stats
debloatmailctl feed       list | add | remove | refresh | show
debloatmailctl list                                    ★ the unified stream
debloatmailctl read       <id> [--thread]
debloatmailctl search     <query>
debloatmailctl mark       read | unread | starred | unstarred <ids...>
debloatmailctl move       <ids...> --to <folder>
debloatmailctl copy       <ids...> --to <folder>
debloatmailctl delete     <ids...> [--expunge]
debloatmailctl compose    [--to ...] [--edit]
debloatmailctl reply      <id> [--all]
debloatmailctl forward    <id>
debloatmailctl attachment list | download | open
debloatmailctl sync       [--account] [--folder] [--wait]
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
| `--folder <NAME\|ROLE>` | Mail folders only. Roles: `inbox`, `sent`, `drafts`, `trash`, `archive`, `junk`, `all`. |
| `--feed <NAME>` | One feed. |
| `--since <DATE>` / `--before <DATE>` | Date bounds, ISO 8601 or relative (`7d`, `2w`). |
| `--threads` | Collapse to one row per conversation. |
| `--sort <date\|received\|author\|subject>` | Default `date`, descending. |
| `-n, --limit <N>` | Default 25. `--all` for everything (beware). |
| `--cursor <TOKEN>` | Keyset cursor from prior output. |
| `--print0` | NUL-terminate lines in `plain` format. |

Default view is the unified inbox: mail in `role = 'inbox'` folders across all
accounts, plus all feed items (DATA-MODEL §4). Read and unread both shown; `--unread`
filters.

**Paging uses keyset cursors, never offsets.** The daemon writes while you read, so an
`OFFSET 50` on a shifting table skips and duplicates rows. The cursor encodes
`(sort_at, public_id)`.

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
through the daemon (§10), then printed.

| Option | Meaning |
|---|---|
| `--headers` | Print headers before the body. |
| `--raw` | Print raw MIME instead of extracted text. |
| `--mark-read` | Mark as read. **Off by default** (see below). |
| `--thread` | Print the whole conversation, oldest first. |

**`read` does not mark as read by default.** This is a deliberate reversal of the
`mutt`/`himalaya` convention. In a scripting tool, `debloatmailctl read 3f2a | grep -q X`
silently clearing unread state is a data-loss-shaped surprise. Marking read is an
explicit intent and gets an explicit flag. The GUI will mark read on display, which is
correct there because display *is* the intent.

---

### `search`

```
debloatmailctl search <query> [list options...]
```

Full-text search over extracted text for both kinds, backed by FTS5. Accepts a
Gmail-style filter language (§9) that compiles to FTS5 plus SQL predicates.

```
$ debloatmailctl search "borrow checker" --unread
$ debloatmailctl search 'from:alice is:starred after:2026-01-01'
$ debloatmailctl search 'kind:feed author:"rust blog" "async"'
$ debloatmailctl search 'invoice -draft' --format jsonl
```

`--explain` prints the generated SQL and FTS5 query instead of results. Search bugs are
otherwise near-impossible to diagnose, and this costs almost nothing to implement.

---

### `mark`

```
debloatmailctl mark read|unread|starred|unstarred <ids...>
```

`seen`/`unseen` are accepted aliases for `read`/`unread`. For mail these write IMAP
flags via `pending_ops`; for feed items they are local-only, which is invisible here
(DATA-MODEL §7).

```
$ debloatmailctl mark read 3f2a 7b0d
$ debloatmailctl list --unread --format plain | debloatmailctl mark read -
```

---

### `move`, `copy`, `delete`

```
debloatmailctl move <ids...> --to <FOLDER>
debloatmailctl copy <ids...> --to <FOLDER>
debloatmailctl delete <ids...> [--expunge] [--purge]
```

- `move`/`copy` are mail-only; they error on feed items with a clear message rather
  than silently doing nothing.
- `delete` on mail moves to `role = 'trash'`. `--expunge` removes it from the server.
- `delete` on a feed item hides it locally. **It must not reappear on the next poll** —
  see §13, this needs a data-model change.
- `--purge` deletes the local row and cached text without touching the server.

Destructive commands require `-y` when stdin is not a TTY, and refuse otherwise.

---

### `compose`, `reply`, `forward`

```
debloatmailctl compose --to <ADDR> [--cc] [--bcc] --subject <S> [--body-stdin] [--attach FILE]...
debloatmailctl compose --edit              # opens $EDITOR on a template
debloatmailctl reply <id> [--all] [--quote]
debloatmailctl forward <id> [--attach-original]
```

Plaintext only. `--body-stdin` reads the body from stdin; `--edit` opens `$EDITOR`
(defaulting to `$VISUAL`, then `vi`) on a pre-filled template.

**Sending is queued, not immediate.** `compose` writes a `pending_ops` row with
`op = 'send'` and returns as soon as the message is durably queued. This means
composing offline works, and a slow SMTP server never blocks a script.

| Option | Meaning |
|---|---|
| `--now` | Block until the message is actually sent. Errors on failure. |
| `--dry-run` | Print the assembled RFC 5322 message; queue nothing. |
| `--save-draft` | Save to `role = 'drafts'` and exit. |
| `--no-append-sent` | Do not append a copy to the Sent folder. |

Default behavior appends a copy to `role = 'sent'` via IMAP `APPEND` — see §13, this
needs an explicit op.

Reply quoting is plaintext (`> ` prefix), and the attribution line is a config option.

---

### `feed`

```
debloatmailctl feed list
debloatmailctl feed add <URL> [--name <N>] [--interval <DUR>]
debloatmailctl feed remove <NAME|ID> [--purge]
debloatmailctl feed refresh <NAME|ID> [--wait]
debloatmailctl feed show <NAME|ID>
```

`feed add` **fetches and validates immediately**, then prints the parsed title and item
count. Adding a URL that 404s or returns HTML should fail loudly at add time, not
discover itself as an empty feed three syncs later.

```
$ debloatmailctl feed add https://blog.rust-lang.org/feed.xml
added feed 'Rust Blog' (20 items, 8 unread)
```

`feed remove --purge` deletes cached articles; without it, articles are retained and the
feed stops being polled.

---

### `account`

```
debloatmailctl account list [--json]
debloatmailctl account add [--kind imap|feed] [--name <N>] [--wizard]
debloatmailctl account remove <NAME> [--purge]
debloatmailctl account check [<NAME>]      # validate config + connectivity
debloatmailctl account show <NAME>
```

`account add` without flags runs the **guided wizard**. This should be built on
`pimalaya-cli`'s IMAP/SMTP account wizards rather than hand-rolled [^321fbf#72-79] —
they already handle per-protocol settings collection.

**Secrets are never accepted on the command line** (they leak into shell history and
`ps`). Passwords are read from a prompt, from `--secret-stdin`, or from
`--secret-command <CMD>` whose stdout is the secret.

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

`account check` is the diagnostic: DNS, TCP, TLS, authentication, IMAP `CAPABILITY`,
and SMTP `EHLO` — each reported pass/fail. This is the single most useful command for
support.

---

### `folder`

```
debloatmailctl folder list [--account <N>]
debloatmailctl folder sync <NAME> [--full]
debloatmailctl folder stats [--account <N>]
```

`folder sync --full` forces a complete refetch, discarding the header cache for that
folder. It is the documented recovery path for a corrupted or desynchronized folder —
and it must preserve `public_id`s (DATA-MODEL §11).

---

### `sync`

```
debloatmailctl sync [--account <N>] [--folder <N>] [--feed <N>] [--wait] [--full]
```

Triggers the daemon and returns immediately unless `--wait` is given. Without the
daemon, runs the sync in-process (taking the standalone lock, §10).

`--full` means "ignore all cached sync state and refetch" — it is not the same as
`folder sync --full`, which is scoped to one folder.

---

### `daemon`

```
debloatmailctl daemon status [--json]
debloatmailctl daemon stop
debloatmailctl daemon reload          # re-read config, reconnect accounts
debloatmailctl daemon logs [-f]
```

`daemon status` reports: running/not, PID, uptime, per-account connection state, last
sync time, pending op count, and any current errors. This is the first thing to check
when sync is not happening.

---

### `doctor`

```
debloatmailctl doctor [--json]
```

End-to-end health check, and the second thing to run when something is wrong:

- config parses and validates
- database integrity (`PRAGMA integrity_check`), schema version, WAL state
- keyring reachable; every `secret_ref` resolves
- each account: connectivity, auth, folder list, TLS chain (including the loopback
  exception for Proton, reported explicitly rather than silently tolerated)
- FTS5 index consistency (`item_text` row count vs. index)
- cache and blob sizes against the configured cap
- daemon reachable and version-matched to the CLI

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

---

## 9. Search query language

A small, predictable filter language that compiles to FTS5 plus SQL.

| Form | Meaning |
|---|---|
| `word`, `"two words"` | Full-text over title, author, body |
| `-word`, `NOT word` | Exclude |
| `from:<addr>` / `to:<addr>` | Mail addresses (substring match) |
| `subject:<text>` / `author:<text>` | Title / author fields |
| `is:unread`, `is:starred` | State filters |
| `is:mail`, `is:feed` | Kind filter |
| `has:attachment` | `has_attachments = 1` |
| `in:<folder>` | Mail folder by name or role |
| `account:<name>`, `feed:<name>` | Scope |
| `after:<date>`, `before:<date>` | Date bounds; ISO 8601 or relative |

**Unknown `key:` prefixes are an error, not a search term.** `searh:foo` returning zero
results is a mystery; `error: unknown search operator 'searh:'` is a five-second fix.

Term combination is `AND` by default. Bare terms also get prefix matching (`async*`) so
as-you-type-style queries work without the user typing wildcards.

Per DATA-MODEL §10, this parser needs **adversarial tests**: FTS5's query syntax is its
own small language, and malformed input must produce zero results rather than an error
or a panic.

---

## 10. The daemon seam

**Reads bypass the daemon. Writes and network operations go through it.**

```
┌────────────────────────┐
│  debloatmailctl        │
│                        │
│  reads ────────────────┼──► SQLite (direct, WAL, no daemon needed)
│                        │
│  writes / network ─────┼──► D-Bus org.noahie.debloatmail
└────────────────────────┘         │
                                   ▼
                            debloatmaild (sole writer, owns I/O)
```

### Methods the CLI depends on

| D-Bus method | Purpose |
|---|---|
| `Mark(ids, state, value)` | Set read/starred; queues IMAP `STORE` for mail |
| `Move(ids, folder)` / `Copy(ids, folder)` | Mail only |
| `Delete(ids, expunge)` | Move to trash, or expunge |
| `Send(draft) -> op_id` | Queue a composed message |
| `FetchBody(id) -> BodyState` | Fetch an uncached body; CLI then re-reads from SQLite |
| `Sync(scope, full, wait)` | Trigger sync |
| `Status() -> SyncStatus` | daemon status payload |

Signals: `ItemsChanged(account_id)`, `SyncProgress(account_id, phase, pct)`,
`SyncError(account_id, message)`. The CLI does not subscribe — it is stateless — but the
GUI does, and defining them here keeps the two consistent.

### The uncached-body flow

Fetching a body is the one read that needs the network, so it is deliberately two-step:

1. CLI calls `FetchBody(id)`. The daemon fetches, stores text, sets
   `body_state = 'cached'`.
2. CLI re-reads the row from SQLite and prints it.

This keeps **all** body reads on one code path (direct SQLite) and makes the daemon's
only read-side job "populate the cache." It also means `--offline` is trivially
implemented: skip step 1, and error with exit 7 if `body_state != 'cached'`.

### Standalone mode (`--no-daemon`)

Required for the week-one spike, before the daemon exists, and useful afterward for
debugging. In this mode the CLI performs I/O in-process and **takes an exclusive
`flock`** on a lock file beside the database. If the daemon holds it, the CLI exits 8
with a clear message rather than double-syncing the same account.

The command surface is **identical** in both modes. Only the executor behind it changes
— an in-process `OpExecutor` versus a D-Bus client. This is why the CLI is the
reference implementation: it exercises the same engine the daemon uses.

---

## 11. The week-one spike

The smallest command set that proves the engine. Every line below must work before any
QML is written.

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

# 8. Add one feed, then see mail and feeds in one list
debloatmailctl feed add https://blog.rust-lang.org/feed.xml
debloatmailctl --no-daemon sync --wait
debloatmailctl list --limit 20              # ← both kinds, one stream

# 9. Search across both
debloatmailctl search 'rust after:2026-01-01'
```

**What this spike proves, in order:** config and keyring, IMAP auth and TLS, folder
discovery, UIDVALIDITY handling, header caching, the unified `Item` model, on-demand
MIME parsing, offline operation, SMTP send, the `pending_ops` queue, feed fetch and
parse, and FTS5 search. That is the entire engine.

---

## 12. Testing

The CLI's testability is the main reason to build it first.

| Layer | Approach |
|---|---|
| Command behavior | `assert_cmd` + `predicates` against a seeded fixture database. |
| Output contract | **Golden files** for `--format json` — a change to the JSON shape fails the test. |
| Search | Table-driven: query string → expected ids, including malformed and adversarial inputs. |
| MIME | Fixtures in `tests/fixtures/`: multipart, malformed, odd charsets, 8-bit, missing boundaries. |
| IMAP | Local GreenMail/Dovecot. **Never a live provider.** |
| Feeds | Recorded HTTP responses (conditional-GET 200/304/404/500, malformed XML, HTML-served-as-feed). |
| Exit codes | Explicitly asserted for every documented failure path. |

**Golden files for the JSON format are the load-bearing test.** The JSON output is a
published interface; without a golden test it will drift, and every downstream script
will break silently.

---

## 13. Gaps found in `DATA-MODEL.md`

Writing this spec surfaced three issues that need to be resolved in the data model
before the corresponding commands can be implemented.

**1. Feed item deletion needs suppression.** `delete` on a feed item cannot simply
remove the row: the next poll matches on `(feed_id, guid)` and re-inserts it, so deleted
articles reappear forever. Fix: add `items.hidden_at INTEGER` (or a `feed_suppressed`
table) and filter hidden rows from every stream. This is a one-line schema change that
is very painful to add after the fact.

**2. Sent-folder append needs its own op.** `compose` defaults to appending a copy to
`role = 'sent'` via IMAP `APPEND`, but `pending_ops.op` has no `append` variant. Either
add `'append'` to the CHECK constraint, or make send-and-append a single atomic op —
but decide, because a message that sends successfully and then fails to append is a
confusing state to be in.

**3. Local drafts have no home.** `compose --save-draft` needs somewhere to put a draft.
The `folder.role = 'drafts'` concept is IMAP-side, but a draft composed offline has no
server counterpart yet. Either store drafts as items in a local drafts folder (which
then need their own upload path), or add a `drafts` table. Currently unspecified.

**Also worth pinning:** the database location. `DATA-MODEL.md` §9 specifies the blob
directory but never states where `mail.db` lives. Per §4 above it must be
`$XDG_DATA_HOME`, because it contains durable feed content.

---

## 14. Dependencies

| Purpose | Crate | Notes |
|---|---|---|
| CLI scaffolding | **`pimalaya-cli`** | clap glue, completions/man/schema generation, printer/reporter, logging, prompts, spinner, **IMAP/SMTP account wizards** [^321fbf#72-79]. Config loading is *not* included [^321fbf#72-82]. |
| Argument parsing | `clap` (derive) | Via `pimalaya-cli` where possible |
| Engine | workspace crates | `debloatmail-core`, `-store`, `-engine`, `-feeds`, `-auth`, `-sync` |
| D-Bus client | `zbus` | Blocking API is fine for a CLI |
| Config | `toml` + `serde` | Not covered by `pimalaya-cli` |
| Dates | `jiff` or `time` | ISO 8601 parsing and formatting |
| Terminal | `anstream`/`anstyle` | Color and TTY detection; respect `NO_COLOR` |
| Tables | via `pimalaya-cli` printer | Do not add `comfy-table` separately |
| Tests | `assert_cmd`, `predicates`, `insta` | Golden files via `insta` |

**Pin exact versions and re-verify each pimalaya crate's README before bumping.** The
crate statuses are hand-curated and `io-email`/`io-keyring` are traps (architecture.md
§5).

---

## 15. Naming decisions

| Decision | Chosen | Rejected | Reason |
|---|---|---|---|
| List command | `list` (top-level) | `envelope list` | The unified stream is the primary concept; no taxonomy lookup to run the most common command |
| Item ids | 8-hex prefix of UUIDv7 | Sequential integers; full UUIDs | Survives daemon writes; typeable; git-style resolution is familiar |
| Marking | `mark read\|unread\|starred` | `flag add --flag seen` | `seen` is IMAP vocabulary; feeds have no flags |
| Reading state | Off by default, `--mark-read` | Auto-mark (mutt/himalaya) | Silent state change in a pipe is a scripting hazard |
| Send | Queued by default | Immediate | Offline compose; never block a script on SMTP |
| Errors | Single-line stderr + documented exit codes | Verbose traces | Scriptable; `--debug` for the long form |
| Output default | `human`, always | Auto-JSON when piped | Auto-switching breaks `\| less` and makes output environment-dependent |
| Search syntax | Gmail-style `key:value` | FTS5 syntax passthrough | FTS5 syntax is a footgun; unknown operators error rather than silently dropping |
