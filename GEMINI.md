# Project: debloatmail — a lightweight Linux desktop email + RSS/Atom client written in Rust with a Qt6/Kirigami UI, built as a deliberately less-bloated alternative to Thunderbird.

## Stack
Rust (stable, pinned in `rust-toolchain.toml`) + Qt6/KF6 Kirigami (QML), bridged with cxx-qt
Protocols: pimalaya `io-imap`, `io-smtp`, `io-oauth` (sans-I/O coroutines)
Storage: rusqlite (bundled) + FTS5 · Credentials: `keyring`/`oo7` (Secret Service portal)
Daemon: zbus over D-Bus · Build: CMake + ECM driving Cargo · Target: Linux (Android later, via `crates/ffi`)

## Commands
- Deps: install Qt6, KF6 Kirigami, ECM, and QtPdf dev packages (see README)
- Configure: `cmake -B build -G Ninja -DCMAKE_BUILD_TYPE=Debug`
- Build all: `cmake --build build`
- Rust only: `cargo build --workspace`
- Test all: `cargo test --workspace`
- Single test: `cargo test -p debloatmail-core <test_name>`
- Lint: `cargo clippy --workspace --all-targets -- -D warnings`
- Format: `cargo fmt --all`
- Run CLI: `cargo run -p debloatmail-cli -- --help`

## Conventions
- `crates/core` has ZERO dependencies — no tokio, no rusqlite, no Qt. If it needs I/O, it's in the wrong crate.
- Only `crates/ffi` and `ui/bridge` may reference Qt. Never import cxx-qt in `core`, `store`, `engine`, `feeds`, `auth`, or `sync`.
- `crates/ffi` is the engine's outer boundary (stable C ABI via UniFFI) and the Android seam. Keep it Qt-free.
- Sockets and TLS live only in `engine/io.rs` and `engine/tls.rs`. Protocol logic is sans-I/O.
- Plaintext-first: no HTML rendering, plaintext compose. No calendar or contacts, now or later.
- `core/item.rs` and `core/stream.rs` are the unified model — mail and feeds are both `Item`s. Add capability to the stream, not to one source.
- Storage is a thin cache (headers, flags, feed items, FTS5). Never a full-body replica.
- Crates are named `debloatmail-*`. App ID is `org.noahie.debloatmail` everywhere: desktop file, D-Bus, gschema path `/org/noahie/debloatmail/`, Flatpak app-id.
- Commit `Cargo.lock`. Pin exact versions of pimalaya crates and re-check each README before bumping.
- Errors: `thiserror` in libraries, `anyhow` only in the `cli` and `daemon` binaries.

## Testing
- Unit tests beside the code; integration tests in `tests/`.
- Fixtures in `tests/fixtures/` — malformed MIME, broken feeds, odd charsets. Prefer real-world samples over synthetic ones.
- IMAP tests run against a local GreenMail/Dovecot in `tests/imap/`. Never hit live providers in tests.
- `core` must be testable with no network at all.

## Boundaries
- ✅ Always: run `cargo fmt` and `cargo clippy -- -D warnings` before finishing
- ✅ Always: build `crates/cli` first — it is the reference implementation for the engine
- ⚠️ Ask first: adding any dependency to `crates/core`; changing the SQLite schema or migrations; adding a Qt type outside `ffi`/`ui/bridge`
- ⚠️ Ask first: implementing OAuth (the architecture is planned; implementation is deferred until engine + UI skeleton are done)
- 🚫 Never: use `io-email` (frozen) or `io-keyring` (deprecated)
- 🚫 Never: add a WebView, an HTML renderer, or calendar/contacts code
- 🚫 Never: commit credentials, tokens, or real email/feed data into `tests/fixtures/`
- 🚫 Never: add Nix packaging — deliberately out of scope
