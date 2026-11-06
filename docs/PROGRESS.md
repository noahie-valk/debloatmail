# Implementation Progress

## Current Status: Phase 3C Complete (`crates/cli` Mutations, Compose, Accounts, Feeds, Diagnostics)

Phase 1 (`core` + `store`), **Phase 2A (`crates/extract` + MIME fixtures & determinism)**, **Phase 2B (`crates/feeds`)**, **Phase 2C (`crates/auth` + `crates/engine`)**, **Phase 2D (`crates/sync` + `store` blob/thread/header helpers + `feeds` SSRF redirect hook)**, **Phase 3A (`crates/ffi` Qt-free engine trait, standalone flock, `InProcessEngine`, `DbusEngine`, and dual-transport parity)**, **Phase 3B (`crates/cli` `debloatmailctl` read/search engine)**, and **Phase 3C (`crates/cli` mutations, RFC 5322 compose, accounts, feeds, sync, daemon control, and doctor diagnostics)** are complete. Workspace dependencies and toolchains are pinned to Rust 1.99.1 (Edition 2024), `clap 4.5.64`, `mail-parser 1.0.0`, `rusqlite 0.41.1`, and `zbus 5.19.0`. All 153 workspace tests pass, `cargo clippy` runs with zero warnings across all targets, `cargo deny` passes advisories/bans/licenses/sources, and `scripts/check-invariants.sh` exits cleanly with code 0.

---

### 1. Work Completed

#### A. Phase 1: Foundation (`crates/core` & `crates/store`)
- **`crates/core`**:
  - Implemented domain primitives with zero dependencies (no tokio, rusqlite, Qt, network, or filesystem).
  - Domain structs: [`Account`](../crates/core/src/account.rs), [`Mailbox`](../crates/core/src/mailbox.rs), [`Item`](../crates/core/src/item.rs), [`StreamItem`](../crates/core/src/stream.rs), [`Cursor`](../crates/core/src/stream.rs), [`ThreadSummary`](../crates/core/src/thread.rs), and [`MessageOp`](../crates/core/src/message.rs).
  - 9 unit tests passing.
- **`crates/store`**:
  - SQLite storage engine with bundled rusqlite and FTS5 full-text indexing.
  - Thin cache schema per `docs/DATA-MODEL.md` v3.
  - Sole-writer architecture ready for daemon execution; read-only WAL access for queries.
  - Keyset pagination with total-order cursors for stream views.
  - FTS5 search query parsing with adversarial quote sanitization.
  - Content-addressed BLAKE3 blob store (`blobs.rs`), JWZ-lite account threading (`threads.rs`), and header/attachment reconciliation (`headers.rs`).
  - 8 unit tests passing.

#### B. Phase 2A: MIME Parsing & Text Extraction (`crates/extract`)
- Conforms strictly to Invariant 4: pure byte slices in, structures out; zero async runtime, filesystem, database, or socket dependencies; depends solely on `core` and approved parsing libraries (`mail-parser`, `charset`, `chardetng`, `html2text`, `thiserror`).
- **`limits.rs`**: Configured resource caps per §13 (25 MB raw message, 20 MIME nesting depth, 500 parts, 1 MB extracted body, 100 attachments, 100k HTML nodes, 10 MB feed response).
- **`charset.rs`**: Decode pipeline per §8 precedence (BOM detection → UTF-8 validation → declared charset via `charset::Charset` → `chardetng` sniffing fallback → `windows-1252` fallback).
- **`flowed.rs`**: RFC 3676 / RFC 2646 `format=flowed` unwrapping with quote depth preservation, space-stuffing removal, trailing space flowed line joining, `DelSp=yes` support, and protected `-- ` signature separator.
- **`html.rs`**: Custom `html2text` `MailDecorator` conforming to §10 (header tags without `#` markers, unordered lists with `- `, blockquotes with `> `, verbatim `<pre>`, image alt text `[image: alt]`, inline link formatting `text <URL>`, relative link resolution against `site_url`).
- **`header.rs`**: Normalized subjects for threading (stripping `Re:`, `Fwd:`, `AW:`, counters), structured `from_addr` without angle brackets, author display name derivation, and Message-ID / References extraction.
- **`structure.rs`**:
  - Recursive MIME structure walk assigning RFC 3501 `section_id` numbering (`"1"`, `"2"`, `"1.1"`, `"2.1"`).
  - Body candidate selection precedence (`multipart/alternative` preferring non-empty plaintext over HTML; `multipart/mixed` selecting primary body and demoting subsequent candidates to attachments; `multipart/signed` content extraction; `multipart/encrypted` placeholder).
  - Attachment classification, Content-Disposition handling, and `%PDF-` magic bytes verification.
  - Plaintext and HTML whitespace normalisation (`normalize_plaintext_whitespace` per §9 and §10.5 Steps 1, 4, 5).
  - Paragraph boundary truncation when exceeding extracted body size limits.
  - Excerpt derivation (first 200 chars of first non-empty paragraph).
  - Mislabelled UTF-8 raw byte recovery from single-byte encodings per §8 step 3.
