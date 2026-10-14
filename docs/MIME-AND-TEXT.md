# MIME and Text Extraction

How raw bytes become the rows in `items`, `item_text`, and `attachments`.

Every document in this project asserts "plaintext only" and none of them defines it.
That is the largest unaddressed engineering risk in the design, because it is where
correctness bugs live: a mail client that renders the wrong part, drops a charset, or
mangles `format=flowed` is wrong in a way no schema change can fix. This document
specifies the conversion precisely enough to be implemented and tested.

**Scope of authority.** This file owns the path from a byte buffer to stored text. It
does not own: the IMAP/SMTP protocol (`SYNC.md`), the search *query* language
(`DATA-MODEL.md` §10), feed-document parsing (`feed-rs`, in `crates/feeds`), HTTP
fetching (the project's single HTTP client, owned by `crates/feeds` per `GEMINI.md`),
row mapping and commits (`crates/store`), or composing a message from scratch (a future
`COMPOSE-AND-SEND.md`). The schema is `DATA-MODEL.md` revision 3; where this document
needs a schema field, §18 maps the dependency.

**This revision** fixes the items the review flagged against this document and
reconciles the pipeline with the verified DATA-MODEL v3 DDL: the pipeline's crate home
— `crates/extract`, not `core` (issues 1; `GEMINI.md`, `architecture.md` §2 invariant
4); `part_id` → `section_id` throughout (v3 DDL, `DATA-MODEL.md` §5); the two-sources-of-
truth contradiction on attachment rows (issues 14) — header-time `BODYSTRUCTURE` is
provisional, the body walk is the sole authority for final rows, and the rule's home is
`DATA-MODEL.md` §11.1; the header-time `item_text` anchor that makes never-opened mail
searchable (issues 7, `DATA-MODEL.md` §11.1); the reply-subject rule against
`subject_norm` (issues 6); `mail_items.from_addr` as the structured sender (issues 5);
the broken §10.4/§10.5 cross-references; and the stale "DATA-MODEL.md v2" citations.
§20 records where each landed and what remains owed to other documents.

---

## 1. Scope

**In scope:**

- Parsing a raw RFC 5322 message into parts.
- Selecting which part is "the body".
- Decoding headers, charsets, and transfer encodings.
- Converting HTML to readable plaintext.
- Unwrapping `format=flowed` text.
- Assigning `section_id`s (RFC 3501 §6.4.5 section numbers) and detecting attachments.
- Deriving `title`, `author`, `excerpt`, `published_at`, `sort_at`.
- The failure taxonomy: what happens to a message we cannot fully parse.

**What this document does not own**, stated once because the old text implied it: the
fetch (HTTP for feeds, IMAP for mail) is `crates/feeds` and `SYNC.md` respectively;
parsing the feed *document* (Atom/RSS/JSON detection) is `feed-rs` in `crates/feeds`;
mapping results to rows and committing them is `crates/store`; and the security rules
that wrap all of it are `SECURITY.md` (§5.4–5.6, §6).

---

## 2. The pipeline

```
raw bytes (IMAP BODY.PEEK[], SYNC.md §6.1 — never BODY[] — or an HTTP
          feed response via crates/feeds)
   │
   ├─ 1. Parse ──────────── MIME structure, headers, parts        (§3, §5)
   │
   ├─ 2. Decode headers ─── RFC 2047 / 2231 / 6532, addresses     (§4)
   │
   ├─ 3. Select body ────── which part is "the body"?             (§6)
   │
   ├─ 4. Transfer-decode ── base64 / quoted-printable             (§7)
   │
   ├─ 5. Charset-decode ─── bytes → UTF-8 String                  (§8)
   │
   ├─ 6. Text conversion ── format=flowed unwrap, or HTML→text    (§9, §10)
   │
   ├─ 7. Normalise ──────── whitespace, line endings, limits      (§10.5, §13)
   │
   └─ 8. Derive + store ─── title, author, excerpt, item_text     (§12)
```

Two properties of this pipeline matter more than the stages:

**It is a pure function.** `extract(bytes) -> ExtractedText`. No clock, no locale, no
network, no environment, no filesystem. The same input produces the same output
forever. This is what makes the golden-file tests in §17 meaningful.

**It is versioned.** The conversion logic *will* change — a new element mapping, a
fixed charset bug — and rows extracted by the old logic are then stale. The marker is
`item_text.extractor_version` (`DATA-MODEL.md` §5); the re-extraction mechanism is
§18.

**Where each stage lives.** Purity is a property of a *crate*, so the stages have a
split that must not blur:

| Stage | Runs in | Why |
|---|---|---|
| 1–7 parsing primitives (`mail-parser`, `charset`, `chardetng`, `html2text`) | `crates/extract` | The only home for those four crates (`GEMINI.md`; `architecture.md` §2 invariant 4): pure byte-slices-in, structures-out; no sockets, no filesystem, no async runtime, no database; depends on `core` **only** — for the types — and nothing else. |
| 1 structure walk (§5.2) and 3 selection policy (§6), as a driver | `crates/sync` | The sync engine walks `extract`'s parse output per fetched body (`SYNC.md` §2: `sync`'s pipeline *is* the §5.2 walk). `extract` does not depend on `sync` — the direction is the other way. |
| 8 derivation and row-write | `crates/store` (committed by the daemon) | `store` maps rows (`DATA-MODEL.md` §14) and computes `excerpt` at text-write time (§4.2.2). This document specifies the rules; it does not execute them. |
| The types the pipeline operates on | `crates/core` | Invariant 1: zero dependencies. `core` cannot hold a parsing library — that is the whole reason issue 1 existed, and the sentence in the earlier revision ("why `core` can hold the conversion logic with zero dependencies") was the error. `core` holds `Item`, `Attachment`, and friends; the logic lives in `extract`. |

---

## 3. The parsing stack

