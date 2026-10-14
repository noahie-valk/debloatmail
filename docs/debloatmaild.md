# D-Bus Interface Specification

The contract between `debloatmaild` (the daemon), `debloatmail` (the GUI), and
`debloatmailctl` (the CLI).

This document is normative: the introspection XML in Appendix A is the source of
truth, and the prose explains the intent behind it. Where the two disagree, the XML
wins and this file has a bug.

---

## 1. Purpose and scope

Three processes share one local database and one set of credentials. This document
defines exactly who may touch what, and how they tell each other about it.

**The governing rule from `DATA-MODEL.md` §6:** `debloatmaild` is the sole writer.
Everyone else reads.

| Process | Reads | Writes | Network | Owns |
|---|---|---|---|---|
| `debloatmaild` | ✓ | ✓ (sole) | ✓ | All I/O, scheduler, credentials, **the tray** (SNI + dbusmenu, §18) |
| `debloatmail` (GUI) | ✓ (direct SQLite) | ✗ (via daemon) | ✗ | Windows |
| `debloatmailctl` | ✓ (direct SQLite) | ✗ (via daemon) | ✗ | Nothing |

**Why reads bypass D-Bus.** A 10,000-item list serialized over D-Bus is slow,
allocation-heavy, and needs its own pagination protocol — while WAL already gives us
lock-free concurrent readers. So the GUI and CLI open the database read-only and query
it directly; the daemon's job on the read side is only to *populate* it (fetch bodies,
attachments) and to *announce* that it changed.

**What that buys:** `debloatmailctl list` works with the daemon stopped, in a cron job,
and over a slow link. `--offline` becomes trivial. And the GUI and CLI share one read
path, which means one place for query bugs instead of two.

**What it costs:** the GUI links `debloatmail-store` (and therefore `rusqlite`), and
must never write. That is enforced by opening the connection `SQLITE_OPEN_READ_ONLY`,
not by convention.

---

## 2. Bus names

**Two bus names, not one.** This is the single most consequential decision in this
document.

| Bus name | Owner | Why |
|---|---|---|
| `org.noahie.debloatmail` | **GUI** (`debloatmail`) | Required for `DBusActivatable=true` and `org.freedesktop.Application` |
| `org.noahie.debloatmail.Daemon` | **Daemon** (`debloatmaild`) | The engine service the GUI and CLI call |

**The GUI owns the primary name because the desktop entry spec requires it.** For
`DBusActivatable=true`, the application must be activatable at a well-known name equal
to its desktop file name minus `.desktop`, and must implement
`org.freedesktop.Application` with `Activate`, `Open`, and `ActivateAction`.
Since our App ID is `org.noahie.debloatmail`, the GUI must own that exact name.

That interface is what makes `mailto:` links, desktop actions, and startup-notification
correctness work — the shell calls `Open(["mailto:..."])` on the running instance
rather than spawning a second process. It is worth the extra name.

### Not activatable: the CLI

`debloatmailctl` owns no bus name. It is a short-lived client that connects, calls,
disconnects. It must not appear in `NameHasOwner` checks, which the daemon uses to
detect GUI presence (§14).

---

## 3. Object tree

```
/org/noahie/debloatmail                      ← root; all global interfaces
├── /org/noahie/debloatmail/accounts/018f3a9c4b2e7d1c…   ← per-account status
├── /org/noahie/debloatmail/accounts/018f3a9e…
├── /org/noahie/debloatmail/feeds/018f3aad…              ← per-feed status
└── /org/noahie/debloatmail/feeds/018f3ae1…

(separate unique name, registered with the watcher — see below)
org.freedesktop.StatusNotifierItem-PID-ID  ← unique name
├── /org/noahie/debloatmail/StatusNotifierItem   ← org.freedesktop.StatusNotifierItem
│                                                + org.kde.StatusNotifierItem (both, §18)
└── /org/noahie/debloatmail/StatusNotifierMenu   ← com.canonical.dbusmenu
```

The root object implements `org.freedesktop.DBus.ObjectManager`. Per-account and
per-feed objects are its managed children, so a client enumerates the whole world with
one `GetManagedObjects()` call and then stays current via `InterfacesAdded` /
`InterfacesRemoved`. This is why there is no `ListAccounts()` method — the standard
interface already does it, better.

**Object paths encode `public_id`, hyphens removed.** A UUID's hyphens are stripped
for the path: `3f2504e0-4f89-4b5c-…` → `/accounts/3f2504e04f894b5c…`. D-Bus path
elements cannot contain `-`, and a UUID has no other characters that need escaping,
so the mapping is lossless and trivial. The earlier "paths use internal rowids"
version violated `DATA-MODEL.md` §3 — rowids never cross the process boundary — and a
rowid-based path is wrong on a delete-and-refetch anyway, since a rowid is exactly
what is not preserved. Paths remain *handles*: they are valid for the daemon run,
clients still must not cache them across a restart, and every method argument that is
an identity is a hyphenated `public_id` string, never a path.

**The SNI objects are not on the daemon's well-known name.** The status item and its
menu live on the unique name the daemon registers with the `StatusNotifierWatcher`
(§18), which is how the spec works. The earlier tree, which showed them under
`org.noahie.debloatmail.Daemon`, contradicted §18's own text; §18 wins.

---

## 4. Interface index

| Interface | Object | Purpose |
|---|---|---|
| `org.noahie.debloatmail.Status` | root | Daemon-level properties, incl. `Features` and `Revision` |
| `org.noahie.debloatmail.Items` | root | Item mutations (read/star/move/delete) |
| `org.noahie.debloatmail.Sync` | root | Trigger sync, fetch bodies and attachments; job progress signals |
| `org.noahie.debloatmail.Accounts` | root | Account lifecycle, incl. `Update` |
| `org.noahie.debloatmail.Feeds` | root | Feed lifecycle, incl. `Update` |
| `org.noahie.debloatmail.Compose` | root | Send, drafts |
| `org.noahie.debloatmail.Queue` | root | Inspect and cancel pending ops, incl. `Get` |
| `org.noahie.debloatmail.Account` | per account | Per-account status, incl. `Capabilities` |
| `org.noahie.debloatmail.Feed` | per feed | Per-feed status |
| `org.noahie.debloatmail.UI` | GUI's bus name | `Present`, `NotificationPolicy` |

**Interface split rationale.** Splitting by concern (not one monolithic interface)
allows capability detection per area and lets a future mobile or test implementation
satisfy a subset. It costs nothing: clients call methods by name.

---

## 5. `org.noahie.debloatmail.Items`

Item mutations. Every method returns the number of items actually affected, so callers
can detect a partial match without a second query.

