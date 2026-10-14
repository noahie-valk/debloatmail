# User Interface

What `debloatmail` (the GUI) is, what it is allowed to touch, how the unified stream
becomes a Qt model, how a click becomes a durable state change, and what it must never
do.

**Scope of authority.** This file owns everything between "the window is on screen" and
"a D-Bus call has been made." It does not own the D-Bus signatures (`debloatmaild.md`),
the schema (`DATA-MODEL.md` revision 3), the engine (`SYNC.md`), or the CLI
(`debloatmailctl.md`). Where it needs something those documents do not provide, §19 asks
for it rather than inventing it.

---

## 1. Scope and non-goals

**In scope:** window and navigation structure, the `ContentStream` → `QAbstractListModel`
mapping, the item delegate, the reader, optimistic updates and reconciliation, compose,
search, account and feed setup, the tray and notification contract, status and progress
surfaces, preferences, threading and performance budgets, accessibility.

---

## 2. What the GUI may touch

Three data paths, and the rule for each.

| Path | Direction | Mechanism | Enforced by |
|---|---|---|---|
| **Read** | DB → GUI | Direct SQLite, read-only, through `crates/store` | `SQLITE_OPEN_READ_ONLY` on the open call, not convention |
| **Write** | GUI → daemon | D-Bus methods on `org.noahie.debloatmail.Daemon` | The GUI has no write connection to open |
| **Notify** | daemon → GUI | Signals via one sender-based match rule | `AddMatch("type='signal',sender='org.noahie.debloatmail.Daemon'")` |

**Why reads bypass D-Bus.** A 10,000-item list serialized over the bus is slow,
allocation-heavy, and needs its own pagination protocol — while WAL already gives
lock-free concurrent readers (`debloatmaild.md` §1). The GUI links `debloatmail-store`
and therefore `rusqlite`, and the read path is the *same code the CLI uses*, so a query
bug has one home rather than two.

**The consequence to protect:** the window renders from the cache before the daemon is
reachable. A cold start with the daemon stopped must still show mail. That is the entire
point of the header cache (`architecture.md` §6) and it is a test, not an aspiration
(§18).

**Two bus names, and which one is ours.** The GUI owns `org.noahie.debloatmail`; the
daemon owns `org.noahie.debloatmail.Daemon`. The GUI owns the primary name because
`DBusActivatable=true` requires the well-known name to equal the desktop file's name
minus `.desktop` (`debloatmaild.md` §2). That is what routes `mailto:` links to the
running instance instead of spawning a second one. The application id must match in the
five places `GEMINI.md` lists — this document does not keep its own copy of that list;
the desktop file and the activation files carry their copies and are checked against
GEMINI in CI (`debloatmaild.md` §21).

---

## 3. The QML ↔ bridge contract

`architecture.md` §4 makes `ui/bridge` **deliberately disposable**: when Android arrives
it gets its own binding path against the same `ffi` surface.

**The rule that follows:** *nothing in `ui/qml` may name a `cxx-qt` type.* QML sees a
small set of named objects; the generated types live behind them and are never
referenced.

```qml
import org.noahie.debloatmail 1.0 as DM

DM.StreamModel { spec: DM.StreamSpec { source: DM.Unified } }
DM.Daemon { }        // write half: MarkRead, Move, Send, Sync, Check, …
DM.Store  { }        // read half: ItemDetail, attachment metadata, counts
DM.Settings { }      // config-file-backed preferences (§15) — deliberately not GSettings
```

A second bridge satisfies the same surface with the same names. If a QML file ever
needs `import org.noahie.debloatmail.bridge`, the boundary has been violated (§21 tests
for it).

### 3.1 The write half must be a trait — and the read half must be tested on desktop

`debloatmaild.md` §20 (gap 7): on Android there is no session bus, so the write half
cannot be a hard D-Bus dependency. The `ffi` write interface is a trait with a D-Bus
implementation on desktop and an in-process implementation on mobile. **This does not
change the beta, but it changes how `ffi` is structured, and retrofitting it later means
re-cutting the ABI.** Decide it before `ffi` is written (the decision is open;
`debloatmaild.md` §20 item 7).

**The read-side consequence, and the obligation that follows from it.** Because reads
go through `store` directly (§2), the desktop GUI **never exercises the `ffi` read
surface** — and that surface is exactly what the Android hedge depends on. The rule
that follows: the `ffi` read surface is tested on desktop even though no desktop code
calls it. A test drives the same read queries through `ffi` over a fixture database
and asserts the results match the direct `store` path — so that on the day the `ffi`
binding is cut, it is proven, not assumed. One more reason the cut cannot be deferred:
an untested ABI surface is the kind of thing that is only noticed when a second platform
starts depending on it.

### 3.2 Everything asynchronous

Every D-Bus call is asynchronous. `Sync`, `Check`, `Feeds.Add`, `FetchBody`, and
`FetchAttachment` can each take seconds; a blocking `call` would freeze the window and
then hit D-Bus's default 25-second method timeout (`debloatmaild.md` §19). The bridge
shape is an async method plus a completion signal, and QML never blocks on a reply.

### 3.3 Reads are synchronous, bounded, and paged

SQLite reads run on the GUI thread. A keyset page over an indexed query is sub-10 ms —
the CLI's own target (`debloatmailctl.md` §2) — so the cost of threading a model is not
worth it, and a cross-thread `QAbstractItemModel` is exactly the Qt threading hazard
`architecture.md` §14 flags. **But the read must be bounded to a page**, because an
unbounded query on a cold cache or a broad FTS5 search is the one read that can stall.

**Never hold a read transaction across an event-loop turn.** A long-lived read snapshot
pins the WAL and blocks checkpointing, so the WAL grows without bound while the GUI sits
open. Open, read one page, commit, return.

### 3.4 Convergence

Kirigami exists to make one QML codebase adapt across desktop and mobile, and that is
why it was chosen over GTK4 (`architecture.md` §4). Beta targets desktop only, but
layout decisions use Kirigami's form-factor detection and `Kirigami.Units` rather than
hard-coded pixels, so the mobile layout is a refinement rather than a rewrite.

---

## 4. Window architecture

```
Kirigami.ApplicationWindow                      ← root
├── globalDrawer: Kirigami.GlobalDrawer         ← accounts, folders, feeds, unread counts
│     modal: !Kirigami.Settings.isMobile        ← persistent sidebar on desktop
├── pageStack: Kirigami.PageRow                 ← the global pageStack
│     ├── [0] StreamPage   (ListView over a DM.StreamModel)
│     └── [1] ReaderPage   (pushed on selection; column 2 on desktop)
└── contextDrawer / OverlaySheets               ← contextual actions, compose, search
```

