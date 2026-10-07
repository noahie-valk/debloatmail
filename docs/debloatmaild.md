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
| `debloatmaild` | ✓ | ✓ (sole) | ✓ | All I/O, scheduler, credentials |
| `debloatmail` (GUI) | ✓ (direct SQLite) | ✗ (via daemon) | ✗ | Windows, tray menu rendering |
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
`org.freedesktop.Application` with `Activate`, `Open`, and `ActivateAction` [^b70bb3].
Since our App ID is `org.noahie.debloatmail`, the GUI must own that exact name.

That interface is what makes `mailto:` links, desktop actions, and startup-notification
correctness work — the shell calls `Open(["mailto:..."])` on the running instance
rather than spawning a second process. It is worth the extra name.

**Correction to `debloatmailctl.md` §10:** that document refers to writes going to
"D-Bus `org.noahie.debloatmail`". That is wrong as written — the daemon's name is
`org.noahie.debloatmail.Daemon`. See §20.

### Not activatable: the CLI

`debloatmailctl` owns no bus name. It is a short-lived client that connects, calls,
disconnects. It must not appear in `NameHasOwner` checks, which the daemon uses to
detect GUI presence (§14).

---

## 3. Object tree

```
/org/noahie/debloatmail                      ← root; all global interfaces
├── /org/noahie/debloatmail/accounts/1       ← per-account status
├── /org/noahie/debloatmail/accounts/2
├── /org/noahie/debloatmail/feeds/7          ← per-feed status
└── /org/noahie/debloatmail/feeds/9

(separate process, separate bus name)
/org/noahie/debloatmail/StatusNotifierItem   ← on org.noahie.debloatmail.Daemon
/org/noahie/debloatmail/StatusNotifierMenu   ← com.canonical.dbusmenu
```

The root object implements `org.freedesktop.DBus.ObjectManager`. Per-account and
per-feed objects are its managed children, so a client enumerates the whole world with
one `GetManagedObjects()` call and then stays current via `InterfacesAdded` /
`InterfacesRemoved`. This is why there is no `ListAccounts()` method — the standard
interface already does it, better.

**Object paths use internal rowids, not `public_id`s.** Paths are ephemeral handles
owned by the daemon; `public_id` is the durable identity that crosses the ABI
(`DATA-MODEL.md` §3). A client must not cache a path across a daemon restart. Every
method that takes an id takes a `public_id` string, never a path.

---

## 4. Interface index

