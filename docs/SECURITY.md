# Security

What `debloatmail` is defending, what it is not, and the specific controls that make
"plaintext only" more than a slogan.

**Scope of authority.** This file owns the threat model and the security-relevant policy
that spans components. It does not re-specify: the D-Bus signatures (`debloatmaild.md`),
the extraction pipeline (`MIME-AND-TEXT.md`), the sync engine (`SYNC.md`), the schema
(`DATA-MODEL.md` revision 3), or the UI (`UI.md`). Where those documents already state a
control correctly, this one cites it rather than restating it. Where they state one
wrongly, or have not yet stated it, §16 says so, with a status on each row.

---

## 1. Scope and non-goals

**In scope:** the threat model; the trust boundaries; credentials and the keyring; TLS and
the loopback exception; the parsing surface; injection of every kind; the filesystem and
the blob store; supply chain; sandboxing and service hardening; privacy; disclosure.

---

## 2. The threat model

### 2.1 Assets

| Asset | Why it matters | Primary control |
|---|---|---|
| **Account credentials** | Full mailbox access; often reused | §9 |
| **Message confidentiality** | The contents of the user's mail | TLS (§5), no remote fetch (§7) |
| **The server-side mailbox** | An integrity failure deletes mail the user cannot restore | `BODY.PEEK`, `UID EXPUNGE`, no plain `EXPUNGE` (`SYNC.md` §5.5, §8.3) |
| **Local feed content** | Frequently *not* re-fetchable — a feed rotates | `durable = 1`, never evicted (`DATA-MODEL.md` v3 §9) |
| **The user's disk and RAM** | A parser or a fetch that exhausts either is a denial of service | `MIME-AND-TEXT.md` §13 limits, §8 |
| **The user's trust that "plaintext" is literal** | The entire security argument rests on it | §7 |

### 2.2 Adversaries, in priority order

| # | Adversary | In scope | Primary controls |
|---|---|---|---|
| **A1** | **A remote sender** — attacker-controlled MIME, HTML, charsets, filenames | **Yes — the primary adversary** | §6, `MIME-AND-TEXT.md` §15 |
| **A2** | **A remote feed server** — attacker-controlled HTTP, XML, JSON, redirects | **Yes** | §5.4 (SSRF), §6.2 (XXE) |
| **A3** | **A network attacker** — MITM on the path | **Yes** | §5 (TLS, no cleartext, no downgrade) |
| **A4** | **A same-user local process** — malware, a rogue script, a sandboxed app | **Partially** | §4 — bounded, not solved |
| **A5** | **A malicious dependency** | **Yes** | §11 |
| **A6** | **A compromised provider** | **No** | Out of scope; TLS only |
| **A7** | **A remote attacker with no local foothold** | **Yes** | The parser and the network path are the only reachable surface |

**A1 is why the project is written in Rust.** `architecture.md` §3 gives the real reason
and it is not performance: the program parses attacker-controlled input continuously, in
the background, as the user. Memory safety is the mitigation, and it is structural rather
than a checklist item.

### 2.3 The design rule that follows

> **Nothing attacker-controlled may reach a renderer, an interpreter, a shell, or a
> network client.**

Every control in this document is an instance of that rule. HTML becomes a string (§6.1).
Filenames become basenames (§8.2). URLs become strings until a human clicks one (§7).
A feed URL becomes an HTTP request only after redirect validation (§5.4).

---

## 3. Trust boundaries

```
┌─────────────────────────────────────────────────────────────────────┐
│  user session                                                       │
│                                                                    │
│   ┌──────────────┐   D-Bus (session bus)   ┌───────────────────┐   │
│   │ debloatmail  │◄───────────────────────►│ debloatmaild      │   │
│   │ (GUI)        │      §4 boundary        │ (daemon)          │   │
│   └──────┬───────┘                         └──────┬────────────┘   │
│    │ SQLite RO                         │          │ §8 fs          │
│    │                        ┌───────────▼────────┘ │               │
│   ┌──────▼───────┐         │                      │               │
│   │ mail.db      │  XDG_   │   ┌──────────────┐   │               │
│   └──────────────┘  DATA_  │   │ crates/      ├───┘               │
│      (shared, WAL)   HOME  │   │ extract      │ ← daemon-side     │
│                            │   │ mail-parser, │   parsers         │
│                            │   │ html2text,   │                   │
│                            │   │ feed-rs      │                   │
│                            │   └──────────────┘                   │
│   ┌──────────────┐         ┌──────────────┐   │                  │
│   │ poppler/mupdf     │   path  │ XDG_CACHE_   │   │                  │
│   │ ← GUI-side   │         │ HOME         │   │                  │
│   │   parser     │         └──────────────┘   │                  │
│   └──────────────┘                            │                  │
│   ┌──────────────┐   any same-user process    │                  │
│   │ debloatmailctl│  ← peer on the same bus   │                  │
│   └──────────────┘    (§4)                    │                  │
└───────────────────────────┬───────────────────┴──────────────────┘
                            │ §5 boundary (TLS)
                  ┌─────────▼──────────┐
                  │ IMAP / SMTP / HTTPS │  ← A1, A2, A3 live here
                  └────────────────────┘
```

| Boundary | Crosses | Enforcement |
|---|---|---|
| **Session bus** (§4) | Method calls and signals between peers | Bus policy only; **no authentication between same-user processes** |
| **Network** (§5) | IMAP, SMTP, HTTPS | TLS with verification, with one loopback exception; **one HTTP client, one redirect policy** (§5.4) |
| **Parser** (§6) | Raw bytes → stored text | Purity, limits, no renderer, no sockets — daemon side in `crates/extract`; the **only** GUI-side parser is QtPdf (§6.3) |
| **Filesystem** (§8) | Data, cache, blob store | Unix permissions + hash-addressed paths |
| **ABI** (`architecture.md` §10) | Rust ↔ QML/Kotlin | Type-erased ids; Qt may live only in `ui/bridge` (`GEMINI.md`), so `core`, `ffi`, `store`, `engine`, `feeds`, `extract`, `auth`, `sync` and both Rust binaries are Qt-free — **including the daemon, which is why it cannot parse PDFs** |

---

## 4. The session bus — and why "point-to-point" is not a security boundary

`debloatmaild.md` §17 opens with the correct premise: **the session bus is per-user and
provides no authentication between that user's applications.** It then draws one
conclusion that does not follow.

