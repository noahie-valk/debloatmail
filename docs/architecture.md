# Architecture

This document explains **why** debloatmail is built the way it is. The rules in
`GEMINI.md` are the short form; this is the reasoning behind them, so that future
contributors (and future you) don't have to reverse-engineer intent from the tree.

---

## 1. What this is

A lightweight Linux desktop client that unifies **email** and **RSS/Atom** in a single
interface. Positioned as "Thunderbird minus the bloat": smaller program, smaller
feature set, simple navigation, mainstream provider integration.

The design goal is not "fewer features than Thunderbird" — it is **less code and
less runtime for the same core job**. Every decision below serves that.

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
| Windows / macOS | Linux-first; mobile is a separate concern (§9) |

OAuth (Gmail / Microsoft 365) is **architected from day one but implemented after**
the engine and UI skeleton are finished (§7).

---

## 2. The three invariants

Everything else in this document is downstream of these. If a change violates one of
them, the change is wrong — not the invariant.

1. **`crates/core` has zero dependencies.** No `tokio`, no `rusqlite`, no Qt, no
   network, no filesystem. It is the domain model and nothing else.
2. **Only `crates/ffi` and `ui/bridge` may reference Qt.** No `cxx-qt` anywhere in
   `core`, `store`, `engine`, `feeds`, `auth`, or `sync`.
3. **`crates/ffi` is the engine's outer boundary.** A stable C ABI, Qt-free. This is
   the seam Android crosses (§9).

These are cheap to maintain today and enormously expensive to retrofit. They should
be enforced by CI, not by discipline — a rule an agent or a contributor can violate
silently will eventually be violated.

---

## 3. Language: Rust

**Decision: Rust for the entire engine.**

The strongest argument is not performance. It is that this program parses
**attacker-controlled input** — MIME messages, feed XML, and PDF attachments. That is
precisely the domain where memory safety is not academic.

Secondary reasons: the pimalaya protocol stack is Rust-only (§5), and `cargo` gives
cross-compilation and reproducible builds that map cleanly onto distro packaging.

### Alternatives considered

| Option | Verdict | Reasoning |
|---|---|---|
| **C** | Rejected | Portability is a property of the **ABI boundary**, not the language — Rust exports a C ABI without being written in C. C would forfeit memory safety in a parser-facing program, forfeit the pimalaya stack, and forfeit cargo/UniFFI tooling. It is also *dominated by C++* here: Qt's native language is C++, so a C engine behind a Qt UI still needs C++ glue. |
| **C++** | Viable, not chosen | Genuinely attractive: one language, official Qt Android support, no FFI. But it gives up Rust's safety and the pimalaya ecosystem. This is the runner-up if the Rust↔Qt bridge ever proves unworkable. |
| **Vala** | Rejected | Geary proves the stack works (Vala/GTK email client, SQLite local copy, in-tree async IMAP). But Vala's own maintainer has described the project as on "life-support" due to GNOME's push toward Rust, with contributions unreviewed for months, and VAPI binding files have no lockfile or versioning — libraries can "break horribly" on upgrade. Too much long-term risk for a project meant to live for years. |
| **Python / Go / Flutter** | Rejected | Python is too heavy for the stated goal; Go's email/MIME tooling lags Rust's; Flutter contradicts "lightweight" outright. |

---

## 4. UI: Qt6 + KF6 Kirigami (QML)

**Decision: QML + Kirigami, bridged to Rust via `cxx-qt` on desktop.**

This reversed an earlier GTK4/libadwaita decision. The reason is platform scope: GTK4
is the better *native Linux* choice, but it has no path to Android, and Kirigami
officially targets Android alongside desktop. Once mobile reuse became a goal,
Kirigami won.

KDE's own guidance matches our split: QML for layout and components, business logic in
another language. That is exactly the `ffi` boundary.

### Consequences

- **Build system changes to CMake + ECM**, driving Cargo. This is forced: Cargo lacks
  the features needed to install GUI desktop applications.
- **`cxx-qt` is desktop-only glue.** Its CI covers Linux, Windows, macOS and WASM;
  Android is not a tested target (§11).
- **`ui/bridge` is deliberately disposable.** When Android arrives it gets its own
  binding path against the same `ffi` ABI. Nothing in `ui/qml` should reference bridge
  types directly — go through QML property and method names so a second bridge can
  satisfy the same surface.

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

Sockets and TLS live only in `engine/io.rs` and `engine/tls.rs`.

---

## 6. Storage: a thin cache, not a replica

**Decision: SQLite via `rusqlite` (bundled) holding headers, flags, feed items, and
an FTS5 index. Bodies fetched on demand. Never a full local mirror.**