| Interface | Object | Purpose |
|---|---|---|
| `org.noahie.debloatmail.Status` | root | Daemon-level properties |
| `org.noahie.debloatmail.Items` | root | Item mutations (read/star/move/delete) |
| `org.noahie.debloatmail.Sync` | root | Trigger sync, fetch bodies and attachments |
| `org.noahie.debloatmail.Accounts` | root | Account lifecycle |
| `org.noahie.debloatmail.Feeds` | root | Feed lifecycle |
| `org.noahie.debloatmail.Compose` | root | Send, drafts |
| `org.noahie.debloatmail.Queue` | root | Inspect and cancel pending ops |
| `org.noahie.debloatmail.Account` | per account | Per-account status |
| `org.noahie.debloatmail.Feed` | per feed | Per-feed status |
| `org.noahie.debloatmail.UI` | GUI's bus name | GUI-side control surface |

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
Move(as ids, s target_folder) -> u affected
Copy(as ids, s target_folder) -> u affected
Delete(as ids, b expunge) -> u affected
Hide(as ids) -> u affected
```

| Method | Semantics |
|---|---|
| `MarkRead` | For mail: sets `items.unread` **immediately** and queues an IMAP `UID STORE` in `pending_ops`. For feeds: local-only. |
| `SetStarred` | Same shape; maps to `\Flagged`. |
| `Move` / `Copy` | Mail only. On a feed item: `Error.Unsupported`, never a silent no-op. |
| `Delete` | Mail: move to `role = 'trash'`; `expunge = true` also removes it server-side. Feed: sets `items.hidden_at`. |
| `Hide` | Feed only: suppress without deleting, so the article is not re-inserted on the next poll (`debloatmailctl.md` §13). |

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
FetchAttachment(s item_id, s part_id) -> s blob_path
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

**`Sync` never blocks.** It returns a `job_id` and reports progress by signal. The CLI
wants this (`sync` returns immediately; `--wait` is a client-side loop). The GUI wants
this (progress bar). A blocking sync would pin a D-Bus method call open for minutes,
which is exactly what D-Bus timeouts are designed to kill.

**Sync is idempotent per scope.** Calling `Sync` for a scope already syncing returns
the *existing* `job_id` rather than raising `Error.Busy`. A double-click on "Sync Now"
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
20 MB attachment must not be marshalled through the bus. The path is valid until the
next GC pass, which is why the daemon pins the blob's `refcount` on return.

### Signals

```
Progress(s account_id, s phase, u percent)
Finished(s account_id, b success, s detail)
```

`phase` is one of `connecting`, `listing`, `fetching`, `storing`, `done`. `percent` is
0–100, or `0xFFFFFFFF` when indeterminate — a convention worth stating, because "0"
and "unknown" are otherwise indistinguishable.

`detail` carries the error message on failure and is empty on success.

---

## 7. `org.noahie.debloatmail.Accounts`

```
Add(a{sv} config) -> s account_id
Remove(s account_id, b purge) -> b removed
Check(s account_id) -> a{sv} report
Reload() -> b reloaded
```

### `Add`

`config` keys: `kind` (`imap`|`feed`, required), `name`, `email`, `provider`, and the
per-kind connection keys mirroring `DATA-MODEL.md` §5 (`imap_host`, `imap_port`,
`imap_security`, `smtp_*`, `poll_interval_secs`).

**Secrets travel as a method argument, never as a property and never as a return
value.** The GUI collects the password, passes it once in `config.secret`, and the
daemon writes it to the keyring and stores only `secret_ref`. There is no
`GetPassword`-shaped method anywhere in this API, by design (§17).

**`Add` does not verify connectivity.** It persists configuration and returns. The
caller then calls `Check`. Separating them means an account can be configured offline,
and a slow or unreachable server never leaves a half-created account.

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

### Signals

```
StateChanged(s account_id, s state, s detail)
```

`state` ∈ `idle`, `connecting`, `syncing`, `error`, `disabled`. Drives the GUI's
per-account status dot and the tray icon's `NeedsAttention` state.

---

## 8. `org.noahie.debloatmail.Feeds`

```
Add(s url, s name, u interval_secs) -> a{sv} result
Remove(s feed_id, b purge) -> b removed
```

`Add` **fetches and validates synchronously** — bounded by the caller's D-Bus timeout,
defaulting to 60s in `debloatmailctl`. `result` carries `feed_id`, `title`, `site_url`,
`item_count`, `unread_count`.

**Why synchronous here but async for `Sync`.** Feed validation is a single HTTP GET
that normally completes in under a second, and the caller needs the result to decide
what to do next (print the title, or show an error in a wizard). Making it async would
force every caller to build a wait loop for a sub-second operation. `Sync` is the
opposite: minutes, and progress matters.

Adding a URL that 404s, returns HTML, or serves malformed XML fails loudly here —
`Error.InvalidArgument` with the parse error — rather than silently creating an empty
feed that looks fine for three syncs.

`Remove` without `purge` stops polling and retains cached articles (which are not
re-fetchable, per `DATA-MODEL.md` §9).

### Signal

```
Updated(s feed_id, u new_items)
```

Emitted after a poll that inserted items. `new_items = 0` for a conditional-GET 304
means nothing changed, and the daemon should not emit at all in that case — signals
that fire on no-ops train clients to ignore them.

---

## 9. `org.noahie.debloatmail.Compose`

```
Send(a{sv} draft, b append_to_sent) -> s op_id
SaveDraft(a{sv} draft) -> s draft_id
DeleteDraft(s draft_id) -> b deleted
```

`draft` keys: `to`, `cc`, `bcc` (`as`), `subject`, `body`, `in_reply_to`, `references`
(`s`), `attachments` (`as` — absolute file paths).

**`Send` queues; it does not send.** It writes a `pending_ops` row, commits, and
returns. Sending is then the daemon's problem: offline compose works, a slow SMTP
server never blocks a caller, and the GUI's Send button returns instantly.

**`op_id` is stable across daemon restarts** — it is the `pending_ops` row's
`public_id`, not a transient job id. The caller can reconnect later and ask
`Queue.Pending()` whether it went out. This is the opposite of `Sync`'s `job_id`, and
the distinction matters: an unsent message must survive a crash.

`append_to_sent` controls the IMAP `APPEND` copy. **When `true`, the append is part of
the same op** — a message that sends and then fails to append leaves the user unable to
find what they sent, which is worse than either failure alone. See §20.

### Signals

`SaveDraft` requires a draft home, which the data model does not yet define (§20).
Until it does, this method raises `Error.Unsupported` rather than inventing a location.

---

## 10. `org.noahie.debloatmail.Queue`

```
Pending() -> as op_ids
Cancel(s op_id) -> b cancelled
```

Properties: `Count` (`u`).

```
Completed(s op_id, b success, s detail)
```

The queue is user-visible because it is user-relevant: "3 messages waiting to send" is
something a mail client should say out loud, not hide. `debloatmailctl daemon status`
reports `Count`; the GUI shows a banner when it is non-zero and the account is
offline.

`Cancel` succeeds only for ops not yet attempted. Once an IMAP `STORE` or SMTP `DATA`
is in flight, cancelling would produce a state the client cannot reconcile, so it
raises `Error.Busy` instead.

---

## 11. `org.noahie.debloatmail.Status`

Properties only — no `Status()` method.

| Property | Type | Meaning |
|---|---|---|
| `ApiVersion` | `u` | Interface version; bumped only on breaking change (§15) |
| `Features` | `as` | Capability strings: `threading`, `feeds`, `oauth`, `pdf` |
| `DaemonVersion` | `s` | Cargo package version, for bug reports |
| `State` | `s` | `idle` \| `syncing` \| `degraded` |
| `Revision` | `t` | Global monotonic commit counter |
| `PendingOps` | `u` | Same as `Queue.Count` |
| `LastSyncAt` | `t` | Unix seconds, UTC; `0` if never |

**No `Status()` method because `GetAll` already is one.** A client wanting the full
snapshot calls `org.freedesktop.DBus.Properties.GetAll("org.noahie.debloatmail.Status")`
— one round trip, same result, standard interface. Adding a custom method would
duplicate it with a second code path to keep in sync.

`LastSyncAt` is `0`, not absent, when never synced. D-Bus has no null; `0` is the
conventional "unset" for a `t` timestamp and must be documented, because `0` is also a
legal Unix time.

---

## 12. Per-object interfaces

### `org.noahie.debloatmail.Account` — `/org/noahie/debloatmail/accounts/<n>`

| Property | Type | Notes |
|---|---|---|
| `Id` | `s` | `public_id` |
| `Name` | `s` | Display name |
| `Kind` | `s` | `imap` \| `feed` |
| `Enabled` | `b` | |
| `State` | `s` | Mirrors `Accounts.StateChanged` |
| `LastSyncAt` | `t` | |
| `LastError` | `s` | Empty when healthy |
| `UnreadCount` | `u` | Drives the tray badge and sidebar |
| `Revision` | `t` | Per-account, so clients re-read only what changed |

Methods: `Sync(b full) -> u job_id`, `Check() -> a{sv} report`.
Signal: `Changed()` — properties were updated.

### `org.noahie.debloatmail.Feed` — `/org/noahie/debloatmail/feeds/<n>`

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
notifications and so a second launch can be routed.

```
Present() -> void
Compose(s to, s subject, s body) -> void
```

| Member | Purpose |
|---|---|
| `Present()` | Raise and focus the main window. The tray's `Activate` handler calls this. |
| `Compose(...)` | Open a compose window pre-filled. Used for `mailto:` routing. |
| `NotificationPolicy` (`s`, read/write) | `all` \| `focused` \| `none` |

**`NotificationPolicy` is a property the GUI writes, not a policy the daemon infers.**
`focused` means "suppress notifications while my window has focus" — a decision that
requires window-manager state the daemon does not have. Letting the GUI own it keeps
that logic where the information is.

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
| `…Error.NotFound` | No such item, folder, feed, or account | Report; refresh the cache |
| `…Error.AmbiguousId` | Short id matched >1 item | Report the candidates |
| `…Error.InvalidArgument` | Malformed id, folder name, URL, or scope key | Bug in the caller |
| `…Error.NotAuthenticated` | Credentials missing or rejected | Prompt; do not retry blindly |
| `…Error.NetworkError` | DNS, TCP, TLS, or protocol failure | Retry with backoff |
| `…Error.Offline` | `--offline` set and the data is not cached | Fetch is refused by policy |
| `…Error.Busy` | Operation cannot proceed now (cancel after attempt) | Retry later |
| `…Error.Unsupported` | Feature not implemented (`SaveDraft` today) | Do not retry |
| `…Error.QueueFull` | `pending_ops` cap reached | Sync first |
| `…Error.Internal` | Invariant violated | Bug report |

Names are prefixed with the bus name's reverse-DNS root:
`org.noahie.debloatmail.Error.NotFound`.

**`NotFound` and `AmbiguousId` are distinct** because the remedies differ: one means
"refresh," the other means "you typed too few characters." Collapsing them produces
misleading errors.

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
refresh if behind. That handles signals missed while the client was not running.

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
| `Move`/`Copy` folder names are validated | Prevents IMAP command injection via a crafted folder string |
| All ids parsed as UUIDs before use | Prevents SQL injection if a query ever interpolates |

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
| `org.freedesktop.Application` | root | **GUI** | `Activate`, `Open`, `ActivateAction` [^b70bb3] |
| `org.freedesktop.StatusNotifierItem` | `…/StatusNotifierItem` | **daemon** | Tray icon |
| `com.canonical.dbusmenu` | `…/StatusNotifierMenu` | **daemon** | Tray context menu |
| `org.freedesktop.Notifications` | `/org/freedesktop/Notifications` | desktop | New-mail notifications |

### Tray: the daemon owns it, not the GUI

The tray icon must survive the window closing, so it belongs to the daemon. The daemon
registers a StatusNotifierItem under the session bus name
`org.freedesktop.StatusNotifierItem-PID-ID` and registers that name with the
StatusNotifierWatcher [^4b6a68#77-81].

| Element | Value |
|---|---|
| `Category` | `Communications` — the spec's value for an email client [^4b6a68#88-164] |
| `Id` | `org.noahie.debloatmail` (stable across sessions) [^4b6a68#88-164] |
| `Title` | `debloatmail` |
| `Status` | `Active` normally; `NeedsAttention` on unread [^4b6a68#88-164] |
| `IconName` | Freedesktop icon name; prefer over `IconPixmap` [^4b6a68#88-164] |
| `ToolTip` | `(icon, pixmap, title, text)` — unread count goes here [^4b6a68#213-243] |
| `ItemIsMenu` | `false` — left click activates, right click opens the menu |
| `Menu` | Path to a `com.canonical.dbusmenu` object [^4b6a68#213-243] |

Methods: `Activate(x, y)` opens the GUI, `SecondaryActivate(x, y)` triggers sync,
`ContextMenu(x, y)` is served by the menu object, `Scroll(delta, orientation)` is
ignored [^4b6a68#249-289].

Signals: `NewIcon`, `NewToolTip`, `NewStatus(status)` when the unread count or
connection state changes [^4b6a68#291-348].

**Compatibility note:** the interface was standardised as
`org.freedesktop.StatusNotifierItem` [^4b6a68#88-164], but the specification
originated in KDE and many hosts still look for `org.kde.StatusNotifierItem`.
KDE's own implementation is described as implementing the D-Bus specification
[^120319]. Emit **both** interface names on the same object — it costs one extra
`<interface>` block and is the difference between a tray icon and no tray icon on
older desktops.

**Fallback:** if `IsStatusNotifierWatcher.IsStatusNotifierHostRegistered` is false
[^4b6a68#350-403], there is no SNI host and the legacy XEmbed tray would be required.
Do not implement XEmbed in beta — every current desktop provides SNI. Log a warning
and continue without a tray.

---

## 19. zbus implementation notes

`zbus` 5.19.0 is current and is pure Rust, so it adds no `libdbus` dependency to any
packaging target [^317b5c][^614f96].

- **Write the XML first.** Keep Appendix A in `data/dbus/org.noahie.debloatmail.Daemon.xml`
  as the authoritative definition and generate bindings with `zbus_xmlgen`, which
  converts D-Bus interface XML into Rust [^852afa]. Hand-writing `#[zbus::interface]`
  impls is fine, but then the XML and the code drift, and `Introspectable` starts lying
  to every debugging tool.