- **`lib.rs`**:
  - `CURRENT_EXTRACTOR_VERSION = 1`.
  - Feed plain text extraction (`extract_feed_text`) prioritizing content over summary and resolving relative links against `site_url`.
  - 33 unit tests passing.

#### C. Phase 2A: Golden MIME Fixture Suite & Determinism Integration Test
- **Golden MIME Fixtures (`tests/fixtures/mime/`)**:
  - 33 `.eml` and matching `.golden` test fixture pairs across all 11 categories in `docs/MIME-AND-TEXT.md` §17:
    - `simple-plain/`: 8bit (`simple-8bit`), quoted-printable (`simple-qp`), base64 (`simple-base64`).
    - `alternative/`: text+html (`text-and-html`), html-only (`html-only`), empty-plaintext-with-html fallback (`empty-plain-html`).
    - `flowed/`: format=flowed (`format-flowed`), DelSp=yes (`flowed-delsp`), space-stuffing (`flowed-stuffed`), `-- ` signature separator protection (`flowed-sig`).
    - `charsets/`: UTF-7 (`charset-utf7`), ISO-2022-JP (`charset-iso2022jp`), GB2312 (`charset-gb2312`), mislabelled UTF-8 override (`charset-mislabelled-utf8`), BOM override (`charset-bom`).
    - `multipart/`: mixed (`multipart-mixed`), related with cid: image (`multipart-related`), nested containers (`multipart-nested`), missing boundary handling (`missing-boundary`).
    - `signed-encrypted/`: PGP signed (`signed`), PGP encrypted (`encrypted`).
    - `attachments/`: RFC 2231 filename continuations (`rfc2231`), inline cid (`inline-cid`), text/plain with filename as attachment (`plain-attachment`).
    - `headers/`: RFC 2047 adjacent words (`rfc2047-adjacent`), SMTPUTF8 raw UTF-8 headers (`smtp-utf8`), group syntax (`group-syntax`).
    - `content-encoding/`: Gzip compressed part treated as opaque without decompression per §7 & §14 (`gzip-opaque`).
    - `malformed/`: truncated base64 (`truncated-base64`), bad QP hex (`bad-qp`), raw binary garbage (`garbage`).
    - `limits/`: oversize body truncated at paragraph boundary (`oversize-body`), deep MIME nesting capped at 20 (`deep-nesting`).
- **Integration Test (`tests/integrations/determinism.rs`)**:
  - `test_mime_fixtures_determinism_and_golden`: asserts byte-identical reproducibility across multiple extraction runs on identical input, validating all fields against `.golden` snapshots.
  - `test_rfc3501_section_id_validity_and_round_trip`: asserts all emitted attachment `section_id`s conform to the RFC 3501 §6.4.5 grammar (`^[0-9]+(\.[0-9]+)*$`), and validates round-tripping for nested multipart and embedded `message/rfc822` (`2` for container, `2.HEADER` for header, `2.1` for body).
  - `test_invariants_across_all_fixtures`: asserts no HTML tags leak into `body`, output is valid UTF-8, `is_pdf` uses `%PDF-` magic bytes, and compressed `Content-Encoding` is treated as opaque with warnings.

---

### 2. Workspace Verification