`Kirigami.ApplicationWindow` is built around the `PageRow` component, and applications
should use the global `pageStack` rather than instantiating their own `PageRow`.
`PageRow` is a row-based navigation model for interlinked pages and shows **multiple
columns side by side on desktop** — which is exactly the list-detail shape a mail
client wants: on desktop the list and the reader are both visible; on a narrow window
the reader covers the list and Back returns.

`Kirigami.GlobalDrawer` looks like a standard sidebar and can be made always-visible in
widescreen by setting `modal: false`; in narrow mode users pull it out on demand.
`Kirigami.ContextDrawer` is the same idea for actions relevant only to the current
page.

**On mobile form factors**, a `Kirigami.NavigationTabBar` is the alternative to a
sidebar for 3–5 top-level destinations — e.g. *Inbox · Feeds · Search · Accounts* —
with the global drawer retained for the full folder tree.

**Window state.** Size and position are persisted across runs. `Qt.labs.settings` is
explicitly not guaranteed to stay compatible; the supported type is `QtCore.Settings`,
which is what the bridge uses. This is the one place Qt's own settings facility is used;
it persists window geometry only — every application preference goes in the config file
(§15).

**One thing that must not happen:** the window must not wait on D-Bus to become
visible. `Status.GetAll` and the `ObjectManager` enumeration fill in the sidebar *after*
the first page of items is already drawn (§18).

---

## 5. Navigation

### 5.1 The drawer tree

```
All Accounts                       ← unified stream
  ▸ Fastmail                       ← account, with UnreadCount
      Inbox  Sent  Drafts  Trash … ← folders, by role
  ▸ Proton
  ▾ Tech                           ← feed account (a named group)
      Rust Blog  LWN  …            ← feeds, with UnreadCount
```

Per-account and per-feed unread counts come from the `Account.UnreadCount` and
`Feed.UnreadCount` properties, refreshed by `Account.Changed` / `Feed.Changed`. The
aggregate is the sum of those values — the GUI does **not** run a second query for the
same number, because `debloatmaild.md` §12 makes per-object counts the contract and a
global count a client-side aggregation.

Feed groups are **feed accounts**: the named grouping that `Feeds.Add` requires (DM
v3 §11.3), so the drawer's feed section and the feed-add wizard (§12.3) address the
same objects.

### 5.2 Streams are the unit of selection

Every destination in the drawer resolves to a `StreamSpec` (`DATA-MODEL.md` §4):

| Selection | `StreamSource` |
|---|---|
| All Accounts | `Unified` |
| An account | `Account(id)` |
| A folder | `Folder(id)` |
| A feed | `Feed(id)` |
| A search | `Search(query)` |
| A conversation | `Thread(id)` |

One list view, one model, one delegate, one reader — for mail and feeds alike. That is
the payoff of the unified stream (`architecture.md` §8) and it is why search results,
folder views, and the inbox are the *same page type* with a different spec.

### 5.3 Selection state

The selected item is identified by `public_id`, never by model row and never by rowid —
rows shift when the daemon inserts, a row index would silently point at a different
message, and rowids never cross the boundary (DM v3 §3; `debloatmaild.md` §12 encodes
`public_id` in per-object paths for the same reason). If the selected item disappears
from the stream (deleted elsewhere, expunged, hidden), the reader shows a "this item is
no longer available" placeholder and returns to the list.

### 5.4 Sorting

Sort options match the CLI's `--sort` exactly (`debloatmailctl.md` §9): **`date`
(default), `author`, `subject`** — the same set, the same meanings, in both front-ends.

**The keyset cursor encodes the active sort key, not a fixed column** (DM v3 §4.2.3).
That is the fix for the previous revision's conflation of `sort_at` with `--sort
date`: the default view pages on `published_at` — which is now indexed
(`idx_items_date`, DM v3 §5), so the sub-10 ms page target holds at scale — and an
author or subject sort pages on its own column. `sort_at` remains the internal sort
value (`DATA-MODEL.md` §4.2.3: mail `INTERNALDATE` → `Date` → `created_at`; feed
`published` → `updated` → fetch time); it is what orders streams that do not care about
display dates, not the default display sort.

The full key is `(active key value DESC, public_id DESC)`, so the order is a **total
order**: two items equal on the key still page stably (DM v3 §4.2.3). A cursor carrying
anything less can skip or duplicate rows under concurrent daemon writes.

Changing the sort changes the query and therefore resets the model (§6.4 / earlier
§6.4 below — reset is the only legitimate case). The reader's position is preserved by
`public_id`, so a sort change does not lose the user's place.

---

## 6. The stream model

This is the heart of the GUI. One `QAbstractListModel` subclass over a `StreamSpec`.

### 6.1 Roles

`QAbstractListModel` is subclassed to expose one-dimensional data to QML; roles are the
binding surface, and a delegate binds to them by name.

| Role | Type | Source |
|---|---|---|
| `idRole` | string | `Item.public_id` — full UUID; **the id** (rowids never reach the model, DM v3 §3) |
| `shortIdRole` | string | first 8 hex — the CLI's addressing convention (`debloatmailctl.md` §5) |
| `kindRole` | int | `ItemKind` — for a glyph only, never for layout (§10) |
| `accountIdRole` | string | `Item.account` |
| `titleRole` | string | `Item.title` |
| `authorRole` | string | `Item.author` (empty when absent) |
| `excerptRole` | string | `Item.excerpt`; **overridden by the FTS5 `snippet()` in a Search stream** |
| `dateRole` | qint64 | `Item.date`, **epoch seconds, UTC**; `0` means unset |
| `unreadRole` | bool | `Item.unread` |
| `starredRole` | bool | `Item.starred` |
| `hasAttachmentsRole` | bool | `Item.has_attachments` |
| `threadIdRole` | string | `Item.thread` (empty when absent) |
| `bodyStateRole` | int | `Item.body_state` |
| `actionsRole` | int | `ItemActions` bitfield (§10) |

**`dateRole` is epoch seconds and `0` means unset.** `DATA-MODEL.md` §14 makes
formatting a UI concern, so the model does not guess a locale — but `0` is also a
legal Unix time, and rendering an absent date as 1970-01-01 is the bug that
convention exists to prevent. The delegate renders nothing when `dateRole === 0`.

**Roles are a contract.** `roleNames()` is asserted against a golden list in CI,
because a rename silently breaks every delegate that binds to it — with no compile
error, since QML binds strings.

### 6.2 Paging

The stream is a keyset cursor over the **active sort key** —
`(active key value DESC, public_id DESC)` (§5.4) — not a fixed pair of columns:
offset paging is not stable under concurrent daemon writes, and a cursor that does not
encode the column the query orders on is the paging bug (issues 8; DM v3 §4.2.3). The
model holds a window of **50 rows** and implements `canFetchMore()` / `fetchMore()`,
which the view calls when it needs more.

`fetchMore` runs on the GUI thread and is synchronous. That is acceptable here because
the read is a bounded, indexed, local page (§3.3). If a page ever proves slow, Qt's
documented escape hatch is a worker thread for `fetchMore` — but that reintroduces the
cross-thread model hazard `architecture.md` §14 warns about, so it is a fallback, not
the default.

**`loading` is a distinct state from `empty`.** Before the first page lands, `rowCount`
is 0 for a reason that is not "there is nothing here." Without a separate flag, the
delegate flashes "Nothing here yet" for a frame on every navigation. The model exposes
`loading`, and the view shows a skeleton, not a placeholder message.

### 6.3 Incremental updates, not resets

`Items.Changed` is coalesced per 100 ms (`debloatmaild.md` §16). A first sync of 50,000
headers would therefore deliver many change notifications, and a model that called
`beginResetModel` on each would destroy scroll position, selection, and the current
index several times a second.

| Change | Model operation |
|---|---|
| New rows newer than the window head | `beginInsertRows(0, n-1)` **if the user is at the top** |
| New rows while scrolled down | Buffer; show an "N new items" affordance |
| Rows whose fields changed | `dataChanged` for the affected indexes only |
| Rows removed from the stream | `beginRemoveRows` |
| Query changed (filter, sort, search, scope) | `beginResetModel` — the only legitimate case |

**The buffered-insert rule is a deliberate UX choice, not a limitation.** Inserting
above a scrolled viewport moves the content under the user's cursor. Gmail's "1 new
message" pill exists for the same reason: when the user is reading, new mail must not
yank the list. Clicking the pill scrolls to the top and flushes the buffer.

### 6.4 The revision protocol

WAL gives readers a consistent snapshot, but the model's read transaction can begin
*before* the daemon's commit and therefore not see the change that triggered the signal
(`debloatmaild.md` §16). A model that trusts "signal received ⇒ data visible" will
occasionally miss updates.

The revision the protocol keys on is **stored, not remembered**: the daemon commits it
with the data (DM v3 `meta` table; issues 3), so it survives daemon restarts and the
GUI can compare against it without any in-memory notion of "where I was."

```
on Items.Changed(account_ids, revision):
    if revision <= lastSeenRevision: return
    if account_ids ∩ accountsCoveredBySpec == ∅: return   // unrelated change
    repeat up to N times:
        observed = freshRead()            // a NEW transaction
        if observed.revision >= revision: break
    lastSeenRevision = revision
    apply the incremental diff (§6.3)