### 4.1 The correction

The current spec says — verified in this revision's re-read of the sentence, which is
still present:

> Secrets accepted only as method arguments — *Point-to-point delivery; the bus daemon
> does not broadcast method calls.*

The first clause is true and the justification is not. **The session bus permits a
same-user process to observe traffic it is not addressed in.** The D-Bus daemon
documentation states that the well-known session bus normally allows receiving any
message, including eavesdropping, and dbus has deliberately not
configured the session bus to forbid the legacy eavesdropping mechanism, for
backwards-compatibility reasons. The modern replacement,
`org.freedesktop.DBus.Monitoring.BecomeMonitor`, is the recommended way to monitor a bus — and on the session bus it is available to any same-user client.

**Consequence:** a password passed as `config.secret` in `Accounts.Add(config)` — or as
`secret` in the rotation path `Accounts.Update` (§9.3) — is visible to any same-user
process that is monitoring the session bus. Passing it as an argument is still *better*
than putting it in a property — `Properties.GetAll` is a single call any process can
make, whereas a method call must be observed in flight — but "the bus does not broadcast"
is the wrong reason, and a design that relies on it is relying on something that is not
true.

**What replaces the wrong reason:** the argument-passing rule is a *minimisation* rule,
not a confidentiality rule. It limits the exposure to a brief window and to a caller
that has to be watching, rather than leaving the secret resident in a readable property.
The confidentiality of a secret on the session bus is **not** achievable, and the design
should not claim it is.

### 4.2 What follows for each rule in `debloatmaild.md` §17

Audited against the current text of that section in this revision.

| Rule | Verdict |
|---|---|
| No method returns a secret | **Keep.** Still correct, and now load-bearing — a return value is trivially observed. |
| No property holds a secret | **Keep.** `Properties.GetAll` is one call. |
| Secrets only as method arguments | **Keep, restated.** Necessary, not sufficient. The reason is minimisation, not confidentiality (§4.1 is still owed to that document). |
| `Send` attachments are validated as regular, readable files | **Keep and extend** (§8.3). The current §17 note still says "oracle"; the FIFO case is a denial of service, and the symlink and size-cap cases are missing. |
| `Move`/`Copy` targets are parsed as UUIDs and resolved to folders server-side | **Keep.** The earlier name-validation framing is gone — there is no name-to-IMAP-path interpolation left to inject. The codec rule (§7.2) remains the first-layer defence for the named commands that do still exist. |
| `FetchAttachment`'s `section_id` must parse as an RFC 3501 §6.4.5 dot-list and match an existing `attachments` row of the item | **Keep.** Both halves matter: the dot-list check blocks section-specifier injection; the row-match makes the daemon useless as a part-existence oracle. §8.2 restates it as the boundary rule. |
| All public ids parsed as UUIDs before use | **Keep.** |

### 4.3 Integrity: the daemon cannot tell our GUI from malware

Every mutating method is callable by any same-user process:

| Method | Abuse |
|---|---|
| `Items.Delete(ids, expunge=true)` | **Destroys mail on the server.** Not recoverable. |
| `Accounts.Remove(account_id, purge=true)` | Destroys an account's cached data and feed archive. (And must delete the keyring entry — §9.3.) |
| `Compose.Send(draft, append)` | Sends mail as the user. |
| `Accounts.Add(config)` | Adds an attacker-controlled account. |
| `Accounts.Update(..., secret)` | Rotates credentials an attacker has guessed or intercepted. |
| `Sync(scope, full=true)` | Forces a full refetch — a resource-exhaustion lever. |

The row this table used to end with — `UI.Compose(to, subject, body)` opening a
pre-filled compose window, a spoofing vector — is **gone with the method itself**
(§10.4). It cannot be fixed on a session bus, and once `mailto:` routed through
`org.freedesktop.Application.Open` it had no legitimate caller left; deletion was the
whole solution.

**There is no way to distinguish the legitimate caller, and no polkit.** This is stated
plainly because a mitigation that only *looks* like one is worse than an acknowledged gap:

- **Claiming a bus name does not help.** Any same-user process can request
  `org.noahie.debloatmail`, and the GUI's name is not a credential.
- **`GetConnectionCredentials` / `GetConnectionUnixProcessID`** (`org.freedesktop.DBus`)
  can tell the daemon the caller's PID and UID. That is useful for **logging** — a
  `doctor` line saying "last mutating call came from PID 4821 (`/usr/bin/foo`)" — but not
  for **authorization**, because the attacker controls its own executable path and can
  race a PID. Use it for auditability, never as a gate.
- **A private socket instead of the session bus** would give real access control
  (filesystem permissions on the socket), at the cost of `DBusActivatable`, `mailto:`
  routing, and desktop integration. **Rejected** — the integration is the point of the
  design, and the attacker model (same-user malware) is one where the user has already
  lost.
- **A confirmation for destructive verbs** helps against accidents and against a careless
  caller, not against a determined one. `Delete(expunge=true)` and
  `Accounts.Remove(purge=true)` should require an explicit confirmation in the *GUI*
  regardless — the same reason `debloatmailctl` requires `-y` when stdin is not a TTY
  (`debloatmailctl.md` §8).

**The honest summary:** on a per-user session bus, a same-user attacker can drive this
application. The design goal is therefore not to *stop* that — it is to ensure the daemon
does not *amplify* it: no shell execution (§7.3), no filesystem traversal (§8.2), no
secret disclosure to a caller who did not already have it (§9), no unbounded resource
use (§8.3), and no second network path by which to skip the one that is validated
(§5.4).

### 4.4 Flatpak narrows this, and that is a real benefit

When packaged as a Flatpak (`architecture.md` §12), the sandbox proxies the session bus,
and a sandboxed peer can only reach names it has been granted via `--talk-name`, and can
only claim names granted via `--own-name`. **That is genuine access control the session
bus does not otherwise provide** — a sandboxed app that was not granted
`--talk-name=org.noahie.debloatmail.Daemon` cannot call the daemon at all.

The flip side: the daemon must then be granted what it needs, and the manifest is a
security-relevant artifact that must be reviewed like code. See §12.

### 4.5 The residual risk, stated once

A same-user process can read the user's mail, delete it, send as them, and observe
credentials in flight. It could equally read `~/.local/share/debloatmail/mail.db`
directly. **The D-Bus surface does not create a new class of exposure** — it is the same
data behind a different door — and that is the standard against which §4.3's mitigations
are judged: they must not make the door *wider* than the filesystem it fronts.

