# debloatmail

A lightweight Linux desktop email + RSS/Atom client written in Rust with a Qt6/Kirigami UI. A less-bloated, less-featured alternative to Thunderbird.

## Stack
- Rust (stable, pinned in `rust-toolchain.toml`); Qt6/KF6 Kirigami (QML) + KirigamiAddons + Ki18n, bridged with cxx-qt on desktop; UniFFI-exposed engine trait for Android (later)
- Protocols: pimalaya `io-imap`, `io-smtp`, `io-oauth` (sans-I/O)
- Storage: rusqlite (bundled) + FTS5. Credentials: `keyring`/`oo7`
- HTTP (feeds only): `reqwest`, no default features, `rustls-tls` only. Owned by `crates/feeds`
- Processes: `debloatmail` (GUI), `debloatmaild` (daemon), `debloatmailctl` (CLI)
  - The daemon is the **sole SQLite writer** and owns all I/O, scheduling, notifications, and the tray. GUI and CLI open the DB read-only (WAL).
  - Daemon D-Bus names: `org.noahie.debloatmail` (GUI, activation) and `org.noahie.debloatmail.Daemon` (engine)
- Build: CMake + ECM driving Cargo. Target: Linux.

## Docs: read the relevant one before starting a task
Do not read all of `docs/`. Read only the doc and section for the task at hand.

| Topic | Doc |
|---|---|
| Why things are built this way, invariants | `docs/architecture.md` |
| Schema, identity, `Item`, flags, eviction | `docs/DATA-MODEL.md` (**wins** on conflicts for these) |
| D-Bus interfaces and signatures | `docs/debloatmaild.md` |
| Sync engine, send queue | `docs/SYNC.md` |
| MIME parsing, text extraction | `docs/MIME-AND-TEXT.md` |
| CLI commands and output | `docs/debloatmailctl.md` |
| Threat model, security rules | `docs/SECURITY.md` |
| UI | `docs/UI.md` |
| Terms | `docs/GLOSSARY.md` |

If code and a doc disagree, or two docs disagree, stop and ask. Do not pick silently.

## Commands
- Configure: `cmake -B build -G Ninja -DCMAKE_BUILD_TYPE=Debug`
- Build all: `cmake --build build`
- Rust only: `cargo build --workspace`
- Test all: `cargo test --workspace`
- Single test: `cargo test -p debloatmail-core <test_name>`
- Lint: `cargo clippy --workspace --all-targets -- -D warnings`
- Format: `cargo fmt --all`
- Run CLI: `cargo run -p debloatmail-cli -- --help`

## Conventions
- `crates/core` has ZERO dependencies: no tokio, rusqlite, Qt, network, or filesystem.
- Only `ui/bridge` may reference Qt. Never import cxx-qt in `ffi`, `core`, `store`, `engine`, `feeds`, `extract`, `auth`, or `sync`.
- `crates/ffi` is the engine's outer boundary: a stable Rust trait exposed via UniFFI bindings, Qt-free. The transport (D-Bus on desktop, in-process on Android) sits behind the trait and is not part of the contract.
- `crates/extract` owns all hostile-content parsing and is runtime-free. It is the only home for `mail-parser`, `charset`, `chardetng`, and `html2text`. Byte slices in, structures out: no sockets, filesystem, async runtime, or database. It depends on `core` only.
- Sockets and TLS: IMAP/SMTP only in `engine/io.rs` and `engine/tls.rs`; HTTP/TLS only in `crates/feeds`. Protocol logic is sans-I/O.
- Plaintext-first: no HTML rendering, plaintext compose. No calendar or contacts, ever.
- `core/item.rs` and `core/stream.rs` are the unified model: mail and feeds are both `Item`s. Add capability to the stream, not to one source.
- Storage is a thin cache (headers, flags, feed items, FTS5). Never a full-body replica.
- Crates are named `debloatmail-*`. App ID `org.noahie.debloatmail` must match in exactly these five places: the desktop file; `org.noahie.debloatmail.service` (activates the GUI); `org.noahie.debloatmail.Daemon.service` (activates the daemon); the Flatpak app-id; the CMake QML module lookup.
- Settings live in exactly four places: `config.toml` (app prefs), SQLite (accounts, per-account sync state), the keyring (secrets), QSettings (GUI-local view state). No GSettings.
- Commit `Cargo.lock`. Pin exact versions of pimalaya crates; re-read each README before bumping.
- Errors: `thiserror` in libraries, `anyhow` only in the `cli` and `daemon` binaries.
- No `unwrap()`/`expect()` outside tests. No `unsafe` unless asked.
- For pimalaya, cxx-qt, UniFFI, and zbus: read the pinned crate's source or docs. Do not guess APIs from memory.

## Testing
- Unit tests beside the code; integration tests in `tests/`.
- Fixtures in `tests/fixtures/` (malformed MIME, broken feeds, odd charsets). Prefer real-world samples over synthetic ones.
- IMAP tests run against local GreenMail/Dovecot in `tests/imap/`. Feed tests use recorded HTTP responses. Never hit live providers.
- `core` and `extract` must be testable with no network at all.
- CI asserts the invariants: `core` has zero deps; no Qt outside `ui/bridge`; `ffi` is Qt-free; `extract` has no runtime or I/O deps; the `ffi` suite passes on both transports (D-Bus and in-process).

## Boundaries
- Always: before finishing, run `cargo fmt --all` and `cargo clippy --workspace --all-targets -- -D warnings`, and run `cargo test` for the crates you touched.
- Always: build `crates/cli` first. It is the engine's first customer, and every hard path (auth, sync, send, feed ingest, extraction, search) must work end-to-end through it before any QML exists.
- Ask first: adding any dependency to any crate, and especially to `crates/core` or `crates/extract`.
- Ask first: changing the SQLite schema or migrations (authoritative DDL: `DATA-MODEL.md` §5).
- Ask first: adding a Qt type outside `ui/bridge`.
- Ask first: implementing OAuth (deferred until engine + UI skeleton are done).
- Never: use `io-email` (frozen) or `io-keyring` (deprecated).
- Never: open HTTP or TLS outside `crates/feeds` (no `ureq`, direct `hyper`, or curl shims).
- Never: add a WebView, HTML renderer, or calendar/contacts code.
- Never: commit credentials, tokens, or real email/feed data into `tests/fixtures/`.