```

**The account filter matters.** A `Changed` for an account outside the stream's spec
must not re-query it — otherwise every account's sync invalidates every folder view.
The over-invalidation that remains (a change to another folder of the same account, for
a `Folder` stream) costs one indexed query returning no new rows, which is cheap enough
to accept rather than add a per-folder revision.

**At startup**, read the stored revision (DM v3 `meta`) and compare against
`Status.Revision`; do one full refresh if behind. That is how signals missed while the
GUI was not running — and the revision differences accumulated while the daemon ran
unattended — are recovered without polling.

### 6.5 Search streams

A Search stream is the same model with a different spec, so search results get the
whole list infrastructure for free: paging, selection, the reader, batch actions.

- As-you-type, debounced ~150 ms. The FTS5 prefix indexes (`prefix = '2 3 4'`) make
  prefix search fast without a `LIKE` scan (DM v3 §10), so the debounce is for
  keystroke coalescing, not for hiding latency.
- `excerptRole` returns the FTS5 `snippet()` rather than the stored excerpt, so a result
  shows *why* it matched. This is only possible because the index is external-content
  (DM v3 §10) — a contentless index could not do it.
- Malformed queries must yield zero results, not an error or a panic (DM v3 §10). The
  GUI surfaces "no results," never a crash.
- The filter language is the CLI's (`debloatmailctl.md` §9), so `from:alice
  is:starred` works identically in both front-ends. `from:` is one of the closed
  operator set (DM v3 §10) and matches the structured `mail_items.from_addr` — the
  reason that field exists in the schema (issues 5). If the GUI ever needs its own
  syntax, the ABI is wrong (`debloatmailctl.md` §1).
- **What search can find:** title and author for **every** item, from first sync — the
  header-time `item_text` row (DM v3 §11.1, `SYNC.md` §5.5) means a never-opened
  message is found by subject and sender. **Body search finds cached bodies only** (DM
  v3 §9.2): a phrase in an evicted or never-fetched body returns nothing, and that is
  the price of the thin cache, not a bug the GUI papers over.

---

## 7. The item delegate

`Kirigami.BasicListItem` is the base. One delegate, both kinds.

```
┌───────────────────────────────────────────────────────────┐
│ ● │ ✉ │ Alice Chen                            14:22  ★ 📎 │
│   │   │ Re: release checklist                               │
│   │   │ Yes, let's push the freeze to Friday…              │
└───────────────────────────────────────────────────────────┘
   │    │    │          │        │   │  │
   │    │    │          │        │   │  └ attachment glyph
   │    │    │          │        │   └ starred glyph
   │    │    │          │        └ time (today) or date
   │    │    │          └ author
   │    │    └ kind glyph: ✉ mail, ⚑ feed  — decoration only
   │    └ unread dot
   └ selection / focus
```

**The kind glyph is not layout branching.** `architecture.md` §8 forbids the UI
branching on `kind` for *layout*; a badge that distinguishes a feed article from a
message is decoration, and it is the one place `kindRole` may be read. If a second
delegate ever appears, the abstraction is leaking.

**State is never encoded in colour alone.** Unread is a dot *and* a bold title weight
*and* part of the accessible name; starred is a glyph. Colour-only state is invisible
to colourblind users and to screen readers (§17).

**The excerpt is one line, elided.** It is a hint, not a preview; a two-line excerpt
doubles the delegate's height and the cost of scrolling 50,000 rows.

**Density** is a preference (§15): comfortable (default) or compact. Compact reduces
the row to title + author + date.

**Mobile** adds `Kirigami.SwipeListItem` for swipe-to-toggle-read and swipe-to-delete,
mirroring the same actions the context menu offers.

---

## 8. The reader

### 8.1 Headers first, body deferred

The thin cache means the reader **always has something to draw immediately**: title,
author, date, and the attachment list all come from headers — and title/author also
from the header-time `item_text` row (DM v3 §11.1) — which were synced without
downloading a single body (`SYNC.md` §5.5).

```
open(item):
    render the header block from Item + ItemDetail (local, no network)
    switch body_state:
        Cached  → render item_text.body
        None    → FetchBody(id); show a skeleton; on return re-read the row and render
        Pending → show a skeleton, subject to the timeout below
        Failed  → show the error and a Retry action
