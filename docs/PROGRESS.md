# Implementation Progress

## Current Status: Phase 2A in Progress (`crates/extract`)

Phase 1 (`core` + `store`) is completed. We are currently implementing **Phase 2A (`crates/extract`)**, the pure, runtime-free MIME parsing and text extraction pipeline per [docs/architecture.md §2 Invariant 4](file:///home/nvalk1/debloatmail/docs/architecture.md#L63) and [docs/MIME-AND-TEXT.md](file:///home/nvalk1/debloatmail/docs/MIME-AND-TEXT.md).

---

### 1. Work Completed in this Session

#### A. Workspace & Dependencies
- **Added `crates/extract` to root Cargo Workspace**: Updated root [Cargo.toml](file:///home/nvalk1/debloatmail/Cargo.toml) to include `"crates/extract"` in `[workspace.members]`.
- **Configured approved Invariant 4 dependencies**: Added locked workspace dependencies adhering strictly to Invariant 4 (pure byte slices in, structures out; zero async runtime, filesystem, database, or socket dependencies):
  - `mail-parser = "0.11.9"` (RFC 5322 & MIME 2045–2049 parsing)
  - `charset = "0.1.5"` (UTF-7, JavaMail labels, and legacy charsets over `encoding_rs`)
  - `chardetng = "1.0.0"` (Sniffing fallback detector)
  - `html2text = "0.17"` (HTML to plaintext parser on `html5ever`)
  - `thiserror = "2.0"` (Workspace error derive)
- **Configured [crates/extract/Cargo.toml](file:///home/nvalk1/debloatmail/crates/extract/Cargo.toml)** depending solely on `debloatmail-core` and the approved parsing libraries.

#### B. Hostile-Content Parsing Modules (`crates/extract/src`)
1. **[src/error.rs](file:///home/nvalk1/debloatmail/crates/extract/src/error.rs)**:
   - Defined [`ExtractError`](file:///home/nvalk1/debloatmail/crates/extract/src/error.rs) (`OversizeMessage`, `ParseFailed`, `InvalidData`).
2. **[src/limits.rs](file:///home/nvalk1/debloatmail/crates/extract/src/limits.rs)**:
   - Implemented [`Limits`](file:///home/nvalk1/debloatmail/crates/extract/src/limits.rs) struct matching [docs/MIME-AND-TEXT.md §13](file:///home/nvalk1/debloatmail/docs/MIME-AND-TEXT.md#L701):
     - `max_raw_message_size`: 25 MB
     - `max_mime_nesting_depth`: 20
     - `max_parts_per_message`: 500
     - `max_extracted_body_size`: 1 MB
     - `max_attachment_count`: 100
     - `max_header_line_length`: 8 KB
     - `max_single_header_value`: 64 KB
     - `html_parser_node_budget`: 100k nodes
     - `feed_response_size`: 10 MB
3. **[src/charset.rs](file:///home/nvalk1/debloatmail/crates/extract/src/charset.rs)**:
   - Implemented `decode_bytes` per §8 precedence:
     1. BOM detection (UTF-8, UTF-16LE, UTF-16BE overriding declared charset).
     2. Valid UTF-8 validation (catches mislabelled UTF-8).
     3. Declared charset resolution via `charset::Charset::for_label` (decoding UTF-7, ISO-2022-JP, GB2312, Windows-1251, ISO-8859-*, etc.).
     4. `chardetng::EncodingDetector` sniffing on the first ~1 KiB.
     5. `windows-1252` fallback, never failing (decodes with U+FFFD replacement characters).
4. **[src/flowed.rs](file:///home/nvalk1/debloatmail/crates/extract/src/flowed.rs)**:
   - Implemented `unwrap_flowed` per RFC 3676 / RFC 2646 and §9:
     - Quote depth counting and preservation.
     - Space-stuffing stripping.
     - Flowed line joining with trailing space handling (or space dropping when `delsp=yes`).
     - Strict protection of signature separator `-- ` (never flowed).
     - Isolation of differing quote depths from flowing together.
5. **[src/html.rs](file:///home/nvalk1/debloatmail/crates/extract/src/html.rs)**:
   - Implemented `html_to_plaintext` using `html2text` and custom `MailDecorator`:
     - Element mappings per §10.3: `h1`–`h6` with no `#` markers, unordered lists with `- `, blockquote prefix `> `, verbatim `<pre>`, image alt text `[image: alt]` or `[image]`.
     - Link formatting per §10.4: `text <URL>`, `mailto:` address normalization, omission when text matches URL, relative URL resolution against base URL (for feeds).
     - Whitespace normalisation per §10.5: CRLF/CR to LF, collapsing spaces/tabs (except inside `<pre>`), stripping trailing whitespace on each line, collapsing 3+ newlines to 2, and stripping leading/trailing blank lines.
6. **[src/header.rs](file:///home/nvalk1/debloatmail/crates/extract/src/header.rs)**:
   - Implemented `normalize_subject` per §4.5 (stripping `Re:`, `Fwd:`, `AW:`, `SV:`, bracketed counters, lowercasing and collapsing whitespace for threading).
   - Implemented `derive_author` producing display name `author` and structured `from_addr` without angle brackets (§4.4, §12.2).
   - Implemented `extract_all_addresses` and `clean_msg_id` for `Message-ID`, `In-Reply-To`, and `References`.
7. **[src/structure.rs](file:///home/nvalk1/debloatmail/crates/extract/src/structure.rs)**:
   - Implemented recursive structure walk and RFC 3501 section numbering (`section_id`: `"1"`, `"2"`, `"1.1"`, `"2.1"`, etc.).
   - Implemented attachment vs. body classification (§6.1) and selection precedence (§6.2):
     - `multipart/alternative`: prefers `text/plain` if non-empty; falls back to `text/html`.
     - `multipart/mixed`: first body candidate is body; subsequent candidates become attachments (§6.3).
     - `multipart/encrypted`: placeholder `"[encrypted message]"` with `body_state = Cached`.
     - `multipart/signed`: first part is content, signature part discarded.
     - Opaque compressed `Content-Encoding` handling (warns, does not decompress, §7).
     - Magic bytes verification (`%PDF-`) for `is_pdf` (§5.2, §15).
     - Paragraph truncation for bodies exceeding 1 MB limit.
     - Excerpt derivation (§12.3: first 200 chars of first non-empty paragraph).
8. **[src/lib.rs](file:///home/nvalk1/debloatmail/crates/extract/src/lib.rs)**:
   - Implemented `CURRENT_EXTRACTOR_VERSION = 1` (§18).
   - Implemented `extract_feed_text` (§11) prioritizing `content` over `summary` and resolving relative links with `site_url`.

---

### 2. Cargo Test and Compilation Failures Encountered

During development and test execution in `crates/extract`, the following compilation and test failures occurred:

1. **`charset` Tuple Mismatch (`E0308`)**:
   - *Error*: `cs.decode(bytes)` returned a 3-tuple `(Cow<'_, str>, Charset, bool)` rather than `(Cow<'_, str>, bool)`.
   - *Fix*: Updated destructuring to `let (decoded, _, _) = cs.decode(...)`.
2. **`chardetng 1.0.0` API Changes (`E0061`, `E0599`)**:
   - *Error*: `EncodingDetector::new()` required argument `Iso2022JpDetection`, and `guess_assess` method did not exist.
   - *Fix*: Used `EncodingDetector::new(chardetng::Iso2022JpDetection::Allow)` and `detector.guess(None, chardetng::Utf8Detection::Allow)`.
3. **`html2text 0.17` Module Privacy (`E0603`)**:
   - *Error*: Attempted to import `text_renderer` which is private in `html2text 0.17`.
   - *Fix*: Imported `TaggedLine`, `TaggedLineElement`, and `TextDecorator` directly from `html2text::render::*`.
4. **`html2text` Image Mode Method Name (`E0599`)**:
   - *Error*: Called non-existent `.image_render_mode(...)` on `html2text::config::Config`.
   - *Fix*: Switched to `.empty_img_mode(ImageRenderMode::ShowAlways)`.
5. **`TaggedLineElement` Tag Vector Mismatch (`E0308`)**:
   - *Error*: `html2text` decorates lines using a tag stack, returning `TaggedLineElement::Str(ts)` where `ts.tag` is `&Vec<MailAnnotation>` instead of `&MailAnnotation`.
   - *Fix*: Updated line rendering to inspect the slice of annotations via `.iter().any(...)` and `.iter().find_map(...)`.
6. **`derive_author` Match Arm Typing (`E0308`)**:
   - *Error*: Incompatible types between match arms returning tuples vs empty unit `()`.
   - *Fix*: Bound match expression to `Option<(Option<String>, Option<String>)>` and returned early when matched.
7. **UTF-7 Test Character Mismatch**:
   - *Error*: `test_utf7_decode` panicked expecting `Hello ☺!` when `+JgM-` decodes to snowman `Hello ☃!`.
   - *Fix*: Corrected test assertion to expect `Hello ☃!`.
8. **Missing Empty Alt Images in HTML**:
   - *Error*: `<img src="bar.png">` with no alt text was dropped in `test_images_render`.
   - *Fix*: Configured `Config::empty_img_mode(ImageRenderMode::ShowAlways)` so `decorate_image` is triggered for all image elements.
9. **Whitespace Normalisation Indentation Loss in `<pre>`**:
   - *Error*: `test_pre_preserves_whitespace` failed because `out.trim()` stripped leading spaces from the first preformatted line.
   - *Fix*: Replaced `out.trim()` with `out.trim_matches(['\n', '\r'])` adhering to §10.5 Step 5 ("Strip leading and trailing blank lines").
10. **`Attribute` Struct Access in `mail_parser` (`E0308`)**:
    - *Error*: Treated `Attribute` as tuple `(k, v)` when it has fields `pub name: Cow<str>` and `pub value: Cow<str>`.
    - *Fix*: Accessed attributes via `a.name` and `a.value`.
11. **Trailing Newline in Plaintext Message Body**:
    - *Error*: `test_simple_plain_message` expected `"Hello World"` but received `"Hello World\n"`.
    - *Cause*: §10.5 whitespace normalisation was applied in `html.rs`, but plaintext messages were returning unwrapped text without stripping trailing blank lines.
12. **Relative Link Resolution in Feed Test**:
    - *Error*: `test_feed_text_content_preferred_over_summary` tested `href="/link"` against `site_url="https://example.com/site/"` expecting `https://example.com/site/link`, but standard origin-relative URLs starting with `/` resolve to `https://example.com/link`.

---

### 3. Steps Needed for Fixing Remaining Failures

1. **Apply §10.5 Whitespace Normalisation to Plaintext Bodies**:
   - In `crates/extract/src/structure.rs`, run `normalize_plaintext_whitespace(&unwrapped)` on plaintext bodies so leading/trailing blank lines and trailing newlines are stripped uniformly across both HTML and plaintext bodies.
2. **Reconcile Relative URL Resolution**:
   - In `crates/extract/src/lib.rs` unit tests, verify whether `href` is root-relative (`"/link"`) or directory-relative (`"link"`), aligning test assertions with RFC 3986 URL resolution rules.
3. **Clean Up Compiler Warnings**:
   - Remove unused variable `content_encoding` in `crates/extract/src/structure.rs`.
   - Run `cargo fmt --all` and verify `cargo clippy --workspace --all-targets -- -D warnings`.

---

### 4. Remaining Steps to Complete Phase 2A (`crates/extract`)

1. **Verify All Unit Tests**:
   - Ensure all 33 unit tests in `crates/extract` pass with zero failures.
2. **Add Golden MIME Fixtures in `tests/fixtures/mime/`**:
   - Per [docs/MIME-AND-TEXT.md §17](file:///home/nvalk1/debloatmail/docs/MIME-AND-TEXT.md#L865):
     - `simple-plain/` (8bit, quoted-printable, base64)
     - `alternative/` (text+html, html-only, empty-plaintext-with-html)
     - `flowed/` (format=flowed, DelSp=yes, space-stuffing, `-- ` separator)
     - `charsets/` (utf-7, iso-2022-jp, gb2312, mislabelled-utf8, BOM)
     - `multipart/` (mixed, related, nested, missing boundary)
     - `signed-encrypted/` (multipart/signed, multipart/encrypted)
     - `attachments/` (RFC 2231 filename continuations, inline cid:, text/plain with filename)
     - `limits/` (oversize body, deep nesting)
3. **Integration & Determinism Tests**:
   - Create integration test in `tests/` verifying determinism (identical input bytes produce byte-identical output) and RFC 3501 `section_id` round-tripping through `BODY.PEEK[section_id]`.
4. **CI Invariant Checks**:
   - Confirm `crates/extract` has zero I/O and runtime dependencies (only `core`, `mail-parser`, `charset`, `chardetng`, `html2text`, `thiserror`).

---

### 5. Next Steps Beyond Phase 2A

Per [docs/architecture.md §14](file:///home/nvalk1/debloatmail/docs/architecture.md#L395):
- **Phase 2B (`crates/feeds`)**: Single HTTP client via `reqwest` on `rustls` (no default features), feed parsing via `feed-rs`, conditional GET (`ETag`, `If-Modified-Since`), response size cap.
- **Phase 2C (`crates/engine`)**: Sans-I/O IMAP and SMTP state machines with pimalaya (`io-imap`, `io-smtp`), sockets and TLS in `engine/io.rs` and `engine/tls.rs`.
- **Phase 3 (`crates/cli` Validation Spike)**: Standalone CLI proving auth, sync, send queue, feed polling, extraction, and FTS5 search end-to-end before any UI/QML code.
