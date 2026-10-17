# Architecture

This document explains **why** debloatmail is built the way it is. The rules in
`GEMINI.md` are the short form; this is the reasoning behind them, so that future
contributors don't have to reverse-engineer intent from the tree.

This revision fixes the issues flagged in the review: the invariants contradiction,
the `extract()` crate home, the ffi/Android framing, the CLI wording, the size claim,
the bus-name count, and the build order. §18 records where each landed.

---

## 1. What this is

A lightweight Linux desktop client that unifies **email** and **RSS/Atom** in a single
interface. Positioned as "Thunderbird minus the bloat": smaller program, smaller
feature set, simple navigation, mainstream provider integration. Thunderbird is bloat because it uses the Gecko engine under the hood and has way too many features. Debloatmail fixes this with a more efficient and memory-safe engine, less features, and a cleaner UI native for Linux users who use more supported desktop environments, and not dumber newer stuff like Wayland or whatever window manager is hot this month. 

### In scope

- Unified inbox across all accounts
- Threaded conversation view
- Plaintext reading **and** plaintext compose
- RSS/Atom reader as a first-class feature, not an add-on
- PDF attachment viewing
- Unified local search across mail and feeds
- Background sync (works with the window closed)
- Attachments; sending via SMTP

### Out of scope

| Excluded | Why |
|---|---|
| HTML rendering | Security surface, bloat, and we read in plaintext anyway |
| Calendar / contacts | Largest single source of Thunderbird's weight; not wanted |
| OpenPGP / S/MIME | Popular but not used by the author; large scope multiplier |
| Full-body local replica | The main reason Thunderbird is heavy on disk |
| Nix packaging | Deliberate scope cut |
| Windows / macOS | Linux-first; mobile is a separate concern (§10) |

OAuth (Gmail / Microsoft 365) is **architected from day one but implemented after**
the engine and UI skeleton are finished (§7).

---

## 2. The invariants

Everything else in this document is downstream of these. If a change violates one of
them, the change is wrong — not the invariant.

1. **`crates/core` has zero dependencies.** No `tokio`, no `rusqlite`, no Qt, no
   network, no filesystem. It is the domain model and nothing else. *(Unchanged.)*
2. **Only `ui/bridge` may reference Qt.** `cxx-qt` lives there and nowhere else — not
   in `ffi`, not in `core`, `store`, `engine`, `feeds`, `extract`, `auth`, or `sync`.
   *(The old invariant 2 also named `ffi`, while old invariant 3 forbade Qt in `ffi`
   — the two contradicted each other. Invariant 3 was right: the seam Android crosses
   must stay Qt-free, so 2 is corrected, not 3. `GEMINI.md` §5 carries the same
   error and is corrected when that document is processed.)*
3. **`crates/ffi` is the engine's outer boundary.** It exposes the `core` types
   through UniFFI-generated, Qt-free bindings, with a stable Rust trait defining
   the surface and the transport behind that trait (§10). This is the seam Android
   crosses.
4. **`crates/extract` owns hostile-content parsing and is runtime-free.** It is the
   only home for `mail-parser`, `charset`, `chardetng`, and `html2text`. Pure
   functions over byte slices: no sockets, no filesystem, no async runtime, no
   database. It depends on `core` (for the types) and nothing else — not `store`
   (store maps rows, it does not parse MIME) and not `sync` (the sync engine must
   not depend on extraction internals; the pipeline is `MIME-AND-TEXT.md` §5). A
   parser that eats attacker-controlled input gets one crate, not scattered modules.

These are cheap to maintain today and enormously expensive to retrofit. They should
be enforced by CI, not by discipline — a rule an agent or a contributor can violate
silently will eventually be violated. The CI checks are spelled out in §13.

---

## 3. Language: Rust

**Decision: Rust for the entire engine.**

The strongest argument is not performance. It is that this program parses
**attacker-controlled input** — MIME messages, feed XML, and PDF attachments. That is
precisely the domain where memory safety is not academic.

Secondary reasons: the pimalaya protocol stack is Rust-only (§5), and `cargo` gives
cross-compilation and reproducible builds that map cleanly onto distro packaging.

---

## 4. UI: Qt6 + KF6 Kirigami (QML)

**Decision: QML + Kirigami, bridged to Rust via `cxx-qt` on desktop.**

The reason is platform scope: GTK4 is the better *native Linux* choice, but it has no path to Android, and Kirigami officially targets Android alongside desktop. Once mobile reuse became a goal, Kirigami won.

KDE's own guidance matches our split: QML for layout and components, business logic in
another language. That is exactly the `ffi` boundary.

### Consequences

- **Build system changes to CMake + ECM**, driving Cargo. This is forced: Cargo lacks
  the features needed to install GUI desktop applications.