---

## 5. The network

### 5.1 TLS is not optional

`SYNC.md` §5.1 specifies the lifecycle; the security requirements are:

| Requirement | Rationale |
|---|---|
| **Implicit TLS by default** — 993 (IMAP), 465 (submission) | RFC 8314 deprecates cleartext access and submission and recommends implicit TLS on dedicated ports. |
| **STARTTLS re-issues `CAPABILITY`** | The pre-TLS capability list is untrusted and can be tampered with. |
| **`imap_security = 'none'` is refused for any non-loopback host** | Cleartext IMAP transmits the password in the clear. Rejected at **account-validation** time, not at connect time, so the config cannot sit dormant and then connect. |
| **Certificate chain and hostname are verified** | The default. There is no global "insecure" switch. |
| **Credentials are sent only after TLS is established** | AUTH over cleartext is a credential leak. |

### 5.2 The loopback exception, and its one invariant

`architecture.md` §7 and `debloatmaild.md` §7 both scope the Proton Bridge exception, in
a way this document endorses:

> `allow_invalid_cert_loopback = 1` permits an invalid certificate chain **only** when
> the resolved peer address of the *connected socket* is `127.0.0.1` or `::1`.

**The invariant: check the connected peer address, never the configured hostname.** A
hostname is attacker-influenceable through DNS, `/etc/hosts`, or a search domain. A check
on the name is a check the attacker controls; a check on the socket's peer address is a
check on the socket.

Two further requirements:

- **The exception is per-account**, not a daemon-wide flag (`architecture.md` §7 is
  explicit: do not build a general "ignore invalid certificates" switch).
- **`doctor` and `Accounts.Check` must report it explicitly** (`debloatmaild.md` §7,
  `debloatmailctl.md` §8; the wizard surfaces the row, `UI.md` §12.1). Silently tolerating
  a bad certificate is the failure mode that turns a Proton convenience into a security
  bug, and the report is what makes it visible.

### 5.3 No downgrade

STARTTLS is vulnerable to stripping by an active network attacker. The client's
obligation:

1. If `imap_security = 'starttls'` and `STARTTLS` fails or is not advertised, **abort**.
   Never fall back to cleartext.
2. After `STARTTLS` completes, re-issue `CAPABILITY` and discard the pre-TLS list.
3. If a TLS handshake fails for any reason other than the scoped loopback exception,
   abort and back off (`SYNC.md` §10).

### 5.4 Feed fetching is an SSRF surface — and it has exactly one hook

**A feed URL is user-supplied, and the daemon fetches it from the user's machine.** That
is a server-side-request-forgery primitive. The design fact that makes it containable
exists now: **there is exactly one HTTP client in the project** — `reqwest`, built with
no default features and `rustls-tls` only, owned by `crates/feeds` (`GEMINI.md`: the
stack line and the "never open HTTP or TLS outside `crates/feeds`" boundary). Every feed
GET therefore flows through one `Client`, and the redirect validation below is a **custom
redirect policy on that client** — the policy sees each redirect target before the
request is made, and it is where the per-feed loopback opt-in is consulted. There is no
second client, in a dependency or a shim, to bypass the policy with.

| Risk | Mitigation (all enforced in the one redirect policy) |
|---|---|
| A redirect to `http://127.0.0.1:...` probes local services | Re-validate the target of every redirect; refuse a redirect whose host resolves to loopback, link-local (`169.254.0.0/16`), or the cloud metadata address (`169.254.169.254`). |
| A redirect to a private RFC 1918 address probes the LAN | Same rule; refuse private ranges unless the original feed URL was already in one (a user may legitimately subscribe to a LAN feed). |
| A redirect chain that bounces forever | Cap at 5 hops (`SYNC.md` §7.1). |
| A scheme downgrade `https → http` | Refuse; a feed that was HTTPS should not silently become cleartext. |
| A non-HTTP scheme (`file:`, `gopher:`, `ftp:`) | Refuse at URL parse, before any request; only `http` and `https` are fetchable. |
| A slow-loris server holding the connection | Connect and read timeouts, and `SYNC.md` §7's size cap (10 MB) on the decoded body. `reqwest`'s `gzip`/gzip-decode is not a bypass: the cap is on the **decoded** body. |

**The default posture:** refuse loopback and link-local targets *by default*, because a
feed pointing at `127.0.0.1` is far more likely to be an attack than a legitimate
subscription. A user who genuinely hosts a feed on their own machine can be given an
opt-in, per-feed, matching the `allow_invalid_cert_loopback` pattern. This document
defines the policy; `SYNC.md` §7 drives the fetch; the implementation is the policy
closure inside `crates/feeds` (§16 records the cross-reference as owed).

### 5.5 Feeds over cleartext

An `http://` feed is integrity- and confidentiality-exposed: a network attacker can
inject articles. We **allow** it (feeds are a public, low-value target and http is still
common) but we **warn at `feed add` time** and report the count in `doctor`. Refusing
outright would break real subscriptions; silently accepting would hide a real risk. The
wizard's outcome table (`UI.md` §12.3) is where the warning and the `validated: false`
note surface; the policy is here.

### 5.6 No remote resources from message content

This is the rule that makes reading mail safe, and it is enforced in two places:

- **The parser never has a socket** (`MIME-AND-TEXT.md` §15; `crates/extract` has **no
  network capability at all** — `GEMINI.md` makes its dependency closure a CI-asserted
  invariant). `cid:`, `http:`, and `file:` are strings.
- **The GUI never fetches a referenced resource** (`UI.md` §8.3). No `<img>`, no web
  font, no favicon, no tracking pixel.

**This is a privacy control as much as a security one.** A client that loads remote
images tells the sender, in real time, that the message was opened, from which IP
address, and when — the mechanism behind read receipts and tracking pixels. Not loading
them is why this client does not send read receipts by accident.

---

## 6. The parsing surface

`MIME-AND-TEXT.md` §15 is the authority and states the mitigations. This section states
the *model* around them and adds the parsers §15 does not cover.

### 6.1 What the pipeline guarantees