This is the single biggest structural difference from Thunderbird, and it needs
justifying because "thin client, no local storage" was the original instinct — and it
is **not achievable**. A feed has no server to sync read state against; `\Seen` exists
in IMAP but RSS has no equivalent. Feed read/starred state **must** be local. The same
applies once background sync and a unified unread count exist: a daemon cannot show a
correct count across N accounts without persistent state.

What we actually want, stated precisely:

> No full-body replica. A header-and-flags cache with bodies fetched on demand.

This is what delivers fast cold start, correct unified unread counts, and low RAM —
and it keeps the database at a few MB rather than gigabytes.

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

```
┌───────────────────────────┐   D-Bus   ┌──────────────────────────────┐
│  debloatmail (QML/Kirigami)│◄────────►│  debloatmaild                │
│  closable, no sync of its  │  signals │  owns I/O: IMAP IDLE, feeds  │
│  own                       │          │  pumps sans-I/O coroutines   │
└───────────────────────────┘          │  notifications, scheduler    │
                                       └──────────────┬───────────────┘
┌───────────────────────────┐                          ▼
│  debloatmailctl (CLI)     │                  SQLite: headers, flags,
│  reference implementation │                  feed items, FTS5
└───────────────────────────┘
```

The split exists because **a closed window cannot sync**. Background sync requires a
process that outlives the UI. Secondary benefits: the UI can be killed and restarted
without losing sync state, notifications originate from the daemon, and there is a
headless test target for free.

**The daemon is the reference implementation.** `debloatmailctl` is built **before any
QML exists** — a headless binary that authenticates, syncs, sends, fetches a feed, and
prints a unified list exercises every hard part of the system. It is also the
regression suite.

*Fallback:* if the daemon proves too much overhead for beta, the interim shape is a
single process that owns everything but still persists the cache and minimizes to a
tray icon. Design the storage and protocol layers as libraries either way, so the
split becomes a packaging change rather than a rewrite.

---

## 10. The ABI boundary and Android

`crates/ffi` exposes a **stable C ABI via UniFFI**, Qt-free. `ui/bridge` is a `cxx-qt`
adapter over it, desktop only.

```
        ┌─────────────────────────────┐
        │   QML + Kirigami  (shared)  │
        └──────────────┬──────────────┘
             ┌─────────┴──────────┐
        desktop bridge        Android bridge
        (cxx-qt)              (UniFFI→Kotlin, or Qt-C++ via C ABI)
             └─────────┬──────────┘
        ┌──────────────▼──────────────┐
        │   ★ STABLE C ABI / UniFFI ★ │  ← the hedge
        ├─────────────────────────────┤
        │   debloatmail-core (Rust)   │
        └─────────────────────────────┘
```

**Why the hedge exists:** `cxx-qt` has **no official Android support**. The only
evidence is an experiment thread in which a developer got a cxx-qt app running on
Android, reported that none of the issues were caused by cxx-qt itself, and estimated
"a day or two worth of work" — but maintainers made no roadmap commitment. Betting the
project's mobile future on that is not acceptable.

With the ABI boundary, Android can bind via UniFFI→Kotlin without `cxx-qt`. UniFFI is
proven at scale (Mozilla uses it across Firefox mobile and desktop, generating Kotlin
for Android), and `cargo-ndk` handles the cross-compilation. If cxx-qt's Android story
ever solidifies, adopting it becomes an optimization rather than a prerequisite.

**The rule that follows:** keep `core` Qt-free and `ffi` Qt-free. The boundary is only
worth having if it is actually clean.

---

## 11. PDF

PDF viewing is in scope for beta, via **QtPdf** (`qt6-pdf`), which offers
`PdfMultiPageView` with text selection, zoom, links, and search. Licensing is LGPLv3 —
compatible with this project's GPL-3.0.

Two known issues:

- **QtPdf is no longer part of standard Qt.** Since Qt 6.8.0 LTS it lives in
  "extensions," and distros package it separately. **Declare an explicit `qt6-pdf`
  dependency in every packaging target** — do not assume it ships with Qt.
- **Android availability is unverified.** Desktop is solid; mobile is an open spike.
  If QtPdf turns out to be unavailable there, the fallback is "PDF is desktop-only,"
  or a poppler/mupdf backend behind the same interface.

Also note: the PDFium snapshot bundled with QtPdf has historically lagged upstream
security fixes. Since this program parses attacker-controlled PDFs, monitor this, or
plan to swap in system poppler/mupdf behind the same interface.

---

## 12. Build and packaging

**CMake + ECM drives Cargo.** Cargo compiles the crates; CMake owns install rules,
finds Qt6/KF6, and carries feature flags as cache variables (e.g.
`-DDEBLOATMAIL_ENABLE_OAUTH=OFF` for beta).

`Cargo.lock` **is committed** — distro packagers need reproducible resolution.

**Targets:** Debian/Ubuntu, Fedora, Arch, openSUSE, plus **Flatpak as the universal
fallback**. Nix is explicitly out of scope.