```
MarkRead(as ids, b read) -> u affected
SetStarred(as ids, b starred) -> u affected
Move(as ids, s target_folder_id) -> u affected
Copy(as ids, s target_folder_id) -> u affected
Delete(as ids, b expunge) -> u affected
Hide(as ids) -> u affected
```

| Method | Semantics |
|---|---|
| `MarkRead` | For mail: sets `items.unread` **immediately** and queues an IMAP `UID STORE` in `pending_ops`. For feeds: local-only. |
| `SetStarred` | Same shape; maps to `\Flagged`. |
| `Move` / `Copy` | Mail only. **`target_folder_id` is a folder `public_id`, not a folder name.** The GUI and CLI resolve names and roles to ids client-side — they read the DB read-only and can do it in one query. The daemon does no name resolution: there is no name-to-IMAP-path interpolation daemon-side, which is precisely the injection surface a name argument would open (§17). On a feed item: `Error.Unsupported`, never a silent no-op. |
| `Delete` | Mail: move to `role = 'trash'`; `expunge = true` also removes it server-side. Feed: sets `items.hidden_at`. |
| `Hide` | Feed only: suppress without deleting, so the article is not re-inserted on the next poll (`DATA-MODEL.md` §11.4; `debloatmailctl.md` §13). |

**Why local state is written before the server op.** `MarkRead` must feel instant and
must work offline. So the daemon commits `items.unread = 0` and *then* queues the IMAP
op. A later network failure does not roll back the local change; it surfaces as a
`Queue.Completed` signal with `success = false`, and the item stays locally read. This
is deliberate: the user's intent is not undone by a flaky network.

**`affected` vs. errors.** If *some* ids resolve and others do not, the method
succeeds and returns the count — partial application is normal. Only a total failure
(no ids resolved, or all are feed items for `Move`) raises an error.

### Signal

```
Changed(as account_ids, t revision)
```

Emitted **after commit**, coalesced per batch (see §16). `account_ids` lists the
affected accounts so a client re-reads only those. `revision` is the daemon's global
monotonic counter — a client that missed signals compares it against
`Status.Revision` and re-reads if it has fallen behind.

---

## 6. `org.noahie.debloatmail.Sync`

```
Sync(a{sv} scope, b full) -> u job_id
Cancel(u job_id) -> b cancelled
FetchBody(s item_id) -> s body_state
FetchAttachment(s item_id, s section_id) -> s blob_path
RefreshFeed(s feed_id) -> u job_id
```

`scope` is a dict with documented optional keys. Absent or empty means "all":

| Key | Type | Meaning |
|---|---|---|
| `account` | `s` | One account's `public_id` |
| `folder` | `s` | One folder's `public_id` |
| `feed` | `s` | One feed's `public_id` |

A dict rather than a struct: scope will grow (per-folder full sync, date-bounded
backfill) and a dict absorbs that without a signature change.

**Job ids are one namespace.** `Sync`, `RefreshFeed`, and the per-account
`Account.Sync` and per-feed `Feed.Refresh` all hand out the same kind of `job_id`
and all report through the same `Progress`/`Finished` signals below. For a feed job,
the signals' `account_id` argument carries the feed's owning feed-account
`public_id`.

**`Sync` never blocks.** It returns a `job_id` and reports progress by signal. The CLI
wants this (`sync` returns immediately; `--wait` is a client-side loop). The GUI wants
this (progress bar). A blocking sync would pin a D-Bus method call open for minutes,
which is exactly what D-Bus timeouts are designed to kill.

**Sync is idempotent per scope.** Calling `Sync` for a scope already syncing returns the
*existing* `job_id` rather than raising `Error.Busy`. A double-click on "Sync Now"
should not produce two syncs or an error dialog.

**`job_id` is transient.** It is valid only for the current daemon run and must not be
persisted. `Compose.Send`'s `op_id` is the opposite (§9) — the asymmetry is intentional:
jobs die with the process, ops survive in the database.

### `FetchBody` and the two-step read

`FetchBody` is the one read-shaped method, and it is deliberately two-step:

1. Caller invokes `FetchBody(item_id)`. The daemon fetches over IMAP, writes
   `item_text`, sets `body_state = 'cached'`, and returns the new state.
2. Caller **re-reads the row from SQLite directly** and displays it.

The method returns a state string, not the body. This keeps every body read on one code
path (direct SQLite) and reduces the daemon's read-side job to "populate the cache."
It also makes `--offline` a two-line change: skip step 1, and if
`body_state != 'cached'`, exit 7.

Return values: `"cached"` on success, `"failed"` if the fetch failed (the caller may
still find a stale cached body). A missing item raises `Error.NotFound`.

`FetchAttachment` returns a **filesystem path** into the blob store, not the bytes. A
20 MB attachment must not be marshalled through the bus. The path is valid for at
least the GC grace window: `DATA-MODEL.md` §9.3 frees a blob only when
`refcount = 0` **and** `last_used_at` is older than the last hour, and there is **no
pin and no release method** — the window is the lifetime.

**The set of valid `section_id`s is exactly the item's `attachments` rows.**
`FetchAttachment` rejects anything else before touching IMAP (§17). Which rows
*exist* for an item is decided by sync and the extraction pipeline
(`SYNC.md` §5.5, `MIME-AND-TEXT.md` §5.1); the daemon addresses parts, it does not
classify them.

### Signals

```
Progress(u job_id, s account_id, s phase, u percent)
Finished(u job_id, s account_id, b success, s detail)
```

Both carry `job_id` first — the earlier form, `Progress(s account_id, s phase, u
percent)`, could not tell which job a percentage belonged to once a feed refresh ran
beside an account sync, and `Cancel` already takes a `job_id`, so clients needed it
either way. `phase` is one of `connecting`, `listing`, `fetching`, `storing`, `done`.
`percent` is 0–100, or `0xFFFFFFFF` when indeterminate — a convention worth stating,
because "0" and "unknown" are otherwise indistinguishable.

`detail` carries the error message on failure and is empty on success.

**Known limitation, stated:** `job_id`s are transient and there is no `GetJob`
method. A client connecting mid-flight learns only that a sync is running, via
`Status.State`, and can render an indeterminate progress bar until a `Finished`
arrives. Requiring job introspection for what is a UI decoration is not worth the
interface; if a mobile build disagrees, that is an `ApiVersion` conversation, not a
silent addition.

---

## 7. `org.noahie.debloatmail.Accounts`

```
Add(a{sv} config) -> s account_id
Update(s account_id, a{sv} config) -> b updated
Remove(s account_id, b purge) -> b removed
Check(s account_id) -> a{sv} report
Reload() -> b reloaded
```

### `Add`