| Guarantee | Consequence |
|---|---|
| **There is no HTML renderer** | HTML is *input to a converter*; the parser is discarded. The output contains no markup. |
| **The converter is pure** | `crates/extract`: byte slices in, structures out — no clock, locale, network, filesystem, async runtime, thread, or database (`GEMINI.md` convention; `architecture.md` §2). Purity is what makes the security argument auditable — and testable with no network at all (§18). |
| **Output is always UTF-8** | No encoding is passed through to a consumer. |
| **Every limit is finite** | `MIME-AND-TEXT.md` §13. The daemon runs continuously; an unbounded parse is a background denial of service. |
| **No execution** | No macro, script, formula, or entity is ever evaluated. |

### 6.2 XML: the second parser

`feed-rs` is a *different* parser from `mail-parser` and is not covered by §15. A feed
is attacker-controlled input, and the two classic XML attacks apply:

- **XXE** — external entity resolution must be disabled. No external entity may ever be
  fetched.
- **Billion laughs** — DTD entity expansion must be disabled. A one-line expansion is a
  trivial denial of service.

`MIME-AND-TEXT.md` §11 requires the XML layer to be configured (or verified) to refuse
DTD entity expansion. **Verified is the operative word** — and the new MIME revision
makes it a **fixture**: the hostile payload is a test that must fail cleanly, run in CI,
so a dependency bump that silently re-enables entity expansion is caught by a test rather
than by review (§16 marks the cross-reference closed).

### 6.3 PDF is a third parser — GUI-side only, and the least controlled

**PDF viewing is in scope** (`architecture.md` §11), which means the **GUI** parses
attacker-controlled PDFs through **QtPdf / PDFium**. And the daemon does none of it.

**The daemon never parses a PDF.** It is Qt-free — Qt may appear only in `ui/bridge`
(`GEMINI.md`), and the daemon is a Rust binary that links zbus, not Qt — so it has no
PDF library and none in its dependency closure. Its two PDF duties are fetching the blob
(`FetchAttachment`, `debloatmaild.md` §6) and letting GC reclaim it (§8.4). The parse,
and the biggest memory-unsafe component in the stack, lives in exactly one process: the
one that already renders on screen. (An earlier draft of this document said "the daemon
and GUI parse through QtPdf" — that sentence was wrong, and `UI.md` §20 records it as a
debt of this document; §17 closes it.)

`architecture.md` §11 itself flags that the PDFium snapshot bundled with QtPdf **has
historically lagged upstream security fixes**. The controls:

| Control | Requirement |
|---|---|
| **Disable embedded JavaScript and actions** | A PDF's JS must not run. Verify the QtPdf configuration; do not assume the default. |
| **Disable external resource loading** | A PDF must not fetch a URL. Same rule as §5.6 — and note it makes sense only where the fetch could actually happen: the GUI, not the daemon. |
| **Treat PDFium as untrusted** | It is a memory-unsafe C++ parser operating on hostile input. It is the single highest-risk component in the stack. |
| **Have a swap path** | A poppler/mupdf backend behind the same interface, so a PDFium CVE can be mitigated without a redesign. |
| **Monitor the lag** | Track PDFium advisories against the QtPdf-bundled snapshot; if the gap is material, prefer system PDFium/poppler. |
| **Keep the daemon Qt-free** | A CI assertion that `debloatmaild`'s linkage contains no Qt library. The moment the daemon links Qt, PDF parsing — and every other Qt attack surface — moves into the always-running process, and this section's "GUI-side only" premise silently dies. |

**PDF text is never extracted for search** (`MIME-AND-TEXT.md` §1), which removes an
entire class of parser exposure from the indexing path. It is not removed from the *view*
path, which is why the controls above matter.

### 6.4 Fuzzing is a security control, not a nice-to-have

`MIME-AND-TEXT.md` §15 makes two `cargo-fuzz` targets required. This document endorses
that and adds: **a fuzz target for the feed XML/JSON path**, because §6.2's parser is
attacker-facing and is not covered by the two existing targets (§16, ask 9). The corpus
is the fixture set plus any crash found in the wild. Rust fuzzing of a C++ parser
(PDFium) is out of this program's reach; that is the "treat as untrusted" row — the
mitigation is the swap path, not the fuzzer.

---

## 7. Injection

### 7.1 SQL

- **Parameterised statements everywhere.** No query is built by string concatenation.
- **Ids are parsed as UUIDs before use** (`debloatmaild.md` §17). `PublicId` has a
  validating `FromStr` (`DATA-MODEL.md` v3 §14); an id that does not parse is rejected at
  the ABI boundary, before it reaches a query.
- **FTS5 query construction is a small language of its own.** `DATA-MODEL.md` v3 §10 and
  `debloatmailctl.md` §9 both require it to be *built*, not interpolated, and to return
  **zero results** on malformed input rather than an error or a panic. It needs
  adversarial tests (`debloatmailctl.md` §12).
- **Extension loading is off.** `rusqlite` does not enable `load_extension` by default;
  this must stay off, because a loaded extension is arbitrary code in the writer.
- **Readers open `SQLITE_OPEN_READ_ONLY`** (`debloatmaild.md` §1). This is a security
  control, not just a correctness one: it means a compromised GUI or CLI cannot corrupt
  the store.

### 7.2 IMAP and SMTP command injection

The mutation surface shrank in this revision: `Move`/`Copy` now take a **folder
`public_id`, parsed as a UUID and resolved server-side to a folder**
(`debloatmaild.md` §17, current text), so there is no folder *name* on those paths and
no name-to-IMAP-path interpolation to inject. That removes the primary historical
vector rather than defending it.

The rule that remains is the first-layer one, for the named commands that still exist:

> **Every IMAP and SMTP command is produced by the codec. No `format!` produces a wire
> command.**

The stack is `imap-codec` (via the pimalaya coroutines), and a codec *encodes*: a mailbox
or message identifier containing `"` or CRLF is serialised as a quoted string or a
literal, not spliced into a line. Any later validation (e.g. rejecting a name that cannot
round-trip) is a *second* layer. This is the same reasoning as using a parameterised
query rather than escaping a string — the primary defence is not interpolating at all,
and `format!` on a wire command is the thing a code-review gate must refuse.

**SMTP has its own injection form — header injection.** A `Subject` or display name
containing CRLF can inject additional headers or a body. The compose path builds RFC
5322, and the encoder **must reject or strip CRLF in every header field**, and must not
permit a `Bcc` to leak into the transmitted headers.

### 7.3 The CLI