| Job | Crate home | Version | Why this one |
|---|---|---|---|
| MIME structure + charset decode | `extract` — **`mail-parser`** | 0.11.9 | RFC 5322 + MIME 2045–2049, decodes 41 charsets including UTF-7, zero-copy `Cow<str>` output |
| Charset → UTF-8 | `extract` — **`charset`** | latest | Wraps `encoding_rs` and adds the UTF-7 and JavaMail-label handling that email actually needs |
| Charset sniffing (last resort) | `extract` — **`chardetng`** | 0.1.x | Mozilla's detector; designed to share `encoding_rs` with no extra weight |
| HTML → text | `extract` — **`html2text`** | 0.17.1 | Servo's `html5ever` parser, MIT, configurable block/link rendering |
| Feed XML/JSON (document level) | `feeds` — **`feed-rs`** | latest | One model over Atom, RSS, and JSON Feed, auto-detecting format |
| Raw XML (feed internals) | `feeds` — via `feed-rs` | — | Do not add a second XML parser |

`mail-parser` v0.11.9 is current and conforms to RFC 5322, MIME (RFC 2045–2049), and
decodes 41 character sets including obsolete ones such as UTF-7. Its model follows
RFC 8621 §4.1.4: rather than handing back a nested tree, it exposes text body parts,
HTML body parts, and attachments — which is exactly the shape this pipeline wants. It
also performs automatic HTML↔text conversion when one alternative is missing —
**we disable that and do our own**, for the reason in §10.1.

**Why not `mailparse`.** It is a fine simple MIME parser, but it returns a nested part
tree and stops there: no charset table, no HTML conversion, no RFC 8621 projection.
Choosing it would mean rebuilding `mail-parser`'s two most valuable features on top of
it. It stays out of the dependency tree.

**Why `charset` and not `encoding_rs` directly.** `encoding_rs` is the right library
for web content, but its own documentation says to use `charset` for email, because
email requires UTF-7 decoding and legacy JavaMail charset labels that `encoding_rs`
does not carry. `charset` is precisely that wrapper: `encoding_rs` plus UTF-7
(RFC 2152) and additional `java.io`/`java.nio` names for JavaMail compatibility. Using
`encoding_rs` alone would mean hand-writing the UTF-7 case and the label table.

**Why `html2text` and not `mail-parser`'s converter.** `html2text` uses `html5ever`,
the Servo HTML parser, and renders to text or to annotated spans with configurable
block and link handling. That control is the point: we need a *specified* output
(§10), not a convenient one. Note its MSRV is 1.85.0 — pin the workspace MSRV at or
above that (the pinned toolchain in `rust-toolchain.toml`), or the build breaks on the
older toolchains the distro targets use.

**Pin exact versions, and treat this table as ask-first.** Same rule as the pimalaya
stack: these parsers see attacker-controlled input, so a version bump is a reviewed
change with a fixture run, not a `cargo update`. Per `GEMINI.md`, any dependency
*addition* to `crates/extract` (or `crates/core`) is an ask-first action — the
`Content-Encoding` codec question in §19.7 is exactly that class of change. CI
asserts the shape of the result: `extract` has no runtime or I/O dependency, and
`core` declares zero dependencies (`architecture.md` §13, `GEMINI.md` testing rules).

---

## 4. Header decoding

Headers are the first place a message lies. Every field below is decoded before use.

### 4.1 Encoded words (RFC 2047)

`=?charset?B?…?=` and `=?charset?Q?…?=` appear in `Subject`, `From`, `To`, `Cc`,
`Reply-To`, and filenames. Rules:

- Decode **before** any other processing, including before `subject_norm` is computed.
- Adjacent encoded words separated only by whitespace are concatenated *without* the
  whitespace (the RFC's rule, and the one implementations get wrong).
- An unknown charset inside an encoded word decodes to the raw bytes with replacement
  characters rather than failing the message.
- A malformed encoded word (missing `?=`) is left as literal text. It is better to show
  `=?utf-8?Q?broken` than to show nothing.

### 4.2 Parameters (RFC 2231)

Filenames and other parameters use the extended syntax: `filename*=utf-8''%E2%82%AC.txt`,
with `filename*0*`/`filename*1*` continuations for long values, `*` marking the encoded
form, `'` delimiting charset and language, and `%` percent-encoding.

Two rules that matter in practice:

- **Continuations must be joined before percent-decoding**, in numeric segment order,
  not string order (`*10` sorts after `*2`).
- When both `filename` and `filename*` are present, **`filename*` wins**. The plain
  parameter is the legacy fallback for clients that could not read the extended one.

### 4.3 Raw UTF-8 headers (RFC 6532)

A growing number of messages put raw UTF-8 in headers with no encoded word at all
(`SMTPUTF8`). Headers are therefore treated as **bytes**, not as ASCII: if a header
value is not valid UTF-8 after encoded-word decoding, run it through the same charset
pipeline as a body (§8) with the declared or sniffed charset. Never `String::from_utf8`
and error.

### 4.4 Folding and addresses

- **Unfold** first: a CRLF followed by WSP is a continuation, replaced by a single space.
- **Address parsing** extracts display name and address for `From`, `To`, `Cc`,
  `Reply-To`. Group syntax (`undisclosed-recipients:;`) is accepted and yields an empty
  address list, not an error.
- **The structured sender is stored, not just displayed.** The parsed `From` address —
  no display name, no angle brackets — is `mail_items.from_addr` (`DATA-MODEL.md` §5,
  index `idx_mail_from`), populated at header sync (§11.1). It backs the `from:` search
  operator (`DATA-MODEL.md` §10), reply prefill (`UI.md` §11.4, `to = reply_to else
  from`), and `ItemDetail.from`. A `From` that fails to parse leaves
  `from_addr = NULL`; `author` degrades per §12.2 — the exact case
  `DATA-MODEL.md` §4.1 points here for.
- **`From` with no display name** yields the address as the author. `From` that fails to
  parse yields the raw header text as the author, never `None` — see §12.2.

### 4.5 Subject normalisation

`subject_norm` strips `Re:`, `Re[2]:`, `Fwd:`, `FW:`, `AW:`, `SV:`, and the other common
localisations, collapses whitespace, and lowercases. It is used **only** for threading
(`DATA-MODEL.md` §8) and stored alongside `display_subject`, which is kept un-normalised
for display.

**Do not overwrite one with the other** — that is a real bug this rule exists to prevent
(`threads.display_subject` vs `threads.subject_norm`, `DATA-MODEL.md` §5; the confusable
pair is recorded in `GLOSSARY.md` §3). **And do not build reply subjects on
`subject_norm`.** Replies and forwards are built from the decoded, un-normalised subject
(`display_subject` / `title`), prefixed `Re: ` / `Fwd: ` — building on `subject_norm`
produces lowercase reply subjects (issues 6; `debloatmailctl.md` §8 compose builds on
the display title). `UI.md` §11.4 still builds `Re: + subject_norm`; the correction is
recorded in §20.

---

## 5. Structure walk and part addressing

### 5.1 `section_id` is the IMAP section number, not an array index

This is the single most important implementation detail in this document.

`attachments.section_id` (renamed from `part_id` in `DATA-MODEL.md` v3 §5; the column
is `TEXT NOT NULL` under `UNIQUE (item_id, section_id)`) must be the value usable in
`UID FETCH <uid> BODY.PEEK[<section_id>]`, because that is exactly how
`FetchAttachment` retrieves the bytes (`debloatmaild.md` §6, `SYNC.md` §6.2). That
value is the RFC 3501 §6.4.5 body section number:

| Structure | Section numbers |
|---|---|
| Non-multipart message | the body is `1` |
| `multipart/*` | parts numbered `1`, `2`, … in order |
| Part inside a part | `1.1`, `1.2`, `2.1` … |
| An embedded `message/rfc822` at part 2 | its body is `2.1`, its header is `2.HEADER` |
| A part's MIME header | `1.MIME` |

`mail-parser` returns parts in its own ordering. **Our own structural walk assigns the
RFC 3501 numbers.** A `section_id` that does not round-trip through
`BODY.PEEK[section_id]` is a silent bug that only appears when a user clicks an
attachment — which is to say, late.

**Who is the authority, when.** The earlier text said, unconditionally, "that walk is
the authority." That asserted one authority too many, because there is a second,
earlier attachment list. The precise split (the formulation in `DATA-MODEL.md` §11.1,
implemented in `SYNC.md` §5.5/§6.1):

- **Header time — provisional.** The server's `BODYSTRUCTURE`, fetched with the headers
  (`SYNC.md` §5.5), is parsed by `crates/extract` and seeded through the *same* §5.1
  numbering, producing **provisional** `attachments` rows and `has_attachments` so the
  attachment list is visible without downloading a single body. `BODYSTRUCTURE` can
  mislead — mis-declared `Content-Disposition`, bodystructures the server summarizes.
- **Body time — final.** The walk over the actually-fetched bytes is the **sole
  authority for final attachment rows and for body selection**.
- **Direction is one-way.** When the two disagree, the walk wins, and that is the only
  direction in which the list may change (§6.1 below and `SYNC.md` §6.1 state the
  reconciliation procedure; this document states the *rules the walk applies*,
  `DATA-MODEL.md` §11.1 is the home of the *rule*).

The numbering must be identical at both times, or the reconciliation has nothing to
join on: the provisional rows carry `section_id`s the body walk can match.

### 5.2 The walk

Recursive, depth-first, with a hard depth cap (§13). At each node:

1. Read `Content-Type`, defaulting to `text/plain; charset=us-ascii` when absent
   (RFC 2045 §5.2).
2. Read `Content-Disposition`, defaulting to `inline` when absent.
3. Classify the part (§6).
4. If it is a container (`multipart/*`), recurse into its children.
5. If it is `message/rfc822`, record it as an attachment and **do not recurse into it as
   body**. An attached email is a file, not this message's content.
6. Otherwise emit a leaf with its section number (`section_id`), MIME type
   (`mime_type`, never empty), decoded filename (`filename`, when present),
   `Content-ID` (`content_id`), and the part size when the server or the bytes make it
   known (`size_bytes`). `is_pdf` is set **provisionally from the declared type** at
   header time and re-derived from **magic bytes** (`%PDF-`) when the part's bytes are
   fetched — the declared type is what the sender claims; §15's rule about magic bytes
   is what makes that second pass load-bearing.

### 5.3 Parameters

`boundary`, `charset`, `name`, `filename`, `format`, `delsp` are all decoded through
§4.2. A `multipart/*` with a missing or unparseable `boundary` is not fatal: the walk
records the failure and treats the part as opaque. Messages with missing boundaries are
common enough that failing on them would drop real mail.

---

## 6. Body selection

Selection is a local decision over the parsed tree. A `BodyFetch` fetches the **whole
message** — `UID FETCH <uid> BODY.PEEK[]` (`SYNC.md` §6.1) — stores the raw MIME as a
blob, and only then runs the walk and this selection. There is no part-by-part body
fetch: per-section fetching (`BODY.PEEK[<section_id>]`) exists for `AttachmentFetch`
only (`SYNC.md` §6.2, `debloatmaild.md` §6), which addresses parts by
`(item_public_id, section_id)`.

### 6.1 Is it an attachment?

Evaluate in order; the first match wins.

| # | Condition | Verdict |
|---|---|---|
| 1 | `Content-Disposition: attachment` | **Attachment** |
| 2 | A `filename`/`name` parameter is present | **Attachment** |
| 3 | Type is `multipart/*` | Container — recurse |
| 4 | Type is `message/rfc822` | **Attachment** |
| 5 | Type is `text/calendar` | **Attachment** (an invite is a file) |
| 6 | Type is `text/plain` or `text/html` | **Body candidate** |
| 7 | Type is `text/*` (other) | **Attachment** (e.g. `text/x-patch`) |
| 8 | Type is `application/*`, `image/*`, `audio/*`, `video/*`, … | **Attachment** |

Rule 2 catches the common case of a `.txt` file attached to a message: it has a
filename, so it is a file, even though its type is a body-candidate type. This is the
rule that prevents a message body being replaced by an attached log file.

`multipart/related` images are attachments with a `content_id`, never body text. They
are what `cid:` references point at, and since we do not render HTML, they are only ever
things a user opens deliberately.

**The refinement rule** (stated here because it decides what the walk's classification
means for the database; executed by the `BodyFetch` path, `SYNC.md` §6.1): after the
walk completes, the attachment list is reconciled against the header-time provisional
rows of §5.1 —

- parts the walk does not produce: rows are removed;
- parts the walk produces that had no row: rows are added;
- parts the walk produces that have a row: updated with the authoritative size,
  `content_type`, and `section_id`.

The walk wins, and that is the only direction in which the list may change. This is the
split recorded in `DATA-MODEL.md` §11.1 — header-time rows are the pre-fetch list a
thin cache must show without a body; the walk is the authority for final rows — and it
is what makes "📎 may change after a body fetch" a *specified correction*, not a
second source of truth.

### 6.2 Which candidate is "the body"?

| Case | Rule |
|---|---|
| `multipart/alternative` with `text/plain` | Use the `text/plain` part. **Never** convert the HTML as well. |
| `multipart/alternative` with only `text/html` | Convert the HTML (§10). |
| `multipart/alternative` with neither | No body; `body_state = 'cached'` with empty text (§14). |
| `multipart/mixed` | The body is the *first* body candidate; the rest are attachments. |
| `multipart/report` (DSN/MDN) | The `text/plain` part, i.e. the human-readable half. |
| `multipart/signed` | The **first** part is the content; the signature part is discarded. |
| `multipart/encrypted` | No body. `body_state = 'cached'`, text is the placeholder in §14. |
| Nested containers | Recurse; the first body candidate in document order wins. |

**Preferring `text/plain` is not laziness.** When a sender provides both, the plaintext
version is the one they *chose to write*, including its quoting and its line breaks.
Converting the HTML instead throws away their formatting decisions and can only add
artefacts. The HTML is the fallback, not the default.

**But a `text/plain` part that is empty or whitespace-only is not a body.** Some senders
ship an empty `text/plain` alternative alongside real HTML. Treat
whitespace-only plaintext as absent and fall through to the HTML. This single rule fixes
a class of "the message appears blank" bug reports.

### 6.3 Multiple body candidates at the same level

`multipart/mixed` with two `text/plain` parts (a message plus a forwarded message inline)
takes the first. The second is recorded as an attachment. Concatenating them would
invent a message that was never sent.

---

## 7. Transfer decoding

`Content-Transfer-Encoding` is applied before charset decoding, and is per-part.

| Encoding | Handling |
|---|---|
| `7bit`, `8bit`, `binary` | Identity. These are not encodings; they are declarations about the channel. |
| `quoted-printable` | Decode `=XX` and soft line breaks (`=` at end of line). |
| `base64` | Decode, ignoring all whitespace and newlines. |
| absent | Identity. |
| anything else | Treat as identity and record a warning. |

Lenient by policy:

- A base64 body with trailing garbage decodes up to the garbage.
- A `=XX` with invalid hex is emitted literally as `=XX`.
- A QP soft break at the very end of the body is ignored.
- A truncated base64 stream decodes to whatever complete quantum it contained.

The alternative — strict decoding — means a message with one bad line loses its entire
body. Lenient decoding means it loses one character. For a mail reader, the second is
obviously right.

**`Content-Encoding` (RFC 2046) is a different header, and in beta we do not decode
it.** `gzip`, `deflate`, `x-compress`, and `x-identify` compressed parts are treated as
opaque: the part is recorded, its bytes are not decompressed, and the message proceeds
with a warning (§14). This is deliberate, not an oversight: it is why §15 can honestly
say "no decompression in this pipeline," and why the §13 size caps apply to bytes we
actually hold. Decoding would require a codec dependency in `crates/extract` —
ask-first per `GEMINI.md` — and is an open question (§19.7).

---

## 8. Charset decoding

Bytes become a Rust `String` (UTF-8) through this order:

1. **The declared charset**, resolved through the `charset` crate's label table, which
   includes the Encoding Standard labels plus the JavaMail names.
2. **BOM detection**, which overrides a contradicting declaration. A UTF-8 BOM on a
   `charset=iso-8859-1` part means the declaration is wrong.
3. **UTF-8 validation.** If the bytes are valid UTF-8, they are UTF-8, whatever the
   header claims. This catches the very common mislabelled-UTF-8 case.
4. **`chardetng`** on the first ~1 KiB. This is the last resort and it is genuinely
   last: a detector's guess is worse than a correct declaration and better than a wrong
   one.
5. **`windows-1252`**, the historically most likely single-byte encoding.

Invalid byte sequences never fail the part. They decode to U+FFFD and processing
continues. A message with one bad byte is still a message.

**Charsets we must handle even though they are obsolete:** `utf-7` (email-only, which is
why `charset` exists), `iso-2022-jp`, `gb2312`/`gbk`, `big5`, `koi8-r`,
`windows-1251`, `iso-8859-*`. `mail-parser` covers 41 charsets natively; `charset`
covers the rest of the label space.

**The charset of the output is always UTF-8.** No stored text is ever in another
encoding, and no downstream stage needs to know what the original was.

---

## 9. Plaintext bodies: `format=flowed`

A `text/plain` part with `format=flowed` is *not* ready to display. It encodes soft line
breaks as a trailing space, protects leading `>` and space with space-stuffing, and may
signal that the trailing space is significant via `DelSp=yes`. Displaying it
raw produces the characteristic ragged text that gives `format=flowed` its bad
reputation.

The unwrap, per RFC 3676:

1. If `format` is not `flowed`, skip all of this. Leave the text alone.
2. Remove space-stuffing: a line beginning with a space has that space removed, unless
   it is part of the quoted-text indicator.
3. A line whose last character is a space is **flowed**: join it to the next line. If
   `DelSp=yes`, drop that trailing space before joining; otherwise keep it as a literal
   space.
4. A line ending without a trailing space is a hard break.
5. **The signature separator `-- ` is never flowed**, even when it has a trailing space.
   This is a known interoperability trap and one implementations get wrong.
6. Quote indicators (`>`, `>>`) are preserved and are part of the quote depth, not
   content.

RFC 2646 is the predecessor of RFC 3676 and is still encountered; treat `format=flowed`
identically regardless of which specification the sender followed.

For a non-flowed part, the only transformation is line-ending normalisation (§10.5).
Whitespace in a fixed-format message is meaningful and is not touched.

---

## 10. HTML to text

This is the conversion that decides whether the client is usable, because most mail is
HTML-only.

### 10.1 Why not `mail-parser`'s automatic conversion

`mail-parser` will convert HTML to text for you when the alternative is missing. That
is convenient and we do not use it. A conversion whose output is not specified cannot be
tested, and its output is not stable across versions of the library — which would
silently invalidate every golden file in §17 on a dependency bump. We call
`body_html()` for the decoded HTML and run it through `html2text` with the configuration
below.

### 10.2 Configuration

| Setting | Value | Reason |
|---|---|---|
| Parser | `html5ever` (via `html2text`) | Spec-compliant, fuzz-hardened, no external resource loading |
| Render mode | Plain, no annotations | We store text, not colour spans |
| Width | Large enough to disable wrapping | See §10.5 |
| Link rendering | Custom (§10.4) | The default is not what a mail reader should show |
| CSS | Ignored | No stylesheet, no `style` attribute, no external CSS |
| `<script>`/`<style>` | Dropped | Never contributes text |

### 10.3 Element mapping

| Element | Output |
|---|---|
| `p`, `div`, `section`, `article`, `figure`, `figcaption` | Block: blank line before and after |
| `br` | Single newline |
| `h1`–`h6` | Block, text on its own line, blank lines around. **No `#` markers.** |
| `ul` / `ol` | Block; items prefixed `- ` / `N. `, nested items indented 2 spaces per level |
| `li` | One line (plus continuations) |
| `blockquote` | Each line prefixed `> `, nested as `> > ` |
| `pre` | Verbatim. No reflow, no whitespace collapse. |
| `code` | Text unchanged, no backticks |
| `a` | Anchor text; URL appended per §10.4 |
| `img` | `[image: alt]`, or `[image]` with no alt |
| `hr` | `---` |
| `table` | One line per row; cells joined with two-spaced pipes; no decorative grid |
| `dl` / `dt` / `dd` | Term line, then the definition indented 2 spaces |
| `strong`, `em`, `b`, `i`, `sup`, `sub` | Text unchanged. **No `*` or `_` markers.** |
| `script`, `style`, `head`, `title`, `noscript`, `template`, `iframe`, `object`, `embed`, `form`, `input`, `button`, `select` | Dropped entirely |
| `video`, `audio` | `[media]` |
| Comments | Dropped |

Two deliberate omissions worth defending:

- **No emphasis markers.** `*bold*` in stored text pollutes search, looks like markdown
  the sender never wrote, and makes the text ambiguous when it is later quoted in a
  reply. Emphasis is presentational and we are storing text.
- **No hard wrapping.** Wrapping is destructive: a stored 72-column wrap is wrong at
  every other width, and it corrupts copy-paste. The reader wraps; the database does not.
  This is the one place where `html2text`'s default must be actively overridden, since
  it is designed to render at a target width.

### 10.4 Link handling

For `<a href="…">text</a>`:

| Condition | Output |
|---|---|
| `text` equals the URL | the URL |
| `href` is `mailto:x@y` | `text <x@y>`, or just `x@y` when they match |
| `text` is empty | the URL |
| otherwise | `text <URL>` |

The URL is emitted inline rather than collected into a footnote block. Footnotes require
matching anchors to positions in a text that has been reflowed and truncated, which is
fragile; inline URLs survive copying, quoting, and truncation intact.

Relative URLs are resolved against the message's base — the feed's `site_url` for feed
content. For mail, a relative URL is emitted as-is; guessing a base from headers would
invent a fact.

### 10.5 Whitespace normalisation

After conversion, applied to both HTML-derived and plaintext bodies:

1. Normalise CRLF and lone CR to LF.
2. Collapse runs of spaces and tabs within a line to one space — **except inside `pre`
   content**, which is protected.
3. Strip trailing whitespace on each line.
4. Collapse three or more consecutive newlines to two (one blank line).
5. Strip leading and trailing blank lines.

Step 4 is a judgement call: a sender who leaves ten blank lines probably meant a
paragraph break, and preserving their exact count makes the stored text full of
arbitrary vertical space that interacts badly with list rendering.

### 10.6 Worked examples

**Input** (HTML-only, common newsletter shape):

```html
<div><p>Hello <b>Alice</b>,</p>
<blockquote><p>Can you review this?</p></blockquote>
<p>See <a href="https://example.com/pr/42">the PR</a>.</p></div>
```

**Output:**

```
Hello Alice,

> Can you review this?

See the PR <https://example.com/pr/42>.
```

**Input** (a `format=flowed` body):

```
This line flows onto the next
one and ends here.
-- 
No flowed signature.
```

**Output:**

```
This line flows onto the next one and ends here.
-- 
No flowed signature.
```

---

## 11. Feed content

Feeds go through the same HTML converter, because most feeds publish HTML in
`content` or `summary`. The split with the rest of the feed path: the *fetch*
(conditional GET, redirects through the SSRF hook, the 10 MB cap) is `crates/feeds`'s
job (`GEMINI.md`, `SYNC.md` §7.1, `SECURITY.md` §5.4); the *document parse* (Atom/RSS/
JSON Feed detection) is `feed-rs`, also in `crates/feeds`; this document owns only the
byte→text conversion of each item's content and summary fields, via §10 in
`crates/extract`.

| Source field | Handling |
|---|---|
| `content` present | Preferred. If HTML, convert via §10; if plain text, use directly. |
| `summary` only | Used as the body. This is the "summary-only feed" case; see §19.4. |
| Both | `content` wins. |
| Neither | Empty body, `body_state = 'cached'`, `durable = 1`. |

**Resolution of relative URLs** uses the feed's `site_url` as the base for links inside
the content. This is the one place a base URL is known with confidence.

**Feed content is durable** (`DATA-MODEL.md` §9.1): extracted text is stored
permanently, never evicted, because the article is frequently gone from the feed by the
time a user looks for it. This document does not change that; it only guarantees that
what is stored is complete.

**Identity and state preservation are not ours.** Matching on `(feed_id, guid)` with
the `link`/content-hash fallback, and the upsert's duty to touch content columns only —
never `unread`, `starred`, `hidden_at`, `body_state`, `thread_id`, `created_at`
(`DATA-MODEL.md` §11.2, §11.4) — is `SYNC.md`'s upsert. This document's obligation ends
at producing complete, correctly-derived fields.

**XML safety.** `feed-rs` auto-detects XML vs. JSON and Atom vs. RSS and presents one
model. Its XML layer must be configured (or verified) to refuse **DTD entity
expansion** — a feed is attacker-controlled input, and a billion-laughs expansion is a
one-line denial of service. No external entity may ever be fetched. Per
`SECURITY.md` §6.2/§16, "verified" means **a fixture, not a review note**: a
billion-laughs payload and an XXE payload must both be in `tests/fixtures/` and must
fail cleanly rather than exhaust memory, so a dependency bump that silently re-enables
entity expansion is caught by a test, not by a reader.

---

## 12. Derived fields

Two write times, one rulebook. At **header sync** the daemon writes the `item_text`
row with `title` and `author` set and `body = ''`, plus the `mail_items` header fields
including `from_addr` (`DATA-MODEL.md` §11.1) — this anchor is what makes
never-opened mail searchable (§16). At **body fetch** (mail) or **ingest** (feeds),
`body` is filled, `excerpt` is derived, and the attachment rows are reconciled (§6.1).
The rules below apply at whichever time the field is first produced; a later write
updates the field, it does not re-decide it from scratch.

### 12.1 `title`

| Source | Rule |
|---|---|
| Mail | Decoded `Subject`; `"(no subject)"` when absent or empty |
| Feed | Entry title → entry link → `"(untitled)"` |

Never empty, per `DATA-MODEL.md` §4.2.1: a list row needs a label and every renderer
inventing its own fallback is how a UI ends up inconsistent. `title` is a `String` in
the projection; the DB column stays nullable, the projection is not.

### 12.2 `author`

| Source | Rule |
|---|---|
| Mail | `From` display name → `From` address → raw `From` header text |
| Feed | Entry author → feed title |

`author` is `Option<String>` in the ABI but this pipeline produces a value whenever the
header exists at all, even unparseable. Showing `=?utf-8?B?…?=` beats showing nothing.

**`author` is the display name; `from_addr` is the address.** They are different fields
with different consumers (`DATA-MODEL.md` §4.1, §5): a `From` with no display name
makes `author` the address text *and* `from_addr` the bare address; a `From` that fails
to parse makes `author` the raw header text and `from_addr` `NULL`. Never derive one
from the other after the fact.

### 12.3 `excerpt`

The first 200 characters of the **first non-empty paragraph** of the extracted body,
with whitespace collapsed and trailing punctuation preserved.

- "Paragraph" means a block separated by a blank line.
- If the first block is shorter than 200 characters, do not continue into the next one.
  A short first line is a signature-style opening, and concatenating the next paragraph
  produces a run-on that reads worse than the short version.
- For an evicted or unfetched body, `excerpt` is `NULL`. It is *not* re-derived from the
  subject; a list showing subjects in the excerpt column is confusing.
- This document specifies the rule; **`store` computes it when the text row is written**
  (`DATA-MODEL.md` §4.2.2), not at read time, so the list query stays a single indexed
  scan. It is not computed in `extract` — `extract` does not depend on `store`.

### 12.4 `published_at` and `sort_at`

| Kind | `published_at` | `sort_at` |
|---|---|---|
| Mail | `Date` header, decoded and parsed to UTC | `INTERNALDATE` (stored in `mail_items.internal_date`); falls back to `Date`, then `created_at` |
| Feed | entry `published` | entry `published` → `updated` → fetch time |

`sort_at` is `NOT NULL`; every fallback chain ends at `created_at`
(`DATA-MODEL.md` §4.2.3). Date parsing is lenient: RFC 5322 format first, then a small
set of known-broken variants. An unparseable `Date` yields `NULL` for `published_at` —
never a guess, because a wrong date sorts a message into the wrong place and is worse
than no date.

---

## 13. Limits

Every limit exists because a message or feed is attacker-controlled and this program
runs continuously in the background.

| Limit | Default | On breach |
|---|---|---|
| Max raw message size | 25 MB | Header stored; `body_state = 'failed'`; not parsed |
| Max MIME nesting depth | 20 | Stop recursing; record a warning |
| Max parts per message | 500 | Remaining parts ignored |
| Max extracted body size | 1 MB | Truncated at a paragraph boundary; warning |
| Max attachment count | 100 | Remaining parts ignored |
| Max header line length | 8 KB | Truncated |
| Max single header value | 64 KB | Truncated |
| HTML parser node budget | 100k nodes | Conversion aborted; warning |
| Feed response size | 10 MB | Fetch rejected before parse (`SYNC.md` §7.1, `SECURITY.md` §5.4) |

Truncation at a paragraph boundary rather than mid-word is deliberate: a truncated body
is shown to a human, and cutting mid-sentence looks like corruption while cutting at a
paragraph looks like a long message.

These are configuration keys with these defaults, not compile-time constants, because
the correct value for a 200k-message account is not the correct value for a test
fixture.

**The attachment *size* cap is not here.** Whether there is one at all is an open
question owned by `DATA-MODEL.md` §15.1 / `SYNC.md` §16.1 (current answer: no cap by
default; if configured, the engine enforces it). It does not belong in this table.

---

## 14. Failure taxonomy

The governing rule: **one unparseable message must never fail a sync.** A mailbox
contains decades of malformed mail; a client that stops on the first one is unusable.

`body_state` values and exactly what sets them (tokens per `DATA-MODEL.md` §14;
transitions per `SYNC.md` §6.1):

| State | Meaning | Set when |
|---|---|---|
| `none` | No body stored locally, fetchable | Initial state; also after eviction (`DATA-MODEL.md` §9.2) |
| `pending` | A fetch is in flight | `FetchBody` starts |
| `cached` | Extraction ran; `item_text` is authoritative | Extraction completed, **including when the result is empty** |
| `failed` | Extraction could not run | Fetch failed, size cap exceeded, or the message is unparseable |

The `none`/`cached` distinction carries real information: `none` means "fetch it if you
want it", `cached` with an empty body means "we fetched it and there was genuinely no
text". Without this distinction the UI cannot tell "loading" from "empty".

Specific outcomes:

| Situation | Result |
|---|---|
| Missing boundary | Part treated as opaque; other parts still processed |
| No body candidate | `cached`, empty text, `has_attachments` set |
| `multipart/encrypted` | `cached`; body text is `"[encrypted message]"` |
| Unknown transfer encoding | Identity decode, warning |
| `Content-Encoding: gzip`/`deflate` part | Part opaque, not decompressed; warning (§7, §19.7) |
| Unknown charset | Sniffed per §8, warning |
| Unparseable date | `published_at = NULL`, message still stored |
| Malformed header block | Headers best-effort; body still attempted |
| Message exceeds size cap | `failed`; header-only item |

Warnings are logged with the item's `public_id` and the failing stage. They are not
stored per-row in beta (§19.2), but `doctor` reports a count so a user can see whether
their mailbox is parsing cleanly.

**A `failed` body is not a `failed` item.** The item exists, appears in the list, and
can be starred, moved, and deleted. Only the body is missing. Conflating the two would
hide real messages from the user because of a parsing bug.

---

## 15. Security

This pipeline is the project's largest attacker-facing surface. The mitigations are
structural, not incidental; `SECURITY.md` §6.1 states the guarantee table and §6.3–6.4
state the model around it.

| Threat | Mitigation |
|---|---|
| HTML renderer exploit | There is no renderer. HTML becomes a string and the parser is discarded. |
| XXE / billion laughs | DTD and external entity resolution disabled in the XML layer; both attacks have **fixtures** (§11, `SECURITY.md` §6.2) |
| External resource fetch | No parser has a socket or a filesystem. `cid:`, `http:`, and `file:` are strings (`SECURITY.md` §5.6) |
| Path traversal via filename | `extract` produces the decoded filename and nothing else — it has no filesystem (`GEMINI.md`). The component that *persists* the file (`store`/daemon) applies the sanitisation: basename only, no `..`, no leading `.`, no NUL, length-capped (`SECURITY.md` §8.2) |
| MIME-type confusion | `is_pdf` is decided by **magic bytes** (`%PDF-`), not by the declared type — header-time rows are provisional (§5.2 step 6); fetched bytes settle it |
| Decompression bomb | No decompression in this pipeline (§7); size caps in §13 apply to bytes we actually hold |
| ReDoS | No user-supplied regex runs against message content |
| Resource exhaustion | Every limit in §13 |
| Encoding trickery | Output is always UTF-8; no encoding is ever passed through to a consumer |

**Two properties to preserve as invariants, and to test for:**

1. **No HTML ever reaches the UI.** The `item_text.body` column contains no `<`-tagged
   markup from the source. A future "view source" feature reads the raw blob, never the
   converted text.
2. **The converter is pure.** If a test finds `html2text` or `mail-parser` opening a
   file, a socket, or a thread, the test fails. Purity is what makes the security
   argument auditable. This is a CI assertion, not a convention: `extract` has no
   runtime or I/O dependency (`architecture.md` §13, `GEMINI.md`).

**Fuzzing is required, not optional.** Two `cargo-fuzz` targets in `crates/extract`:
one on `extract()` with raw bytes, one on the HTML converter with arbitrary HTML. The
corpus is the fixture set in §17 plus any crash found in the wild. This is the one part
of the codebase where fuzzing is cheaper than reasoning. The feed XML/JSON path is a
*third* target, owned by `crates/feeds` (`SECURITY.md` §6.4) — it is outside `extract`
and this document does not claim it.

---

## 16. Search implications

What this pipeline produces is exactly what FTS5 indexes (`DATA-MODEL.md` §10):

| Indexed | Not indexed |
|---|---|
| `title`, `author`, `body` from `item_text` | Attachment contents (no PDF extraction; §19.6) |
| HTML-derived text | Raw HTML |
| Quoted replies | — |

**Every message is searchable by title and author from first sync, whether it was ever
opened.** Header sync writes the `item_text` row — `title`, `author`, `body = ''`
(`DATA-MODEL.md` §11.1, §9.2) — so the FTS-indexed `title`/`author` columns exist for
every message, not only for bodies that were once fetched. Eviction empties `body`; it
retains `title` and `author` and the row itself.

**Body search covers cached bodies only** (`DATA-MODEL.md` §9.2, §10). A phrase that
appeared in an evicted mail body returns nothing — the price of the thin cache, and the
correct price. `doctor` reports the ratio of `body_state = 'none'` items so a user can
see how much mail is currently body-unsearchable.

**`from:` is a structured operator, not full-text** — it matches
`mail_items.from_addr` (indexed), is mapped to SQL by `store` rather than passed to
FTS5, and is part of the closed operator set in `DATA-MODEL.md` §10: positional terms,
`title:`, `author:`, `from:`, `is:mail`, `is:feed`. This document produces the values
those operators run over; it does not define them.

**Quoted text is indexed.** It is real content and excluding it requires a reliable
quote-stripping heuristic, which this project does not have and which would silently
change search results when it changed. The consequence is that a widely-quoted paragraph
matches many messages. That is honest behaviour, and it is an open question in §19.1
whether to revisit.

---

## 17. Determinism and testing

**The contract:** identical input bytes produce byte-identical `item_text` output, on
every platform, forever, until the extractor version changes (§18). No locale, no clock,
no network, no hash-map iteration order in the output. `core` and `extract` must be
testable with no network at all — byte slices in, structures out, no filesystem
(`architecture.md` §13, `GEMINI.md`).

| Layer | Approach |
|---|---|
| Unit | Per-stage: header decode, QP/base64, charset, flowed unwrap, each element mapping |
| Golden files | `insta` snapshots of full `extract()` output for every fixture |
| Fixtures | `tests/fixtures/mime/` — see the categories below |
| Property | Truncation never panics; output is always valid UTF-8; output never contains a tag |
| Fuzz | `cargo-fuzz` on `extract()` and on the HTML converter (§15) |
| Adversarial | Deep nesting, huge headers, missing boundaries, 0-byte parts, base64 garbage |
| XML safety | Billion-laughs and XXE payloads; both must fail cleanly, memory bounded — fixtures, not assumptions (§11, `SECURITY.md` §6.2) |

Fixture categories (each a directory, each with a golden file):

```
tests/fixtures/mime/
  simple-plain/          8bit, quoted-printable, base64 variants
  alternative/           text+html, html-only, empty-plaintext-with-html
  flowed/                format=flowed, DelSp=yes, space-stuffing, -- separator
  charsets/              utf-7, iso-2022-jp, gb2312, mislabelled-utf8, BOM
  multipart/             mixed, related, nested, missing boundary
  signed-encrypted/      multipart/signed, multipart/encrypted
  attachments/           filename (RFC 2231), continuations, inline cid:, text/plain-with-filename
  headers/               RFC 2047 adjacent words, raw UTF-8 headers, group syntax
  content-encoding/      gzip/deflate parts — the opaque path, asserted, not assumed (§7)
  malformed/             truncated base64, bad QP, no headers, binary garbage
  limits/                oversize, deep nesting, many parts
```

**Never commit real email or real feed content** (`architecture.md` §13, `GEMINI.md`).
Fixtures are synthesised or anonymised, and the corpus is reviewed before commit.

**Round-trip is a fixture, not an assumption.** At least one fixture must drive the
§5.1 walk and assert that every emitted `section_id` is a legal
`BODY.PEEK[section_id]` value — including a nested `multipart` and an embedded
`message/rfc822` (its body `2.1`, its header `2.HEADER`).

---

## 18. Schema dependencies and re-extraction

| Ask (v2-era) | v3 location | Note |
|---|---|---|
| `item_text.extractor_version INTEGER NOT NULL DEFAULT 1` | §5 DDL (the DDL comment points back here) | Taken verbatim from this document's §18 (`DATA-MODEL.md` §16, row 7) |
| `attachments.part_id` → `section_id` | §5 DDL: `section_id TEXT NOT NULL`, `UNIQUE (item_id, section_id)`; `DATA-MODEL.md` §3 cites this document §5.1 as the authority on the value's shape | This revision applies the rename in its own text (§5.1, §6.1, §15) |
| (with SYNC's ask) `item_text.title`/`author` `NOT NULL DEFAULT ''` + the header-time row | §5 DDL, §11.1, §9.2 | What makes §16's "searchable from first sync" true |
| (DM ask 1) `mail_items.from_addr` + `idx_mail_from` | §5 DDL + index, §10, §11.1 | Backs `from:` search, reply prefill, `ItemDetail.from` |

**No further schema change is requested.** `body_state`, `hidden_at`, `durable`,
`has_attachments`, and `is_pdf` all exist in v3 and cover the outcomes in §14.

**Re-extraction.** The version marker exists for one reason: when the conversion logic
changes (a fixed `format=flowed` case, a new element mapping), rows produced by the old
logic are stale, and `doctor` can count them (`extractor_version != current`). The fix
runs offline: for a mail item whose raw MIME blob is still stored
(`mail_items.raw_blob` is set), re-run this pipeline over the blob and rewrite the
`item_text` row — no network. Scope, stated precisely:

- **Mail, blob present** — re-extractable offline.
- **Mail, blob evicted** (`raw_blob` NULL) — re-extraction means re-fetching the body
  (`BODY.PEEK[]`); it is a `BodyFetch`, not a parse.
- **Feeds** — no raw blob is kept (text is the content, §9.1's durable row), so feed
  rows are re-extractable only by re-fetching the feed, which may be impossible. Feed
  text is therefore only re-derived if the feed still serves the article.

What *triggers* the pass — version bump, idle, or never automatically — is open;
`SYNC.md` §16.4 owns the question (it is a housekeeping job, `SYNC.md` §3.1).

---

## 29. Verification checklist

Each item is a test, fixture, or CI assertion — not a review note.

- [ ] No `part_id` identifier anywhere in this document's pipeline; every part address
      is a `section_id` (RFC 3501 §6.4.5) usable verbatim in `BODY.PEEK[section_id]`,
      including in the D-Bus signatures (`DATA-MODEL.md` §17).
- [ ] The §5.1 round-trip fixture passes: nested `multipart` and an embedded
      `message/rfc822` (body `2.1`, header `2.HEADER`) emit only legal section numbers.
- [ ] Every new mail item has an `item_text` row from header sync (`title`/`author`
      set, `body = ''`); a never-opened message is found by both a subject search and an
      author search; eviction empties `body` but leaves `title`/`author` indexed
      (`DATA-MODEL.md` §9.2, §11.1).
- [ ] After a `BodyFetch`, the attachment list equals the walk's output exactly:
      header-time-only rows removed, walk-only rows added, shared rows updated; a row
      removed by the walk is never restored from a later header-time pass (walk-wins is
      the only direction, `SYNC.md` §6.1).
- [ ] `body_state` transitions match the DDL `CHECK` and `SYNC.md` §6.1 exactly; a
      crash leaves no `pending` behind (startup sweep, `SYNC.md` §11.1).
- [ ] No `<`-tagged markup appears in any `item_text.body`; the converter is proven
      pure (no file, socket, or thread) by test; CI asserts `extract` has no runtime or
      I/O dependency and `core` declares zero dependencies (`architecture.md` §13).
- [ ] The billion-laughs and XXE feed fixtures fail cleanly with bounded memory; an
      external-entity payload fetches nothing (`SECURITY.md` §6.2, §17).
- [ ] A `Content-Encoding: gzip` part is stored via the opaque path with a warning —
      never decompressed (§7).
- [ ] `extractor_version` bump: `doctor` reports the stale count; re-extraction from a
      stored mail blob is offline and byte-identical for unchanged inputs (§17, §18).
- [ ] Every fixture category in §17 exists with a golden file; two runs produce
      byte-identical output.
- [ ] `from:` search resolves against `mail_items.from_addr`; reply prefill is
      `to = reply_to else from`; a reply subject is never lowercased (§4.4, §4.5 — the
      last item flips when `UI.md` §11.4 lands).
- [ ] `is_pdf` for a `Content-Type: application/pdf` part whose bytes are *not*
      `%PDF-` is corrected at fetch time, not trusted at header time (§5.2, §15).