- **Tests**: `cargo test --workspace --all-targets` passes all 139 tests:
  - `debloatmail-auth`: 6 unit tests
  - `debloatmail-cli`: 17 unit tests + 10 integration tests (`cli_read_tests`, `resolver_tests`, `search_parser_tests`)
  - `debloatmail-core`: 9 unit tests
  - `debloatmail-engine`: 23 unit tests
  - `debloatmail-extract`: 33 unit tests + 3 integration tests (`determinism`)
  - `debloatmail-feeds`: 1 unit test + 5 integration tests
  - `debloatmail-ffi`: 8 unit tests + 4 integration tests (`dual_transport`)
  - `debloatmail-store`: 8 unit tests
  - `debloatmail-sync`: 12 unit & scenario integration tests
- **Clippy**: `cargo clippy --workspace --all-targets -- -D warnings` passes cleanly (0 warnings).
- **Formatting**: `cargo fmt --all -- --check` passes cleanly.

---

### 3. Completed Phase 2B (`crates/feeds`)

- Implemented feed ingestion crate `crates/feeds` with a blocking `reqwest` client (rustls‑only) and `feed‑rs` parser.
- Added conditional GET handling (`ETag`, `If‑Modified‑Since`) and a 10 MiB response size cap.
- Implemented safety checks against HTML content, XXE, and Billion‑Laughs attacks.
- Provided `fetch_feed`, `parse_feed`, and `poll_feed` public API (re‑exported in `crates/feeds/src/lib.rs`).
- Added comprehensive unit and integration tests with a sample RSS fixture (`crates/feeds/tests/fixtures/rss_sample.xml`).
- All workspace tests pass (`cargo test`), clippy reports 0 warnings, and formatting checks succeed.

### 4. Completed Phase 2C (`crates/auth` & `crates/engine`)

- **`crates/auth` (`debloatmail-auth`)**:
  - Implemented `SecretStore` trait with `KeyringStore` (`keyring` v4 with `zbus-secret-service-keyring-store`) and `MemorySecretStore` for headless/unit tests (`store.rs`).
  - Implemented SASL credential builders (`build_sasl_credentials`) and mechanism negotiation (`select_password_mechanism` preferring `SCRAM-SHA-256` → `PLAIN` → `LOGIN`) using `io-sasl = "=0.1.0"` and `secrecy::SecretString` (`sasl.rs`).
  - Implemented provider presets (`Fastmail`, `Proton`, `Generic`) and `validate_account_security` refusing cleartext (`Security::None`) on non-loopback hosts (`preset.rs`).
- **`crates/engine` (`debloatmail-engine`)**:
  - Confined all IMAP/SMTP TLS handshakes to `crates/engine/src/tls.rs` (`rustls` + `ring` + `rustls-platform-verifier`), enforcing `allow_invalid_cert_loopback` strictly against the connected socket's `peer_addr().ip().is_loopback()` (`127.0.0.1` or `::1`) and surfacing `TlsVerifyMode::LoopbackExceptionUsed`.
  - Confined all IMAP/SMTP TCP sockets to `crates/engine/src/io.rs` (`EngineStream` + `MemoryStream` for zero-socket coroutine tests).
  - Implemented sans-I/O `io-imap = "=0.7.1"` coroutine pumps in `crates/engine/src/imap/` (`ImapConnection`, `discover_folders` with RFC 6154 `SPECIAL-USE` + localized name fallback + role uniqueness + Gmail `\All` handling, `fetch_header_window`, `fetch_body_peek`, `fetch_attachment_section` with RFC 3501 `section_id` validation, `uid_expunge_messages` requiring `UIDPLUS`, and `run_idle_once` with a 28-minute cap).
  - Implemented sans-I/O `io-smtp = "=0.6.0"` coroutine pumps in `crates/engine/src/smtp/` (`SmtpConnection`, `build_rfc5322_message` with CRLF header injection guard and `Bcc` stripping, and `send_email` distinguishing `Sent`, `Rejected`, `ConnectionFailed`, and mid-`DATA` `NoVerdict`).
  - Implemented `AccountStateMachine`, `FailureClass`, and exponential backoff + jitter schedules (`account_reconnect_backoff`, `feed_poll_backoff`, `op_retry_backoff`, `deterministic_account_jitter`) in `crates/engine/src/state.rs`.
  - Implemented `run_account_check` in `crates/engine/src/check.rs` for `debloatmailctl account check` and `debloatmailctl doctor`.