- **Verify the drift, don't assume it.** A CI test should introspect the running daemon
  and diff against `data/dbus/*.xml`. This catches the most common D-Bus bug there is.
- **`ObjectManager` must be implemented by hand.** zbus's `ObjectServer` does not
  synthesize `GetManagedObjects`; you emit `InterfacesAdded` yourself on registration.
- **Async or blocking.** The daemon is async (`tokio` or `async-io`, zbus supports
  both); `debloatmailctl` should use zbus's blocking API, since a CLI has nothing to
  do concurrently [^7f66af].
- **Method timeouts.** D-Bus's default 25-second method timeout will fire on `Sync` if
  it were blocking — another reason it is job-based. Clients should raise their own
  timeout for `Feeds.Add` (network) and `Accounts.Check` (network).
- **Name flags.** The daemon should request
  `RequestName("org.noahie.debloatmail.Daemon", DO_NOT_QUEUE)` so a second daemon exits
  immediately with a clear message rather than sitting in the activation queue.

---

## 20. Gaps and corrections found

Writing this spec surfaced issues in the earlier documents.

**Corrections to `debloatmailctl.md`:**

1. **§10 names the wrong service.** Writes go to `org.noahie.debloatmail.Daemon`, not
   `org.noahie.debloatmail`. The GUI owns the latter (§2).
