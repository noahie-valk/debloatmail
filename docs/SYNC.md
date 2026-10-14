# Sync Engine

What runs in the background, what it owns, and how it fails: the scheduler, the job
model, mail sync, feed sync, body and attachment fetch, the `pending_ops` drain, offline
behaviour, crash recovery, and the failure taxonomy.

`DATA-MODEL.md` (revision 3, authoritative) specifies the sync state the schema stores
(§11 and §5). `debloatmaild.md` specifies the API surface through which jobs are
triggered and reported. `MIME-AND-TEXT.md` specifies what the fetched bytes are turned
into. `debloatmailctl.md` specifies the one client that must behave correctly when a job
is in flight. Where this document needs something those documents do not provide, §17
lists the dependency rather than inventing it.

This revision fixes the items the review flagged against this document and reconciles
the engine with the verified DATA-MODEL v3 DDL: the interactive-fetch stall and the
connection-count contradiction (§3.1, §3.2, §12); the UIDVALIDITY recovery procedure —
now safe under `UNIQUE (folder_id, uidvalidity, uid)` and `pending_ops.item_id … ON
DELETE SET NULL`, and idempotent on rerun (§5.4); header-sync searchability and the
provisional-attachment refinement rule (§5.5, §6.1); the retry-table/formula
contradiction and offline retry burn (§8.2, §8.6, §10.2), with `in_flight` state
semantics exactly as DATA-MODEL §11.6 defines them; the unassigned HTTP client (§2,
§7.1); `Feeds.Add` — **hybrid per DATA-MODEL v3 §11.3** (hard failures create no row;
timeouts insert a transient `validated: false` row, §7.4); and `part_id` → `section_id`
throughout. §17 records where each landed, verified against the v3 upload.

---

## 1. Scope

**In scope:**

- The scheduler: what runs when, at what priority, under what concurrency cap.
- The job model: ids, states, cancellation, idempotency.
- Mail sync: connection, capability negotiation, folder discovery, UIDVALIDITY,
  initial/incremental header sync, flag sync, the IDLE watch loop.
- Feed sync: conditional GET, upsert, scheduling, backoff.
- Body and attachment fetch.
- The `pending_ops` drain, including send semantics.
- Offline mode, crash recovery, and the failure taxonomy.

---

## 2. Where sync lives

```
crates/engine      IMAP/SMTP sockets, TLS, the coroutine pump, the scheduler
crates/sync        mail sync, feed sync, the op drain — protocol logic, no sockets
crates/store       the only writer to SQLite
crates/feeds       feed-rs parsing, the project's single HTTP client (reqwest,
                   no default features, rustls-tls), conditional GET, the redirect hook
crates/extract     MIME and feed-content extraction — pure, runtime-free
                   (MIME-AND-TEXT.md)
crates/auth        credentials, keyring, SASL
```

**`sync` contains no sockets.** This is the consequence of the sans-I/O decision in
`architecture.md` §5: pimalaya's `io-imap` is a set of I/O-free coroutine state machines
that emit read and write requests instead of performing I/O [^0a3a99][^26871d]. The
crate ships three feature-gated layers — the `no_std` coroutines, a light blocking client
over `Stream: Read + Write`, and a full client that owns TCP and TLS [^64586a] — and it
is built on `imap-codec`, re-exported as `codec` with `imap-types` as `types` [^fb1d68].

**We use the coroutine layer, not the full client.** `engine/io.rs` owns the IMAP/SMTP
socket and `engine/tls.rs` owns IMAP/SMTP TLS, per `architecture.md` §5. The pump:

```
loop {
    match coroutine.resume(&mut fragmentizer, incoming_bytes) {
        Yield(Read)       => { let n = socket.read(buf)?;  incoming_bytes = &buf[..n]; }
        Yield(Write(out)) => socket.write_all(out)?,
        Done(result)      => return result,
    }
}
```

Note that `resume` takes a borrowed `Fragmentizer` — a connection-wide parser buffer
that must be shared across every coroutine run on that connection, so that a partial
read survives between commands. Getting this wrong produces intermittent parse
failures that look like server bugs. One `Fragmentizer` per connection, held by the
connection, not by the coroutine.

**The HTTP client is assigned.** `architecture.md` §5's socket rule governs IMAP/SMTP;
HTTP/TLS for feeds lives in `crates/feeds`, where it is owned by exactly one client:
`reqwest` with no default features and `rustls-tls`, per `GEMINI.md`'s dependency list.
No second HTTP stack anywhere. The client is built with a **per-redirect hook**: before
each hop is followed the hook receives the destination host, scheme, and full URL plus
the hop count, and `SECURITY.md` §5.4's SSRF rules (no private/link-local targets;
loopback only where the user's feed URL had loopback in it) run inside the hook. The
hook is the difference between "follows redirects" and "is a proxy" (§7.1).

**The consequence that matters:** sync logic is testable against in-memory buffers with
no network and no TLS. Every scenario in §15 is a byte sequence, not a server.

---

## 3. The scheduler

One scheduler task owns a priority queue of `Job`s and a bounded worker pool.

### 3.1 Priority classes

| Class | Jobs | Rationale |
|---|---|---|
| **Interactive** | `BodyFetch`, `AttachmentFetch`, user-triggered `Sync`, `QueueDrain` | A human is waiting. These must preempt everything. |
| **Maintenance** | scheduled `AccountSync`, `FeedRefresh`, `QueueDrain` (background) | Background correctness. |
| **Housekeeping** | `Gc`, `FtsOptimize`, `ThreadRebuild`, `Reextract` | Never urgent, always deferrable. |

The rule: **a housekeeping job never delays a network job.** Recomputing threads during
a sync is a way to make sync slower for no user-visible benefit.

`BodyFetch`/`AttachmentFetch` preempt on two rules:

1. A scheduled `AccountSync` of *any* account is interrupted immediately.
2. An in-progress `AccountSync` of the *same* account is interrupted **at a committed
   window boundary** — the sync loop releases the account mutex between 2,000-UID
   windows (§11.3 defines the boundaries), and a queued interactive fetch runs in that
   gap on the work connection.

Preemption is between protocol commands, never mid-command (§4.4). **The stall the
review flagged is closed:** opening a message during a large initial sync of that
account used to wait behind the whole sync; now it waits for at most one 2,000-UID
window. A UIDVALIDITY staged recovery (§5.4) is the one exception: it holds the mutex
for its duration.

### 3.2 Concurrency

| Limit | Default | Why |
|---|---|---|
| Worker pool | 4 | More parallel accounts than this does not help; it multiplies failure modes. |
| Concurrent connections per account | 2 | One IDLE + one work connection (§5.7). One for accounts without an inbox role. |
| Concurrent HTTP fetches | 4 | Feed polling is cheap but should not look like a scraper. |
| Concurrent fetches per host | 1 | Politeness (§12). |
| Total open sockets | 16 | A ceiling that makes a leak obvious. |

**Per-account serialization, at batch granularity.** At most one *sync* job runs per
account at a time — two concurrent header syncs on one account would interleave `UIDNEXT`
updates and produce a corrupted cursor. But the account mutex is **held per sync batch,
not per job**: the `AccountSync` loop releases it when it commits a window (§11.3), so
an interactive `BodyFetch` on the same account can run in the gap (§3.1 rule 2). Holding
it per job instead is exactly the "opening a message during a large sync of the same
account waits forever" bug that §3.1 fixes.

### 3.3 What triggers a job

| Trigger | Job |
|---|---|
| Daemon start | `AccountSync` (incremental, all enabled accounts) |
| Scheduled (per account) | `AccountSync` |
| Scheduled (per feed) | `FeedRefresh` |
| `Sync` D-Bus call | `AccountSync` scoped |
| `RefreshFeed` | `FeedRefresh` scoped |
| `Feeds.Add` | background validation / first fetch (§7.4) |
| `FetchBody` / `FetchAttachment` | `BodyFetch` / `AttachmentFetch` |
| `sync --full` (CLI) | `AccountSync` full-staged per mail folder (§5.4) |
| An op entered `pending_ops` | `QueueDrain` |
| New items committed | `ThreadRebuild` (deferred to end of sync) |
| Idle + size over cap | `Gc` |
| Extractor version bump | `Reextract` (housekeeping, throttled) |

### 3.4 Scheduling intervals

```
effective_feed_interval = COALESCE(feeds.interval_secs,
                                   accounts.poll_interval_secs,
                                   config.default_feed_interval_secs)

effective_mail_interval = COALESCE(accounts.poll_interval_secs,
                                   config.default_mail_interval_secs)
```

