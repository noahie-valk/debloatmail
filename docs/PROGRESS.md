# Implementation Progress

## Current Status: Phase 2A (`crates/extract`) Implementation Complete

Phase 1 (`core` + `store`) is completed. **Phase 2A (`crates/extract`)** core implementation is now complete, with all 33 unit tests passing, clean `cargo clippy` with zero warnings, and formatted via `cargo fmt`. The entire workspace passes all 47 tests.

---

### 1. Work Completed

#### A. Phase 1: Foundation (`crates/core` & `crates/store`)
- **`crates/core`**:
  - Implemented domain primitives with zero dependencies (no tokio, rusqlite, Qt, network, or filesystem).
  - Domain structs: [`Account`](file:///home/nvalk1/debloatmail/crates/core/src/account.rs), [`Mailbox`](file:///home/nvalk1/debloatmail/crates/core/src/mailbox.rs), [`Item`](file:///home/nvalk1/debloatmail/crates/core/src/item.rs), [`StreamItem`](file:///home/nvalk1/debloatmail/crates/core/src/stream.rs), [`Cursor`](file:///home/nvalk1/debloatmail/crates/core/src/stream.rs), [`ThreadSummary`](file:///home/nvalk1/debloatmail/crates/core/src/thread.rs), and [`MessageOp`](file:///home/nvalk1/debloatmail/crates/core/src/message.rs).
  - 8 unit tests passing.
- **`crates/store`**:
  - SQLite storage engine with bundled rusqlite and FTS5 full-text indexing.
  - Thin cache schema.
  - Sole-writer architecture ready for daemon execution; read-only WAL access for queries.
  - Keyset pagination with total-order cursors for stream views.
  - FTS5 search query parsing with adversarial quote sanitization.
  - 6 unit tests passing.

#### B. Phase 2A: MIME Parsing & Text Extraction (`crates/extract`)
- Conforms strictly to Invariant 4: pure byte slices in, structures out; zero async runtime, filesystem, database, or socket dependencies; depends solely on `core` and approved parsing libraries (`mail-parser`, `charset`, `chardetng`, `html2text`, `thiserror`).
- **limits.rs**: Configured resource caps (25 MB raw message, 20 MIME nesting depth, 500 parts, 1 MB extracted body, 100 attachments, 100k HTML nodes, 10 MB feed response).
- **charset.rs**: Decode pipeline per §8 precedence (BOM detection → UTF-8 validation → declared charset via `charset::Charset` → `chardetng` sniffing fallback → `windows-1252` fallback).
- **flowed.rs**: RFC 3676 / RFC 2646 `format=flowed` unwrapping with quote depth preservation, space-stuffing removal, trailing space flowed line joining, `DelSp=yes` support, and protected `-- ` signature separator.
- **html.rs**: Custom `html2text` `MailDecorator` conforming to §10 (header tags without `#` markers, unordered lists with `- `, blockquotes with `> `, verbatim `<pre>`, image alt text `[image: alt]`, inline link formatting `text <URL>`, relative link resolution against `site_url`).
- **header.rs**: Normalized subjects for threading (stripping `Re:`, `Fwd:`, `AW:`, counters), structured `from_addr` without angle brackets, author display name derivation, and Message-ID / References extraction.
- **structure.rs**:
  - Recursive MIME structure walk with RFC 3501 `section_id` numbering (`"1"`, `"2"`, `"1.1"`, `"2.1"`).
  - Body candidate selection precedence (`multipart/alternative` preferring non-empty plaintext over HTML; `multipart/mixed` selecting primary body and demoting subsequent candidates to attachments; `multipart/signed` content extraction; `multipart/encrypted` placeholder).
  - Attachment classification, Content-Disposition handling, and `%PDF-` magic bytes verification.
  - Plaintext and HTML whitespace normalisation (`normalize_plaintext_whitespace` per §9 and §10.5 Steps 1, 4, 5).
  - Paragraph boundary truncation when exceeding extracted body size limits.
  - Excerpt derivation (first 200 chars of first non-empty paragraph).
- **lib.rs**:
  - `CURRENT_EXTRACTOR_VERSION = 1`.
  - Feed plain text extraction (`extract_feed_text`) prioritizing content over summary and resolving relative links against `site_url`.
  - 33 unit tests passing.

---

### 2. Workspace Verification

- **Tests**: `cargo test --workspace --all-targets` passes all 47 tests (8 core, 33 extract, 6 store).
- **Clippy**: `cargo clippy --workspace --all-targets -- -D warnings` passes cleanly.
- **Formatting**: `cargo fmt --all -- --check` passes cleanly.

---

### 3. Immediate Next Tasks (Finishing Phase 2A)

1. **Golden MIME Fixtures**:
   - Add test fixtures to `tests/fixtures/mime/`
     - `simple-plain/` (8bit, quoted-printable, base64)
     - `alternative/` (text+html, html-only, empty-plaintext-with-html)
     - `flowed/` (format=flowed, DelSp=yes, space-stuffing, `-- ` separator)
     - `charsets/` (utf-7, iso-2022-jp, gb2312, mislabelled-utf8, BOM)
     - `multipart/` (mixed, related, nested, missing boundary)
     - `signed-encrypted/` (multipart/signed, multipart/encrypted)
     - `attachments/` (RFC 2231 filename continuations, inline cid:, text/plain with filename)
     - `limits/` (oversize body, deep nesting)
2. **Determinism Integration Test**:
   - Add integration test under `tests/` verifying byte-identical reproducibility and RFC 3501 section ID round-tripping.

---

### 4. Next Project Phases Beyond Phase 2A

1. **Phase 2B: Feed Ingest (`crates/feeds`)**:
   - Single HTTP client via `reqwest` on `rustls` (no default features), owned exclusively by `crates/feeds`.
   - RSS/Atom parsing via `feed-rs`.
   - Conditional GET support (`ETag`, `If-Modified-Since`).
   - Strict response size cap (10 MB).
2. **Phase 2C: Engine & Protocol State Machines (`crates/engine`)**:
   - Sans-I/O IMAP and SMTP state machines with pimalaya (`io-imap`, `io-smtp`).
   - Sockets and TLS isolated strictly to `engine/io.rs` and `engine/tls.rs`.
   - Credential management via `crates/auth` (`keyring` / `oo7`).
3. **Phase 3: CLI Validation Spike (`crates/cli` / `debloatmailctl`)**:
   - Implement CLI as the engine's first customer before touching QML.
   - Prove auth, sync, send queue, feed ingest, MIME extraction, and FTS5 search end-to-end.
4. **Phase 4: Daemon & D-Bus (`debloatmaild`)**:
   - Sole SQLite writer process.
   - Background sync scheduler, system tray, desktop notifications.
   - D-Bus interfaces `org.noahie.debloatmail` and `org.noahie.debloatmail.Daemon`.
5. **Phase 5: Desktop GUI (`ui/bridge` & Kirigami QML)**:
   - Qt6/KF6 Kirigami QML interface bridged via `cxx-qt`.
   - Plaintext-first reader and composer, unified stream list.