`config` keys: `kind` (`imap`|`feed`, required), `name`, `email`, `provider`,
`username`, `auth_kind` (`password`|`oauth2`, default `password`),
`allow_invalid_cert_loopback` (`b`, default false), and the per-kind connection keys
mirroring `DATA-MODEL.md` §5 (`imap_host`, `imap_port`, `imap_security`, `smtp_host`,
`smtp_port`, `smtp_security`, `poll_interval_secs`).

The last few are load-bearing: `username` is where authentication starts, `auth_kind`
selects the path, and `allow_invalid_cert_loopback` is the Proton-Bridge loopback
exception. None of them can be set after `Add` either — they are `Update` keys
below, but they must also be accepted at `Add` time, or an account can be created in a
state no later call can fix.

**Secrets travel as a method argument, never as a property and never as a return
value.** The GUI collects the password, passes it once in `config.secret`, and the
daemon writes it to the keyring and stores only `secret_ref`. There is no
`GetPassword`-shaped method anywhere in this API, by design (§17).

**`Add` does not verify connectivity.** It persists configuration and returns. The
caller then calls `Check`. Separating them means an account can be configured offline,
and a slow or unreachable server never leaves a half-created account.

### `Update`

```
Update(s account_id, a{sv} config) -> b updated
```

Applies the keys present in `config`; keys absent are unchanged. Keys: `name`,
`email`, `provider`, `enabled` (`b`), `username`, `auth_kind`,
`allow_invalid_cert_loopback`, the connection keys (same set as `Add`), and `secret`.

- **`secret` obeys the one-way rule of `Add`:** accepted only as a method argument,
  never a property, never a return value. Present means *replace*: the daemon writes
  the new keyring entry, repoints `secret_ref` in the same transaction, and
  invalidates any live connection for the account (its next `Check`/sync
  re-authenticates). This is credential rotation — the only way to fix a rejected or
  expired password without deleting the account. Proton Bridge passwords and app
  passwords expire; there is no other path to refresh them, and an auth failure stops
  the schedule (`SYNC.md` §10.1).
- **`name` renames; the `public_id` is invariant.** All UI and CLI references survive,
  because they address by `public_id`, and the schema's `UNIQUE` on `accounts.name`
  (`DATA-MODEL.md` §5) makes the rename fail loudly against an existing account.
- **`enabled = false`** stops scheduling for the account; `StateChanged` reports
  `disabled`. Re-enabling re-validates nothing until the next `Check` or sync.
- Returns `true` on any applied change, `false` if no recognised keys were present. An
  unknown key is `Error.InvalidArgument`, same as in `Add` — a typo'd key should not
  be a silent no-op.

### `Check`

Returns a structured report, never raising for connectivity failures — a failed check
is a successful call:

```yaml
{
  "dns":         {"ok": true,  "detail": "imap.fastmail.com → 66.111.x.x"},
  "tcp":         {"ok": true,  "detail": "993 reachable in 24ms"},
  "tls":         {"ok": true,  "detail": "valid chain, expires 2027-02-11"},
  "auth":        {"ok": true,  "detail": "IMAP LOGIN ok"},
  "capabilities":{"ok": true,  "detail": "IDLE MOVE SPECIAL-USE"},
  "smtp":        {"ok": true,  "detail": "EHLO ok, AUTH PLAIN"},
  "folders":     {"ok": true,  "detail": "12 folders discovered"}
}
```

This is the payload behind `debloatmailctl account check` and the GUI's account wizard
"Test" button. **The TLS entry must report the loopback certificate exception
explicitly** when `allow_invalid_cert_loopback = 1` — silently tolerating a bad
certificate is the failure mode that turns a Proton convenience into a security bug.

The negotiated capability set from the `capabilities` entry is also recorded as the
`Account.Capabilities` property (§12), where the GUI can read it without parsing the
report.

### Signals

```
StateChanged(s account_id, s state, s detail)
```

`state` ∈ `idle`, `connecting`, `syncing`, `error`, `disabled`. Drives the GUI's
per-account status dot and the tray icon's `NeedsAttention` state.

---

## 8. `org.noahie.debloatmail.Feeds`

```
Add(s account_id, s url, s name, u interval_secs) -> a{sv} result
Update(s feed_id, a{sv} config) -> b updated
Remove(s feed_id, b purge) -> b removed
```

`Add` names the feed account (group) the feed belongs to — `feeds.account_id` is
`NOT NULL`, and the CLI resolves the account *name* to a `public_id` client-side
(`DATA-MODEL.md` §11.3, §5). `result` carries `feed_id`, `title`, `site_url`,
`item_count`, `unread_count`, and `validated` (`b`).