```

This is the visible benefit of the two-step read (`debloatmaild.md` §6): the reader
never waits on the network to show *who* and *what*, only to show *the text*.

**Bounded even during a big sync.** An interactive `FetchBody` on the same account
mid-initial-sync runs at a committed 2,000-UID window boundary (`SYNC.md` §3.1), so the
skeleton is shown at most for the remaining window — not for the whole sync. The
existing skeleton behaviour is therefore sufficient; no special handling.

### 8.2 `pending` must not hang

`SYNC.md` §6.1 has the daemon sweep `body_state = 'pending'` back to `none` on
startup, because a crash mid-fetch leaves it behind. **If the daemon is not running,
nothing performs that sweep**, and a reader that trusts `pending` indefinitely spins
forever.

The rule: a `pending` state is trusted only while the daemon is reachable and only for
a bounded time. Otherwise it is treated as `none` and the reader offers a Fetch action.

### 8.3 Plaintext rendering — and the one thing that is banned

The body is rendered with `textFormat: TextEdit.PlainText`.

**`TextEdit.AutoText` is an HTML renderer and must not be used.** `AutoText` sniffs the
content and switches to rich text if it looks like markup, which (a) reintroduces
exactly the HTML path `architecture.md` §1 excludes and (b) turns every message body
into an injection vector against the renderer. A one-word property default is the
difference between "plaintext client" and "HTML client with extra steps."

**Linkification is allowed, under one rule.** URLs may be rendered as clickable spans,
but only by a transform that **escapes every character of the body and then adds `<a>`
spans**. No tag originating in the body may ever reach the renderer. Activating a link
calls `Qt.openUrlExternally(url)` — the system browser fetches it; the GUI never does.
`href` values are validated to `http`/`https`/`mailto` before being emitted.

**The GUI never loads a remote resource.** No `<img>`, no web fonts, no tracking
pixels, no favicon fetch for a feed. Since the body is plaintext this is nearly free,
which is precisely why it must be written down: it is the invariant a future "let's
render a bit of HTML" would quietly break.

### 8.4 Source-specific detail

`Item` deliberately omits `Message-ID`, `References`, `To`, `Cc`, and a feed item's
`link` (DM v3 §4). The reader needs them for the header block; reply needs them for
prefill; a feed item needs its link. One mechanism — a single-item `ItemDetail` lookup
— rather than fields on every row of a 50,000-row model.

`ItemDetail.from` is the **structured sender** — `mail_items.from_addr` (DM v3 §5),
populated at header sync — not the display text. That is what reply-to-sender
prefills (§11.4) and what `from:` search matches (DM v3 §10). A feed item's
`ItemDetail` carries the entry `link`.

### 8.5 Mark read on display

Displaying an item in the reader marks it read. The CLI deliberately does the opposite
(`debloatmailctl.md` §8) and gives the reason: in a scripting tool a silent state
change is a hazard, but "in the GUI, display *is* the intent." Both are right for
their contexts.

The gesture that causes display differs by layout — selection in a reading-pane layout,
push in a narrow layout — but the rule is uniform: **mark read when the item is
displayed in the reader**, whichever gesture got it there. It goes through the
optimistic path in §9, so it feels instant and survives a flaky network.

A preference (§15) controls this: *immediately*, *after N seconds of dwell*, or
*manually*. The dwell option exists because arrow-keying past an item is not intent.

### 8.6 Attachments

The attachment list renders from the `attachments` table — metadata only, no fetch (DM
v3 §9: the blob is not local until the user opens the file). This is what keeps
"sync" from meaning "download everything."

**The list is provisional until the first body fetch.** At header time the list comes
from the server's `BODYSTRUCTURE` (parsed by `crates/extract`, `SYNC.md` §5.5); when
the body is fetched, the walk over the actual bytes is reconciled against it —
walk-wins, and that is the only direction the list may change (`SYNC.md` §6.1,
`MIME-AND-TEXT.md` §6.1). The reader therefore re-reads the attachment rows after a
`FetchBody` returns, so a corrected list (a mis-declared `Content-Disposition`, a
bodystructure the walk sees through) is what the user sees. 📎 may change after a body
fetch *by design and in one direction only*.

Opening one calls `FetchAttachment(item, section_id)` — the value is the RFC 3501
§6.4.5 section number, usable verbatim in `BODY.PEEK[section_id]` (DM v3 §5; renamed
from `part_id` for exactly this reason; `debloatmaild.md` §6, §17) — and returns a
**path**, not bytes. A 20 MB attachment must not be marshalled through the bus
(`debloatmaild.md` §6).

| Type | Action |
|---|---|
| PDF, desktop | Render inline with QtPdf (`architecture.md` §11). The daemon never parses PDFs — that is the GUI's parse (`architecture.md` §11; `SECURITY.md` §6). |
| PDF, mobile | Unavailable in beta; `architecture.md` §11 keeps QtPdf desktop-only |
| Anything else | `Qt.openUrlExternally("file://" + path)` — hand off to the desktop |

**The path has a lifetime.** `SYNC.md` §6.2 gives the blob a one-hour grace window
rather than a pin/refcount protocol — matching `debloatmaild.md` §6 — so a path may be
gone by the time the user clicks. The reader handles "file missing" by re-fetching,
not by erroring. If the user wants to *keep* a file, the GUI copies it to a
user-chosen location — it does not rely on the cache path.

---

## 9. Optimistic updates

DM v3 §6 states the requirement: the UI applies a change optimistically and reconciles
on the signal. This section is the state machine.

### 9.1 Why an overlay at all

The daemon commits the local change and *then* queues the server op
(`debloatmaild.md` §5), so the round trip is short. But it is not zero, and the model
is also being refreshed by `Items.Changed`. Without an overlay, the sequence "user
clicks unread → model re-queries before the commit lands → row renders unread again →
commit lands → row renders read" is a visible flicker. The overlay makes the user's own
action the last word until the database agrees.

### 9.2 The overlay

Keyed by `(public_id, effect)`, held in the model, never persisted:

```
Overlay { public_id, effect, op_id?, applied_at }
Effect  = SetField(field, value)     // unread, starred
        | RemoveFromStream           // delete, move out of scope, hide
```

`RemoveFromStream` is the generalisation that makes delete and move work. Deleting a
mail item moves it to trash; the row leaves an inbox stream but still exists. Hiding a
feed item suppresses it; the row leaves every stream. Both are "remove from this
stream," not "set a field."

### 9.3 The state machine

```
Clean ──user action──► Optimistic ──DB agrees──► Confirmed (overlay dropped)
                          │
                          ├──D-Bus call reports id unresolved──► Clean (re-read)
                          ├──expiry, no confirmation──────────► Clean (re-read)
                          └──Queue.Completed(success=false)───► Unsynced