2. **The CLI's `Mark(ids, state, value)` becomes two typed methods.** `MarkRead(as, b)`
   and `SetStarred(as, b)` replace the stringly-typed `state` argument. A typo in
   `"starred"` should be a compile error in the client, not a runtime no-op.

**Gaps in `DATA-MODEL.md`:**

3. **`pending_ops` needs a `public_id`.** `Compose.Send` returns a stable `op_id` that
   must survive daemon restarts, and `DATA-MODEL.md` §3 forbids exposing internal
   rowids. Add `pending_ops.public_id TEXT NOT NULL UNIQUE` (UUIDv7). Same one-line
   change as the `items.hidden_at` gap — cheap now, a migration later.
4. **Feed `Hide` needs `items.hidden_at`.** Already flagged in `debloatmailctl.md` §13;
   confirmed here, since `Items.Hide` cannot be implemented without it.
5. **Drafts have no home.** `Compose.SaveDraft` cannot be implemented until they do.
6. **Sent-append needs its own op or an atomic composite.** `Compose.Send` with
   `append_to_sent = true` must be one op, or `pending_ops.op` needs an `append`
   variant. The current CHECK constraint has neither.

**New gap found here:**

7. **The read path breaks on Android.** The GUI and CLI read SQLite directly and write
   over D-Bus. On Android there is no session bus. So `ffi` cannot hard-depend on a
   D-Bus client — the write half must be a trait with a D-Bus implementation on desktop
   and a direct in-process implementation on mobile. This does not change the beta, but
   it does change how `ffi` is structured, and retrofitting it later would mean
   re-cutting the ABI. Decide it before `ffi` is written.