### 5. Completed Phase 2D (`crates/sync` + `store` & `feeds` Extensions)

- **Storage Helpers (`crates/store`)**:
  - `blobs.rs`: Content-addressed BLAKE3 sharded file store (`blobs/ab/cd/...` with mode `0600` atomic write + rename), reference counting (`put_blob`, `increment_blob_ref`, `decrement_blob_ref`, `touch_blob`), and 1-hour grace-window garbage collection (`gc_blobs`).
  - `threads.rs`: JWZ-lite account thread builder (`rebuild_account_threads`) with `References` / `In-Reply-To` chaining, 30-day same-`subject_norm` merging, `root_message_id`-anchored `public_id` preservation across rebuilds, and rollup calculation (`reply_count`, `unread_count`, `last_activity`, up to 3 distinct `participants`).
  - `headers.rs`: Mail header upsert (`upsert_mail_header` seeding `item_text(title, author, body = '')`), provisional `BODYSTRUCTURE` attachment insertion (`replace_provisional_attachments`), and on-demand extracted attachment reconciliation (`reconcile_extracted_attachments` preserving `blob_hash` across `section_id` matches and decrementing orphaned blob refs).
- **Feed Redirect Hook & Taxonomy (`crates/feeds`)**:
  - Custom SSRF redirect policy (`build_feed_redirect_policy`) enforcing max 5 hops, rejecting `https:// -> http://` downgrades and non-HTTP(S) schemes, and rejecting loopback (`127.0.0.0/8`, `::1`, `localhost`), RFC 1918 private ranges, CGNAT (`100.64.0.0/10`), and link-local / cloud metadata (`169.254.0.0/16`, `fe80::/10`, `fc00::/7`).
  - Explicit `404`/`410` (`FetchError::Gone`), `429`/`503` (`FetchError::RateLimited` with `Retry-After` delta-seconds parsing), `301`/`308` permanent redirect tracking (`permanent_redirect_url`), and `site_url` / `entry.content` / `entry.summary` extraction.
- **Sync Orchestration (`crates/sync`)**:
  - Zero sockets and zero TLS: generic over `ImapConnection<S: AsyncByteStream>`, `SmtpConnection<S: AsyncByteStream>`, and `FeedFetcher`.
  - `id.rs`: Monotonic RFC 9562 UUIDv7 generator (`mint_uuid_v7_from_clock`).
  - `scheduler.rs`: 4-class priority queue (`UserInitiated` > `QueueDrain` > `BackgroundScheduled` > `Housekeeping`), scope coalescing with 1-second post-`Done` window, `CancelToken`, and 100 ms `Progress` throttle with guaranteed `percent == 100` terminal emission.
  - `recovery.rs`: Crash-safe startup recovery (`run_startup_recovery` resetting interrupted `body_state = 'pending'` to `'none'` while leaving `pending_ops.state = 'in_flight'` intact) and 7-day terminal `pending_ops` retention purge (`purge_expired_terminal_ops`).
  - `mail.rs`: Folder discovery reconciliation preserving manual roles, 2,000-UID windowed header sync advancing `folders.uidnext` atomically per window with between-window preemption, 7-step staged `UIDVALIDITY` reset recovery preserving `items.public_id` by `Message-ID` and rebinding `pending_ops.item_id`, and `CONDSTORE` `CHANGEDSINCE` / fallback flag sync with pending-op-wins protection.
  - `body.rs`: On-demand `BODY.PEEK[]` message body extraction and `BODY.PEEK[<section_id>]` attachment download with BlobStore deduplication.
  - `ops.rs`: FIFO `pending_ops` drain (`flag`, `move`, `delete`, `expunge` via `UID EXPUNGE` when `UIDPLUS` is negotiated, and `send` with pre-built RFC 5322 blob, Sent-folder `Message-ID` duplicate guard after `SendOutcome::NoVerdict`, and `APPEND` fallback when `BURL` is absent) plus `queue_send_from_draft`.
  - `feeds.rs`: Conditional feed polling (`304 Not Modified`, `200 OK` non-destructive upsert preserving `unread`/`starred`/`hidden_at`, `301`/`308` URL update, `410 Gone` cached-item retention) and hybrid 15-second `Feeds.Add` validation (`add_feed_hybrid` + `complete_pending_feed_validation`).