Defaults: feed 3600 s, mail 900 s. Minimums: feed 300 s, mail 60 s. A configured value
below the minimum is clamped, not rejected, with a warning — a user who types `5` for
"5 seconds" meant "frequently", and erroring on that is unhelpful. A stored
`interval_secs` of `0` is treated as `NULL` (DATA-MODEL §11.3), not as "poll every
second".

**Every scheduled time gets jitter of ±10%.** Without it, N accounts configured at
install time all wake at the same second forever. Jitter is deterministic per account
(derived from its `public_id`) so schedules do not wander between runs and make
`doctor` output unstable.

---

## 4. The job model

### 4.1 Jobs are not ops

| | Job | Op |
|---|---|---|
| Lives in | daemon memory | `pending_ops` table |
| Id | `job_id` (`u`), per-run | `op_id` (`s`), `public_id`, durable |
| Survives restart | No — by design | Yes — by design |
| Meaning | "something is running now" | "the user asked for this; make it happen" |

This asymmetry is stated in `debloatmaild.md` §6 and §9 and it is the right one: a job is
progress reporting, an op is user intent. A job that dies with the process is correct;
an op that dies with the process is data loss.

### 4.2 Job states

```
Queued ──► Running ──► Done
   │          │
   │          ├──► Failed ──► (retry, backoff) ──► Queued
   │          │
   └──────────┴──► Cancelled
```

### 4.3 Idempotency and coalescing

`Sync` is idempotent per scope (`debloatmaild.md` §6): calling it for a scope already
running returns the existing `job_id`. The scope key is the tuple
`(kind, account_id?, folder_id?, feed_id?)`; an empty scope keys on `(AccountSync, all)`.