**Packaging gaps:**

8. **A second service file is needed.** `data/` currently has one
   `org.noahie.debloatmail.service`; it needs
   `org.noahie.debloatmail.Daemon.service` as well (§2, §21).

---

## 21. Activation files

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
`org.freedesktop.Application.Open` [^b70bb3].

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
      <arg name="target_folder" type="s" direction="in"/>
      <arg name="affected" type="u" direction="out"/>
    </method>
    <method name="Copy">
      <arg name="ids" type="as" direction="in"/>
      <arg name="target_folder" type="s" direction="in"/>
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
      <arg name="part_id" type="s" direction="in"/>
      <arg name="blob_path" type="s" direction="out"/>
    </method>
    <method name="RefreshFeed">
      <arg name="feed_id" type="s" direction="in"/>
      <arg name="job_id" type="u" direction="out"/>
    </method>
    <signal name="Progress">
      <arg name="account_id" type="s"/>
      <arg name="phase" type="s"/>
      <arg name="percent" type="u"/>
    </signal>
    <signal name="Finished">
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
      <arg name="url" type="s" direction="in"/>
      <arg name="name" type="s" direction="in"/>
      <arg name="interval_secs" type="u" direction="in"/>
      <arg name="result" type="a{sv}" direction="out"/>
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
  </interface>

  <interface name="org.noahie.debloatmail.Compose">
    <method name="Send">
      <arg name="draft" type="a{sv}" direction="in"/>
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
    <property name="Id"          type="s" access="read"/>
    <property name="Name"        type="s" access="read"/>
    <property name="Kind"        type="s" access="read"/>
    <property name="Enabled"     type="b" access="read"/>
    <property name="State"       type="s" access="read"/>
    <property name="LastSyncAt"  type="t" access="read"/>
    <property name="LastError"   type="s" access="read"/>
    <property name="UnreadCount" type="u" access="read"/>
    <property name="Revision"    type="t" access="read"/>
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
    <method name="Compose">
      <arg name="to" type="s" direction="in"/>
      <arg name="subject" type="s" direction="in"/>
      <arg name="body" type="s" direction="in"/>
    </method>
  </interface>
</node>
```