### 6. Security Hardening (Completed)

- **Secure DB file creation** – SQLite database files are created with mode 0600 and flags `O_EXCL|O_NOFOLLOW` to prevent symlink attacks. Implemented in [`secure_file.rs`](../crates/store/src/secure_file.rs). *(Docs: [SECURITY.md §8.5](SECURITY.md#85-permissions--secure-file-creation))*
- **Feed parser hardening** – XML external entities and DTD processing are disabled in the RSS/Atom parser to mitigate XXE and Billion‑Laughs attacks. Implemented in [`parse.rs`](../crates/feeds/src/parse.rs). *(Docs: [SECURITY.md §6.2](SECURITY.md#62-xml-external-entity-protection))*
- **Clippy lint compliance** – Fixed inner doc comment issue in `limits.rs` to satisfy `clippy::empty_line_after_doc_comments`. *(Docs: [SECURITY.md §8.3](SECURITY.md#83-lint-compliance))*
- **Low‑level system calls** – Added `libc` dependency to use `open` flags securely in `crates/store`. *(Docs: [SECURITY.md §8.4](SECURITY.md#84-low-level-system-calls))*

### 7. Toolchain Modernization & Scripts Verification (Completed)

- **Toolchain Pinned**: Reinstated `rust-toolchain.toml` pinned to Rust `1.99.1`, updated workspace edition to `2024`.
- **Parser Stack Migration (`mail-parser 1.0.0`)**: Refactored `crates/extract` (`header.rs` and `structure.rs`) to adapt to breaking 1.0.0 API changes (hierarchical `part.children()`, `part.decoded()` transfer decoding, `AddressList` mailbox iterators, and `ContentType` attribute accessors).
- **SQLite 0.41 Integration**: Updated `crates/store` to `rusqlite = "0.41.1"` (bundled SQLite) and mapped `size_bytes: Option<u64>` to `i64` for `ToSql` compatibility.
- **Verification Scripts**:
  - [`scripts/check-invariants.sh`](../scripts/check-invariants.sh): Added `last_activity` to the timestamp field allowlist per `docs/DATA-MODEL.md` §4.1 and fixed the clean-run exit code logic.
  - [`scripts/run_security_checks.sh`](../scripts/run_security_checks.sh): Fully clean verification across `cargo fmt`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`, and `cargo audit` (0 vulnerabilities).

### 8. License & Dependency Hygiene (Completed — 2026-11-01)

- **MIT license declared** across all workspace crates via `license = "MIT"` in `[workspace.package]` and `license.workspace = true` in each crate's `Cargo.toml`.
- **`crates/feeds` joined the workspace** (`members` list in root `Cargo.toml`); its `version`, `edition`, and `license` now inherit from `[workspace.package]`. Edition bumped to `2024`.
- **`deny.toml` configured** with `cargo-deny` license allow-list (MIT, Apache-2.0, BSD-2/3, ISC, Zlib, Unlicense, MPL-2.0, CDLA-Permissive-2.0), security advisory enforcement, and explicit bans on `io-email`, `io-keyring`, OpenSSL, curl, and WebView crates.
- **`scripts/check-deny.sh`** added — run `./scripts/check-deny.sh` at the end of any session to check licenses, bans, and advisories. Accepts an optional check name (e.g. `./scripts/check-deny.sh licenses`). Requires `cargo-deny` (`cargo install cargo-deny --locked`).
- **`data/org.noahie.debloatmail.metainfo.xml`** created with `<project_license>MIT</project_license>` and basic AppStream metadata.
- Fixed pre-existing bug: `reqwest` feature flag renamed from `rustls-tls` to `rustls` in reqwest 0.13.

### 9. Completed Phase 3A: Engine Boundary & Dual Transports (`crates/ffi`)

- **Qt-Free Engine Traits (`traits.rs`)**:
  - Defined `EngineReads` (synchronous direct SQLite WAL queries), `EngineMutations` (asynchronous state modifications and job triggering), and blanket `Engine` trait (`EngineReads + EngineMutations`).
  - Strict adherence to workspace invariants: zero Qt / cxx-qt imports, zero external dependencies in `crates/core`, transfer types strictly mapped.
- **Standalone File Locking (`lock.rs`)**:
  - Implemented `StandaloneLock` via `libc::flock` (exclusive non-blocking `LOCK_EX | LOCK_NB`) on `$XDG_STATE_HOME/debloatmail/debloatmail-standalone.lock`.
  - Records process PID into the lockfile for clear diagnostics and surfaces `FfiError::LockHeld { path, pid }` on contention.
- **In-Process Engine (`in_process.rs`)**:
  - Standalone engine implementation supporting `--no-daemon` CLI and future Android target.
  - Connects directly to SQLite with WAL, in-memory or keyring `SecretStore`, and `ReqwestFeedFetcher`.
- **D-Bus Engine Adapter (`dbus.rs`)**:
  - Proxy client adapter communicating with `org.noahie.debloatmail.Daemon` using `zbus 5.19`.
  - Implements `ItemsProxy`, `SyncProxy`, `AccountsProxy`, `FeedsProxy`, `ComposeProxy`, `QueueProxy`, and `StatusProxy`.
  - Translates D-Bus error names (`NotFound`, `InvalidArgument`, `Unsupported`, `ServiceUnknown`, `NameHasNoOwner`) into unified `FfiError` variants.
- **Shared SQLite WAL Reader (`reader.rs`)**:
  - Shared query layer for stream pagination, threads, full item detail, account/folder/feed listings, search, and queue status.
  - Guarantees 100% read parity across both standalone and daemon-backed executions without serializing large item sets across D-Bus.
- **Dual-Transport Integration Testing (`tests/dual_transport.rs`)**:
  - Verified exact read parity between `InProcessEngine` and `DbusEngine` on seeded data.
  - Verified standalone advisory file lock mutual exclusion and PID reporting across concurrent runs.
  - Verified thread-safe dynamic trait boundary (`Box<dyn Engine> + Send + Sync`).

---

### 10. Completed Phase 3B: CLI Core Infrastructure & Read/Search Engine (`crates/cli`)

- **Binary & Workspace Scaffolding (`crates/cli`)**:
  - Registered `debloatmail-cli` and binary `debloatmailctl` in workspace with approved dependencies (`clap 4.5.64`, `clap_complete 4.5.64`, `clap_mangen 0.2.33`, `toml 1.2.0`, `anstream 1.0.0`, `anstyle 1.0.14`, `rpassword 7.5.4`, `assert_cmd 2.2.2`, `predicates 3.1.4`).
- **Contractual Exit Codes & Errors (`error.rs`)**:
  - Implemented `CliError` strictly matching `docs/debloatmailctl.md` §7: exit codes 0 (success), 1 (internal), 2 (usage/unknown operator), 3 (not found), 4 (auth), 5 (network), 6 (daemon unavailable), 7 (offline mode), 8 (lock held), 9 (ambiguous id prefix).
  - Emits contractual single-line error messages to `stderr` or structured `{"error": {"code": "...", "message": "..."}}` to `stdout` under `--format json`.
- **Addressing & Short ID Resolution (`resolve.rs`)**:
  - Git-style 8-hex prefix matching for `items.public_id`. Detects ambiguous prefixes and exits with code 9; missing IDs exit with code 3.
  - Supports space-separated IDs and stdin pipeline reading via literal `"-"`.
  - Client-side resolution for accounts, folders (name or role), and feeds (name, title, or URL).
- **Output Formats & Styling (`format/`)**:
  - `human`: Right-aligned columns with ANSI color highlighting unread items in bold and starred in yellow, respecting `--color` and `NO_COLOR`.
  - `json`: Contractual single structured document with ISO 8601 UTC dates, full UUIDv7s, and keyset cursor envelope.
  - `jsonl`: Streaming JSON objects without envelope.
  - `plain`: Tab-separated columns (`short_id\tpublished_at\tauthor\ttitle`) and `--print0` NUL-separated full UUIDs.
- **Unified Stream `list` Command (`commands/list.rs`)**:
  - Reads directly from SQLite WAL with zero daemon/network overhead.
  - Full filter support: `--mail`, `--feed`, `--unread`, `--starred`, `--folder`, `--feed <NAME>`, `--since`, `--before` (supporting relative `7d`, `2w` and ISO dates).
  - Keyset cursor minting and validation (rejects mismatched sort key with exit code 2).
  - Conversation thread view (`--threads`).
- **Gmail-Style Search Query Parser & Search (`search_parser.rs`, `commands/search.rs`)**:
  - Strict closed operator set per `docs/debloatmailctl.md` §9 and `docs/DATA-MODEL.md` §10.
  - Rejects unknown operators with exit code 2.
  - Automatic bare term prefix matching (`async*`) and safe FTS5 quote escaping.
  - `--explain` diagnostics printing the compiled FTS5 expression and full SQL statement.
- **Plaintext `read` Command (`commands/read.rs`)**:
  - Displays body text, with optional RFC 5322 `--headers` and raw MIME via `--raw` (mail-only).
  - Uncached body flow: under `--offline` exits code 7; online triggers on-demand fetch via engine (`InProcessEngine` or `DbusEngine`) and re-reads SQLite.
  - Full thread printing (`--thread`).
- **Inspecting & Utility Subcommands**:
  - `folder list` & `folder stats`.
  - `feed list` & `feed show`.
  - `account list` & `account show` (secrets masked as `••• (in keyring)`).
  - `config path`, `show`, `validate`, `get`, `set` (deep-merged TOML management).
  - `generate completions <shell>`, `generate man`, `generate schema` (`0001_init.sql`).
  - `version`.
- **Integration Test Suite (`tests/`)**:
  - `cli_read_tests.rs`: End-to-end `assert_cmd` test matrix against seeded SQLite fixture DB, asserting exit codes (0, 2, 3, 7, 9) and formatting output.
  - `resolver_tests.rs`: Prefix resolution, ambiguity, and not-found tests.
  - `search_parser_tests.rs`: Adversarial input safety, operator combinations, and unknown operator rejection.

#### G. Phase 3C: CLI Write & Action Surface (`crates/cli`)
- **Engine Dispatch & CLI Guard Rails (`executor.rs`)**:
  - `init_engine`: Uniform engine dispatch acquiring `StandaloneLock` on `$XDG_STATE_HOME/debloatmail/debloatmail-standalone.lock` for direct `InProcessEngine` when `--no-daemon` is specified or daemon is inactive, or connecting seamlessly via `DbusEngine`.
  - Non-interactive safety: `confirm_destructive` protects `--expunge` and feed purge/hide actions, requiring `-y`/`--yes` when not attached to a TTY.
  - Offline guards: Enforces `--offline` mode check with exit code 7 before dispatching online network operations.
- **Item Mutations (`commands/mark.rs`, `move_cmd.rs`, `copy_cmd.rs`, `delete.rs`, `hide.rs`)**:
  - `mark read|unread|starred|unstarred <ids...>`: Supports multi-ID arguments and stdin `-` pipelines, git-style prefix matching, `--dry-run`, and emits contractual human, JSON, JSONL, and plain format summaries.
  - `move` and `copy`: Validates mail-only targets (rejecting feeds with contractual message), resolves folder names/roles to UUIDs, and enqueues folder migration ops.
  - `delete`: Mail-only deletion with support for `--expunge` (triggering physical expunge op after confirmation).
  - `hide`: Feed-only action setting `hidden_at` timestamp with destructive confirmation guard.
- **RFC 5322 Generator & Compose Surface (`rfc5322.rs`, `commands/compose.rs`, `reply.rs`, `forward.rs`, `attachment.rs`)**:
  - `rfc5322.rs`: Clean RFC 5322 and MIME message generator supporting single-part plaintext and multipart/mixed base64-encoded file attachments.
  - `compose`: Handles `--to`, `--cc`, `--bcc`, `--subject`, `--body-stdin`, interactive `$EDITOR` / `$VISUAL` / `nano` editing, `--save-draft`, `--draft <ID>` revision, `--dry-run` inspection, and `--now` expedited delivery.
  - `reply <id>`: Mail-only action prefilling To/Cc from author and recipient headers (distinguishing single reply vs `--all`), prepending `Re:` idempotently, populating `In-Reply-To` and threading `References`, and quoting original body with `--quote`.
  - `forward <id>`: Mail-only action prepending `Fwd:`, generating formatted original header quote blocks, and optionally attaching the complete original `.eml` raw blob via `--attach-original`.
  - `attachment`: `list` item attachments, `download` part or all attachments to destination folder or stdout (`-`), and `open` attachments using `xdg-open`.
- **Feed Management Surface (`commands/feed.rs`)**:
  - `feed add <URL>`: Validates feed format, performs initial network poll with 15-second timeout, gracefully falls back to unvalidated state (`validated: false`) on slow servers per `docs/debloatmailctl.md` §8, and persists feed with custom polling interval.
  - `feed update`: Updates title and poll interval.
  - `feed set-url <NAME|OLD_URL> <NEW_URL>`: Executes atomic soft-remove of old feed and inserts new feed per §8 specification.
  - `feed remove`: Soft-removes feed by default (sets `removed_at`), or purges completely with `--purge`.
  - `feed refresh`: Triggers immediate background sync and supports `--wait` progress polling.
- **Account Management Surface (`commands/account.rs`)**:
  - `account add`: Interactive terminal wizard with masked password entry via `rpassword` when run without arguments, or flag configuration with `--provider proton` preset auto-fill (`127.0.0.1`, STARTTLS ports 1143/1025). Adheres to Invariant 6: secrets are never passed via argv; accepts `--secret-stdin` or `--secret-command`. Initializes SQLite store bootstrap if database does not yet exist.
  - `account update`: Edits server endpoints, ports, security modes, and performs credential rotation.
  - `account remove`: Disables accounts (`enabled = 0`) or cascades deletions with `--purge`.
  - `account check`: Pings remote IMAP and SMTP endpoints using stored credentials and reports diagnostic connection health.
- **Sync & Daemon Control (`commands/sync_cmd.rs`, `folder.rs`, `daemon.rs`)**:
  - `sync` and `folder sync`: Dispatches account, folder, or global sync ops to the engine and polls completion under `--wait`.
  - `daemon status`: Contractually exits code 0 with `Daemon: stopped` or JSON `{"daemon": {"running": false}}` when daemon process is inactive per §8.
  - `daemon stop`, `daemon reload`, `daemon logs`.
- **System Diagnostics (`commands/doctor.rs`)**:
  - Implements all 11 diagnostic checks specified in `docs/debloatmailctl.md` §8: configuration file presence, database schema version (`<= 3`), SQLite WAL & integrity check, secret store reachability & credential retrieval, blob store refcount reconciliation, FTS5 full-text index consistency, folder UIDVALIDITY continuity, send queue health, unvalidated feeds, cache size vs limit, and daemon D-Bus reachability.
  - Diagnostic `--fix` triggers SQLite `VACUUM` and integrity repairs; outputs structured JSON with `--json`.
- **Integration Test Suite**:
  - Added 4 comprehensive integration test binaries:
    - `cli_mutation_tests.rs`: Item flag mutations, move/copy folder resolution, feed hide vs mail delete restrictions, dry-run, and stdin batch execution.
    - `cli_compose_tests.rs`: RFC 5322 generation, draft creation, dry-run compose, reply/forward header prefilling, and attachment download/listing.
    - `cli_feed_account_tests.rs`: Account creation wizard/flags, ProtonMail preset, secret rotation, feed add/update/set-url/remove, and sync dispatch.
    - `cli_daemon_doctor_tests.rs`: Daemon status stopped exit code 0 parity and doctor diagnostic test suite.
  - 14 integration tests passing in `crates/cli`.

---

### 11. Upcoming Phases

#### Phase 3D – Golden CLI Tests & Week-One Validation Spike
- CLI golden test harness matching `docs/debloatmailctl.md`.
- End-to-end integration test validating offline/online operation, feed ingestion, and FTS5 search across the week-one validation spike (§11).

#### Phase 4 – Daemon & D‑Bus (`debloatmaild`)
- Implement the sole SQLite writer process, background sync scheduler, tray integration, and desktop notifications.
- Expose D‑Bus interfaces `org.noahie.debloatmail` and `org.noahie.debloatmail.Daemon`.

#### Phase 5 – Desktop GUI (`ui/bridge` & Kirigami QML)
- Bridge Qt6/KF6 Kirigami QML via `cxx‑qt`.
- Provide a plaintext‑first reader/composer with unified stream list.