- **`cxx-qt` is desktop-only glue, and it lives only in `ui/bridge`** (invariant 2).
  `cxx-qt`'s CI covers Linux, Windows, macOS and WASM; Android is not a tested
  target (§10).
- **`ui/bridge` is deliberately disposable.** When Android arrives it gets its own
  binding path against the same `ffi` surface. Nothing in `ui/qml` should reference
  bridge types directly — go through QML property and method names so a second
  bridge can satisfy the same surface.

---

## 5. Protocol layer: sans-I/O

**Decision: pimalaya `io-imap`, `io-smtp`, `io-oauth` — not a tokio-based IMAP client.**

Pimalaya's libraries are **sans-I/O coroutine state machines**: they open no sockets,
impose no TLS, and choose no async runtime. The daemon owns all I/O and pumps the
coroutines.

This matters for a concrete reason: a conventional async IMAP library (tokio-based)
fights a Qt/GLib event loop, and you end up maintaining a bridge between two runtimes.
Sans-I/O sidesteps that entirely, and it makes the protocol logic testable against
in-memory buffers with no network.

It also gives us provider autodiscovery and modern SASL mechanisms (oauthbearer,
xoauth2, SCRAM-SHA-256) rather than hand-rolling them.

### Dependency rules

- **Use** `io-imap`, `io-smtp`, `io-oauth` (all stable).
- **Never use `io-email`** — the multi-backend aggregator. It looks like the obvious
  choice and is explicitly **frozen**, superseded by protocol-direct clients.
- **Never use `io-keyring`** — **deprecated**. Use a third-party crate (`keyring` or
  `oo7`) over the Secret Service portal instead.
- Pimalaya's statuses are hand-curated; when in doubt the crate README is
  authoritative. **Pin exact versions and re-verify before bumping.**

IMAP/SMTP sockets and TLS live only in `engine/io.rs` and `engine/tls.rs`. The one
other place a socket is opened is `crates/feeds`, which owns the project's **single
HTTP client: `reqwest` on `rustls`** (no default features, no native-TLS backend).
One HTTP stack for the whole project, an async client that sits in the daemon's
existing runtime, and a streaming body-size cap — feed content is attacker-controlled
and must never be buffered unbounded, which is also where the redirect-validation
hook from `SECURITY.md` §5.4 belongs. No `ureq`, no curl shim, no direct hyper.

---

## 6. Storage: a thin cache, not a replica

**Decision: SQLite via `rusqlite` (bundled) holding headers, flags, feed items, and
an FTS5 index. Bodies fetched on demand. Never a full local mirror.**

A feed has no server to sync read state against; `\Seen` exists
in IMAP but RSS has no equivalent. Feed read/starred state **must** be local. The same
applies once background sync and a unified unread count exist: a daemon cannot show a
correct count across N accounts without persistent state.

What we actually want, stated precisely:

> No full-body replica. A header-and-flags cache with bodies fetched on demand.

### Consequences

- **Search is local, not server-side.** Server-side `IMAP SEARCH` is slow,
  inconsistent across providers, and returns nothing for feeds. One fast search box
  across mail and feeds requires FTS5, which means caching bodies for recently-viewed
  items. This was chosen deliberately.
- **The header cache is what gives sub-second startup.** Without it, launch is blocked
  on N network round-trips.
- **`store/flags.rs` owns the mail/feed asymmetry.** Containing it there prevents it
  leaking into the UI.

---

## 7. Auth: password now, OAuth later

**Decision: beta ships with password authentication only. OAuth is architected from
day one and implemented after the engine and UI skeleton are complete.**

This is a deliberate sequencing win. The beta requires **no OAuth code at all** — no
Google/Microsoft app registration, no PKCE, no token refresh, no keyring complexity.

Day-one architecture work, so OAuth drops in without redesign:

- an `AuthKind` enum in `core` with OAuth variants,
- a token-store trait,
- provider profiles able to carry a loopback-redirect configuration.

`io-oauth` is stable, so this can be built against with confidence later. When the
Gmail/Graph phase arrives, expect **2–4 weeks plus permanent app-registration
maintenance**.

### Provider notes

| Provider | Beta path | Caveats |
|---|---|---|
| **Fastmail** | Standard IMAP + SMTP, app password | JMAP is its native protocol but IMAP works fine; JMAP deferred |
| **Proton** | IMAP/SMTP to a **local Bridge** on `127.0.0.1:1143` / `:1025` with STARTTLS | Requires Bridge installed and a **paid plan** |
| Gmail / M365 | Post-beta, via OAuth | Requires app registration and the loopback/PKCE flow |

**Proton is an IMAP account with a preset, not a provider integration.** Two specifics
must be designed for and must not be generalized:

- **The Bridge presents a self-signed localhost certificate.** The TLS layer needs a
  deliberate exception scoped to loopback only. **Do not build a general "ignore
  invalid certificates" switch** — make it a per-account flag permitted only on
  `127.0.0.1` / `::1`.
