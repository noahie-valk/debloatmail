# Project: debloatmail: a lightweight Linux desktop email + RSS/Atom client written in Rust with a Qt6/Kirigami UI, built as a less-bloated and less-featured alternative to Thunderbird.

## Stack
Rust (stable, pinned in `rust-toolchain.toml`) + Qt6/KF6 Kirigami (QML) + `KirigamiAddons` + `Ki18n`, bridged with cxx-qt on desktop; a UniFFI-exposed engine trait for Android
Protocols: pimalaya `io-imap`, `io-smtp`, `io-oauth` (sans-I/O coroutines)
Storage: rusqlite (bundled) + FTS5 · Credentials: `keyring`/`oo7` (Secret Service portal)
HTTP (feeds only): `reqwest`, no default features, `rustls-tls` only — the project's single HTTP client, owned by `crates/feeds`
Processes: `debloatmail` (GUI), `debloatmaild` (daemon), `debloatmailctl` (CLI). The daemon is the **sole SQLite writer** and owns all I/O, scheduling, notifications, and the tray (SNI + dbusmenu); GUI and CLI open the database read-only (WAL)
Daemon: zbus, **two names** — `org.noahie.debloatmail` (GUI, activation) and `org.noahie.debloatmail.Daemon` (engine) · Build: CMake + ECM driving Cargo · Target: Linux (Android later, via `crates/ffi`)

## Commands
- Deps: install Qt6, KF6 Kirigami (+ KirigamiAddons, Ki18n), ECM, and poppler/mupdf
- Configure: `cmake -B build -G Ninja -DCMAKE_BUILD_TYPE=Debug`
- Build all: `cmake --build build`
- Rust only: `cargo build --workspace`
- Test all: `cargo test --workspace`
- Single test: `cargo test -p debloatmail-core <test_name>`
- Lint: `cargo clippy --workspace --all-targets -- -D warnings`
- Format: `cargo fmt --all`
- Run CLI: `cargo run -p debloatmail-cli -- --help`

## Conventions
- `crates/core` has ZERO dependencies — no tokio, no rusqlite, no Qt, no network, no filesystem. If it needs I/O, it's in the wrong crate.
- Only `ui/bridge` may reference Qt. Never import cxx-qt in `ffi`, `core`, `store`, `engine`, `feeds`, `extract`, `auth`, or `sync`. (The old rule also named `ffi`; that contradicted the next rule, and the Qt-free seam rule was right — the boundary Android crosses must not carry Qt.)
- `crates/ffi` is the engine's outer boundary: a stable Rust trait exposed through UniFFI-generated bindings (Kotlin/Swift/Python), Qt-free. The transport — D-Bus on desktop, in-process on Android — sits behind the trait and is **not part of the contract**. This is not a hand-versioned C ABI; UniFFI generates bindings, it does not version anything.
- `crates/extract` owns hostile-content parsing and is runtime-free: the only home for `mail-parser`, `charset`, `chardetng`, and `html2text`. Pure byte-slices-in, structures-out; no sockets, no filesystem, no async runtime, no database. It depends on `core` **only** (for the types) — not `store` (which maps rows) and not `sync` (whose pipeline is `MIME-AND-TEXT.md` §5.1's walk, not the reverse). A parser that eats attacker input gets one crate, not scattered modules.
- Sockets and TLS: IMAP/SMTP only in `engine/io.rs` and `engine/tls.rs`; HTTP/TLS only in `crates/feeds` (`reqwest`/`rustls`). Protocol logic is sans-I/O.
- Plaintext-first: no HTML rendering, plaintext compose. No calendar or contacts, now or later.
- `core/item.rs` and `core/stream.rs` are the unified model — mail and feeds are both `Item`s. Add capability to the stream, not to one source.
- Storage is a thin cache (headers, flags, feed items, FTS5). Never a full-body replica.
- Crates are named `debloatmail-*`. App ID is `org.noahie.debloatmail` and must match in exactly these five places — **this list is the single source of truth**; other docs point here rather than keep copies: the desktop file; `org.noahie.debloatmail.service` (activates the GUI); `org.noahie.debloatmail.Daemon.service` (activates the daemon); the Flatpak app-id; the CMake QML module lookup. A mismatch produces runtime failures that look like missing files.
- Settings live in exactly four homes and nowhere else: `config.toml` (app preferences), SQLite (accounts and per-account sync state), the keyring (secrets), QSettings (GUI-local view state). No GSettings — GTK-era leftover, incompatible with the Android goal, and it collides with Qt's own `QSettings`.
- Commit `Cargo.lock`. Pin exact versions of pimalaya crates and re-check each README before bumping.
- Errors: `thiserror` in libraries, `anyhow` only in the `cli` and `daemon` binaries.

## Testing
- Unit tests beside the code; integration tests in `tests/`.
- Fixtures in `tests/fixtures/` — malformed MIME, broken feeds, odd charsets. Prefer real-world samples over synthetic ones.
- IMAP tests run against a local GreenMail/Dovecot in `tests/imap/`. Feed tests use recorded HTTP responses. Never hit live providers in tests.
- `core` and `extract` must be testable with no network at all (`extract`: byte slices in, structures out, no filesystem).
- CI asserts the invariants, not just the tests: `crates/core` declares zero dependencies; no Qt dependency outside `ui/bridge`; `ffi` is Qt-free; `extract` has no runtime or I/O dependency; and the `ffi` binding suite passes against **both** transports — D-Bus under `dbus-run-session` against a real daemon binary, and in-process against a scratch database. Desktop production code never touches the in-process path, so an unexercised seam rots; the dual-transport run is what keeps the Android hedge honest.

## Boundaries
- Always: run `cargo fmt` and `cargo clippy -- -D warnings` before finishing
- Always: build `crates/cli` first — it is the engine's **first customer**: every hard path (auth, sync, send, feed ingest, extraction, search) runs end-to-end before any QML exists. The daemon owns the protocol; the CLI demonstrates it.
- Ask first: adding any dependency to `crates/core` or `crates/extract`; changing the SQLite schema or migrations — the authoritative DDL is `DATA-MODEL.md` §5, which has absorbed the SYNC/MIME asks (`highestmodseq`, `consecutive_failures`, `role_source`, `extractor_version`, `section_id`, plus `hidden_at`, `pending_ops.public_id`, the `meta` revision counter, and `accounts.name UNIQUE`); future schema changes remain ask-first; adding a Qt type outside `ui/bridge`
- Ask first: implementing OAuth (the architecture is planned; implementation is deferred until engine + UI skeleton are done)
- Never: use `io-email` (frozen) or `io-keyring` (deprecated)
- Never: open HTTP or TLS outside `crates/feeds` — no `ureq`, no direct `hyper`, no curl shim
- Never: add a WebView, an HTML renderer, or calendar/contacts code
- Never: commit credentials, tokens, or real email/feed data into `tests/fixtures/`