```

| # | Event | Action |
|---|---|---|
| 1 | User toggles a state | Write the overlay, emit `dataChanged`, call the D-Bus method |
| 2 | Method returns `affected` | If the id is **not** in the affected set, drop the overlay and re-read — it did not resolve (`debloatmaild.md` §5) |
| 3 | `Items.Changed` with a newer revision | Re-read the row. DB equals overlay → `Confirmed`, drop it. DB differs → **the DB wins**; drop the overlay |
| 4 | `Queue.Completed(op_id, false, detail)` | `Unsynced`: **keep the local value**, mark the row, add it to the "not synced" surface |
| 5 | `Queue.Completed(op_id, true, "")` | Clear the marker |

**Step 4 is the pending-op-wins rule.** DM v3 §7: a pending op wins over an incoming
server flag update until it is drained. Flipping the value back on a network failure is
exactly the flicker that rule exists to prevent, and it also contradicts
`debloatmaild.md` §5: "the user's intent is not undone by a flaky network." Note the
asymmetry with step 3: *DB wins on confirmation*, because the DB now includes the
server's truth; *local wins on a failed op*, because the DB includes nothing the
server never accepted.

**Step 3 must be keyed, not global.** A re-read caused by an unrelated account must not
clear an overlay on this one.

### 9.4 The overlay expires

If the daemon dies between the call and the commit, an overlay with no confirmation
would show a lie indefinitely. **Bound it:** if neither `Items.Changed` nor
`Queue.Completed` arrives within ~10 seconds, re-read the row and drop the overlay. The
database is the truth even when it disagrees with the user's last click — the
difference is that after the timeout the UI *says so* rather than guessing.

### 9.5 Mutations require the daemon

The GUI has no standalone mode (§1). If the D-Bus call fails with a connection error,
the overlay is **not applied at all**: the row stays unchanged and an inline "background
sync is not running" message appears with a Start action.

Applying the overlay anyway would be the worst of both worlds — the user believes the
change happened, the database disagrees, and the timeout in §9.4 silently reverts it.
This is the deliberate difference from `debloatmailctl --no-daemon`, which *can* write
because it becomes the writer; the GUI never is.

### 9.6 Partial application is normal

`debloatmaild.md` §5: if some ids resolve and others do not, the method succeeds and
returns a count. A batch action over a mixed selection therefore reports honestly —
"3 of 5 moved" — rather than claiming success or raising an error.

---

## 10. Actions, and how not to branch on `kind`

Mail can be replied to and moved; a feed article can be opened at its source. A UI
that writes `if (kind === Mail) showReply()` has leaked the abstraction that
`architecture.md` §8 exists to provide.

**Proposal (§19.2): `ItemActions`, a bitfield computed below the abstraction and
carried on `Item`.**

```
Reply  Forward  OpenLink  Move  Copy  Delete  Star  MarkRead
```

The action bar renders `Reply` iff `actions & Reply`. QML never tests `kind`.

This also makes `Error.Unsupported` unreachable from the GUI for these verbs, because
the UI never *offers* a verb the item does not support. Offering Move on a feed item
and then showing an error dialog is strictly worse than not offering it — and the error
exists precisely because the API cannot know what the caller already knows.

**One correction falls out of this:** `debloatmaild.md` §5 gives `Delete` and `Hide`
identical behaviour for feed items (both set `items.hidden_at`). Two verbs for one
operation is an API bug; the GUI exposes one feed verb — *Dismiss* — and calls `Hide`.

**Move target.** A `Move` carries the **destination folder's `public_id`** — the
corresponding approved change (`debloatmaild.md` §12). The move list is the drawer's
folder tree minus the current folder and the send/submit roles; the id carried is the
same one §5.3 uses for selection.

---

## 11. Compose

### 11.1 Where it lives

A **separate top-level window on desktop**, a **pushed page** on mobile. Rationale:
compose must coexist with reading — replying to one message while consulting another is
the normal case on desktop, and a pushed page makes that impossible.

### 11.2 The payload

`Compose.Send(draft, append_to_sent)` takes the `draft` dict from `debloatmaild.md`
§9: `to`/`cc`/`bcc` (`as`), `subject`/`body` (`s`), `in_reply_to`/`references` (`s`),
`attachments` (`as`, absolute paths). Plaintext only — the same exclusion as reading.

**Attachments are file paths, and the daemon validates them** as regular, readable
files inside the user's home (`debloatmaild.md` §17). The GUI pre-validates for a good
error message, but the check that matters is the daemon's: a mail client that will
attach `/proc/self/environ` is a bug regardless of who asked.

### 11.3 Send is a queue, and the GUI must say so

`Send` returns an `op_id` immediately — it queues, it does not send
(`debloatmaild.md` §9). The GUI:

1. Calls `Send`, gets `op_id`, closes the compose window. The message is **durable** at
   this point, so closing is honest — a passive notification confirms "Message queued."
2. Watches `Queue.Completed(op_id, success, detail)`.
3. On `success = false`, notifies with the `detail` and offers **Reopen**.

**The GUI must retain the composed content keyed by `op_id` until the op reaches a
terminal state.** `Queue.Completed`'s `detail` carries the error, not the message;
without retained content, "Reopen" has nothing to reopen. This is in-memory for the
unsaved case — with local drafts (§11.5), Reopen prefers the last saved draft and the
in-memory copy is the fallback for content never saved.

**"Failed" is not the same as "discarded."** An op in retry backoff (`SYNC.md` §8.6) is
waiting, not dead. The GUI distinguishes *waiting to send* from *failed*, using
`Queue.Pending()` and the terminal state, so a user does not resend a message that is
still queued.

**An uncertain send is not a failed send.** `SYNC.md` §8.4 gives the send op a
`Message-ID` confirmation path precisely because SMTP cannot be retried blindly. The
GUI must not offer a "Resend" that would produce a duplicate; it offers *Reopen* (a new
message) or nothing.

### 11.4 Reply and forward

Prefill comes from `ItemDetail` (§8.4), not from `Item`:

| Field | Source | Note |
|---|---|---|
| `to` | `reply_to` if set, else `from` | `from` is `ItemDetail.from` — the **structured** sender address (DM v3 `mail_items.from_addr`; issues 5) |
| `cc` | `to_addrs` + `cc_addrs` minus self | reply-all only |
| `subject` | `Re: ` + `display_subject` with any existing reply prefix stripped | Stripping removes only the leading `Re:`/`Fwd:` (and localised equivalents), case-insensitively on the marker; **the rest of the subject passes through verbatim, case preserved**. Never built on `subject_norm` — that is lowercased (issues 6; `MIME-AND-TEXT.md` §4.5) |
| `in_reply_to` | `message_id` | |
| `references` | `references_hdr` | |
| `body` | quoted with `> ` | attribution line is a config option (`debloatmailctl.md` §8); the option set is shared, §19.6 |

Forwarding: subject `Fwd: ` + the same prefix-stripped `display_subject`. Forwarding
with `--attach-original` (`debloatmailctl.md` §8) means attaching the raw MIME blob;
the GUI offers it only when the blob is present.

**The rule, stated once for both verbs:** the stripped display subject prevents `Re:
Re: Re:` in both cases — a subject that arrives already prefixed gets one marker, and a
subject that does not gets its original casing kept. A lowercase `Re:` is the visible
fingerprint of the bug this exists to remove.

### 11.5 Save draft — local-only, and the GUI says so

`Compose.SaveDraft` is **implemented** (`debloatmaild.md` §9), not `Unsupported`. The
v3 decision (DM v3 §11.5, issues 15): drafts are a **local-only table that never
syncs**. A saved draft lives in this database, on this machine — it does not appear in
the server's `role='drafts'` folder, and it does not exist on another device. The
earlier behaviour (raising `Error.Unsupported` because "drafts have no home") is
superseded; the home now exists and the choice is made, so the GUI's job is to make the
consequence legible rather than silent:

- The compose header labels the state: an unsaved compose is *draft — not yet saved*;
  a saved one is **Local draft**. The server's `role='drafts'` folder is a separate
  thing in the drawer; mixing the two lists without a marker would be how a user
  concludes the draft "vanished."
- **Closing with unsaved content** prompts: *Save as local draft / Discard* (plus
  *Keep editing*). The previous warning-only flow is replaced by a real save.
- Saved local drafts are listed in a clearly-labelled compose section — not folded into
  the mail `Drafts` folder view — and opening one restores the compose window.
  Whether they later merge into one list with a local badge is open (§19.5).
- **Failed-send reopens** (§11.3) restore from the last saved draft when one exists.
- There is **no autosave in beta** (§19.4). A local-only draft that appears in a list
  the user never asked for is a surprise cut from the other direction of the same
  lesson.

---

## 12. Account and feed setup

### 12.1 The wizard

```
collect config ──► Accounts.Add(config) ──► Check(account_id) ──► show report ──► Sync
```

`Accounts.Add` **does not verify connectivity** — it persists configuration and
returns, so an account can be configured offline and a slow server never leaves a
half-created account (`debloatmaild.md` §7). The caller then calls `Check`. The wizard
follows that shape exactly; it does not invent a combined "add and test" call.

The `Check` report is per-stage, and the wizard renders each row rather than collapsing
to pass/fail — a failure at `tls` is a different conversation from a failure at
`auth`:

| Stage | Shown |
|---|---|
| `dns` | resolved address |
| `tcp` | port and latency |
| `tls` | chain validity and expiry |
| `auth` | the IMAP login result |
| `capabilities` | `IDLE MOVE SPECIAL-USE …` |
| `smtp` | `EHLO` and the auth mechanism |
| `folders` | folder count discovered |

**The TLS row must report the loopback exception explicitly** when
`allow_invalid_cert_loopback = 1`. Silently tolerating a bad certificate is the failure
mode that turns a Proton convenience into a security bug (`debloatmaild.md` §7;
`SYNC.md` §5.1's no-downgrade rule fires here: `none` against a non-loopback host is
refused at validation, and the wizard's report is where that refusal is explained).

`Check` is a network operation and can exceed the default D-Bus timeout; the GUI raises
its own timeout for it (`debloatmaild.md` §19) and shows progress while it runs.

### 12.2 Secrets

The password is collected once and passed as `config.secret` in the `Add` call. The
daemon writes it to the keyring and stores only `secret_ref`. There is no
`GetPassword`-shaped method anywhere in the API, by design (`debloatmaild.md` §17) —
and the GUI must not cache one either. A password held in a QML property is a password
readable by any same-user process through the session bus. The same rule applies to
the rotation flow in §12.4: the new secret is a transient value in the call, touched by
secure input, gone after the reply.

### 12.3 Adding a feed

`Feeds.Add` takes the **feed account** the feed belongs to — `feeds.account_id` is
`NOT NULL` (DM v3 §11.3) — plus the URL, name, and interval. The wizard collects the
group first (the drawer's feed accounts, §5.1), then the URL.

The call is **synchronous** — one GET — and the GUI raises its D-Bus timeout for it
(`debloatmaild.md` §8), because the daemon carries its own 15-second validation budget
(`SYNC.md` §7.4). There are three outcomes, and all three are specified:

| Daemon outcome | GUI does |
|---|---|
| Validated within the budget | Feed appears, with the parsed title (the caller asked for it — that is why the call is synchronous). No further note. |
| `validated: false` (timeout) | Show the feed as **added, "not yet verified"** — a note, not an error. The background continuation resolves it; the first scheduled poll tells the truth (`SYNC.md` §7.4). Deleting it in the meantime is a normal `Feeds.Remove`, no special path. |
| Hard failure — `Gone` / `NotAFeed` / `Malformed` / `RedirectRejected` | **No feed row exists** (or was deleted). Report the specific failure and keep the URL in the field for correction. This is the loud half of "fail loudly, don't create an empty feed": a timeout can create a row, a verdict cannot (`SYNC.md` §7.4, DM v3 §11.3). |

### 12.4 Settings: what is possible now, and what is not

`Accounts` has `Add`, **`Update`**, `Remove`, `Check`, `Reload`; `Feeds` has `Add`
(with account) and `Remove`. The previous revision's "there is no Update" is stale: the
approved change landed (`debloatmaild.md` §7), and it closes the rejected-password
dead-end end to end (issues 10):

| Control | Method | Note |
|---|---|---|
| Rename account | `Accounts.Update` | Display name is unique (DM v3 §5) — a collision is rejected, and the settings page shows it rather than failing silently. |
| Enable / disable | `Accounts.Update` | Disabled stops the schedule without removing data. |
| Poll interval | `Accounts.Update` | Clamped to the minimums, same rule as configured values (`SYNC.md` §3.4). |
| **Rotate the secret** | `Accounts.Update` with `secret` | **The fix path for `NotAuthenticated`** (`debloatmaild.md` §15): the account's error state names it. Secure input per §12.2; the daemon re-writes the keyring and clears the error state on the next successful validation. This is the answer to "the password expired, now what", which Proton Bridge and app passwords guarantee will be asked. |

The auth-error banner therefore *is* the affordance: an errored account that reports an
authentication failure offers **Fix credentials** directly, not just "details" that end
at a log.

`Feeds.Remove` without purge is the default path: `removed_at` soft-removal — polling
stops, **the articles remain visible in streams and search** (DM v3 §11.2). Purge is
the explicit, confirmed second action. The settings page must not make purge the
obvious button.

**What is still not possible:** renaming or re-interval-ing an existing feed — no
`Feeds.Update` is in the approved API as of this revision; §19.3 asks for it rather
than papering over it with remove-and-re-add, which would discard the soft-removed
articles' schedule state and is exactly the kind of "setting" that becomes a trap.

---

## 13. Tray, notifications, and window lifecycle

### 13.1 The tray is the daemon's

`debloatmaild.md` §18 is explicit: **the tray icon must survive the window closing, so
it belongs to the daemon.** The GUI does not create a StatusNotifierItem and does not
own a `dbusmenu`. What the GUI does own is the other end of the interaction:

| The GUI implements | So that |
|---|---|
| `org.noahie.debloatmail.UI.Present()` | the tray's `Activate` can raise the window |
| `org.noahie.debloatmail.UI.NotificationPolicy` | the daemon knows whether to notify |
| `org.freedesktop.Application` | `DBusActivatable=true` and shell activation work |

**`UI.Compose` is gone.** The earlier interface — `(to, subject, body)` on the GUI's
bus name — has no legitimate caller once `mailto:` arrives through
`org.freedesktop.Application.Open` activation (`debloatmaild.md` §21), and it accepted
an arbitrary pre-filled message from any same-user process, which `SECURITY.md` §10.4
treats as a spoofing vector. Both reasons point the same way: it is deleted, and the
activation path is the mail door.

The daemon detects GUI presence with `NameHasOwner("org.noahie.debloatmail")`, so a
crashed GUI leaves no stale state (`debloatmaild.md` §13).

### 13.2 Close means quit

**Default: closing the window quits the GUI.** The daemon survives and keeps syncing,
and its tray icon is unaffected — so nothing is lost, and the user-visible difference
between "quit" and "hidden" is nearly nil, because the tray icon is there either way.

The tray's `Activate` calls `org.freedesktop.Application.Activate`, which D-Bus-activates
the GUI if it is not running and then calls `Present()`. One start path, no fork/exec
race (`debloatmaild.md` §14).

A preference offers *keep running in the background*, which hides instead of quitting.

### 13.3 The notification policy has a shutdown bug

`NotificationPolicy` is `all` | `focused` | `none`, and `focused` means "suppress while
my window has focus" — a decision requiring window-manager state the daemon does not
have, so the GUI owns it (`debloatmaild.md` §13).

But a tri-state property cannot be both the user's *preference* and the *effective*
value the daemon should honour right now, and the difference matters at shutdown:

> The GUI, while focused and set to `focused`, writes `none`. The user quits. **Nothing
> ever writes `all` again, and notifications are suppressed forever.**

The resolution: the **property carries the effective policy the daemon honours**
(`all` or `none`), and the **tri-state preference lives in the config file** (§15 —
the GSettings home for it is gone with GSettings). The GUI computes effective = `all`
unless (preference is `focused` and the window has focus) or (preference is `none`),
writes it on focus change, and **restores the non-focused value on shutdown**.

### 13.4 The bubble

The daemon sends notifications with a stable `replaces_id` per account so a burst
coalesces, actions `["default", "Open"]`, and a rate limit of one per account per 60
seconds (`debloatmaild.md` §14). A mailing list delivering 200 messages produces one
bubble saying "200 new messages."

**In-app toasts are for GUI-initiated events only** — message queued, copy complete,
error. New mail is the daemon's notification to make; duplicating it in-app
double-notifies and trains the user to ignore both.

**The deep-link gap:** `Present()` carries no context, so a notification click raises
the window without saying which account or item it was about. §19.1 proposes an
optional argument. Until then the GUI navigates to the account's inbox and selects the
newest unread item, which is usually what the user wanted and is honest about being a
guess.

---

## 14. Status, progress, and banners

**The daemon is the only source of state; the GUI never polls for it.** Everything in
this section is driven by signals — `Status.StateChanged`, `Sync.Progress` /
`Sync.Finished`, `Accounts.StateChanged`, `Feeds.Updated`, `Queue.Completed`,
`Items.Changed` — plus the bounded revision-protocol refresh on activation (§6.4). A
GUI that polls for status is both slower than the signal path and teaches future code
that polling is acceptable.

**Progress, per job.** `Sync.Progress` carries `job_id`, `phase`, and `percent`
— 0–100, or `0xFFFFFFFF` for indeterminate (`SYNC.md` §13.1). The GUI:

- Draws progress **per phase** (the percentage is scoped to the current phase; `40%
  storing` after `99% fetching` is not a regression),
- Distinguishes jobs by `job_id` — a scheduled `AccountSync` and an interactive
  `BodyFetch` on the same account are different bars in different places,
- Shows deterministic progress during a large initial sync (windows are counted,
  `SYNC.md` §5.5/§11.3) and indeterminate for `Check`-shaped work whose length is
  unknown,
- Keeps the reader's skeleton and the model's `loading` as the *local* progress
  surfaces — progress reporting is about the daemon's jobs, not about painting.

**Banners.** One banner at a time for account health, in priority order:

| Condition | Banner | Carries |
|---|---|---|
| An account errored on **auth** | "Fastmail needs new credentials" | **Fix credentials** → the §12.4 rotation flow. This is where issues 10 ends for the user: a rejected password ends at a working button, not a log line (`debloatmaild.md` §15). |
| An account errored on **transient** failure | "Proton is unreachable — retrying" | The backoff, humanised (`SYNC.md` §10.2): waiting, with a cap. No action offered — retrying is automatic, and offering it implies it would skip the backoff. |
| `Queue` has entries needing attention | "2 messages not sent" | Opens the queue surface below. `SYNC.md` §8.6: a failed op **must surface, not vanish silently** — the tray's `NeedsAttention` and the banner are the GUI's half of that. |
| `Status.State = 'degraded'` | "Working, but something needs attention" (`SYNC.md` §10.3) | The details list, no fixed copy |

A healthy background sync produces **no banner, no toast, no sound** — the model
updating is the acknowledgement. A banner is a state with a required response, not a
status display.

**The queue surface.** Lists the ops the drain is carrying — pending and failed, the
same set `Queue.Pending()` returns (`SYNC.md` §8.6) — with per-op `detail`. What it
offers is defined by the op, under the send's rules: a failed *send* gets **Reopen**
(never a blind resend, §11.3); everything else gets retry-or-drop as the API
provides (`debloatmaild.md` §10). A row here that no daemon method can act on is an
API gap to log (§19), not a button the GUI invents.

---

## 15. Preferences

| Preference | Values | Default |
|---|---|---|
| List density | comfortable / compact | comfortable (§7) |
| Mark-read behaviour | immediately / after N s dwell / manually | immediately (§8.5) |
| Notification policy | all / focused / none | all (§13.3) |
| Close behaviour | quit / keep running | quit (§13.2) |
| Reply attribution | with attribution line / without | with (§11.4) |

**Storage: one config file, no schema system.** This is the resolution of the GSettings
debt: the preference store is a file the app owns, in the application's configuration
location — the same file format the CLI reads for its settings (`debloatmailctl.md`
§4), not a GSettings schema (removed with GTK, per `GEMINI.md`), not a database table
(the GUI may not write the database, §2), and not `QtCore.Settings` (that is window
geometry only, §4). The GUI is therefore the **second writer to the config file**,
after the CLI — and the two must not own different files, or "one config file" becomes
two.

The file's format and exact path are `debloatmailctl.md` §4's, cited, not copied. The
GUI edits only preference keys; it does not read or write anything else in the file.
(If the config ever needs a key the CLI does not know, the CLI document gains it — the
GUI never invents schema unilaterally.)

Window size and position remain the one `QtCore.Settings` exception (§4).

---

## 16. Threading, performance, and budgets

**Threading.** The model, the delegate, and every SQLite read sit on the GUI thread —
the bounded-page argument of §3.3. `fetchMore` is synchronous by default; the worker-
thread escape hatch exists and is last-resort (§6.2). No other work is threaded from
QML: the dangerous parts (I/O, TLS, parsing) are all daemon-side by construction, and
the only in-GUI cost that could ever exceed a frame is a page read, which the budget
below bounds.

**Budgets.** These are targets, enforced by the §18 tests with a synthetic 50,000-item
database, not aspirations:

| Budget | Target |
|---|---|
| Cold start, daemon stopped | First page of the stream rendered **before any D-Bus readiness** — the header-cache test (§4) |
| Page read | Sub-10 ms for a 50-row keyset page over an indexed query (the CLI's parity target, `debloatmailctl.md` §2) at 50,000 items |
| List scroll | No dropped frames from model work scrolling a 50,000-row list; the model only ever holds a bounded window (§6.2's 50 rows + the buffered-new queue) |
| Main-thread work | Nothing the GUI schedules exceeds the frame budget; the one thing that could is the paged read, which is bounded |
| Reader body render | Bounded by the extraction cap (1 MB body, `MIME-AND-TEXT.md` §13) — a message the size of the cap must not jank the reader |
| WAL hygiene | No read transaction held across an event-loop turn (§3.3) — asserted, not assumed |

A budget that is not a test is an ornament; each row above has a §18 entry.

---

## 17. Accessibility and translation

**Screen readers and keyboard.** The delegate's state is part of its accessible name —
unread/starred are *in* the name, not just a dot and a glyph (§7) — so a screen reader
and a keyboard-only user get the same information a mouse user gets. The whole app is
operable without a mouse: arrows move selection, Enter opens, Space toggles read, and
the context actions are reachable from the keyboard in the context drawer. Colour is
never the only channel for any state.

**HiDPI and scaling.** Layout in Kirigami units, never fixed pixels (§3.4): the delegate
heights, drawer widths, and line counts scale, and a mixed-density display does not
produce a list that is too dense in one viewport and too sparse in another.

**Translation.** Every user-visible string — including the check-stage labels (§12.1),
banner copy (§14), and the "not yet verified" feed note (§12.3) — goes through the
localisation stack (Ki18n per the `GEMINI.md` toolkit list); nothing is a hard-coded
`qsTr`-less literal in QML, and nothing in C++ crosses the bridge carrying user-facing
text. The daemon's error `detail` strings are translated at the GUI boundary, not in
the daemon (the daemon is not QML's problem and its strings must not require Qt to
read).

**Dates and locale.** The model emits epoch seconds; **formatting is a UI concern**
(DM v3 §14), done by the delegate per locale — today shows as a time, old items as a
date, and the `0`-means-unset rule of §6.1 is applied *after* locale formatting is
decided, never before.

---

## 18. Testing

No test touches the network or a real mailbox (`architecture.md` §13): the fixture
database is synthetic, and daemon behaviour is supplied by a **fake daemon** — a test
service that owns `org.noahie.debloatmail.Daemon` on a private session bus and emits
the real signal vocabulary. That one substitute makes the entire optimistic-update
state machine testable in isolation.

| Layer | Approach |
|---|---|
| Model logic (Rust, `store`/model crate) | Temp-database tests: paging and the **total-order cursor** (fixed fixture: two items equal on the active key — the cursor must order them and page stably, §5.4); the active-key cursor under each sort option, including the `published_at` default; the incremental paths of §6.3 (insert-at-top buffered when scrolled, `dataChanged` scoped, reset only on spec change); `roleNames()` against the golden list (§6.1) |
| QML | Offscreen (`QT_QPA_PLATFORM=offscreen`) with Qt Quick Test; golden screenshots of the delegate in every state combination (unread×starred×attachment×loading) and of the banner set (§14) |
| Optimistic updates | Fake daemon: (a) commit-then-signal → overlay confirms and drops; (b) no signal → 10 s expiry re-reads and reverts (§9.4, deterministic clock); (c) `Queue.Completed(false)` → `Unsynced`, local value kept (§9.3 step 4); (d) connection error → overlay **never applied** + the start banner (§9.5) |
| Reader | Plain-text regressions: a fixture body full of markup renders as literal text (the `AutoText` ban, §8.3); a fixture that is `pending` with the fake daemon *absent* degrades to `none` + Fetch (§8.2); attachment metadata renders from header-time rows and **re-reads after a body fetch** to the refined list (§8.6) |
| Setup flows | `Feeds.Add` against a fake daemon returning each of the three outcomes — validated, `validated: false`, hard failure (no row) — asserting the table in §12.3; the auth-error banner offering **Fix credentials** and the rotation call carrying the secret once (§12.4) |
| Lifecycle | Close-to-quit leaves the fake daemon and its tray objects untouched (§13.2); `Application.Open` with a mailto URI activates the instance and pre-fills compose — **and no `UI.Compose` name exists on the bus to call** (§13.1); notification policy written on focus change and **restored to the non-focused value on shutdown** (§13.3) |
| Resources & isolation | The GUI test environment is **network-isolated** — any socket attempt by the GUI process fails the build (§1's non-goal, enforced); no fixture contains real mail or feed content (synthesised or anonymised, `architecture.md` §13) |

---

## 20. Verification checklist

Each item is a test or a CI assertion, not a review note. See §18 for the harness.

- [ ] No QML file imports from the `.bridge` namespace of any form; the named-object
      surface (§3 sample) is the complete import set.
- [ ] No GSettings anywhere: no dependency, no schema, no store (the config file of
      §15 is the only preference storage; window geometry is the single `QtCore.Settings`
      exception).
- [ ] Reply/forward subject fixture: source subject `re: q3 planning` (lower-case
      marker) → `Re: q3 planning`; no double marker on an already-prefixed subject; no
      lowercasing of any other