- **Secrets are never accepted on the command line** (`debloatmailctl.md` §8) — they leak
  into shell history and `ps`. Passwords come from a prompt, `--secret-stdin`, or
  `--secret-command`.
- **`--secret-command` must be executed directly, not through a shell.** If the
  implementation runs it via `sh -c`, a crafted config file becomes command execution.
  Use `exec` with an argument vector.
- **`--debug` protocol traces redact credentials** (`debloatmailctl.md` §3 says so; this
  document makes it a test — §18).
- **`--dry-run` must not perform I/O.** It prints what would happen; a dry run that
  connects is a dry run that can leak.

---

## 8. Filesystem

### 8.1 Permissions

| Path | Contents | Mode |
|---|---|---|
| `$XDG_DATA_HOME/debloatmail/` | `mail.db` + `-wal` + `-shm` — **durable feed content** | `0700` dir, `0600` files |
| `$XDG_CACHE_HOME/debloatmail/blobs/` | Raw MIME, fetched attachments — evictable | `0700` dir, `0600` files |
| `$XDG_CONFIG_HOME/debloatmail/config.toml` | Preferences only — **never a secret** | `0600` |
| `$XDG_STATE_HOME/debloatmail/` | Logs, last-sync state | `0700` dir |

**The daemon must set these modes explicitly, not rely on the umask.** A user with a
permissive umask (`0002`) would otherwise get a world-readable mailbox. Create the
directory with `0700` and the files with `0600` at open time.

**The database is in `DATA`, not `CACHE`** (`debloatmailctl.md` §4). This is a
*durability* requirement with a security consequence: putting it in `CACHE` means a
cache wipe silently destroys the user's feed archive — the exact silent data loss
`DATA-MODEL.md` v3 §9 exists to prevent.

### 8.2 Path traversal

- **The blob store is hash-addressed** — `blobs/<hash[0:2]>/<hash>` (`DATA-MODEL.md`
  v3 §9). The path is derived from a BLAKE3 hash, not from a filename or a user string,
  so it is traversal-safe **by construction**. This is why the layout is worth stating
  as a security property and not just a storage detail.
- **Attachment filenames are sanitised on save** (`MIME-AND-TEXT.md` §15): basename
  only, no `..`, no leading `.`, no NUL, length-capped. A filename is attacker-controlled
  and arrives from `Content-Disposition` or an RFC 2231 parameter.
- **`section_id` is validated at the boundary** to the full daemon §17 rule: it must
  parse as an RFC 3501 §6.4.5 dot-list **and** match an existing `attachments` row of
  the item. It is used verbatim as an IMAP section, not as a path, but a crafted value
  must not be able to escape a future use — validate at the boundary, not at the use
  site. (Renamed from `part_id` with the schema, `DATA-MODEL.md` v3 §5; the daemon's
  validation rule already carries both halves.)
- **`Send` attachment paths are validated** (`debloatmaild.md` §17): regular files,
  readable, **inside the user's home**. Extend the rule (§8.3): refuse symlinks (or
  resolve and re-check the target), refuse paths under `/proc`, `/sys`, `/dev`, and
  enforce a size cap.

### 8.3 `Send` attachments: the spec understates this one

`debloatmaild.md` §17 says the daemon should "refuse non-regular files (FIFOs, device
nodes) and paths outside the user's home, because a mail client that will cheerfully
attach `/dev/urandom` or `/proc/self/environ` is a bug regardless of who asked." That
current text still frames the risk as an "oracle," and it is worth stating what happens
without the full rule:

| File | Without validation |
|---|---|
| A FIFO | The daemon **blocks forever** reading it — a one-line denial of service, and the daemon does it *on purpose* because the caller asked. |
| `/dev/zero` | Unbounded read; memory or disk exhaustion. |
| A 40 GB file | The daemon reads it into memory to base64 it — OOM. |
| `/proc/self/environ` | The user's environment, including any secret that leaked into it, is **mailed to the attacker**. |
| A symlink to any of the above | The check must be on the **resolved** target. |

So the requirement is not just "regular file" but: **regular, not a symlink (or resolved
and re-checked), readable, inside `$HOME`, and under a size cap**, with the FIFO case
stated as it is — indefinite daemon block, i.e. DoS, not oracle. The size cap should
exist even if the attachment-**fetch** limit (`DATA-MODEL.md` v3 §15, open question)
does not: sending is not the same as receiving.

### 8.4 Blob GC, and a leak that would have been a DoS

The old daemon spec said `FetchAttachment` "pins the blob's `refcount` on return" and
specified **no release method**. `DATA-MODEL.md` v3 §9 replaced this with a one-hour
`last_used_at`-based grace window and no pin (`SYNC.md` §6.2; the daemon's §6 now tells
the same story, which is why this row is marked closed in §16). The reason it lost is
security-relevant: an unpinnable, unreleasable refcount grows monotonically, so every
fetched attachment becomes permanently un-collectable — unbounded disk growth driven by
an attacker who sends mail with attachments.

**The remaining requirement:** GC eligibility must be enforced on the **server side**
(in the daemon), never delegated to a client that has already exited. The grace window
covers the gap between a path being returned and a file being opened; a client that
wants to *keep* a file copies it out of the cache (`UI.md` §8.6).

### 8.5 Temp files and the standalone lock

- Any temporary file (a draft written to disk for `$EDITOR`, an exported message) is
  created `O_EXCL | O_NOFOLLOW` with `0600`.
- The `--no-daemon` lock file (`debloatmailctl.md` §10) lives in the state directory and
  is opened with `O_NOFOLLOW` so a symlink planted in advance cannot redirect the lock
  or the write.

---

## 9. Credentials and the keyring

### 9.1 The model

```
config.toml  ──► preferences only, never a secret
accounts.secret_ref  ──► an opaque handle, never the secret
Secret Service  ──► the secret itself
```

`GEMINI.md` pins the implementation — **`keyring` or `oo7` over the Secret Service
portal, and never `io-keyring`** (deprecated; it is also in the never-use list) — and
the config homes: `config.toml` (preferences), SQLite (accounts and sync state), the
keyring (secrets), QSettings (GUI-local view state). `DATA-MODEL.md` v3 §5 stores only
`secret_ref`.

### 9.2 The keyring is storage, not access control

**The Secret Service has no per-application access control.** Once the user's collection
is unlocked — which it is for the whole login session, because the desktop unlocked it —
any same-user process can read any item in it. So:

> The keyring protects secrets **at rest** (they are not in a plaintext file, not in the
> database, not in shell history) and **in transit to disk**. It does **not** protect
> them from a same-user process, and must not be described as doing so.

This is consistent with §4.5: the same-user attacker who can read the keyring can also
read the database. The keyring's job is to keep the secret out of places it would
otherwise *leak* — a config file that gets committed, a backup, a `ps` listing, a log.

### 9.3 Rules

| Rule | Where |
|---|---|
| The secret is never in `config.toml` | `debloatmailctl.md` §4; the four-homes rule, `GEMINI.md` |
| Only `secret_ref` is in the database | `DATA-MODEL.md` v3 §5 |
| No method returns a secret; no property holds one | `debloatmaild.md` §17 |
| Never on `argv`; prompt, `--secret-stdin`, or `--secret-command` | `debloatmailctl.md` §8 |
| Never in a log, including `--debug` traces | §7.3 |
| **Prefer the daemon reading from the keyring over receiving the secret again** | §4.1 — a re-transmission is a fresh exposure window |
| **Secret rotation is the one sanctioned re-transmission** | New in this revision, with `Accounts.Update` (`debloatmaild.md` §7): the call accepts `secret` when the stored one no longer works (a rejected password is otherwise a dead end; `UI.md` §12.4 wires the **Fix credentials** flow to it). The §4.1 window logic applies to it exactly as to `Add`; the daemon rewrites the keyring entry, and the auth error clears on the next successful validation. |

**One requirement still open:** `Accounts.Remove(purge=true)` must also **delete the
keyring entry** for the account. An orphaned credential survives account deletion, is
discoverable by any same-user process, and is never rotated (§16, ask 1).

### 9.4 Memory hygiene

Rust `String` is not zeroed on drop, and a secret can be copied by the allocator, by a
`clone()`, or into a `Vec` during base64. The requirement:

- Hold secrets in a `secrecy::SecretString` (or `zeroize`-backed equivalent) so they are
  zeroed on drop and cannot be printed by accident (`Debug` is redacted by construction).
- Do not `clone()` a secret; pass by reference into the auth call and drop it there.
- **This is a hardening measure, not a boundary.** A same-user process can read the
  daemon's memory if it can ptrace it, and it can read the keyring regardless (§9.2).
  Its value is reducing the window and preventing accidental leakage into logs and core
  dumps.

### 9.5 OAuth (post-beta), designed now

`architecture.md` §7 defers OAuth but requires the architecture to accommodate it
(`GEMINI.md` likewise: architecture planned, implementation deferred until engine + UI
skeleton are done; `io-oauth` is already in the stack list). The security requirements,
so they are not retrofitted:

- **The loopback redirect listener binds `127.0.0.1` only** — never `0.0.0.0`.
- **`state` is validated** and is unguessable.
- **PKCE is mandatory** (S256); no implicit flow.
- **Refresh tokens are long-lived secrets** and follow every rule in §9.3.
- **The loopback listener closes as soon as the code arrives**, so the port is not held.
- The auth flow's own network traffic stays inside the sanctioned I/O homes
  (`engine/tls.rs`, `crates/feeds`) — no new client for a new protocol (§5.4's
  one-client rule would apply to any HTTP the flow performs).

---

## 10. The user-facing surface

### 10.1 Plaintext is a security control

`architecture.md` §1 excludes HTML rendering for several reasons; the security one is
that **there is no renderer to exploit.** `UI.md` §8.3 makes the implementation
requirement concrete: the reader uses `TextEdit.PlainText`, and **`TextEdit.AutoText` is
banned** because it sniffs content and switches to rich text when the body looks like
markup — reintroducing the exact HTML path the project excludes, and turning every
message body into a renderer injection. One property default is the difference.

### 10.2 Links

- A URL is **rendered inline as its real target** (`MIME-AND-TEXT.md` §10.4). This is an
  anti-phishing property: the user sees `https://evil.example` rather than the display
  text "click here."
- Linkification is allowed only as **escape-everything, then add spans** (`UI.md` §8.3).
  No tag originating in the body reaches the renderer.
- `href` values are validated to `http`, `https`, or `mailto` before being emitted.
- Activation calls `Qt.openUrlExternally` — **the system browser performs the fetch, the
  client never does** (§5.6).

### 10.3 Notifications

- Coalesced per account with a stable `replaces_id` and rate-limited to one per account
  per 60 seconds (`debloatmaild.md` §14). Beyond tidiness, this bounds what a burst of
  mail can do to the notification daemon.
- **No message body text in a notification.** Sender and subject are the ceiling. A
  notification is rendered on a lock screen, and a body excerpt there is a disclosure
  the user did not ask for.
- The GUI's `NotificationPolicy` tri-state has a shutdown bug (`UI.md` §13.3): if the
  preference and the effective value are the same property, quitting while focused
  suppresses notifications permanently. The fix the new UI revision implements: the
  **property carries the effective value** the daemon honours, and the tri-state
  **preference lives in the config file** — GSettings is gone from the project entirely
  (`GEMINI.md`: four config homes, no GSettings) — with the non-focused value restored on
  shutdown.

### 10.4 `UI.Compose` was a spoofing vector — now deleted

Any same-user process could call `UI.Compose(to, subject, body)` and open a compose
window with attacker-chosen content, indistinguishable from one the user opened. As §4.3
establishes, **that could not be fixed on a session bus**, and it had outlived its
legitimate purpose anyway: once `mailto:` URLs route through
`org.freedesktop.Application.Open` activation (`debloatmaild.md` §21), no caller needed
a dedicated pre-fill method. So the interface is **deleted** — from the daemon's API
(`debloatmaild.md` §13) and from the GUI (`UI.md` §13.1), with an `UI.md` §18 test that
asserts the name does not exist on the bus.

What remains is the standard door, and the honest analysis of it:

- **A same-user process can still deliver a `mailto:` URI via `Application.Open`.** That
  is what every mailcap-integrated desktop allows (it is how a browser hands you a
  mailto: link); it is platform semantics, not an amplifier this project introduced.
  Accept it; do not pretend to defend it.
- **`Send` is always an explicit user action.** No activation path, notification action,
  or tray item ever auto-sends. This invariant is what made `UI.Compose` dangerous even
  in its mitigated form; it is retained as unconditional.