- **The password is Bridge-generated**, not the account password. This is a
  configuration-profile concept.

---

## 8. The unified content stream

**`core/item.rs` + `core/stream.rs` are the heart of the design.**

A single `Item` represents **either** a mail message **or** a feed article, and a
`ContentStream` presents a merged, ordered view of them. One list view, one reader
view, one unread model, one notification path, one search index — written once and
working for both sources.

This is where the "smaller than Thunderbird + a feed reader" claim is actually won.
A combined client is normally *bigger* than either alone; the unified abstraction is
what makes it smaller here.

**Guidance:** when adding a capability, add it to the stream, not to one source. If a
feature can only work for mail, that is a signal to reconsider the abstraction before
special-casing it.

---

## 9. Process model: daemon + UI + CLI

**Decision: three binaries — `debloatmail` (UI), `debloatmaild` (daemon),
`debloatmailctl` (CLI).**

| Process | Reads | Writes | Owns |
|---|---|---|---|
| `debloatmail` (GUI) | SQLite direct (WAL, read-only) | via D-Bus to the daemon | Windows |
| `debloatmaild` (daemon) | SQLite | **SQLite — sole writer** | All I/O (IMAP IDLE, feed polls, the sans-I/O pumps), scheduler, credentials, notifications, **the tray** (SNI + dbusmenu) |
| `debloatmailctl` (CLI) | SQLite direct (WAL, read-only) | via D-Bus to the daemon | Nothing |

The split exists because **a closed window cannot sync**. Background sync requires a
process that outlives the UI. Secondary benefits: the UI can be killed and restarted
without losing sync state, notifications originate from the daemon, and there is a
headless test target for free.

**Two D-Bus names, one per process.** The GUI owns `org.noahie.debloatmail` (required
by desktop activation, `DBusActivatable=true`) and the daemon owns
`org.noahie.debloatmail.Daemon`. That specification — and the security reasoning
behind the split — lives in `debloatmaild.md` §2; this section only pins the
architecture rule: one name per process, both derived from the app id.

**The daemon also owns the tray**, so the icon and its menu survive the window
closing. The contract (both `org.freedesktop.StatusNotifierItem` and the legacy
`org.kde` name) is `debloatmaild.md` §18.

**The CLI is the engine's first customer, not a reference implementation.** `debloatmailctl` is built **before any
QML exists**: in standalone mode it runs the same engine crates in-process (taking
the exclusive `flock`, `debloatmailctl.md` §10); otherwise it is a pure D-Bus client
plus a read-only database. Either way, by the time the first QML component is
written, every hard part — auth, sync, send, feed ingest, extraction, search — has
been exercised end-to-end. That is why the CLI doubles as the regression suite.

*Fallback:* if the daemon proves too much overhead for beta, the interim shape is a
single process that owns everything but still persists the cache and minimizes to a
tray icon. Design the storage and protocol layers as libraries either way, so the
split becomes a packaging change rather than a rewrite.

---

## 10. The ABI boundary and Android

`crates/ffi` is a Qt-free layer exposing the engine surface through
**UniFFI-generated bindings** — with one structural change from the earlier framing:
the surface is a **Rust trait**, and the **transport is not part of the
contract**.

| Transport | Where | What it does |
|---|---|---|
| D-Bus + read-only DB | Desktop | Mutations and job control go over D-Bus to `debloatmaild`; reads open the WAL database read-only (the read path of `debloatmaild.md` §1) |
| In-process | Android | There is no session bus on Android; the app process hosts the same engine logic and is the direct writer. One app process, no daemon |

The trait defines the surface: streams and item reads, search, mutations
(mark/star/move/delete/hide), sync and compose control, queue inspection, and change
callbacks. `ui/bridge` is a `cxx-qt` adapter over the trait, desktop-only (invariant
2). The Android app links the UniFFI-generated Kotlin side directly — no `cxx-qt`, no
Qt-C++.

```
        ┌─────────────────────────────┐
        │   QML + Kirigami  (shared)  │
        └──────────────┬──────────────┘
             ┌─────────┴──────────┐
        desktop bridge        Android bridge
        (cxx-qt over the     (UniFFI → Kotlin,
         trait)                links the trait directly)
             └─────────┬──────────┘
        ┌──────────────▼──────────────┐
        │  UniFFI-generated bindings  │  ← the seam
        │  stable Rust trait, Qt-free │
        ├─────────────────────────────┤
        │  transport (not contract):  │
        │   desktop  → D-Bus + DB     │
        │   Android  → in-process     │
        ├─────────────────────────────┤
        │   debloatmail-core (Rust)   │
        └─────────────────────────────┘
```