The Flatpak fallback is not optional. A Rust/Qt app built against a current distro
stack will demand newer Qt/Kirigami than older LTS releases provide, so native
packages are progressive enhancements and Flatpak is the path that always works.

`data/` is kept separate from `ui/qml/` because CMake installs them to different
destinations (`share/applications`, `share/metainfo`, `share/dbus-1` vs. the QML
module path).

**The App ID `org.noahie.debloatmail` must match in all of:** the desktop file, D-Bus
service name, gschema path (`/org/noahie/debloatmail/`), Flatpak app-id, and the CMake
QML module lookup. A mismatch produces runtime failures that look like missing files.

---

## 13. Testing

- Unit tests beside the code; integration tests in `tests/`.
- **`core` must be testable with no network and no filesystem.**
- Fixtures in `tests/fixtures/` — malformed MIME, broken feeds, odd charsets. Prefer
  real-world samples over synthetic ones.
- IMAP tests run against a local GreenMail/Dovecot in `tests/imap/`. **Never hit live
  providers in tests.**
- Never commit credentials, tokens, or real email/feed data into fixtures.

**CI should enforce the invariants, not just the tests:** assert that
`crates/core/Cargo.toml` declares no dependencies, and that `cxx-qt` appears in no
crate outside `ffi` and `ui/bridge`.

---

## 14. Known risks

| Risk | Severity | Mitigation |
|---|---|---|
| `cxx-qt` Android is unofficial and untested | High | The `ffi` C ABI / UniFFI boundary (§10) |
| QtPdf on Android unverified | Medium | Desktop-only fallback, or poppler/mupdf backend |
| Pimalaya is grant-funded (NGI Zero, 2026→27), not commercially sustained | Medium | Pin versions; the sans-I/O design keeps crates replaceable behind `engine` |
| PDFium snapshot lags security fixes | Medium | Monitor; interface-compatible backend swap |
| `io-email` / `io-keyring` look attractive but are dead | Low | Documented as explicit "never" rules in `GEMINI.md` |
| Agent-generated Qt threading code (GTK/Qt objects aren't `Send`/`Sync`) | Medium | Treat protocol and threading code as hand-written |

---

## 15. Effort

Roughly **15–23 full-time weeks solo** (~4–7 months at 20 hrs/week). The dominant
phases are the CLI vertical slice (2–3 weeks) and the QML GUI (2–4 weeks).

AI assistance realistically saves **25–40%** — most on scaffolding, packaging
boilerplate, D-Bus XML, AppStream metadata, and serde types; least on protocol
correctness, MIME edge cases, threading, and anything crossing the Rust↔Qt boundary.

**Build order:** `core` model → `debloatmailctl` vertical slice → feeds → daemon →
QML UI → threading → search → packaging. **No QML before the CLI works.**

---

## 16. Decision log

| Decision | Chosen | Rejected | Decisive reason |
|---|---|---|---|
| Engine language | Rust | C, C++, Vala | Memory safety while parsing hostile input; pimalaya is Rust-only |
| UI toolkit | Qt6/KF6 Kirigami | GTK4/libadwaita | Shared QML with Android |
| Rust↔Qt bridge | `cxx-qt` (desktop) + UniFFI (mobile) | Single bridge | cxx-qt has no official Android support |
| Protocol client | pimalaya sans-I/O | `imap` / `async-imap` (tokio) | No runtime imposed; no event-loop bridging |
| Storage | Thin header/flag cache + FTS5 | Full local replica; no storage | RSS read-state and unified unread counts require persistence |
| Search | Local FTS5 | Server-side IMAP SEARCH | One fast unified search box across mail and feeds |
| Auth for beta | Password only | OAuth from day one | Ships without app registration; OAuth architected, not implemented |
| Process model | Daemon + UI + CLI | Single process | Background sync requires a process that outlives the window |
| Mail rendering | Plaintext only | HTML, WebView | Security surface and bloat |
| PDF | QtPdf | WebView/pdf.js, poppler | Native, LGPLv3, no WebView |
| Packaging | 4 distros + Flatpak | Nix, AppImage-only | Distro-native with a universal fallback; Nix deliberately cut |
| License | GPL-3.0-or-later | AGPL | Maximum distro and contributor friendliness |

---

## 17. Prior art

Worth reading before designing further:

- **Flectar Mail** — Rust, ~20 MB RAM, no WebView, Gmail/Outlook support. **No RSS.**
  Its scope is nearly identical minus the feed reader, and it validates that Rust +
  a native renderer hits the RAM target. AGPL-3.0-only, so it cannot be relicensed
  into a GPL-3.0 fork. Read it for architecture, not as a shortcut.
- **Geary** — Vala/GTK email client. Useful reference for conversation view and a
  SQLite local copy, even though we did not follow its stack.