- **A compose window populated via activation is visually distinguishable** from one the
  user typed — an "opened from a link" note, at minimum. The old §10.4 mitigation ("make
  the externally-opened compose visible, never auto-send") survives the deletion of the
  method it was guarding, attached instead to the activation path that replaced it.

---

## 11. Supply chain

The project parses hostile input with third-party parsers, so a dependency is part of
the attack surface — and the parser stack now has exactly one home: `crates/extract`
(`GEMINI.md`), with its dependency closure asserted in CI.

| Control | Where |
|---|---|
| **`Cargo.lock` is committed** | `architecture.md` §12 — distro packagers need reproducible resolution, and it pins the tree. |
| **Exact versions, not ranges, for the parser stack** | `MIME-AND-TEXT.md` §3, §17: "a version bump is a reviewed change with a fixture run, not a `cargo update`." |
| **Pimalaya crates are pinned and re-verified** | `architecture.md` §5 — the statuses are hand-curated and `io-email`/`io-keyring` are traps (`GEMINI.md` keeps both in the never-use list). |
| **The parser stack has one crate, and the check is automatic** | New in this revision: `crates/extract` is the only home for `mail-parser`, `charset`, `chardetng`, `html2text` (`GEMINI.md`); CI asserts its closure is runtime-free and I/O-free, so a "quiet" new parser dependency cannot appear without the invariant failing. |
| **Reproducible builds** | `architecture.md` §12 — lets a distro verify the binary matches the source. |

**Two additions this document asks for (§16):**

1. **`cargo-audit` and `cargo-deny` in CI** — the former for known advisories, the latter
   for licences and duplicate/unmaintained crates. The project already has a licence
   constraint (GPL-3.0-or-later, `architecture.md` §16), so `cargo-deny` earns its place
   twice.
2. **A dependency-review gate specifically for the parser crates** — `mail-parser`,
   `charset`, `chardetng`, `html2text`, `feed-rs`, `imap-codec`. A change to any of these
   is a change to the attacker-facing surface and should require the fixture run to pass.

**Build scripts and proc macros are arbitrary code at build time.** They are covered by
review, not by a control — but the point of pinning the tree is that a new one cannot
appear without the lockfile changing.

---

## 12. Sandboxing and packaging

### 12.1 Flatpak

Flatpak is the universal fallback (`architecture.md` §12), and it is the packaging
target with the most security relevance because the sandbox is where access control
finally exists (§4.4).

| Area | Requirement |
|---|---|
| **Filesystem** | Grant only the XDG directories §8.1 names. Use `org.freedesktop.portal.FileChooser` for user-initiated open/save so broad home access is not needed. |
| **Notifications** | A sandboxed app is expected to use `org.freedesktop.portal.Notification` rather than `org.freedesktop.Notifications` directly. `debloatmaild.md` §14 flags this as unverified — **verify it before beta**, because it affects the notification path. |
| **Tray** | The StatusNotifierItem belongs to the daemon (`debloatmaild.md` §18), which registers the unique name `org.freedesktop.StatusNotifierItem-PID-ID` and an object under it. Verify a sandboxed daemon can register with the host's `StatusNotifierWatcher` (the `--talk-name` grant for the watcher is part of the same manifest review as the D-Bus row below). |
| **D-Bus** | The daemon needs `--own-name=org.noahie.debloatmail.Daemon`; the GUI needs `--own-name=org.noahie.debloatmail`. The manifest's talk/own lists *are* the access-control policy (§4.4) and must be reviewed as such. |
| **Network** | The daemon needs it; the GUI does not. If both ship in one Flatpak, the GUI shares the daemon's network namespace — accept, or split the app. |

### 12.2 systemd user unit hardening

Nothing in the current documents specifies a hardened unit, and the daemon is the
process with network access, a parser, and write access to the store — so it is the one
that should be constrained.

```ini
[Service]
# debloatmaild.service — user unit (debloatmaild.md §21)
NoNewPrivileges=yes
PrivateTmp=yes
ProtectSystem=strict
ProtectHome=read-only          # attachments are read from $HOME, never written there
ProtectKernelTunables=yes
ProtectKernelModules=yes
ProtectControlGroups=yes
RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6
RestrictNamespaces=yes
LockPersonality=yes
MemoryDenyWriteExecute=yes     # safe for Rust; the daemon has no JIT (and no Qt — §6.3)
SystemCallFilter=@system-service
SystemCallArchitectures=native
CapabilityBoundingSet=
ReadWritePaths=%h/.local/share/debloatmail %h/.cache/debloatmail %h/.local/state/debloatmail
```

**Three caveats, because a hardening directive that breaks the app is worse than none:**

- **`MemoryDenyWriteExecute=yes` is safe for the daemon and NOT for the GUI.** The QML
  engine's JavaScript V4 has a JIT and needs W^X. Apply this to `debloatmaild.service`
  only — and note the daemon-only placement is reinforced by §6.3: the daemon linking no
  Qt is what keeps its own JIT-exemption argument clean.
- **`ProtectHome=read-only` plus `ReadWritePaths=` carve-outs** is the shape that lets
  the daemon read attachments from the home directory while writing only to its own XDG
  directories. Verify the carve-outs resolve on each target distro, since `%h` and the
  XDG variables can differ.
- **`SystemCallFilter=@system-service`** is a good default but must be tested against the
  actual syscalls the runtime uses (epoll, io_uring if used, `getrandom`). A blocked
  syscall is an `EPERM` in the middle of a sync.

This is a proposal to be validated on each packaging target, not a copy-paste.

---

## 13. Privacy

Not a security boundary, but adjacent enough to state once:

| Property | Status |
|---|---|
| **No telemetry, no analytics, no phone-home** | The only network traffic is IMAP, SMTP, and the feeds the user configured. (`GEMINI.md`'s "the daemon owns all I/O" makes this assertable about the whole project, not one component.) |
| **No remote resource loading** | §5.6 — no read receipts, no tracking pixels, no IP disclosure on open. |
| **Feed `User-Agent`** | `debloatmail/<version> (+<project-url>)` (`SYNC.md` §12). It identifies the client and a contact URL — a deliberate trade for politeness, and a fingerprinting surface worth knowing about. |
| **Feed polling reveals interests** | Unavoidable: the feed host sees the client's IP on the poll interval. |
| **`doctor` output** | Contains hostnames and account names. It is for the user to share or not; the tool must not upload it. |
| **Logs are local** | Never transmitted. |

---

## 14. Vulnerability handling

| Item | Requirement |
|---|---|
| **Reporting** | A `SECURITY.md` and a `security.txt` in the **repository root**, with a contact address and a PGP key. |
| **Name of this document** | **Open, flagged here rather than decided here:** the design-document `SECURITY.md` (this file) collides with the repo-root `SECURITY.md` that GitHub treats as the disclosure policy (issues list, smaller items). The convention hands the root file to the disclosure policy; this document needs a non-colliding home (e.g. `docs/SECURITY-MODEL.md`) — the rename ripples through every document that cites "SECURITY.md", so it is a repo-layout decision for the coordination pass, not a unilateral edit (§16). |
| **Scope** | In scope: memory-safety in any parser, RCE, credential disclosure, remote resource fetch, unintended mail deletion, path traversal, an SSRF in feed fetching. |
| **Out of scope** | §1's non-goals — a same-user attacker, root, physical access, provider-side compromise. |
| **Coordinated disclosure** | A fixed window (90 days is the convention) before public disclosure, with the reporter credited unless they prefer otherwise. |
| **Advisories** | Published on the release channel and in the repo; a fixed release is named per CVE. |
| **CVEs** | Requested for memory-safety and RCE issues; a plain `SECURITY` note suffices for lower-severity issues. |

**A "won't fix" list that matches §1 is part of the policy.** A report of "a same-user
process can call `Items.Delete`" is a correct observation and a documented non-goal
(§4.3), not a vulnerability — and saying so in advance is what keeps the disclosure
channel useful.

---

## 15. What this document does not cover

Stated explicitly so the gaps are known rather than assumed:

- **A malicious mail server.** It can send any message; we render it safely and do not
  authenticate the sender.
- **PGP/S/MIME verification.** Signatures are parsed for content and discarded
  (`MIME-AND-TEXT.md` §1); an encrypted part has no readable text.
- **Spam and phishing classification.** The client shows the real URL (§10.2); it does
  not judge it.
- **DNS integrity.** DNS is not authenticated by default; the loopback check in §5.2 is
  on the connected peer address precisely because the name is not trustworthy.
- **The desktop environment.** The notification daemon, the tray host, the secret
  service, and the bus daemon are all trusted components. A compromise of any is out of
  scope.

---

## 16. Verification checklist

Each item is a test or a CI assertion, not a review note.

- [ ] The session bus is **not** treated as a confidentiality boundary anywhere in the
      code or the docs (§4.1).
- [ ] A same-user process that monitors the bus can be shown to observe a secret passed
      to `Accounts.Add` — the test exists to keep the claim honest, and the docs say so.
- [ ] `imap_security = 'none'` is rejected at account validation for a non-loopback host.
- [ ] `allow_invalid_cert_loopback` is checked against the **connected peer address**,
      and a hostname resolving to `127.0.0.1` does **not** satisfy it unless the socket
      is loopback.
- [ ] A `STARTTLS` failure aborts; there is no cleartext fallback.
- [ ] `CAPABILITY` is re-read after `STARTTLS` and after `AUTHENTICATE`.
- [ ] No HTTP client exists outside `crates/feeds` — CI dependency-closure assertion
      (`GEMINI.md` boundary) — so the redirect policy in §5.4 has no bypass.
- [ ] A feed redirect to `127.0.0.1`, `169.254.169.254`, or a private range is refused
      by the redirect policy.
- [ ] An `https → http` redirect is refused; a non-`http(s)` scheme is refused at parse.
- [ ] A feed response over 10 MB is rejected *after* decode, before parse.
- [ ] A feed with a DTD entity-expansion payload fails cleanly; memory does not grow.
- [ ] An XML external-entity payload fetches nothing.
- [ ] No HTML tag appears in any `item_text.body`; the converter is proven pure (no
      socket, file, thread, runtime) by test.
- [ **No Qt linkage in `debloatmaild` — CI assertion on the daemon's dependencies and the
      binary's dynamic section; the daemon-side parser list is `crates/extract`
      (`mail-parser`, `charset`, `chardetng`, `html2text`) plus `feed-rs`.**
- [ ] QtPdf does not execute embedded JavaScript and does not fetch a remote resource.
- [ ] No SQL is built by string interpolation; every id is parsed as a `PublicId`
      before use.
- [ ] A malformed FTS5 query returns zero results — not an error, not a panic.
- [ ] No IMAP or SMTP command is produced by `format!`; the codec encodes every command,
      including named ones.
- [ ] A `Subject` or display name containing CRLF cannot inject a header; `Bcc` never
      reaches transmitted headers.
- [ ] `--secret-command` is executed without a shell.
- [ ] No secret appears in a `--debug` trace, a log, a core dump, or `--dry-run` output.
- [ ] `mail.db` and the blob store are created `0600`/`0700` **regardless of umask**.
- [ ] A `Send` attachment that is a FIFO, a device node, `/proc/self/environ`, a symlink
      to any of those, outside `$HOME`, or over the size cap is refused — with the FIFO
      case asserted as *immediate refusal*, not a hang.
- [ ] `section_id` is validated at the boundary: RFC 3501 §6.4.5 dot-list **and** a
      match against an existing `attachments` row of the item (`debloatmaild.md` §17).
- [ ] Blob GC runs server-side and collects an attachment fetched an hour ago with no
      remaining reference.
- [ ] `Accounts.Remove(purge=true)` removes the keyring entry.
- [ ] `Accounts.Update` secret rotation rewrites the keyring entry, and the auth error
      clears only on the next successful validation.
- [ ] In-memory secrets are zeroed on drop and their `Debug` is redacted.
- [ ] `cargo-audit` and `cargo-deny` pass in CI; a change to a parser crate fails the
      review gate.
- [ ] `debloatmaild.service` passes the hardening directives on every target distro, and
      the GUI's unit does not set `MemoryDenyWriteExecute`.
- [ ] A notification contains no message body text.
- [ ] **No `org.noahie.debloatmail.UI.Compose` name exists on the bus** (the interface is
      deleted, `UI.md` §13.1); a compose window populated via `Application.Open`
      `mailto:` activation carries the externally-opened marker and there is no code
      path from activation to `Send` without an explicit user action.
- [ ] `SECURITY.md` (repo root) and `security.txt` exist and name a disclosure contact;
      this document's filename does not collide with them (the rename decision lands in
      the repo pass, §14).