**Why the hedge exists:** `cxx-qt` has **no official Android support**. UniFFI is proven at scale (Mozilla uses it across Firefox mobile and desktop,
generating Kotlin for Android), and `cargo-ndk` handles the cross-compilation.

**The "stable C ABI" claim, corrected.** UniFFI generates stable *bindings*, not a
versioned C ABI with semantic-versioning semantics. The stability promise that
matters is the **Rust trait** — the engine surface — and the generated code is a
disposable artifact reproducible from `core` with `cargo uniffi`. This wording change
stops the document promising a versioning scheme the tooling does not provide.

**The hedge must be exercised, or it rots.** On desktop, production never touches the
in-process transport: the GUI reads the database directly and mutates via D-Bus.
That means divergence in the in-process path would first surface on Android, on
day one — unacceptable for a seam this was built to protect. The rule: the ffi test
suite runs against **both** transports in CI (D-Bus harness against a real daemon
binary under `dbus-run-session`; in-process harness against a scratch database).

**The rule that follows:** keep `core` Qt-free and `ffi` Qt-free. The boundary is only
worth having if it is actually clean.

---

## 11. PDF

PDF viewing is in scope for beta, via poppler/mupdf since Qt's PDF reader doesn't come shipped with most distros. This will also help when making the Android app later.

---

## 12. Build and packaging

**CMake + ECM drives Cargo.** Cargo compiles the crates; CMake owns install rules,
finds Qt6/KF6, and carries feature flags as cache variables (e.g.
`-DDEBLOATMAIL_ENABLE_OAUTH=OFF` for beta).

`Cargo.lock` **is committed** — distro packagers need reproducible resolution.

**Targets:** Debian/Ubuntu, Fedora, Arch, openSUSE, plus **Flatpak as the universal
fallback**.

The Flatpak fallback is not optional. A Rust/Qt app built against a current distro
stack will demand newer Qt/Kirigami than older LTS releases provide, so native
packages are progressive enhancements and Flatpak is the path that always works.
The portal caveats specific to Flatpak (notification portal, SNI host) are flagged in
`debloatmaild.md` §14 and §18 and must be verified against the final manifest before
beta.

`data/` is kept separate from `ui/qml/` because CMake installs them to different
destinations (`share/applications`, `share/metainfo`, `share/dbus-1` vs. the QML
module path).

**The App ID `org.noahie.debloatmail` must match in all of:** the desktop file,
**the two D-Bus activation `.service` files** — `org.noahie.debloatmail.service`
(activates the GUI) and `org.noahie.debloatmail.Daemon.service` (activates the
daemon; `debloatmaild.md` §21) — the
Flatpak app-id, and the CMake QML module lookup. (The earlier revision listed one
D-Bus name; there are two, one per process — §9.) A mismatch produces runtime
failures that look like missing files.

**Binary size is a measurement, not a claim.** No size number appears in any project
document until the stripped release binary is measured; beta publishes the measured
value (see §6).

---

## 13. Testing

- Unit tests beside the code; integration tests in `tests/`.
- **`core` must be testable with no network and no filesystem.** `extract` must be
  testable the same way: byte slices in, structures out.
- Fixtures in `tests/fixtures/` — malformed MIME, broken feeds, odd charsets. Prefer
  real-world samples over synthetic ones.
- IMAP tests run against a local GreenMail/Dovecot in `tests/imap/`. **Never hit live
  providers in tests.** Feed tests use recorded HTTP responses (conditional-GET
  200/304/404/500, malformed XML, HTML served as a feed).
- Never commit credentials, tokens, or real email/feed data into fixtures.

**CI should enforce the invariants, not just the tests:** assert that
`crates/core/Cargo.toml` declares no dependencies; that `cxx-qt` appears in no crate
outside `ui/bridge`; that `ffi` has no Qt dependency; that `extract` has no runtime
or I/O dependency; and that the ffi trait suite (§10) passes against **both**
transports.

---

## 14. Effort

**Build order.**:

1. `core` + `store` — the model, the schema, migrations.
2. Engine — `engine` (IMAP/SMTP over pimalaya), `feeds` (the HTTP client), `extract`
   (the MIME/text pipeline).
3. **Week-one validation spike — the CLI vertical slice in standalone mode.** This is
   a **gate, not a phase**: it proves the two riskiest reads — feed ingest and FTS5
   search — and, with them, the entire engine (auth, sync, send queue, offline
   operation) end-to-end, before committing to a GUI architecture. It is a
   *validation* spike: its job is to fail early, not to build the product.
4. `debloatmaild` — the D-Bus interface, scheduler, tray, notifications.
5. QML GUI — `ui/bridge` + `ui/qml`.
6. Polish — threading view, search UX.
7. Packaging.

**No QML before the CLI works.** (The earlier ordering listed feeds and search
*after* the GUI; the spike proves both before any QML exists, so both move into
step 3.)