**`Add` fetches and validates, with one timeout exception.** Hard validation
failures — 404, non-feed content, malformed XML — fail loudly:
`Error.InvalidArgument` with the parse error, and **no row is created**. That is the
"don't create an empty feed" rule, and it is absolute: a feed is never left
permanently unvalidated. The exception, per `SYNC.md` §7.4, is a **timeout** (bounded
by the caller's D-Bus deadline, 60 s in the CLI): the row is inserted first and the
call returns with `validated: false`, so a slow server never makes a good feed
impossible to add. Validation continues in the background; on success the daemon fills
`title`, `site_url`, and `etag`. If it ultimately fails, the row is removed — which
fires `ObjectManager.InterfacesRemoved` as usual — and the daemon emits
`ValidationFailed(feed_id, reason)`, because the feed object that would otherwise
carry `LastError` is gone too.

`interval_secs = 0` means "inherit the account default" (`DATA-MODEL.md` §11.3's
resolution ladder); any positive value is a per-feed override.

**Why synchronous here but async for `Sync`.** Feed validation is a single HTTP GET
that normally completes in under a second, and the caller needs the result to decide
what to do next (print the title, or show an error in a wizard). Making it async would
force every caller to build a wait loop for a sub-second operation. `Sync` is the
opposite: minutes, and progress matters.

`Remove` without `purge` sets `feeds.removed_at` (soft-remove, `DATA-MODEL.md` §5):
polling stops and the cached articles are retained — they are not re-fetchable
(`DATA-MODEL.md` §9) — remain, and are not deleted, so they stay searchable. Purge
deletes the feed row, cascading to its articles.

### Signals

```
Updated(s feed_id, u new_items)
ValidationFailed(s feed_id, s reason)
```

`Updated` is emitted after a poll that inserted items. `new_items = 0` for a
conditional-GET 304 means nothing changed, and the daemon should not emit at all in
that case — signals that fire on no-ops train clients to ignore them.

`ValidationFailed` fires at most once per `validated: false` addition, when background
validation concludes. It is the only post-return error for `Add`, and it must be
followed by the feed object's removal.

### `Update`

Keys (present-only, as in `Accounts.Update`): `name` (`s`) and `interval_secs` (`u`,
`0` = inherit the account default).

**There is deliberately no `url` key — changing a feed's URL is Remove + Add.**
The in-place alternative has no usable failure modes: a hard validation failure would
only be able to leave the row pointing at the old URL (a failed change doing exactly
what no change does), and a *timeout* has nowhere to land — nothing may be committed
before validation passes, so the call can only report "not updated" or lie. Every
failure path of an in-place URL swap is a no-op or a falsehood. Remove + Add keeps the
existing guarantees: the old feed is soft-removed (articles retained and searchable,
no longer polled — the `feeds.removed_at` semantics from the `Remove` paragraph
above), and the new feed is added under `Add`'s rules, including hard-fail validation
and the `validated: false` / `ValidationFailed` path. A "change feed URL" request is
therefore two calls made by the client (`debloatmailctl.md` will name the command);
the daemon does not need to know they were one gesture, and a failure between the two
calls leaves the old feed still working. Passing `url` is an unknown key and raises
`Error.InvalidArgument`, consistent with the rest of this section.

---

## 9. `org.noahie.debloatmail.Compose`

```
Send(a{sv} draft, s draft_id, b append_to_sent) -> s op_id
SaveDraft(a{sv} draft) -> s draft_id
DeleteDraft(s draft_id) -> b deleted
```

`draft` keys: `to`, `cc`, `bcc` (`as`), `subject`, `body`, `in_reply_to`, `references`
(`s`), `attachments` (`as` — absolute file paths).

**`Send` takes exactly one of an inline `draft` dict or a non-empty `draft_id`**
(empty string = absent, per the "0 means unset" convention of this API). Supplying
both, or neither, is `Error.InvalidArgument`. With `draft_id`, the daemon reads the
row from the local `drafts` table (`DATA-MODEL.md` §11.5), embeds the full draft in
the op payload — so the op is self-sufficient; the payload also carries
`append_to_sent` and `sent_folder` (`DATA-MODEL.md` §11.7) — and deletes the draft
row, all in one transaction. `DATA-MODEL.md` §11.5's "accepts either a `draft_id` or
an inline `a{sv}` draft" is the contract; this is its wire form.

**`Send` queues; it does not send.** It writes a `pending_ops` row, commits, and
returns. Sending is then the daemon's problem: offline compose works, a slow SMTP
server never blocks a caller, and the GUI's Send button returns instantly.

**`op_id` is stable across daemon restarts** — it is the `pending_ops` row's
`public_id`, not a transient job id. The caller can reconnect later and ask
`Queue.Pending()` whether it went out. This is the opposite of `Sync`'s `job_id`, and
the distinction matters: an unsent message must survive a crash.

`append_to_sent` controls the IMAP `APPEND` copy. **When `true`, the append is a
composite part of the same op** (`DATA-MODEL.md` §11.7): a message that sends and then
fails to append would otherwise leave the user unable to find what they sent, which is
worse than either failure alone. **Success is defined by the send** — if the append
fails, the op still terminates `done`, `success = true`, with a `detail` naming the
missing Sent copy: reporting failure after a successful send implies the message did
not go out, which is the more dangerous error of the two.

### Signals

`SaveDraft` has a home: the local `drafts` table (`DATA-MODEL.md` §11.5). It inserts
the row and returns the row's `public_id`. The earlier behavior of raising
`Error.Unsupported` until drafts existed is superseded — and note the consequence
`DATA-MODEL.md` §11.5 records: a draft saved here is a *local* draft, invisible in the
server's `drafts` folder and on other devices.

---

## 10. `org.noahie.debloatmail.Queue`

```
Pending() -> as op_ids
Get(s op_id) -> a{sv} report
Cancel(s op_id) -> b cancelled
```

Properties: `Count` (`u`).

```
Completed(s op_id, b success, s detail)
```

`Get` inspects one op by `public_id` (terminal rows are retained 7 days,
`DATA-MODEL.md` §11.6) and returns the op's state; an unknown id raises
`Error.NotFound`.

```yaml
{
  "op": "send",
  "state": "done",              # pending | in_flight | failed | done
  "attempts": 1,
  "last_error": "",             # non-empty only on partial failure / failed
  "finished_at": 1760000090,    # 0 while non-terminal
  "account": "<account public_id>",
  "item": "<item public_id, or empty if none or deleted>"
}
```

The queue is user-visible because it is user-relevant: "3 messages waiting to send" is
something a mail client should say out loud, not hide. `debloatmailctl daemon status`
reports `Count`; the GUI shows a banner when it is non-zero and the account is
offline.

`Pending()` returns `pending`, `in_flight`, and `failed` ops — i.e. everything not
`done` (`DATA-MODEL.md` §11.6) — so a `failed` op is discoverable without the caller
having kept the `op_id`, and `Count` counts the same set (`Status.PendingOps` is the
same number).

`Cancel` succeeds only for `pending` ops. Once an IMAP `STORE` or SMTP `DATA` is in
flight, cancelling would produce a state the client cannot reconcile, so it raises
`Error.Busy` instead. Cancelling a `failed` op is a no-op that returns `false`; the
row stays for the retention window. The `item` field of an op goes empty when the
referenced item is deleted, because `pending_ops.item_id` is `ON DELETE SET NULL`
(`DATA-MODEL.md` §5) — queued user intent survives the item's deletion.

---

## 11. `org.noahie.debloatmail.Status`

Properties only — no `Status()` method.

| Property | Type | Meaning |
|---|---|---|
| `ApiVersion` | `u` | Interface version; bumped only on breaking change (this section) |
| `Features` | `as` | What **this binary** implements: `threading`, `feeds` — see below |
| `DaemonVersion` | `s` | Cargo package version, for bug reports |
| `State` | `s` | `idle` \| `syncing` \| `degraded` |
| `Revision` | `t` | Global monotonic commit counter, stored in the database — see below |
| `PendingOps` | `u` | Same as `Queue.Count` |
| `LastSyncAt` | `t` | Unix seconds, UTC; `0` if never |

**No `Status()` method because `GetAll` already is one.** A client wanting the full
snapshot calls `org.freedesktop.DBus.Properties.GetAll("org.noahie.debloatmail.Status")`
— one round trip, same result, standard interface. Adding a custom method would
duplicate it with a second code path to keep in sync.

`LastSyncAt` is `0`, not absent, when never synced. D-Bus has no null; `0` is the
conventional "unset" for a `t` timestamp and must be documented, because `0` is also a
legal Unix time.

**`Features` is a static, per-binary list, and it advertises only what is
implemented.** The earlier list — `threading`, `feeds`, `oauth`, `pdf` — advertised
OAuth, which is unimplemented (`architecture.md` §7), and `pdf`, which is not a daemon
capability at all: PDF rendering is GUI-side (`SECURITY.md` §6.3 said the daemon
parses PDFs via QtPdf; it does not — the daemon is Qt-free). Rule: adding a token is
non-breaking; removing or redefining one is, and bumps `ApiVersion`.

Protocol capabilities are not features: whether a *server* supports `idle`, `move`,
`copy`, `special-use`, or `condstore` is per account, negotiated, and read from
`Account.Capabilities` (§12).

**`Revision` is stored, not in memory.** The counter lives in the database's `meta`
table and is bumped inside every committed write transaction (`DATA-MODEL.md` §5);
it therefore survives daemon restarts and is what makes the §16 re-read protocol work
after a restart, not only after a missed signal.

---

## 12. Per-object interfaces

### `org.noahie.debloatmail.Account` — `/org/noahie/debloatmail/accounts/<public_id, hyphens removed>`

| Property | Type | Notes |
|---|---|---|
| `Id` | `s` | `public_id` |
| `Name` | `s` | Display name (unique, `DATA-MODEL.md` §5) |
| `Kind` | `s` | `imap` \| `feed` |
| `Enabled` | `b` | |
| `Capabilities` | `as` | Negotiated: `idle`, `move`, `copy`, `special-use`, `condstore`; empty until first successful `Check` |
| `State` | `s` | Mirrors `Accounts.StateChanged` |
| `LastSyncAt` | `t` | |
| `LastError` | `s` | Empty when healthy |
| `UnreadCount` | `u` | Drives the tray badge and sidebar |
| `Revision` | `t` | Per-account, so clients re-read only what changed |

Methods: `Sync(b full) -> u job_id`, `Check() -> a{sv} report`.
Signal: `Changed()` — any property, including `Capabilities`, was updated.

`Capabilities` is what lets the UI decide behavior per server: offer "Move" only when
`move` is present, fall back to copy-then-delete when only `copy` is, and render
progress determinate only when `idle` (or `condstore`) reporting is available. The
"no global unread count" rule below applies to `Capabilities` too — it is never
aggregated.

### `org.noahie.debloatmail.Feed` — `/org/noahie/debloatmail/feeds/<public_id, hyphens removed>`

Properties: `Id`, `Url`, `Title`, `LastFetchAt`, `NextFetchAt`, `LastError`,
`UnreadCount`.
Methods: `Refresh() -> u job_id`, `Remove(b purge) -> b`.
Signal: `Changed()`.

**`UnreadCount` is per-account and per-feed, never global.** Computing a global unread
count in the daemon would duplicate a query the client can run itself against SQLite,
and would need its own invalidation logic. Clients aggregate the per-object values they
already receive.

---

## 13. `org.noahie.debloatmail.UI` — on the GUI's bus name

The only interface the GUI exposes. It exists so the daemon can coordinate
notifications and routing.

```
Present() -> void
```

| Member | Purpose |
|---|---|
| `Present()` | Raise and focus the main window. The tray's `Activate` handler calls this. |
| `NotificationPolicy` (`s`, read/write) | `all` \| `focused` \| `none` |

**`Compose(s to, s subject, s body)` has been removed.** The earlier revision had it
for `mailto:` routing; once routing goes through `org.freedesktop.Application.Open`
(§14, §21 — the shell's path, the only supported one), nothing legitimate called it.
`SECURITY.md` §10.4 had additionally identified it as a spoofing vector: any
same-user process could make the GUI open a convincing "Reply to …" window
impersonating a mailto link. A method whose only remaining use is an attack surface is
deleted, prose and XML together.

**`NotificationPolicy` is a property the GUI writes, not a policy the daemon
infers.** `focused` means "suppress notifications while my window has focus" — a
decision that requires window-manager state the daemon does not have. Letting the GUI
own it keeps that logic where the information is. Where the GUI *persists* the value
across restarts is a GUI configuration concern (`UI.md`) — one config file, no schema
system — not part of this contract; this interface specifies the transport, and the
GUI starts with the value it persisted or `all` if it has none.

The daemon detects GUI presence with `org.freedesktop.DBus.NameHasOwner` on
`org.noahie.debloatmail` (§14) rather than requiring a registration call, so a crashed
GUI leaves no stale state.

---

## 14. Activation and notification flow

### Who starts what

```
Desktop shell ──Open(["mailto:…"])──► org.noahie.debloatmail        (GUI, activated)
                                              │
                                              │ Items/Sync/Compose calls
                                              ▼
                                      org.noahie.debloatmail.Daemon (activated on demand)
                                              │
debloatmailctl ───────────────────────────────┘
```

Neither process starts the other directly — both go through D-Bus activation, so there
is exactly one start path per process and no fork/exec races.

### Notification path

1. A sync inserts new items for an account with notifications enabled.
2. The daemon checks `NameHasOwner("org.noahie.debloatmail")`. If the GUI is not
   running, it proceeds; if it is, it reads `UI.NotificationPolicy` and honors it.
3. It calls `Notify` on `org.freedesktop.Notifications` at `/org/freedesktop/Notifications`
   [^af38cb], with a stable `replaces_id` per account so a burst coalesces into one
   bubble instead of forty.
4. Actions are `["default", "Open"]`. `ActionInvoked(id, "default")` →
   `org.freedesktop.Application.Activate` on the GUI, then `UI.Present()`.
   `NotificationClosed(id, reason)` [^bfbe17] lets the daemon stop tracking the id
   [^bfdbf6].

**Rate limit: one notification per account per 60 seconds.** A mailing list delivering
200 messages must produce one bubble saying "200 new messages", not 200 bubbles.

**Flatpak caveat:** sandboxed applications are expected to use
`org.freedesktop.portal.Notification` rather than talking to
`org.freedesktop.Notifications` directly. This must be verified against the Flatpak
manifest before beta — it affects both the notification path and the tray (§18).

---

## 15. Errors

All errors are raised on the caller's interface with these names:

| Error name | Raised when | Client action |
|---|---|---|
| `…Error.NotFound` | No such item, folder, feed, account, or op | Report; refresh the cache |
| `…Error.InvalidArgument` | Malformed id or UUID, unknown `config`/`scope` key, unknown `section_id` for the item, invalid `Send` draft/`draft_id` combination | Bug in the caller |
| `…Error.NotAuthenticated` | Credentials missing or rejected | Prompt for new credentials; `Accounts.Update` with `secret` is the fix path (§7) |
| `…Error.NetworkError` | DNS, TCP, TLS, or protocol failure | Retry with backoff |
| `…Error.Busy` | Operation cannot proceed now (cancel after attempt) | Retry later |
| `…Error.Unsupported` | Feature not implemented | Do not retry |
| `…Error.QueueFull` | `pending_ops` cap reached | Sync first |
| `…Error.Internal` | Invariant violated | Bug report |

Names are prefixed with the bus name's reverse-DNS root:
`org.noahie.debloatmail.Error.NotFound`.

**What was removed and why.** `Error.AmbiguousId` is unreachable: methods take
hyphenated `public_id`s, and the *short id* the CLI offers to humans is resolved
client-side against the read-only DB (`debloatmailctl.md`), so the daemon never sees
an ambiguous form — ambiguity is a CLI condition (exit code 9, settled when that
document is processed). `Error.Offline` is unreachable for the same reason:
`--offline` is a client policy over a client-owned data source (refuse to call, print
what's cached), not a precondition the daemon enforces. A dead error name is worse
than none: it documents a failure mode the client will never handle.

**Errors are for programmer-visible failures, not user-visible ones.** An unreachable
server during `Check` returns a report with `ok: false`, not `Error.NetworkError`,
because the caller asked "is this reachable?" and "no" is a valid answer. The error
names above are for calls whose *preconditions* failed.

---

## 16. Signals: delivery and coalescing

### Subscription

One match rule covers everything:

```
AddMatch("type='signal',sender='org.noahie.debloatmail.Daemon'")
```

Signals are spread across interfaces by concern, but a sender-based match catches all
of them, so clients never enumerate interfaces. Clients that only care about one
account narrow further with `path='/org/noahie/debloatmail/accounts/3'`.

### Coalescing

`Items.Changed` is **coalesced per 100 ms window**, accumulating `account_ids` and
emitting once with the latest `revision`. An initial sync of 50,000 headers would
otherwise emit tens of thousands of signals and spend more time on the bus than on the
network.

Coalescing must never *drop* the final emission: the timer is reset on each new change
and the signal fires on flush, so a client cannot end up permanently stale.

### The snapshot race, and why `Revision` exists

WAL gives readers a consistent snapshot, but a client's read transaction can begin
before the daemon's commit and therefore not see the change that triggered the signal.
A client that trusts "signal received ⇒ data visible" will occasionally miss updates.

**The rule:** on `Changed`, re-read in a *fresh* transaction and compare the observed
`Revision`. If the observed revision is older than the signal's, re-read again. This is
cheap (one integer) and closes the race without polling.

At startup, compare the cached revision against `Status.Revision` and do one full
refresh if behind. That handles signals missed while the client was not running. The
comparison is exact across daemon restarts as well, because the counter is
database-backed (`DATA-MODEL.md` §5) and does not reset with the process (§11).

---

## 17. Security

**The session bus is per-user and provides no authentication between that user's
applications.** Every design decision here follows from that single fact.

| Rule | Reason |
|---|---|
| No method returns a secret | Any same-user process can call any method |
| No property holds a secret | `Properties.GetAll` is callable by anyone on the bus |
| Secrets accepted only as method arguments | Point-to-point delivery; the bus daemon does not broadcast method calls |
| `Send` attachments are validated as regular, readable files | Prevents the daemon being used as an oracle for special files |
| `Move`/`Copy` targets are parsed as UUIDs and resolved to folders server-side | No name-to-IMAP-path interpolation exists daemon-side, so there is no IMAP command injection via a crafted folder string (§5) |
| `FetchAttachment`'s `section_id` must parse as an RFC 3501 §6.4.5 dot-list and match an existing `attachments` row of the item | Prevents IMAP section-specifier injection and use of the daemon as a part-existence oracle |
| All public ids parsed as UUIDs before use | Prevents SQL injection if a query ever interpolates |

**Attachment validation deserves a note.** The daemon runs as the user, so a same-user
attacker could read those files anyway — this is not a privilege boundary. But the
daemon should still refuse non-regular files (FIFOs, device nodes) and paths outside
the user's home, because a mail client that will cheerfully attach `/dev/urandom` or
`/proc/self/environ` is a bug regardless of who asked.

**No polkit.** Nothing here needs privileges the user does not already have.

**The eventual risk is a system-bus variant.** If a future version ever exposes this
API on the system bus, every rule above breaks. That is why they are written down now,
while it is hypothetical.

---

## 18. Standard interfaces used

| Interface | Path | Owner | Purpose |
|---|---|---|---|
| `org.freedesktop.DBus.ObjectManager` | root | daemon | Enumerate accounts/feeds; `InterfacesAdded`/`Removed` |
| `org.freedesktop.DBus.Properties` | all | daemon, GUI | Property access (generated by zbus) |
| `org.freedesktop.DBus.Peer` | all | daemon, GUI | `Ping`, `GetMachineId` (generated by zbus) |
| `org.freedesktop.DBus.Introspectable` | all | daemon, GUI | Must match `data/dbus/*.xml` |
| `org.freedesktop.Application` | root | **GUI** | `Activate`, `Open`, `ActivateAction` |
| `org.freedesktop.StatusNotifierItem` | `…/StatusNotifierItem` | **daemon** | Tray icon |
| `com.canonical.dbusmenu` | `…/StatusNotifierMenu` | **daemon** | Tray context menu |
| `org.freedesktop.Notifications` | `/org/freedesktop/Notifications` | desktop | New-mail notifications |

### Tray: the daemon owns it, not the GUI

The tray icon must survive the window closing, so it belongs to the daemon. The daemon
registers the unique name `org.freedesktop.StatusNotifierItem-PID-ID` and registers
**the SNI object path** with the `StatusNotifierWatcher`. Both the SNI
object and the `com.canonical.dbusmenu` menu object live **on that unique name** — not
on the daemon's well-known name, whose object tree (§3) holds only the global and
per-account/per-feed objects. The watcher never calls back on a name it cannot own.

| Element | Value |
|---|---|
| `Category` | `Communications` — the spec's value for an email client |
| `Id` | `org.noahie.debloatmail` (stable across sessions) |
| `Title` | `debloatmail` |
| `Status` | `Active` normally; `NeedsAttention` on unread |
| `IconName` | Freedesktop icon name; prefer over `IconPixmap` |
| `ToolTip` | `(icon, pixmap, title, text)` — unread count goes here |
| `ItemIsMenu` | `false` — left click activates, right click opens the menu |
| `Menu` | Path to the `com.canonical.dbusmenu` object, on the same unique name as the SNI object |

Methods: `Activate(x, y)` opens the GUI, `SecondaryActivate(x, y)` triggers sync,
`ContextMenu(x, y)` is served by the menu object, `Scroll(delta, orientation)` is
ignored.

Signals: `NewIcon`, `NewToolTip`, `NewStatus(status)` when the unread count or
connection state changes.

**Compatibility note:** the interface was standardised as
`org.freedesktop.StatusNotifierItem`, but the specification
originated in KDE and many hosts still look for `org.kde.StatusNotifierItem`.
KDE's own implementation is described as implementing the D-Bus specification. Emit **both** interface names on the same object — it costs one extra
`<interface>` block and is the difference between a tray icon and no tray icon on
older desktops.

**Fallback:** if `IsStatusNotifierWatcher.IsStatusNotifierHostRegistered` is false, there is no SNI host and the legacy XEmbed tray would be required.
Do not implement XEmbed in beta — every current desktop provides SNI. Log a warning
and continue without a tray.

---

## 19. zbus implementation notes

`zbus` 5.19.0 is current and is pure Rust, so it adds no `libdbus` dependency to any
packaging target.

- **Write the XML first.** Keep Appendix A in `data/dbus/org.noahie.debloatmail.Daemon.xml`
  as the authoritative definition and generate bindings with `zbus_xmlgen`, which
  converts D-Bus interface XML into Rust. Hand-writing `#[zbus::interface]`
  impls is fine, but then the XML and the code drift, and `Introspectable` starts lying
  to every debugging tool.
- **Verify the drift, don't assume it.** A CI test should introspect the running daemon
  and diff against `data/dbus/*.xml`. This catches the most common D-Bus bug there is.
- **`ObjectManager` must be implemented by hand.** zbus's `ObjectServer` does not
  synthesize `GetManagedObjects`; you emit `InterfacesAdded` yourself on registration.
- **Async or blocking.** The daemon is async (`tokio` or `async-io`, zbus supports
  both); `debloatmailctl` should use zbus's blocking API, since a CLI has nothing to
  do concurrently.
- **Method timeouts.** D-Bus's default 25-second method timeout will fire on `Sync` if
  it were blocking — another reason it is job-based. Clients should raise their own
  timeout for `Feeds.Add` (network) and `Accounts.Check` (network). For `Feeds.Add`
  the raised timeout *is* the contract: the CLI's 60 s is exactly the deadline at which
  the daemon's internal validation timing out produces a `validated: false` result
  rather than a failed call (§8).
- **Name flags.** The daemon should request
  `RequestName("org.noahie.debloatmail.Daemon", DO_NOT_QUEUE)` so a second daemon exits
  immediately with a clear message rather than sitting in the activation queue.

---

## 20. Activation files

`data/org.noahie.debloatmail.service` — activates the **GUI**:

```ini
[D-BUS Service]
Name=org.noahie.debloatmail
Exec=/usr/bin/debloatmail
```

`data/org.noahie.debloatmail.Daemon.service` — activates the **daemon**:

```ini
[D-BUS Service]
Name=org.noahie.debloatmail.Daemon
Exec=/usr/bin/debloatmaild
SystemdService=debloatmaild.service
```

**The daemon also needs a systemd user unit**, because D-Bus activation alone means it
starts only on first use — and background sync must start at login, before any client
calls it. On systemd sessions, a unit with `BusName=org.noahie.debloatmail.Daemon`
serves *both* purposes and makes the `.service` file redundant; ship the unit as
primary and the `.service` file as the non-systemd fallback. Verify with
`dbus-run-session` that they do not double-start.

The GUI's desktop file needs `DBusActivatable=true` and
`MimeType=x-scheme-handler/mailto;` so `mailto:` links route through
`org.freedesktop.Application.Open`.

---

## Appendix A — Introspection XML

`data/dbus/org.noahie.debloatmail.Daemon.xml`

```xml
<!DOCTYPE node PUBLIC "-//freedesktop//DTD D-BUS Object Introspection 1.0//EN"
  "http://www.freedesktop.org/standards/dbus/1.0/introspect.dtd">
<node name="/org/noahie/debloatmail">

  <interface name="org.freedesktop.DBus.ObjectManager">
    <method name="GetManagedObjects">
      <arg name="objects" type="a{oa{sa{sv}}}" direction="out"/>
    </method>
    <signal name="InterfacesAdded">
      <arg name="object_path" type="o"/>
      <arg name="interfaces_and_properties" type="a{sa{sv}}"/>
    </signal>
    <signal name="InterfacesRemoved">
      <arg name="object_path" type="o"/>
      <arg name="interfaces" type="as"/>
    </signal>
  </interface>

  <interface name="org.noahie.debloatmail.Status">
    <property name="ApiVersion"    type="u" access="read"/>
    <property name="Features"      type="as" access="read"/>
    <property name="DaemonVersion" type="s" access="read"/>
    <property name="State"         type="s" access="read"/>
    <property name="Revision"      type="t" access="read"/>
    <property name="PendingOps"    type="u" access="read"/>
    <property name="LastSyncAt"    type="t" access="read"/>
  </interface>

  <interface name="org.noahie.debloatmail.Items">
    <method name="MarkRead">
      <arg name="ids" type="as" direction="in"/>
      <arg name="read" type="b" direction="in"/>
      <arg name="affected" type="u" direction="out"/>
    </method>
    <method name="SetStarred">
      <arg name="ids" type="as" direction="in"/>
      <arg name="starred" type="b" direction="in"/>
      <arg name="affected" type="u" direction="out"/>
    </method>
    <method name="Move">
      <arg name="ids" type="as" direction="in"/>
      <arg name="target_folder_id" type="s" direction="in"/>
      <arg name="affected" type="u" direction="out"/>
    </method>
    <method name="Copy">
      <arg name="ids" type="as" direction="in"/>
      <arg name="target_folder_id" type="s" direction="in"/>
      <arg name="affected" type="u" direction="out"/>
    </method>
    <method name="Delete">
      <arg name="ids" type="as" direction="in"/>
      <arg name="expunge" type="b" direction="in"/>
      <arg name="affected" type="u" direction="out"/>
    </method>
    <method name="Hide">
      <arg name="ids" type="as" direction="in"/>
      <arg name="affected" type="u" direction="out"/>
    </method>
    <signal name="Changed">
      <arg name="account_ids" type="as"/>
      <arg name="revision" type="t"/>
    </signal>
  </interface>

  <interface name="org.noahie.debloatmail.Sync">
    <method name="Sync">
      <arg name="scope" type="a{sv}" direction="in"/>
      <arg name="full" type="b" direction="in"/>
      <arg name="job_id" type="u" direction="out"/>
    </method>
    <method name="Cancel">
      <arg name="job_id" type="u" direction="in"/>
      <arg name="cancelled" type="b" direction="out"/>
    </method>
    <method name="FetchBody">
      <arg name="item_id" type="s" direction="in"/>
      <arg name="body_state" type="s" direction="out"/>
    </method>
    <method name="FetchAttachment">
      <arg name="item_id" type="s" direction="in"/>
      <arg name="section_id" type="s" direction="in"/>
      <arg name="blob_path" type="s" direction="out"/>
    </method>
    <method name="RefreshFeed">
      <arg name="feed_id" type="s" direction="in"/>
      <arg name="job_id" type="u" direction="out"/>
    </method>
    <signal name="Progress">
      <arg name="job_id" type="u"/>
      <arg name="account_id" type="s"/>
      <arg name="phase" type="s"/>
      <arg name="percent" type="u"/>
    </signal>
    <signal name="Finished">
      <arg name="job_id" type="u"/>
      <arg name="account_id" type="s"/>
      <arg name="success" type="b"/>
      <arg name="detail" type="s"/>
    </signal>
  </interface>

  <interface name="org.noahie.debloatmail.Accounts">
    <method name="Add">
      <arg name="config" type="a{sv}" direction="in"/>
      <arg name="account_id" type="s" direction="out"/>
    </method>
    <method name="Update">
      <arg name="account_id" type="s" direction="in"/>
      <arg name="config" type="a{sv}" direction="in"/>
      <arg name="updated" type="b" direction="out"/>
    </method>
    <method name="Remove">
      <arg name="account_id" type="s" direction="in"/>
      <arg name="purge" type="b" direction="in"/>
      <arg name="removed" type="b" direction="out"/>
    </method>
    <method name="Check">
      <arg name="account_id" type="s" direction="in"/>
      <arg name="report" type="a{sv}" direction="out"/>
    </method>
    <method name="Reload">
      <arg name="reloaded" type="b" direction="out"/>
    </method>
    <signal name="StateChanged">
      <arg name="account_id" type="s"/>
      <arg name="state" type="s"/>
      <arg name="detail" type="s"/>
    </signal>
  </interface>

  <interface name="org.noahie.debloatmail.Feeds">
    <method name="Add">
      <arg name="account_id" type="s" direction="in"/>
      <arg name="url" type="s" direction="in"/>
      <arg name="name" type="s" direction="in"/>
      <arg name="interval_secs" type="u" direction="in"/>
      <arg name="result" type="a{sv}" direction="out"/>
    </method>
    <method name="Update">
      <arg name="feed_id" type="s" direction="in"/>
      <arg name="config" type="a{sv}" direction="in"/>
      <arg name="updated" type="b" direction="out"/>
    </method>
    <method name="Remove">
      <arg name="feed_id" type="s" direction="in"/>
      <arg name="purge" type="b" direction="in"/>
      <arg name="removed" type="b" direction="out"/>
    </method>
    <signal name="Updated">
      <arg name="feed_id" type="s"/>
      <arg name="new_items" type="u"/>
    </signal>
    <signal name="ValidationFailed">
      <arg name="feed_id" type="s"/>
      <arg name="reason" type="s"/>
    </signal>
  </interface>

  <interface name="org.noahie.debloatmail.Compose">
    <method name="Send">
      <arg name="draft" type="a{sv}" direction="in"/>
      <arg name="draft_id" type="s" direction="in"/>
      <arg name="append_to_sent" type="b" direction="in"/>
      <arg name="op_id" type="s" direction="out"/>
    </method>
    <method name="SaveDraft">
      <arg name="draft" type="a{sv}" direction="in"/>
      <arg name="draft_id" type="s" direction="out"/>
    </method>
    <method name="DeleteDraft">
      <arg name="draft_id" type="s" direction="in"/>
      <arg name="deleted" type="b" direction="out"/>
    </method>
  </interface>

  <interface name="org.noahie.debloatmail.Queue">
    <property name="Count" type="u" access="read"/>
    <method name="Pending">
      <arg name="op_ids" type="as" direction="out"/>
    </method>
    <method name="Get">
      <arg name="op_id" type="s" direction="in"/>
      <arg name="report" type="a{sv}" direction="out"/>
    </method>
    <method name="Cancel">
      <arg name="op_id" type="s" direction="in"/>
      <arg name="cancelled" type="b" direction="out"/>
    </method>
    <signal name="Completed">
      <arg name="op_id" type="s"/>
      <arg name="success" type="b"/>
      <arg name="detail" type="s"/>
    </signal>
  </interface>

</node>
```

`data/dbus/org.noahie.debloatmail.Account.xml` (at each account path):

```xml
<!DOCTYPE node PUBLIC "-//freedesktop//DTD D-BUS Object Introspection 1.0//EN"
  "http://www.freedesktop.org/standards/dbus/1.0/introspect.dtd">
<node>
  <interface name="org.noahie.debloatmail.Account">
    <property name="Id"           type="s" access="read"/>
    <property name="Name"         type="s" access="read"/>
    <property name="Kind"         type="s" access="read"/>
    <property name="Enabled"      type="b" access="read"/>
    <property name="Capabilities" type="as" access="read"/>
    <property name="LastSyncAt"   type="t" access="read"/>
    <property name="LastError"    type="s" access="read"/>
    <property name="UnreadCount"  type="u" access="read"/>
    <property name="Revision"     type="t" access="read"/>
    <method name="Sync">
      <arg name="full" type="b" direction="in"/>
      <arg name="job_id" type="u" direction="out"/>
    </method>
    <method name="Check">
      <arg name="report" type="a{sv}" direction="out"/>
    </method>
    <signal name="Changed"/>
  </interface>
</node>
```

`data/dbus/org.noahie.debloatmail.Feed.xml` (at each feed path):

```xml
<!DOCTYPE node PUBLIC "-//freedesktop//DTD D-BUS Object Introspection 1.0//EN"
  "http://www.freedesktop.org/standards/dbus/1.0/introspect.dtd">
<node>
  <interface name="org.noahie.debloatmail.Feed">
    <property name="Id"          type="s" access="read"/>
    <property name="Url"         type="s" access="read"/>
    <property name="Title"       type="s" access="read"/>
    <property name="LastFetchAt" type="t" access="read"/>
    <property name="NextFetchAt" type="t" access="read"/>
    <property name="LastError"   type="s" access="read"/>
    <property name="UnreadCount" type="u" access="read"/>
    <method name="Refresh">
      <arg name="job_id" type="u" direction="out"/>
    </method>
    <method name="Remove">
      <arg name="purge" type="b" direction="in"/>
      <arg name="removed" type="b" direction="out"/>
    </method>
    <signal name="Changed"/>
  </interface>
</node>
```

`data/dbus/org.noahie.debloatmail.UI.xml` (on the GUI's bus name):

```xml
<!DOCTYPE node PUBLIC "-//freedesktop//DTD D-BUS Object Introspection 1.0//EN"
  "http://www.freedesktop.org/standards/dbus/1.0/introspect.dtd">
<node name="/org/noahie/debloatmail">
  <interface name="org.noahie.debloatmail.UI">
    <property name="NotificationPolicy" type="s" access="readwrite"/>
    <method name="Present"/>
  </interface>
</node>
```
