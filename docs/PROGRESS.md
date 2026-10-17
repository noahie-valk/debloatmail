# Implementation Progress

## Current Status: Phase 2A Complete & Verified

Phase 1 (`core` + `store`) and **Phase 2A (`crates/extract` + MIME fixtures & determinism)** are complete and verified against `docs/MIME-AND-TEXT.md` and `GEMINI.md`. All 51 unit and integration tests in the workspace pass, `cargo clippy` runs with zero warnings across all targets, and `cargo fmt` is clean.

---

### 1. Work Completed

#### A. Phase 1: Foundation (`crates/core` & `crates/store`)
- **`crates/core`**:
  - Implemented domain primitives with zero dependencies (no tokio, rusqlite, Qt, network, or filesystem).
  - Domain structs: [`Account`](file:///home/nvalk1/debloatmail/crates/core/src/account.rs), [`Mailbox`](file:///home/nvalk1/debloatmail/crates/core/src/mailbox.rs), [`Item`](file:///home/nvalk1/debloatmail/crates/core/src/item.rs), [`StreamItem`](file:///home/nvalk1/debloatmail/crates/core/src/stream.rs), [`Cursor`](file:///home/nvalk1/debloatmail/crates/core/src/stream.rs), [`ThreadSummary`](file:///home/nvalk1/debloatmail/crates/core/src/thread.rs), and [`MessageOp`](file:///home/nvalk1/debloatmail/crates/core/src/message.rs).
  - 9 unit tests passing.
- **`crates/store`**:
  - SQLite storage engine with bundled rusqlite and FTS5 full-text indexing.
  - Thin cache schema per `docs/DATA-MODEL.md` v3.
  - Sole-writer architecture ready for daemon execution; read-only WAL access for queries.
  - Keyset pagination with total-order cursors for stream views.
  - FTS5 search query parsing with adversarial quote sanitization.
  - 6 unit tests passing.

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

- **Tests**: `cargo test --workspace --all-targets` passes all 51 tests:
  - `debloatmail-core`: 9 unit tests
  - `debloatmail-extract`: 33 unit tests + 3 integration tests (`determinism`)
  - `debloatmail-store`: 6 unit tests
- **Clippy**: `cargo clippy --workspace --all-targets -- -D warnings` passes cleanly (0 warnings).
- **Formatting**: `cargo fmt --all -- --check` passes cleanly.

---

### 3. Next Project Phase: Phase 2B (`crates/feeds`)

1. **Feed Ingest (`crates/feeds`)**:
   - Single HTTP client via `reqwest` on `rustls` (no default features), owned exclusively by `crates/feeds`.
   - RSS/Atom/JSON parsing via `feed-rs`.
   - Conditional GET support (`ETag`, `If-Modified-Since`).
   - Strict response size cap (10 MB).
   - Anti-XXE and anti-billion-laughs XML safety tests with fixtures.
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