**Coalescing rule:** if a job for the same key is `Queued` or `Running`, return its id.
If it has just finished and a new request arrives within 1 second, do not start a second
run — return the finished job's id. This makes `debloatmailctl sync && debloatmailctl
sync` produce one sync, not two, which is what a shell script expects.

### 4.4 Cancellation

`Cancel(job_id)` sets a cancellation flag the pump checks **between protocol commands**,
never mid-command. Interrupting a coroutine mid-`FETCH` leaves the connection in an
undefined parse state and the only safe recovery is to drop it.

Cancellation is cooperative and bounded: a job that is mid-`UID FETCH` of 50,000 headers
finishes that command before stopping. The alternative — tearing down the socket — throws
away work that was already paid for and leaves the server holding an aborted session.

**On cancel, committed work is kept.** A cancelled `AccountSync` does not roll back the
headers it already inserted; it just stops. The next sync resumes from the stored
`UIDNEXT`. This is why the sync cursor is committed *after* each batch (§11.3), not at
the end of the job.

---

## 5. Mail sync

### 5.1 Connection lifecycle

```
connect → TLS → CAPABILITY → (STARTTLS if needed) → AUTHENTICATE → CAPABILITY (post-auth)
```

**TLS first, always.** RFC 8314 deprecates cleartext for mail access and submission and
recommends implicit TLS on dedicated ports — 993 for IMAP, 465 for submission — with
STARTTLS retained for the transition period. Our defaults follow that,
and `SECURITY.md` §5.3 makes the no-downgrade rule binding:

| `imap_security` | Port | Behaviour |
|---|---|---|
| `tls` (default) | 993 | Implicit TLS. Handshake before any protocol byte. |
| `starttls` | 143 | Plaintext connect, `STARTTLS`, then re-issue `CAPABILITY` (the pre-TLS capability list is untrusted). |
| `none` | 143 | **Refused unless the host is loopback.** See below. |

`imap_security = 'none'` against a non-loopback host is rejected at account validation
time, not at connect time. Cleartext IMAP to a remote host sends the password in the
clear; a config that permits it is a bug in the config.

**The Proton exception is scoped to loopback, per `architecture.md` §7.** The Bridge
presents a self-signed certificate on `127.0.0.1:1143`, so
`allow_invalid_cert_loopback = 1` permits an invalid chain **only** when the resolved
peer address is `127.0.0.1` or `::1`. The check is on the *connected socket's* peer
address, not on the configured hostname — a hostname can resolve to anything, and a
check on the name is a check an attacker controls.

**Post-auth `CAPABILITY` is mandatory.** Servers advertise extensions after
authentication that they do not advertise before. Reading capabilities once, pre-auth,
is a classic bug that silently disables `MOVE`, `IDLE`, and `SPECIAL-USE`. Capabilities
are per connection and per session, held in memory, and re-read on every connect. They
are not persisted — a server upgrade that adds `MOVE` should take effect on the next
reconnect (`Account.Capabilities`, `debloatmaild.md` §12, is how a client reads them
out).

### 5.2 Capability handling

| Capability | Use | If absent |
|---|---|---|
| `IDLE` | The watch loop (§5.7) | Fall back to polling the inbox on the mail interval |
| `MOVE` | `Move` as one command | `COPY` + `STORE +FLAGS \Deleted` + `EXPUNGE` |
| `UIDPLUS` | `UID EXPUNGE`, `APPENDUID`, `COPYUID` | Plain `EXPUNGE` unsafe (§8.3); re-sync the Sent folder after `APPEND` |
| `SPECIAL-USE` | Folder role discovery | Name heuristics (§5.3) |
| `CONDSTORE` | Cheap flag resync (§5.6) | Full flag fetch over the tracked window |
| `QRESYNC` | Explicit `VANISHED` for expunges | UID-set diff to detect expunges |
| `NOTIFY` | Push for non-inbox folders | Not used in beta (§5.7) |
| `UTF8=ACCEPT` | Send/receive raw UTF-8 | MIME-encode outgoing headers |

### 5.3 Folder discovery and role mapping

`LIST "" "*"` returns `(\HasNoChildren \Sent)`-style attributes when `SPECIAL-USE` is
advertised. The mapping:

| Attribute | `folders.role` |
|---|---|
| `\Inbox` | `inbox` |
| `\Sent` | `sent` |
| `\Drafts` | `drafts` |
| `\Trash` | `trash` |
| `\Archive` | `archive` |
| `\Junk` | `junk` |
| `\All` | `all` |

**When `SPECIAL-USE` is absent, fall back to name matching** — case-insensitive, with
the common localisations (`Sent`, `Sent Items`, `Sent Messages`, `Gesendet`, `Envoyés`,
`Elementi inviati`, `Отправленные`, …) and the `INBOX.` prefix stripped. This fallback
is necessary but it is a *guess*: every role carries its provenance in
`folders.role_source` (`special-use`, `name`, or `manual` for an explicit user
assignment — DATA-MODEL §5), so `doctor` can report that a role was inferred rather
than told.

**A role is a uniqueness constraint we enforce, not the server.** Some servers have two
folders claiming `\Sent`. The first wins; the second is stored with `role = NULL` and
reported by `doctor`. Two folders with `role = 'sent'` would make "append to Sent"
non-deterministic.

**Folders are not deleted on absence.** A folder missing from `LIST` is marked
`subscribed = 0`, not deleted — its headers are still valid, and some servers transiently
omit folders under load. Deletion happens only on explicit account purge.

### 5.4 UIDVALIDITY: the one case that invalidates everything

Per RFC 3501, the combination of mailbox name, `UIDVALIDITY`, and `UID` refers to a
single immutable message forever, and if a mailbox is deleted and recreated the server
must either preserve UIDs or assign a new `UIDVALIDITY`. A changed
`UIDVALIDITY` therefore means **every cached UID in that folder is meaningless**.

**Why delete-then-reinsert, and why the schema makes it safe.** The folder's rows are
addressed by `(folder_id, uidvalidity, uid)`; after a reset the incoming messages carry
the *new* epoch and would coexist with the stale rows. But a preserved `public_id`
collides with `items.public_id UNIQUE` the moment the replacement row is inserted, so
the stale rows must go first. Two v3 schema properties make that gap safe:

- `pending_ops.item_id` is `ON DELETE SET NULL` (DATA-MODEL §5, §11.6), so deleting the
  folder's rows cannot cascade-delete queued user intent; orphaned ops become
  `item_id IS NULL` rows that the drain handles explicitly.
- The upsert's unique key includes `uidvalidity`, so once the stale rows are gone no
  incoming message can bind to a stale row's `public_id` without an explicit
  `Message-ID` re-map (DATA-MODEL §11.1).

**The recovery procedure — staged, in this order, for one folder in one account (the
account mutex is held for the whole procedure — §3.1's exception; bodies are never
fetched here, only headers, so it is short):**

1. **Detect** mismatch on `SELECT`/`STATUS` (or via `UIDNEXT` going backwards — below).
2. **Build the preservation map** from the folder's existing rows:
   `message_id → public_id`. Bounded and one-shot: one pass over the folder's rows and a
   small `HashMap` (hundreds of KB even for a 200,000-message folder), not a copy of
   the rows.
3. **Snapshot the account's queued ops** that reference this folder: for each, record
   `op_id → message_id`. (After step 4 the old rowids are dead, and this is the only way
   we can re-attach an op to the message's replacement row.)
4. **Delete every row of the folder** in one transaction — `items` and `mail_items`
   rows; the ops referencing them become `item_id IS NULL` by the `SET NULL` above, with
   their `op_id → message_id` record safe in step 3's snapshot.
5. **Refetch headers for the whole folder** in 2,000-UID windows, committing per window
   (§11.3). The table is empty for this folder, so insertions cannot collide. For each
   incoming message: its `Message-ID` is in the map → **reuse the preserved
   `public_id`**; not in the map → mint a new one.
6. **Rebind the snapshot ops:** `UPDATE pending_ops SET item_id = <new rowid>` for every
   op whose recorded `message_id` reappeared. Ops whose item did not reappear are left
   with `item_id IS NULL`; the drain handles them per DATA-MODEL §11.6 — self-
   sufficient parts (a `send`'s inline draft) complete, item-dependent parts fail into
   `last_error` — and they surface via `Queue`. **Never dropped silently.**
7. **Advance `uidvalidity`, `uidnext`, and `highestmodseq`** in the folder's final
   window commit. `highestmodseq` resets with the folder: a MODSEQ from before the
   reset is meaningless against a mailbox with new UIDs, so the first post-reset flag
   sync is a full tracked-window fetch (§5.6).

**The limit, stated, not discovered:** step 5 preserves a `public_id` **only when the
message has a `Message-ID`.** A message with none gets a new `public_id`, and any UI
reference to the old one breaks. This is unavoidable — there is nothing to match on —
and `doctor` must report, per reset, how many items had to mint new ids (DATA-MODEL
§11.1).

**The procedure is idempotent under rerun.** If the daemon dies mid-reset, the folder
is left with a stale `folders.uidvalidity` and (partially) new-epoch rows. The next
sync detects the mismatch again, rebuilds the map from whatever rows now exist — which
include the surviving new-epoch rows, whose `public_id`s preserve themselves — and
finishes. No additional bookkeeping is needed for crash safety; the `SET NULL` op gap
is covered by the §11.1 sweep and the drain.

**Local-only state survives; server-projected state does not need to.** `starred` and
`unread` are re-derived from server flags, so they are correct after the refetch by
construction. `hidden_at` does not apply to mail.

**`UIDNEXT` going backwards is the same case.** A server whose `UIDNEXT` is lower than
our stored value has either expunged en masse or silently reset UIDs without changing
`UIDVALIDITY` (which some broken servers do). Treat it as a `UIDVALIDITY` change: run
this procedure.

**`sync --full` (CLI) is the manual trigger of this procedure**, per folder, and
**applies to mail folders only** — it does not touch feed content
(`debloatmailctl.md` §8). It is not "delete the database"; it is this recovery with the
preservation map, invoked deliberately.

### 5.5 Header sync

**Initial** (folder never synced):

```
UID FETCH 1:* (UID FLAGS INTERNALDATE RFC822.SIZE ENVELOPE BODYSTRUCTURE)
```

**Incremental** (UIDNEXT advanced):

```
UID FETCH <stored_uidnext>:* (UID FLAGS INTERNALDATE RFC822.SIZE ENVELOPE BODYSTRUCTURE)
```

**All five fetch items are mandatory.** `INTERNALDATE` is the source of mail
`sort_at` (DATA-MODEL §4.2.3); `ENVELOPE` gives the identity and display fields;
`BODYSTRUCTURE` is what makes the attachment list available without downloading a
single body. The `(ENVELOPE FLAGS)`-only fetch list in the earlier documents was the
bug this rule is recorded against; DATA-MODEL §11.1 carries the corrected list.

**Header sync writes the item-text anchor.** For every new item the upsert writes not
only the `items` + `mail_items`/`feed_items` rows but also the `item_text` row —
`title`, `author`, and `body = ''` (DATA-MODEL §11.1; the columns exist in v3:
`item_text.title TEXT NOT NULL DEFAULT ''`, `item_text.author TEXT NOT NULL DEFAULT ''`).
**The consequence:** a message the user has never opened is still searchable by subject
and sender, because its text row exists from the moment its headers do. The unanchored
variant — text rows written only at body fetch — is why the CLI spike's `search '…'` on
unread mail found nothing. This anchor is what makes DATA-MODEL §9.2's "title and
author remain searchable for every message" true.

**`BODYSTRUCTURE` yields provisional attachment rows.** For each message, the
bodystructure (parsed by `crates/extract` from the data fetched with the headers)
produces the attachment list: per part, a row carrying `section_id` — the RFC 3501
§6.4.5 section number (`MIME-AND-TEXT.md` §5.1), usable verbatim in
`BODY.PEEK[section_id]` — `content_type`, `filename` when present, and the row's
provisional status. `has_attachments` is derived from these rows. **They are
provisional.** `BODYSTRUCTURE` can mislead (mis-declared `Content-Disposition`,
bodystructures the server summarizes); the refinement rule in §6.1 is how the
attachment list changes when the body arrives, and the only way it may change. Without
`BODYSTRUCTURE` at header time, the attachment list would require a body fetch per
message, which is the thing this whole design exists to avoid (DATA-MODEL §11.1:
header-time seeding is provisional, the body walk is authoritative for final rows).

**Neither fetch uses `BODY[]`.** `ENVELOPE` and `BODYSTRUCTURE` are metadata fetches
and do not set `\Seen`. `BODY[]` *does* set `\Seen` implicitly; the `.PEEK` variant
does not. Every body and part fetch in §6 therefore uses `BODY.PEEK[...]`. Getting this
wrong marks the user's entire mailbox read on first sync, and it is the single most
common way an IMAP client destroys a mailbox's state on setup.

**What we do not fetch:** `RFC822.TEXT`, `BODY[]`, and any part body, during header
sync. Headers only.

**Batching.** A folder with 200,000 messages is not fetched in one command. Headers are
fetched in UID windows of 2,000, committed per window (§11.3) — and each committed
window is an interaction boundary: the account mutex is released for the duration of
any queued interactive fetch (§3.1, §3.2). A cancelled or crashed sync resumes at the
last committed window rather than from zero.

### 5.6 Flag sync

Flags change without new mail arriving, so `UIDNEXT` advancing is not sufficient.

**With `CONDSTORE`** (the good path): the mailbox has a `HIGHESTMODSEQ`, and each
message has a `MODSEQ`. Store the last-synced value in `folders.highestmodseq`
(DATA-MODEL §5, v3) and fetch only what changed:

```
SELECT <folder>            → HIGHESTMODSEQ
UID FETCH 1:* (FLAGS) (CHANGEDSINCE <folders.highestmodseq>)
```

CONDSTORE exists precisely to let a disconnected client resynchronize flag changes
quickly. **After a UIDVALIDITY reset, `highestmodseq` resets with the folder**
(§5.4 step 7).

**Without `CONDSTORE`:** fetch `UID FETCH <low_uid>:* (FLAGS)` over the *tracked
window* — the UIDs we hold rows for, capped at the most recent N (default 5,000) — and
diff. This is O(messages) per sync and is exactly why CONDSTORE matters. Report the
fallback in `doctor` so a slow account is explainable rather than mysterious.

**Expunge detection.** Without `QRESYNC`, expunges are invisible: the message simply
stops appearing. Detect them by diffing the server's UID set against our rows:

```
UID SEARCH ALL              (or: UID FETCH 1:* (UID))
```

Rows whose UID is absent were expunged remotely → delete them locally, **and their
pending ops become `item_id IS NULL`** (the `SET NULL`, DATA-MODEL §5) and are handled
at the drain exactly as in §5.4 step 6 — the cascade is not the thing that decides
what happens to queued intent. With `QRESYNC`, the server sends `VANISHED` explicitly
and the diff is unnecessary.

**Note the provider reality:** Gmail supports `CONDSTORE` but **not** `QRESYNC`. So Gmail gets the cheap flag path and the UID-diff expunge path. Both paths
must work, and the UID-diff path is therefore not dead code that can be left untested.

**The pending-op-wins rule** (DATA-MODEL §7) applies here: a server flag update for an
item with an undrained `flag` op is **discarded**. Without this, marking a message read
while an in-flight `FETCH FLAGS` reports it unread produces a flicker, and the user
concludes the client is broken.

### 5.7 The watch loop (IDLE)

IDLE pushes notifications for **the selected mailbox only** — that is the limitation
RFC 5465 was written to address. So:

- The watch connection selects `role = 'inbox'` and IDLEs there.
- Other folders are covered by the periodic sweep, not by push.
- If the account has no `inbox` role, there is no watch connection and the account
  polls (and holds one connection, not two).

**The 29-minute rule is not optional.** RFC 2177 advises clients to terminate IDLE and
re-issue it at least every 29 minutes, because a server with a 30-minute inactivity
timeout may log an idle client off. We re-issue at **28 minutes**,
with the timer starting when IDLE is entered.

```
SELECT <inbox>
loop {
    IDLE
    wait for EXISTS / RECENT / FETCH / EXPUNGE, or 28 minutes, or cancellation
    DONE
    if (changed) { incremental header sync; flag sync }
    if (cancelled or account disabled) { break }
}
```

**The watch connection is not a work connection.** While IDLE is in progress the client
must not issue other commands — it has to send `DONE` first. So the watch connection
only watches; every fetch, store, and append uses the work connection (§3.2's second
one). Sharing them means either blocking the watch for the duration of every fetch, or
tearing down IDLE constantly.

**IDLE is a hint, not a source of truth.** An `EXISTS` means "something changed,
look"; the client still runs the incremental sync to find out *what*. Treating the
IDLE notification payload as authoritative is how clients end up with an off-by-one on
message counts.

**`NOTIFY` (RFC 5465) would let one connection watch many mailboxes**, and
Dovecot, Cyrus, and Fastmail advertise it. It is **deferred to post-beta**,
for one reason: it is a second push implementation with its own state machine, and beta
needs one push path that works everywhere rather than two that each work somewhere.
The upgrade is clean because it is gated on a capability check that already exists
(§5.2).

**Reconnect on watch failure** uses the same backoff as any other connection (§10). The
watch connection dropping must not mark the account errored — it is the expected
outcome of a laptop suspending.

### 5.8 Provider quirks

| Provider | Quirk | Handling |
|---|---|---|
| **Gmail** | Labels are folders; `\All` holds every message; `CONDSTORE` but no `QRESYNC` | Do not treat `\All` as a mailbox to sync by default — it duplicates every message. Sync `inbox`, `sent`, `drafts`, `trash`, and skip `all` unless the user asks. |
| **Gmail** | `[Gmail]/All Mail` reports `\All` | Covered by the role, not the name. |
| **Fastmail** | Standard IMAP, `SPECIAL-USE`, `MOVE`, `CONDSTORE`, `IDLE` | The reference path. |
| **Proton Bridge** | Loopback, self-signed cert, STARTTLS on 1143 | The loopback-scoped exception (§5.1). |
| **Dovecot** | `UIDPLUS`, `MOVE`, `CONDSTORE`, `QRESYNC` | Full-feature path. |
| Any | `\Noselect` folders | Listed, not selected, never synced. |
| Any | Server renumbers UIDs without changing `UIDVALIDITY` | Caught by the `UIDNEXT`-went-backwards check (§5.4). |

**Syncing `\All` on Gmail would double every message.** It is a real trap and it is
avoided by the role mapping, not by a hostname check.

---

## 6. Body and attachment fetch

### 6.1 `BodyFetch`

The two-step read from `debloatmaild.md` §6: the method populates the cache, the caller
re-reads SQLite.

```
SELECT <folder>
UID FETCH <uid> (BODY.PEEK[])
→ blob (raw MIME) → extract() (MIME-AND-TEXT.md) → item_text → body_state = 'cached'
```

**`BODY.PEEK[]`, not `BODY[]`.** Stated twice because it is the most damaging possible
mistake: a body fetch that marks the message read turns "click to read" into
"everything you look at is now read," which is exactly the behaviour
`debloatmailctl.md` §8 rejects for the CLI and `architecture.md` implies for the GUI.

**The raw MIME is stored as a blob** before extraction, so re-extraction (a version
bump, a parser fix) is possible offline with no network — the reason for
`item_text.extractor_version` (DATA-MODEL §5).

**The refinement rule — one authority, one direction.** Extraction of the fetched blob
(by `crates/extract` — the only parsing home, which never sees the server's
`BODYSTRUCTURE` response) reconciles the provisional attachment rows of §5.5:

- Parts the walk does not produce: rows are removed.
- Parts the walk produces that had no row: rows are added.
- Rows the walk produces: updated with the authoritative size, `content_type`, and
  `section_id`.

**Direction is load-bearing:** when header-time and extract-time disagree, **the walk
wins, and that is the only direction in which the list may change.** `BODYSTRUCTURE`
can lie about what the message contains; the parser of the actual bytes cannot. This
is the split recorded in DATA-MODEL §11.1 — header-time rows are the pre-fetch list a
thin cache must show without a body; the walk is the authority for final attachment
rows — and §17 carries the corresponding rewording owed to `MIME-AND-TEXT.md` §5.1,
whose unconditional "the walk is the authority" currently asserts one authority too
many.

**States:**

| Transition | When |
|---|---|
| `none` → `pending` | Fetch starts |
| `pending` → `cached` | Extraction completed, **including when the result is empty** |
| `pending` → `failed` | Fetch failed, size cap exceeded, or unparseable |

**`pending` is cleared on restart.** A crash mid-fetch leaves `pending` behind, and no
job is running to clear it. Startup sweeps `body_state = 'pending'` back to `none`
(§11.1), so a retry is possible rather than the item being stuck forever.

### 6.2 `AttachmentFetch`

```
UID FETCH <uid> (BODY.PEEK[<section_id>])
```

`section_id` is the RFC 3501 §6.4.5 section number (`MIME-AND-TEXT.md` §5.1) — the
column was renamed from `part_id` precisely because "part id" read like an array index,
and the value must be usable verbatim here. A value the parser invented cannot be sent
to the server. Attachments are addressed by `(item_public_id, section_id)` everywhere,
including the D-Bus signatures (DATA-MODEL §3, §5).

Returns a blob path. **No pin, no release method** — the caller gets the path and the
blob is protected by the one-hour `last_used_at` grace window (DATA-MODEL §9.3). This
removes both a leak and a missing D-Bus method.

**Attachment size cap** is an open question (DATA-MODEL §15.1, this document §16.1).
The engine enforces the cap if one is configured; the default is "no cap, the click
wins," because refusing to open a file the user clicked is worse than a slow fetch.

---

## 7. Feed sync

### 7.1 The fetch

```
GET <feed.url>
If-None-Match: <feeds.etag>
If-Modified-Since: <feeds.last_modified>
Accept: application/atom+xml, application/rss+xml, application/feed+json, application/xml;q=0.9, text/xml;q=0.8
User-Agent: debloatmail/<version> (+https://<project-url>)
```

The client is `reqwest` (no default features, `rustls-tls`) in `crates/feeds` —
`GEMINI.md`'s dependency decision, made binding here. The client is built with the
**redirect hook** of §2: each hop is validated before it is followed (§5.4 of
`SECURITY.md`); a rejected hop fails the fetch.

Conditional GET is the whole point: a `304 Not Modified` costs headers and nothing
else, and the server tells us the cached copy is current. The
`ETag` is sent back verbatim in `If-None-Match`; we do not parse or compare it
ourselves.

| Response | Action |
|---|---|
| `304` | Nothing changed. Update `next_fetch_at`. **Emit no signal** (`debloatmaild.md` §8). |
| `200` | Parse and upsert. |
| `301`/`308` | Permanent redirect: update `feeds.url` to the final URL, then parse. See note below. |
| `302`/`307` | Temporary: follow, do not update `feeds.url`. |
| `404`/`410` | Gone. Increment `consecutive_failures`; **never delete cached articles** (DATA-MODEL §9.1, §11.2). |
| `429`/`503` | Honor `Retry-After` exactly; it overrides computed backoff. |
| `5xx` | Transient. Backoff. |
| Redirect loop / >5 hops / hook rejects a hop | Fail; count as an error. |
| Content-Type is HTML | Fail with a clear message: "this URL returns HTML, not a feed." |
| Body > 10 MB | Reject before parsing (`MIME-AND-TEXT.md` §13). |

**On 301/308, the in-place URL update is deliberate, not a violation of the
remove-and-add rule.** The API-level rule that "changing a feed's URL is remove + add"
governs *user actions* — there is no `Feeds.Update(url)` and the user's provenance check
runs on re-add. A *server-side permanent relocation* during a poll is different: the
feed's identity (`public_id`), its `(account_id, url)` slot, and — critically — its
articles are all keyed on the feed row, not the URL. Wiping the row to re-add it would
destroy durable primary data (DATA-MODEL §9.1) to update a string.

### 7.2 The upsert

Match on `(feed_id, guid)`, falling back to `link`, then a content hash.

**The upsert updates content columns only.** The full list of columns it must never
touch (DATA-MODEL §11.4, verbatim):

`unread`, `starred`, `hidden_at`, `body_state`, `thread_id`, `created_at`.

Stated as a list because "don't reset read state" invites a partial fix that still
resets suppression. `hidden_at` in particular is what makes `Items.Hide` survive the
next poll; a hidden article that reappears every hour is the bug that list was created
to prevent.

**Extracted text is durable** (`durable = 1`), never evicted, and goes through the same
HTML converter as mail (`MIME-AND-TEXT.md` §11). A feed article whose text is evicted
is frequently gone forever, because the feed no longer serves it.

### 7.3 Scheduling and backoff

```
on success:  consecutive_failures = 0; backoff_secs = 0
             next_fetch_at = now + effective_interval + jitter
on failure:  consecutive_failures += 1          (column: DATA-MODEL §5, v3)
             backoff_secs = min(base * 2^(consecutive_failures-1), 86400)
             next_fetch_at = now + backoff_secs + jitter
```

`base = 300 s`, cap 24 h. After 10 consecutive failures the feed is marked errored
(`last_error` set, `next_fetch_at` pushed to the cap) but **still retried daily** —
feeds come back, and a feed that is permanently disabled is a feed the user has to
notice; the data stays (DATA-MODEL §11.2).

**The due-feed query is `removed_at IS NULL AND next_fetch_at <= now`, and nothing
else.** Two hard-won boundaries:

- It does **not** filter on `last_error` — that filter excluded exactly the feeds whose
  daily retry the paragraph above requires. v3's `idx_feeds_next` is
  `ON feeds(next_fetch_at) WHERE removed_at IS NULL` (DATA-MODEL §5), which we verified
  covers the query without the `last_error` predicate. Errored feeds are scheduled;
  diagnostic state is not schedule state.
- It does **not** exclude feeds awaiting first validation (§7.4): a `validated: false`
  row carries `next_fetch_at = now + effective_interval + jitter` from insert time and
  simply polls like any other feed — its scheduler-side treatment is identical. That is
  why the v3 index can be the index it is.

**Soft-removed feeds are not scheduled.** `Feeds.Remove` without purge sets
`removed_at` (DATA-MODEL §11.2): polling stops, but **the articles remain visible in
streams and search** — they are primary data, not cache. Only the purge form deletes:
the `feeds` row in one transaction, the `feed_items` rows by FK cascade, **and the
spine `items` (and their `item_text`) rows deleted explicitly, because there is no FK
from `items` back to `feed_items`** — the cascade does not reach them, and leaving them
would produce visible items with no source (DATA-MODEL §11.2). A feed in its soft-
remove state keeps its `public_id` and its articles until that happens.

**Feeds are never auto-removed by failure.** A server having a bad week must not
convert primary data into nothing (DATA-MODEL §9.1). The one exception is a feed that
never validated (§7.4).

### 7.4 `Feeds.Add` — hybrid validation, per DATA-MODEL v3 §11.3

`debloatmaild.md` §8 makes `Feeds.Add` fetch and validate. **How a timeout interacts
with "fail loudly, don't create an empty feed" is decided by DATA-MODEL §11.3, and this
section adopts that decision verbatim as engine behaviour.** Two failure
kinds, two treatments:

**Hard failures create no row.** A hard failure is a verdict, not a delay: `404`/`410`,
HTML or other non-feed content, malformed XML, a redirect the hook rejects, a redirect
loop, or more than 5 hops. If one occurs — synchronously *or* in the background
continuation below — **no feed row exists** (synchronous) **or the row is deleted**
(background), and the addition is reported as failed with a specific error:
`Gone`, `NotAFeed`, `Malformed`, `RedirectRejected`. A verdict is final; retrying a
`404` at install time would only paper over "you pasted the wrong URL."

**A timeout is not a verdict.** The synchronous validation budget is **15 s** —
engine-side, chosen comfortably under the D-Bus caller timeout (60 s). If the fetch +
parse has not produced a verdict by 15 s:

1. **Insert the row now** — `public_id`, `account_id`, `url` (as given), `name` (as
   given, or the parsed title if one arrived before the budget), `interval_secs`
   (DATA-MODEL §11.3), `next_fetch_at = now + effective_interval + jitter`, with
   `title`, `site_url`, `etag`, `last_modified`, `last_fetch_at` all `NULL`.
2. **Return the feed to the caller with the result field `validated: false`** — honest:
   the row exists, the content is not yet confirmed.
3. **The in-flight request continues in the background** with the same redirect hook:
   - *Succeeds* → fill `title`, `site_url`, `etag`, `last_modified`, set
     `last_fetch_at = now`. `validated` semantics complete; the next scheduled poll is
     then a conditional GET.
   - *Hard-fails* → **delete the row** and report the addition as failed (same error
     classes as above).
   - *Again undecided* → it is now simply the feed's first poll, on the §7.3 schedule,
     with the §7.3 failure escalation if it keeps not connecting.

**The invariant: no feed is ever left *permanently* unvalidated.** An unvalidated feed
is one with `last_fetch_at IS NULL` (there is no `validated` column in v3 — the state
is derived, and that is what keeps the schema quiet about it). Such a feed either
validates, or reaches a hard failure, or exhausts the §7.3 escalation; all three
endings are decided and surfaced. A `validated: false` row is *transient by
construction* — that is DATA-MODEL §11.3's resolution of the earlier
insert-first-then-validate vs fail-loudly contradiction, and it is what this section
was written to adopt (the previous engine draft's pure validate-first, which would have
left nothing on a timeout, is withdrawn in favour of this).

**Fast path.** If validation succeeds *within* the budget, the row is inserted after
the fetch, with `etag` and `last_modified` captured from the first response — so the
first scheduled poll is conditional — and the result carries `validated: true`.

**Two consequences to hold:**

- `validated: false` means *transient*, not *broken*. Clients (CLI `feeds add`
  output, GUI) must not render it as an error; §17 carries the documentation debt to
  `debloatmaild.md` §8 and the naming of the failure signal.
- Whether `Feeds.Add`'s first fetch also upserts the feed's items — one fetch instead
  of two — is `debloatmaild.md` §8's API contract to state; the engine supports it
  either way (the row carries an `etag` once validated, so a separate first poll would
  cost a `304`).

---

## 8. The `pending_ops` drain

### 8.1 Ordering

Ops for one account drain **in `created_at` order, oldest first**. Ordering matters
because ops compose: mark read, then move, then delete must happen in the order the
user performed them.

**Ops are per-account, not global.** A global queue would make a broken account block
a working one.

**Drafts enter as sends, not as their own op kind.** Drafts are local-only
(DATA-MODEL §11.5): `Compose.Send(draft_id)` reads the draft row, queues a `send` op
whose **payload carries the draft inline** (self-sufficient, per DATA-MODEL §11.6),
and deletes the draft row. From that moment the op is identical to any inline send —
including the §8.4 confirmation semantics — and the drain has no draft-specific code.
There is no `draft` op kind, by design.

### 8.2 The drain loop — and what counts as an attempt

`in_flight` means "attempted at least once; may be retried" (DATA-MODEL §11.6). The
loop follows that definition exactly — an op *stays* `in_flight` across retries, and
`pending` is for ops not yet tried:

```
take next op (created_at order)
SET state = 'in_flight', persist          -- before the first attempt (and before any I/O)
SELECT <folder>          (mail ops only)
issue the IMAP/SMTP command
  definitive success →              state = 'done',  finished_at = now
  server-side failure (the server answered, or the ack was lost) →
      attempts += 1
      attempts >= 10  →  state = 'failed', finished_at = now, last_error = <reason>
      otherwise       →  stay 'in_flight', last_error = <reason>,
                         next retry per §8.6
  connection-side failure (DNS / TCP / TLS / auth / disconnect before a verdict) →
      attempts UNCHANGED; stay 'in_flight'; the op rides the account's
      reconnect backoff (§10.2) with no retry clock of its own
```

**`attempts` increments only on server-side failures** — the connection was
established, authenticated, and the server engaged with the command. A connection-side
failure does not touch `attempts`; the op simply waits behind the account's own
reconnect backoff.

**This is the fix for the offline laptop.** The review's scenario: a laptop offline
for ~12 hours with a `send` op queued. Under "any failure increments `attempts`," 12
hours of reconnect failures would burn the op to `failed` **without ever having
reached a server** — a message the network, not the user, failed. Under this rule the
op sits at `attempts = 0` for the whole outage: the account backs off (§10.2), the op
rides along, and the first successful connection is also the first *attempt*. This is
the correction DATA-MODEL §11.6 names (`SYNC.md` §8.2/§9 "corrected to match"); this
revision is that correction, and §9 below is the offline-mode statement it
cross-references.

**Drain before sync, not after.** Draining first means the subsequent flag fetch
already reflects the user's changes, so there is nothing to reconcile. The
pending-op-wins rule (§5.6) covers the remaining race.

### 8.3 Semantics per op

| `op` | IMAP/SMTP | Idempotent? | Notes |
|---|---|---|---|
| `flag` | `UID STORE <uid> ±FLAGS` | **Yes** | Re-running sets the same flags. Safe to retry blindly. |
| `move` | `UID MOVE`, else `COPY`+`STORE \Deleted`+`EXPUNGE` | No | Retry must tolerate "no such UID" as success — the move already happened. |
| `delete` | `STORE \Deleted`, then `UID EXPUNGE` if `UIDPLUS` | No | Same tolerance. |
| `expunge` | `UID EXPUNGE <uid-set>` | No | **Never plain `EXPUNGE`.** Plain `EXPUNGE` removes every `\Deleted` message in the mailbox, including ones another client flagged. This is a data-loss bug, not a style choice. |
| `send` | SMTP `DATA` (+ `APPEND`) | **No** | See §8.4. |

**`EXPUNGE` without `UIDPLUS`:** if the server lacks `UIDPLUS`, `UID EXPUNGE` is
unavailable and plain `EXPUNGE` is unsafe. In that case we **do not expunge
implicitly** — the message is moved to Trash and left `\Deleted`. Emptying Trash
becomes an explicit user action that warns it may affect other clients. The CLI's
`delete --expunge` must carry the same warning when the account lacks `UIDPLUS`
(`debloatmailctl.md` §8).

### 8.4 Send: the one op that cannot be retried blindly

SMTP has no idempotency key. If the connection drops after `DATA` but before the `250`,
the client genuinely does not know whether the message was sent.

```
Send op payload includes: message_id (generated at queue time), append_to_sent, sent_folder
```

**The no-verdict rule:** a send attempt ends in either a *definitive* outcome (a
`250`, or a rejection from the server) or *no verdict* (the connection died mid-
transfer, **or the process died after persisting `in_flight`** — a crash is a no-verdict
ending for the engine's successor). The treatments differ:

- *Definitive rejection:* ordinary server-side failure per §8.2 — `attempts`
  increments, the op retries on the formula. Nothing was accepted; resending is safe by
  the server's own say-so.
- *No verdict:* the op **must not be resent blindly.** Its next retry is the
  confirmation path, and — deliberately — **a no-verdict attempt does not increment
  `attempts`**: it is a delivery uncertainty, not a server rejection, and punishing
  the one op kind where blind retry is most dangerous with the same counter as
  everything else would let one bad week of lines turn a queued message into a
  `failed` op (DATA-MODEL §11.6's offline rule, extended by its reasoning; logged in
  §17 as the one-line note owed to that section).

**The confirmation path, on the next connection:**

1. If `append_to_sent` is set, search the Sent folder for the payload's `message_id`;
   else search the account's own Sent folder by `Message-ID` header (`UID SEARCH
   HEADER Message-ID <id>`).
2. **Found** → `state = 'done'`, `last_error` noting the send was confirmed after an
   interrupted attempt. (If the confirmation ran as a re-send, the `APPEND` step of
   §8.5 still applies.)
3. **Not found** → safe to send as a fresh delivery: the message goes out under the
   *same* `Message-ID`, and this fresh attempt counts as a server-side failure or
   success per §8.2, like any other.

**Why this is the right rule.** The two risks are a duplicate message and a lost
message. A duplicate is annoying and visible; a lost message is silent and
unacceptable in a mail client. But *blindly* retrying converts a rare ambiguity into
routine duplicates whenever the network is flaky, which is worse than either.
Confirming by `Message-ID` costs one `SEARCH` and resolves the ambiguity instead of
guessing at it.

**The generated `Message-ID` is stable in the payload**, not regenerated per attempt —
that is what makes the search meaningful. It is generated once, when the op is
created, which is also why an orphaned `send` op (item gone, `item_id IS NULL`) is
still fully executable: its purpose is the payload, not the row (DATA-MODEL §11.6).

### 8.5 Send + append, restated

`op = 'send'` with `payload.append_to_sent` is **one op** (DATA-MODEL §11.7):

- Send first, then append.
- **Success is defined by the send.** If the append fails, the op is `done`,
  `success = true`, with a non-empty `detail` describing the missing Sent copy.
- Reporting failure after a successful send is the more dangerous error, because a
  user told a message failed will send it again.

**With `UIDPLUS`, the `APPENDUID` response gives the Sent copy's UID**, so
the copy can be inserted locally without re-syncing the Sent folder. Without it, the
Sent folder is re-synced on the next cycle. This is a real performance difference on a
folder that only ever grows.

### 8.6 Retry and terminal states

One formula for op retries, and **the earlier stepped table (1 min, 5 min, 15 min,
then 1 h for attempts 4–8) is deleted** — it contradicted the exponential formula, and
two schedules for one knob is how they drift. DATA-MODEL §11.6 cites §10.2 as the
single schedule; this is the same formula:

```
after failure k (attempts = k):  delay = min(60 s × 2^(k-1), 6 h)   (× jitter, §10.2)
the 10th failure  →  state = 'failed'  (terminal)
```

| `attempts` (= k) | Next retry in |
|---|---|
| 1 | 1 min |
| 2 | 2 min |
| 3 | 4 min |
| 4 | 8 min |
| 5 | 16 min |
| 6 | 32 min |
| 7 | 64 min |
| 8 | 2 h 8 m |
| 9 | 4 h 16 m |
| 10 | — (terminal: `failed`) |

The `6 h` cap is stated for symmetry with §10.2 and future-proofing; at `k ≤ 9` it
never binds. (The previously printed rows "7 → 1 h, 8 → 2 h, 9 → 4 h, 10 → 6 h" were
not the formula and are gone.)

`failed` is terminal. Terminal rows (`done`, `failed`) are retained **7 days**
(DATA-MODEL §11.6), then purged by a housekeeping pass, so `Queue.Get` can answer
"did it go out?" within that window.

**A `failed` op must surface, not vanish silently.** `Queue.Pending()` includes
`failed`, `Queue.Count` counts the same set (not `done`), the tray shows
`NeedsAttention`, and `doctor` lists them (DATA-MODEL §11.6, §15.6). A message that
never sends and never says so is the worst outcome this system can produce.

---

## 9. Offline mode

`--offline` (CLI) and the daemon's own network detection are two different things.

**CLI `--offline`:** the CLI never calls a network method. `list`, `search`, and
`read` of a cached body work; `read` of an uncached body exits 7
(`debloatmailctl.md` §7). No daemon call is made, so this works with the daemon
stopped.

**Daemon offline:** the daemon has no offline switch. It attempts connections, fails,
backs off, and keeps the local data fully usable. There is no "offline mode" to enter
or leave, because a daemon that had to be told it was offline would be wrong exactly
when it mattered.

**Op queue under offline:** a long outage consumes no `attempts` (§8.2 —
connection-side failures are not attempts, and DATA-MODEL §11.6 says the same: "a
week offline must not burn the ten attempts that `failed` is terminal for"). The
queue rides the account backoff (§10.2) and everything that was queueable is still
queueable when the connection returns.

**The one interaction:** when the daemon is running and the CLI passes `--offline`,
the CLI must not *call* `FetchBody` even though the daemon could reach the network.
`--offline` is a promise to the caller about this invocation, not a statement about
the machine.

---

## 10. Failure taxonomy and backoff

### 10.1 Classification

| Class | Examples | Response |
|---|---|---|
| **Transient network** | DNS failure, `ECONNREFUSED`, TLS handshake timeout, socket reset | Retry with exponential backoff + jitter. **Connection-side: consumes no op `attempts` (§8.2).** |
| **Transient server** | `NO [UNAVAILABLE]`, `BYE` with `[ALERT]`, `5xx` HTTP | Backoff. |
| **Auth failure** | `NO [AUTHENTICATIONFAILED]`, bad password | **Do not retry.** Mark the account errored, surface it, stop the schedule. The only exit is the user rotating the secret via the credential-rotation surface (`debloatmaild.md` `Accounts.Update` — owed, §17); that reset clears the error state and backoff and re-validates. Retrying a bad password is how accounts get locked, and an un-rotatable failure is a dead account, not a slow one. |
| **Protocol violation** | Unparseable response, unexpected state | Drop the connection, reconnect once, then mark errored. A parser that loops on a bad response burns CPU forever. |
| **Permanent** | `404`/`410` on a feed, `\Noselect` folder | Record, stop retrying that item, keep the data — except §7.4's never-validated case, where the row is deleted. |
| **Local** | Disk full, DB locked, write failure | **Stop everything.** A sync that cannot commit is a sync that is losing data. |

**Connectivity failures stop the schedule; everything else backs off — and auth
failures stop it until the secret is rotated.** The distinction that matters: a wrong
password is not a transient condition, and a client that retries it every 15 minutes
is indistinguishable from an attacker.

### 10.2 Backoff

```
delay = min(base * 2^(failures-1), cap) * (0.9 + rand*0.2)
```

| Target | Base | Cap |
|---|---|---|
| Account reconnect | 30 s | 15 min |
| Feed poll (on error) | 5 min | 24 h |
| Op retry (server-side failures only, §8.2) | 1 min | 6 h |

The op row is op attempts only; connection-side op failures ride the account row —
offline is not an attempt (§8.2, §9; DATA-MODEL §11.6).

**Jitter is mandatory**, not a nicety. Without it, every account that failed during
the same outage reconnects at the same instant and re-creates the outage.

**Backoff resets on success**, not on a timer. A connection that succeeds and then
immediately fails again should back off further, not start over — so the counter
resets only after a successful *operation*, not a successful *connect*.

### 10.3 The degraded state

`Status.State = 'degraded'` means: at least one account is errored, or the pending-op
queue has `failed` entries. It is not an error state — it is "working, but something
needs attention," and it is what the tray icon's `NeedsAttention` reflects.

---

## 11. Resumability and crash recovery

### 11.1 On startup

| Leftover state | Action |
|---|---|
| `items.body_state = 'pending'` | Reset to `none` (§6.1). |
| `pending_ops.state = 'in_flight'`, `op != 'send'` | **Stay `in_flight`** — "attempted at least once; may be retried" remains true, and the interrupted attempt has no verdict, so it re-runs without incrementing `attempts` (§8.2 connection-side rule). `flag` is idempotent; `move`/`delete`/`expunge` tolerate "already done" (§8.3). |
| `pending_ops.state = 'in_flight'`, `op = 'send'` | **Stay `in_flight`; the first retry is the §8.4 `Message-ID` confirmation, never a blind resend.** Persisting `in_flight` before sending means "crashed after possibly sending," which is by definition a no-verdict ending. |
| Orphaned ops (`item_id IS NULL`, from a §5.4 reset, an expunge, or a purge) | Left for the drain (DATA-MODEL §11.6): self-sufficient parts complete, item-dependent parts fail with an explanatory `last_error`, and the row survives to surface. |
| Any job state | Discarded — jobs are in-memory (§4.1). |
| `folders.last_sync_at` in the future | Clamp to now. A clock change should not disable sync for a week. |
| A folder mid-UIDVALIDITY-reset | Detected again by the normal mismatch check; the §5.4 procedure is idempotent under rerun. |

### 11.2 What makes sync resumable

Everything the sync needs to resume is in the schema: `folders.uidvalidity`,
`folders.uidnext`, `folders.highestmodseq`, `feeds.etag`, `feeds.last_modified`,
`feeds.next_fetch_at`, `feeds.backoff_secs`, `feeds.removed_at`,
`pending_ops.(state, attempts, last_error)`. There is no in-memory sync state that
matters.

**This is the property to preserve.** A sync engine whose progress lives in memory
cannot survive a laptop lid closing, and this one closes constantly.

### 11.3 Commit granularity

| Operation | Commit point |
|---|---|
| Header batch (2,000 UIDs) | After the batch, with `uidnext` advanced **in the same transaction** — and the account mutex released across the inter-batch gap, which is where interactive fetches run (§3.1/§3.2) |
| UIDVALIDITY staged recovery | The whole procedure holds the mutex (§3.1's exception); each refetch window commits per the row above; `uidvalidity`, `uidnext`, `highestmodseq` advance in the **final** window's commit |
| Flag batch | After the batch |
| Feed poll | After the upsert, with `etag`/`last_modified`/`next_fetch_at` in the same transaction |
| Op drain | After each op (the `in_flight` write happens *before* the command, on its own) |
| Body extraction | After the text is written |

**The cursor and the data commit together.** Advancing `uidnext` in a separate
transaction from the rows it describes is the bug that produces a permanently skipped
window after a crash: the cursor says "fetched," the rows were never written, and
nothing will ever fetch them again.

**Op state commits the other way around, deliberately:** `in_flight` precedes the
command, so the crash-recovery rules above can distinguish "crashed after possibly
sending" from "never tried." Under-committing the op state is what destroys send
confirmation on crash.

### 11.4 Idempotency of a resumed sync

Re-running a header fetch for a UID window already stored is harmless: the upsert
matches on `(folder_id, uidvalidity, uid)` and updates — **so long as the folder's
`uidvalidity` matches the server's.** That condition is the caveat the review caught:
when it does *not* match, the same-folder window is not an update, it is the staged
recovery case, and "resume" must never be used as an excuse to fall into an
in-place upsert against a stale epoch. The rule:

- **Same `uidvalidity`:** upsert as normal; idempotent.
- **Changed `uidvalidity`:** the staged recovery procedure (§5.4) is the *only* path —
  delete-then-reinsert with the preservation map, never in-place upsert.

Re-running a feed poll is unconditionally harmless: the upsert matches on
`(feed_id, guid)` and preserves state (§7.2). This is why resumption needs no "was
this committed?" bookkeeping beyond the cursor — and why the uidvalidity check is
load-bearing, not decorative.

---

## 12. Politeness and rate limits

| Rule | Value |
|---|---|
| Per-host concurrency | 1 |
| Minimum gap between requests to one host | 1 s |
| `Retry-After` | Honored exactly, overrides computed backoff |
| Redirects followed | 5, each hop through the security hook (§2, §7.1) |
| Conditional GET | Always, when the server supports it |
| `User-Agent` | `debloatmail/<version> (+<project-url>)` — identifies us and offers contact |
| Connection reuse | Keep-alive for feeds; **two** IMAP connections per active account (watch + work, §3.2/§5.7) and **one** for accounts without an inbox role |
| Max header fetch window | 2,000 UIDs per command |

*(The earlier "one IMAP connection per account" in this table contradicted §3.2; this
line is the fix — an account with an IDLE watch holds two connections by design, and
steady state is the table above.)*

**We do not fetch a feed we were not subscribed to, and we do not fetch linked
pages.** Summary-only feeds are an open question (DATA-MODEL §15.2, this document
§16.2) and the current answer is no — which means the daemon makes exactly one
request per feed per interval and no others.

**A feed poll is not a crawler.** No `robots.txt` handling is needed because we fetch
one URL the user explicitly added, at an interval the feed's own `ttl`/
`sy:updatePeriod` may lengthen (if present, it is used as a floor — open question
§16.5).

---

## 13. Progress and signals

### 13.1 Phase mapping

`debloatmaild.md` §6 enumerates `connecting`, `listing`, `fetching`, `storing`,
`done`; the `parsing` addition for feeds was agreed in the daemon revision (accepted;
still owed to the introspection XML — §17). The phases are per-source:

| Source | Phases |
|---|---|
| Mail | `connecting` → `listing` (folder enumeration) → `fetching` (headers) → `storing` → `done` |
| Feed | `connecting` → `fetching` (HTTP) → `parsing` → `storing` → `done` |
| Body fetch | `fetching` → `storing` → `done` |

`percent` is 0–100, or `0xFFFFFFFF` for indeterminate — the convention from
`debloatmaild.md` §6, restated because "0" and "unknown" are otherwise
indistinguishable.

**`percent` is per-phase, not per-job.** A sync that reports 40% during `fetching` and
then 0% during `storing` looks like a regression to a progress bar. Clients must treat
the percentage as scoped to the current phase, and the phase string is what tells them
the phase changed. Both `Progress` and `Finished` carry `job_id` (added by the daemon
revision — §17), so a scheduled sync and an interactive body fetch on one account are
distinguishable.

### 13.2 Signal emission points

| Signal | Emitted |
|---|---|
| `Items.Changed` | After any commit that touched `items` — coalesced per 100 ms (`debloatmaild.md` §16) |
| `Sync.Progress` | On phase transitions, and at most every 250 ms within a phase |
| `Sync.Finished` | Once per job, at terminal state |
| `Feeds.Updated` | After a poll that inserted items — **not** on `304`, not on a poll with zero new items, and not on a bare validation fill (§7.4) |
| `Accounts.StateChanged` | On state transitions only |
| `Queue.Completed` | Once per op, at terminal state, carrying `success` and `detail` (DATA-MODEL §11.7) |

**No signal on a no-op.** `debloatmaild.md` §8 already states this for
`Feeds.Updated`; it is a general rule. A signal that fires when nothing happened
trains clients to ignore signals, and then they miss the one that mattered.

The signal names above are the only ones that exist; the `ItemsChanged`/`SyncProgress`/
`SyncError` names in the older CLI text are dead (`debloatmailctl.md` §10).

### 13.3 Throttling

A first sync of 200,000 headers would otherwise emit tens of thousands of `Progress`
signals. Within a phase, progress is throttled to 4 Hz, and the **final** value in each
phase is always emitted — a throttled signal stream must never lose the 100%.

---

## 14. The consolidated state machine

Per account:

```
                 ┌──────────────────────────────────────────┐
                 ▼                                          │
  Disabled ──► Idle ──► Connecting ──► Authenticating ──► Syncing
                 ▲          │              │                │
                 │          ▼              ▼                │
                 │       Backoff ◄──── Error ◄──────────────┘
                 │          │
                 └──────────┘ (timer)

  Syncing ──► Watching (IDLE)   [inbox only, if IDLE supported]
       ▲            │
       └────────────┘ (EXISTS → incremental sync → re-IDLE)
```

| State | D-Bus `Account.State` | Meaning |
|---|---|---|
| `Idle` | `idle` | Connected or dormant; nothing in flight |
| `Connecting` | `connecting` | DNS/TCP/TLS in progress |
| `Authenticating` | `connecting` | Credentials exchanged |
| `Syncing` | `syncing` | Header/flag/feed work in progress |
| `Watching` | `idle` | IDLE held; this is a healthy state, not a busy one |
| `Error` | `error` | Last attempt failed; `detail` carries why. **Auth errors stay here until the secret is rotated** (§10.1). |
| `Backoff` | `error` | Waiting to retry |
| `Disabled` | `disabled` | `accounts.enabled = 0` |

**`Watching` reports as `idle`, deliberately.** A user seeing "syncing" forever because
IDLE is held would reasonably conclude the client is stuck. Watching is the steady
state of a healthy account.

---

## 15. Testing

`architecture.md` §13 sets the rule: **never hit live providers in tests.**

| Layer | Approach |
|---|---|
| Coroutine pump | In-memory buffers. Scripted server responses, asserted client bytes. No socket. |
| Scenario tests | `tests/imap/` against local **GreenMail** or **Dovecot** |
| Failure injection | Truncated responses, mid-command disconnects (including **mid-`DATA`**, which must drive the §8.4 confirmation), `BYE` at awkward moments, server that closes after `AUTHENTICATE`, a week of simulated unreachable host |
| Feeds | Recorded HTTP responses: `200`/`304`/`404`/`410`/`429`/`500`, redirect chains (including a hook-rejected hop), malformed XML, HTML-served-as-feed, oversized bodies, and a target that **takes 20 s** (the §7.4 timeout path) |
| Idempotency | Run every scenario twice; assert the second run changes nothing |
| Crash recovery | Kill the process mid-sync at a scripted byte offset — including *mid-UIDVALIDITY-reset*, which must rerun idempotently (§5.4) — restart; assert no window is skipped and no op is dropped |
| Backoff | Deterministic clock injection; assert the sequence, including jitter bounds, and that offline time leaves `attempts` at 0 |

**The scenario fixtures that matter most, in order:**

1. `UIDVALIDITY` change with `Message-ID` re-mapping — asserts `public_id`
   preservation, the documented failure for messages with no `Message-ID`, **and that
   queued ops re-bind to the replacement rows via the §5.4 snapshot while ops on
   vanished items end `failed` with an explanatory `last_error`, none dropped** — plus
   a kill mid-procedure, asserting the rerun converges.
2. Crash between "rows written" and "cursor advanced" — asserts the atomic commit
   (§11.3).
3. Send interrupted after `DATA` (connection drop *and* process kill) — asserts the
   `Message-ID` confirmation runs, no duplicate is produced, `attempts` is unchanged
   by the no-verdict ending, and a fresh re-send reuses the payload's `Message-ID`.
4. `EXPUNGE` attempted on a server without `UIDPLUS` — asserts we do **not** issue a
   plain `EXPUNGE`.
5. Gmail profile: `CONDSTORE` without `QRESYNC` — asserts the UID-diff expunge path.
6. `\All` folder present — asserts it is not synced by default.
7. `Feeds.Add` against a 404 / HTML / malformed target — asserts **no row exists**
   afterwards; against a >15 s target — asserts a `validated: false` row exists, the
   background continuation fills it on success **or deletes it + reports failure on a
   hard failure**, and a no-verdict continuation simply becomes the feed's first
   normal poll (§7.4).
8. `sync --full` on a folder with queued ops — asserts the staged recovery, the op
   re-binding, and the unpreservable count.
9. Interactive `BodyFetch` issued mid-initial-sync of the same account — asserts it
   completes at a window boundary, not after the sync (§3.1); steady state holds two
   IMAP connections per active account.
10. Hiding a feed item and re-polling — asserts `hidden_at` survives; a `304` emits
    no `Feeds.Updated`.

**Determinism requirement:** the scheduler takes an injected clock and an injected RNG,
so backoff sequences and jitter are assertable. A sync engine tested against
`SystemTime` is not testable.

---

## 16. Verification checklist

- [ ] No `BODY[]` appears anywhere; every body/part fetch uses `BODY.PEEK`.
- [ ] No `part_id` identifier anywhere; part addressing is by `section_id` (RFC 3501 §6.4.5), verbatim in `BODY.PEEK[section_id]`.
- [ ] `CAPABILITY` is re-read after `AUTHENTICATE`, and after `STARTTLS`.
- [ ] Every header window fetches `UID FLAGS INTERNALDATE RFC822.SIZE ENVELOPE BODYSTRUCTURE`; `INTERNALDATE` feeds `sort_at`; `BODYSTRUCTURE` seeds provisional attachment rows.
- [ ] Every new mail item has an `item_text` row from header sync (`title`/`author` set, `body = ''`); a message never opened is found by subject and author search.
- [ ] A `UIDVALIDITY` change (or `sync --full`) runs the staged procedure: map built, ops snapshotted, rows deleted, refetch with `public_id` preservation by `Message-ID`, ops re-bound, cursors advanced last; kill-mid-procedure converges on rerun; `doctor` reports the unpreservable count; no op is ever silently dropped.
- [ ] A crash between row-write and cursor-advance skips no UID window (cursor and rows commit together).
- [ ] `in_flight` is persisted *before* the command; after a failed attempt the op is still `in_flight` (not reset to `pending`); `attempts` increments only on server-side failure and is unchanged by a week of unreachable host and by a no-verdict send ending.
- [ ] On restart, in-flight non-send ops re-run without incrementing `attempts`; in-flight send ops first run the `Message-ID` confirmation, producing one delivered message or a confirmation, never a duplicate.
- [ ] Op retry delays follow `min(60 s × 2^(k-1), 6 h)` with the 10th failure terminal — no stepped 1/5/15 table survives anywhere in the code or docs.
- [ ] `folders.highestmodseq` advances monotonically, resets with the folder on uidvalidity change, and a `CONDSTORE` flag fetch returns only changed messages; a post-reset flag sync fetches the full tracked window.
- [ ] Gmail's `\All` folder is not synced by default; its `CONDSTORE`-without-`QRESYNC` profile exercises the UID-diff expunge path.
- [ ] Hiding a feed item and re-polling leaves `hidden_at` set; a `304` and a no-insert poll emit no `Feeds.Updated`.
- [ ] `Feeds.Add` against a 404/410/HTML/malformed/rejected-redirect target leaves **no row**; against a >15 s target it returns `validated: false` with a row whose background continuation either fills `etag`/`title`/`last_modified` + `last_fetch_at` or deletes the row and reports the failure; no feed stays with `last_fetch_at IS NULL` permanently.
- [ ] Due-feed selection is `removed_at IS NULL AND next_fetch_at <= now` with no `last_error` predicate; a soft-removed feed is not polled but its articles remain in streams and search; purge deletes `feed_items` by cascade and the spine rows explicitly, in one transaction.
- [ ] A 301/308 updates `feeds.url` in place and preserves the feed's `public_id` and its articles.
- [ ] Every feed redirect passes the hook before the hop; a rejected hop fails the fetch; >5 hops or a loop fails.
- [ ] Plain `EXPUNGE` is never issued; `UID EXPUNGE`
