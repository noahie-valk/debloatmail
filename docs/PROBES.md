# Probes

`architecture.md` §13 ends with a flat rule: **never hit live providers in tests.**
`SYNC.md` §15 repeats it and builds a five-layer test stack that never needs the
network. Both rules are correct. This document does not weaken them.

But a mail client has to talk to Gmail eventually, and no GreenMail container will
report that a provider advertises `CONDSTORE` without `QRESYNC` (`SYNC.md` §5.6),
duplicates a message across a label and `\All`, or starts answering `NO [LIMIT]` under
load. That knowledge has to come from somewhere. The alternative to a controlled probe
is an uncontrolled one: a debug build pointed at someone's personal mailbox.

So a **probe** is a live-provider run with its own account, its own credentials, its own
database, a hard scope guard, and a recording. It is not a test. It never gates a merge,
and everything it learns is frozen into a fixture that runs offline forever.

---

## 1. Tests and probes are different objects

“Fixtures … Prefer real-world samples over synthetic ones” (`architecture.md` §13) is the
sentence this document exists to serve. A probe is how real-world samples are obtained
without a live server in the loop afterwards.

| | Tests (`architecture.md` §13, `SYNC.md` §15) | Probes (this document) |
|---|---|---|
| Network | Never, beyond loopback containers | Real provider, deliberately |
| Determinism | Required; injected clock and RNG | Not available; recorded instead |
| Failure means | A defect | A defect *or* a provider divergence |
| Cadence | Every commit | On demand, and before a beta tag |
| Credentials | None | One dedicated probe account |
| Gates a merge | Yes | **Never** |
| Output | Assertion | A new fixture, or a filed finding |

**The rule that follows:** if a live provider can turn CI red, the next commit adds
`sleep(2)` for Gmail. Probes exist so that pressure never reaches the test suite.

---

## 2. The invariants

As with `architecture.md` §2, these are cheap to keep today and expensive to retrofit.
If a change violates one, the change is wrong — not the invariant.

1. **A probe account is never a personal account.** Synthetic mail only, created for this
   purpose. No exception, including "just this once, to reproduce it."
2. **A probe writes only inside a folder it created in this run.** Everything else is
   unaddressable, and the refusal lives in `engine`, not in the harness (§6).
3. **No probe credential touches `argv`, the environment, `config.toml`, a log, or a
   capture** (`SECURITY.md` §7.3, §9.3). The keyring holds it; the redaction filter
   removes it.
4. **A capture passes the redaction filter or it does not exist.** Recording is not
   opt-out once enabled.
5. **A probe never gates a merge.** Findings graduate to fixtures; fixtures gate.
6. **A probe failure never loosens a test or an assertion** (`GEMINI.md`, Boundaries).
7. **One probe answers one declared question**, written down before the run.

---

## 3. Where the harness lives

**Decision: a separate binary crate, `crates/probe` (`debloatmail-probe`),
`publish = false`, absent from CMake install rules and from the Flatpak manifest.**

| Rejected | Why not |
|---|---|
| A `debloatmailctl` subcommand | That command surface is a contract with its own naming rule and exit-code table (`debloatmailctl.md` §8, §14); probe verbs would be permanent API |
| An `xtask` | The project has no xtask convention, and this needs a real binary with its own keyring service and database |
| Shipping probe code behind a feature flag in `debloatmaild` | A live-provider client in a user binary is a credential-leak surface with no user-facing purpose |

The harness runs the engine **in-process**, exactly like `--no-daemon`
(`debloatmailctl.md` §10): it takes the exclusive `flock`, uses its own `--db` path and
its own `XDG_CONFIG_HOME` / `XDG_STATE_HOME`, and stores secrets under the keyring
service `debloatmail-probe` so a probe secret can never be mistaken for a real one.

```
debloatmail-probe plan --provider fastmail --case idle-keepalive     # prints commands; opens no socket
debloatmail-probe run  --provider fastmail --case idle-keepalive \
                        --account probe-fastmail --record --budget 5m
debloatmail-probe reap --older-than 30d
debloatmail-probe sanitize --check tests/imap/recorded/
```

`plan` is the analogue of `--dry-run` (`debloatmailctl.md` §3) and is mandatory reading
before a `run`: it prints the exact command sequence, including the folder it intends to
create, and must show **zero** write commands when the session is read-only.

---

## 4. Accounts

- **One account per provider family** in §10, never shared with real correspondence.
- **Hosted on a domain you control** where the provider allows it, so the corpus never
  contains a third party's address. Otherwise use reserved names (`*.invalid`,
  `example.*`) in the seed data.
- **Seed small, then grow.** A brand-new account issuing a few thousand `FETCH`es on day
  one looks like abuse to spam heuristics; the harness's first run is deliberately tiny.
- **Workspace and M365 probes use a dev tenant**, never a production tenant whose admin
  will notice the throttling before you do.
- **Provider terms come first.** Read the AUP before automating a provider you do not
  control, and never create accounts programmatically.

Seed corpus is fixed, not improvised — it exists so runs are comparable:

| Message | Purpose |
|---|---|
| Plain `text/plain`, one line | Baseline header/body fetch |
| `multipart/alternative` with HTML part | Proves the plaintext path never renders it |
| 5 KB attachment | `AttachmentFetch` and blob GC |
| Message with **no `Message-ID`** | The UIDVALIDITY limit in `SYNC.md` §5.4 |
| Subject with non-ASCII, quoted-printable | Charset and header-decoding path |
| Thread of 3 replies | Threading, `References`/`In-Reply-To` |
| Already-`\Seen`, flagged, and `$Label`-free | Flag sync baseline |

---

## 5. Credentials

| Rule | Where |
|---|---|
| Secret in the keyring under `debloatmail-probe` only | `SECURITY.md` §9.1, §9.3 |
| Never on `argv`; prompt or `--secret-stdin` | `SECURITY.md` §7.3 |
| Separate OAuth client per environment, loopback + PKCE | `SECURITY.md` §9.5 |
| No standing credential in CI | §12 below |
| Teardown asserts the keyring entry is gone | §14 below |

**The beta matrix follows `architecture.md` §7, not convenience.** §7 ships password
auth and defers OAuth; therefore Gmail-on-Workspace, M365, and Graph probes are **post-beta
by construction**. Until then the matrix is Fastmail, Proton Bridge, iCloud, and
self-hosted. A harness that requires OAuth to exist before the engine does inverts the
build order (`architecture.md` §14) — probe what the beta can actually authenticate.

Where a password path does exist, use the provider's app-password mechanism, never the
account password: it is revocable in isolation, which is the entire point of using it on
a throwaway account.

---

## 6. The scope guard

**Decision: the guard is a capability on the engine session, not a check in the
harness.** A bug in the harness must not be able to exceed the fence, so the fence is
below it, in the same place `SECURITY.md` §5.2 puts the loopback check — at the socket,
not at the caller.

```rust
pub struct ProbeScope {
    pub folder_allowlist: Vec<String>,   // exactly one entry, created by this run
    pub read_only: bool,                 // default true
    pub max_commands: u32,               // default 500
    pub max_download_bytes: u64,         // default 20 MiB
    pub max_upload_bytes: u64,           // default 2 MiB
    pub deadline: Instant,               // default 10 min
}
```

| Operation | Read-only default | Escalation | Why it is fenced |
|---|---|---|---|
| `SELECT` / `EXAMINE` | Allowlisted folder only | None | The only way to reach a foreign mailbox |
| `FETCH`, `SEARCH`, `UID FETCH` | Allowed, counted | None | Bandwidth and politeness budget |
| `APPEND` | Refused | `--allow-append`, seed phase only | The only way to create data |
| `STORE`, `MOVE`, `COPY` | Refused | Never on a probe account except `\Seen` inside the probe folder | Flag corruption on real mail is unrecoverable |
| `EXPUNGE`, `UID EXPUNGE` | Refused | Teardown only, folder you created | `EXPUNGE` on a server without `UIDPLUS` is unsafe (`SYNC.md` §5.2) |
| `DELETE` (folder) | Refused | Teardown only | Deletes correspondence if misaimed |
| `CREATE` | Refused | Setup phase only, single folder | Folder-set pollution; Gmail labels are visible in the UI |

Refusals happen **before the coroutine is pumped**: the test asserts an empty outbound
buffer, not merely an error return. Budget exhaustion sends a clean `LOGOUT` and exits
`21` (§12). A circuit breaker stops the run after three consecutive transient failures
instead of backing off through the night (`SYNC.md` §10.2) — a probe is not a production
daemon and must not behave like one.

---

## 7. Seeding and teardown

**Decision: one folder per run, created and deleted by the run.**

```
CREATE "Probes/run-<uuidv7>"
# seed phase — APPEND only
APPEND "Probes/run-<uuidv7>" (\Seen) {n}
X-Debloatmail-Probe: <run-id>
Message-ID: <probe-<run-id>-<n>@probe.invalid>
...
# teardown — after asserting every message carries the run header
DELETE "Probes/run-<uuidv7>"
```

- **The run id is in the folder name**, so a crashed run is identifiable and is never
  adopted by the next one.
- **Teardown is `DELETE` on a folder you created**, not `EXPUNGE` on messages you tagged.
  It needs no `UIDPLUS`, and it cannot touch a message you did not put there.
- **Never address `INBOX`, `[Gmail]/All Mail`, `Sent`, `Drafts`, or `Trash`.** The
  allowlist is one entry long, which is what makes this checkable.
- **Header assertion before deletion**: enumerate the folder, and refuse teardown if any
  message lacks `X-Debloatmail-Probe: <run-id>`. A failed assertion leaves the folder for
  a human and exits non-zero.
- **`--keep`** leaves the folder for debugging and stamps it with an expiry header;
  `reap` deletes only `Probes/run-*` older than the retention window and only after the
  same assertion.

**Provider specifics that must not be generalized:**

- **Gmail:** a created folder is a label. A message in the label also lives in `\All`, so
  `DELETE` of the label leaves the message in `All Mail`. Either accept that residue (and
  size the corpus accordingly) or run `UID EXPUNGE` inside the probe label first under the
  teardown escalation. Pick one per case and write it down; do not discover it at run 40.
- **Proton:** the Bridge maps folders to labels too, the password is Bridge-generated, a
  paid plan is required, and the certificate is self-signed on loopback — permitted only
  by the per-account `allow_invalid_cert_loopback` flag checked against the **connected
  peer address** (`SECURITY.md` §5.2). A probe must never add a general insecure switch to
  make itself work.
- **`[Gmail]/All Mail` is not synced by default** (`SYNC.md` §15, fixture 6). A probe that
  syncs it will double every message and mis-measure bandwidth.

---

## 8. Recording, redaction, and replay

**Decision: record at the sans-I/O coroutine boundary, not with a TLS-intercepting proxy.**

`architecture.md` §5 chose sans-I/O precisely because it "makes the protocol logic
testable against in-memory buffers with no network." A `RecordingIo` wrapper tees those
same buffers. Consequences, all good: no CA to install on the probe machine, no second
TLS stack, no proxy to keep alive, and the bytes are already post-TLS plaintext — no MITM
is needed to see them.

```
engine pump ──► RecordingIo ──► TLS socket
                    │
                    └──► redaction filter (SECURITY.md §7.3) ──► .imap transcript
```

- **The tee writes through the same redaction pass as `--debug` traces**, so
  `LOGIN` / `AUTHENTICATE` payloads land on disk as a corpus password marker and replay
  authenticates with a dummy secret. A raw credential must be unable to reach the file
  even if the run is killed mid-write.
- **Sanitizer**, run before anything enters the repository: deterministic HMAC pseudonyms
  for addresses, display names, domains, and subjects (a committed salt, so two
  contributors produce identical output); `Message-ID` mapping preserved so threading
  stays testable; bodies replaced with synthetic text of the same shape (charset,
  transfer encoding, part structure preserved); attachment payloads truncated to a 64 KiB
  MIME-structure stub.
- **Layout**: `tests/imap/recorded/<provider>/<yyyy-mm-dd>/<case>/`, holding
  command/response pairs **and** the engine's parsed result — so a regression surfaces as
  either a byte diff or a semantic diff, and you can tell which.
- **Retention**: the last three runs per provider per case. Older captures are pruned;
  the point is drift detection, not an archive.
- **`corpus-diff`** reports changed `CAPABILITY` lines, new response codes, changed folder
  lists, changed `PERMANENTFLAGS`. This is the early-warning system for provider drift,
  and it is the reason to keep probing at all once the corpus exists.

> **Sanitizing is a fallback, not a licence.** The corpus is synthetic by construction. A
> capture that touched real mail is **deleted, not cleaned** — the sanitizer is a
> safety net for accidents, not a permission slip.

---

## 9. What only a probe can answer

Everything else belongs to §13's lower layers, and must not be probed "because it's
easier live."

| Question | Becomes |
|---|---|
| Which capabilities does this provider actually advertise, and which lie? | A recorded `CAPABILITY` transcript |
| What does throttling look like on the wire — text, code, recovery time? | `SYNC.md` §10.1 classification fixtures |
| What do `SPECIAL-USE` flags and folder nesting really look like? | Role-mapping fixtures (`SYNC.md` §5.3) |
| How does the server behave on a 28-minute IDLE boundary? | Watch-loop fixtures (`SYNC.md` §5.7) |
| Does the loopback certificate exception actually work against a real Bridge? | The §5.2 assertion, exercised |
| What happens on a revoked app password or rotated token? | Auth-failure fixtures, exit `4` |
| Does a real `DELETE` + `CREATE` of the probe folder change `UIDVALIDITY`? | **The one sanctioned live destructive test** — safe because the folder is ours, and it destroys the run's seed data, so it is a dedicated case, not a step in another one |

**Never provoke on a live account:** mass expunge, `UIDNEXT` regression, quota
exhaustion, or anything whose recovery path (`SYNC.md` §5.4) is what you are trying to
observe. Provoke those at Layer 2 by recreating a mailbox in the container, where the cost
of getting it wrong is a `docker restart`.

---

## 10. Provider matrix

| Provider | Probe auth | Why probe it | The trap |
|---|---|---|---|
| **Gmail** (consumer) | App password | `CONDSTORE` without `QRESYNC`; label/folder duality | `\All` duplication; per-connection and bandwidth limits |
| **Google Workspace** | OAuth — **post-beta** (`architecture.md` §7) | Admin policy, tenant throttling | Blocked until the OAuth phase; do not fake it with an app password |
| **M365 / Outlook** | OAuth — **post-beta** | Per-mailbox throttling is slow to clear | Same block; basic auth is gone |
| **Fastmail** | App password | Cleanest reference implementation; advertises `NOTIFY` (`SYNC.md` §5.7) | Little — this is the control |
| **Proton** | Bridge password, paid plan | The loopback certificate exception, for real | Self-signed localhost cert; Bridge label semantics |
| **iCloud** | App-specific password | Low connection caps | Silent disconnects; treats concurrency as abuse |
| **Self-hosted (Dovecot, Stalwart)** | Password | Version drift against the containers | Layer 2 already covers conformance; probe only for what changed |
| **Feeds (RSS/Atom)** | None | Real conditional-GET, real `429`, real malformed XML | HTML served as a feed; 20-second timeouts (`SYNC.md` §7.4) |

A provider not in this table is added by editing this document first, and the edit is the
review.

---

## 11. Politeness and budgets

Aligned with `SYNC.md` §12 wherever the rule already exists — a probe is not exempt from
the client's own politeness rules, it is the first place they get exercised against a real
server.

| Rule | Probe value | Source |
|---|---|---|
| Concurrent connections per host | 1 | `SYNC.md` §12 |
| Minimum gap between commands | 1 s | `SYNC.md` §12 |
| `Retry-After` | Honored exactly | `SYNC.md` §12 |
| Header fetch window | 200 UIDs (smaller: the corpus is tiny) | `SYNC.md` §12 caps at 2,000 |
| Download / upload per run | 20 MiB / 2 MiB | §6 |
| Commands per run | 500 | §6 |
| Wall clock per run | 10 min | §6 |
| `User-Agent` | `debloatmail/<version> (+<project-url>)` | `SYNC.md` §12 |

**Provider limits are measured, not quoted.** Record what the server did and when it
recovered; set the budget below that. No number in the probe config may originate from a
blog post.

---

## 12. Running one

**Pre-flight.** Credential present in the `debloatmail-probe` keyring entry; no daemon
running against the probe profile (the `flock` will refuse, exit `8`); `plan` output read
and the folder name checked; budget set; `--record` on; one question written down.

**Post-flight.** Teardown verified (folder gone, header assertion passed); keyring entry
deleted and asserted empty; `sanitize --check` run; `corpus-diff` reviewed; one line
appended to the findings file — question, result, capture path, date.

**Cadence.** On demand for a declared question; the full matrix once before a beta tag;
quarterly otherwise.

**CI.** `workflow_dispatch` only, behind a protected `environment:` with required
reviewers. No refresh token in repository secrets reachable by a PR workflow — for a
pre-alpha project, a standing mailbox credential is a supply-chain liability far out of
proportion to the benefit (`SECURITY.md` §9.2 is explicit that the keyring is not access
control, and a CI secret is weaker than a keyring). Uploaded artifacts may contain only
post-sanitizer output.

**Exit codes.** Identical meaning where the meaning is identical; new codes start at 20 so
the CLI's table (`debloatmailctl.md` §7) remains the sole owner of 0–9.

| Code | Meaning |
|---|---|
| `0` | Probe completed and the capture replays |
| `1` | Internal harness error |
| `2` | Usage error |
| `4` | Authentication failed |
| `5` | Network or TLS failure |
| `8` | Lock held (daemon or another probe on this profile) |
| `20` | Refused by the scope guard |
| `21` | Budget or deadline exhausted |
| `22` | Provider degraded (throttled, `BYE`, limited) — inconclusive |
| `23` | Sanitizer gate failed: capture not committable |

---

## 13. What a probe result means

| Result | Meaning | Next action |
|---|---|---|
| **Pass** | The engine did the declared thing, and the capture replays offline | Add the capture to the corpus; delete nothing |
| **Engine defect** | The server behaved sanely; we did not | File it, fix it, then add the fixture as the regression |
| **Provider divergence** | The engine is right and this server is not what the docs said | Classify it (`SYNC.md` §10.1), record it in §10's trap column, add the fixture |
| **Inconclusive** | No verdict — throttled, timed out, budget exhausted | Re-run once with a larger budget, then stop. Inconclusive is not a pass |

**A probe never closes an issue. The fixture does.** "It worked when I pointed it at my
account" is not a result; a replayable capture is.

---

## 14. Data hygiene and legal

- **The probe has its own database and its own `XDG_*` roots**, and deletes them after the
  run. The FTS5 index will contain every body you synced, so a probe pointed at a real
  mailbox would be a data-retention problem with a search box attached.
- **`config.toml` never holds the secret** (`SECURITY.md` §9.1); only `secret_ref` reaches
  SQLite (`DATA-MODEL.md` §5).
- **Keyring cleanup is asserted, not assumed.** `Accounts.Remove(purge=true)` deleting the
  keyring entry is still open (`SECURITY.md` §9.3, §16); until it lands, the harness
  deletes the `debloatmail-probe` item itself and **fails the run** if it survives. An
  orphaned probe credential is precisely the leak this document exists to prevent.
- **Read the AUP** before automating a provider you do not control. No automated account
  creation, no scraping, identification via `User-Agent` (`SYNC.md` §12).
- **Never probe a mailbox you do not own**, including with written permission. The
  knowledge is never worth the liability.

---

## 15. What this document does not cover

- **The lower test layers.** Coroutine-pump tests, `tests/imap/` scenario tests, and
  failure injection are `architecture.md` §13 and `SYNC.md` §15. Probes are the top layer
  and the only one allowed a socket.
- **OAuth implementation.** `architecture.md` §7 and `SECURITY.md` §9.5 own it; probes
  consume it.
- **Benchmarking on real accounts.** Different budgets, different ethics, different
  document.
- **Feed discovery or subscription.** `SYNC.md` §7 owns fetching; probes only record what
  real feeds return.

---

## 16. Verification checklist

Each item is a test or a CI assertion, not a review note.

- [ ] `ProbeScope` refuses `SELECT` of a folder outside the allowlist **before the
      coroutine is pumped**; the test asserts an empty outbound buffer.
- [ ] A read-only session refuses `STORE`, `MOVE`, `COPY`, `EXPUNGE`, and `APPEND` with no
      wire bytes emitted.
- [ ] Command, byte, and deadline budgets abort with `LOGOUT` and exit `21`.
- [ ] Teardown enumerates the probe folder and refuses `DELETE` if any message lacks
      `X-Debloatmail-Probe: <run-id>`.
- [ ] `reap` deletes only `Probes/run-*` past the retention window, after the same
      assertion.
- [ ] No probe credential appears in `argv`, the environment, `config.toml`, a log, a
      capture, or `--dry-run` output (`SECURITY.md` §7.3, extended to the harness).
- [ ] `sanitize --check` fails on: an address outside `*.invalid` / `example.*`, a base64
      run over 64 KiB, or a high-entropy string over 40 characters.
- [ ] A recorded Gmail capture replays with no network namespace available.
- [ ] `corpus-diff` reports a changed `CAPABILITY` line as a diff, not a failure.
- [ ] `debloatmail-probe` is `publish = false` and appears in no CMake install rule, no
      packaging spec, and no Flatpak manifest.
- [ ] `debloatmailctl --help` is byte-identical before and after this document lands; no
      CLI subcommand reaches the harness.
- [ ] The `debloatmail-probe` keyring service is empty after a successful run and after a
      failed one.
- [ ] The loopback certificate exception is exercised against a real Bridge with the
      connected-peer check (`SECURITY.md` §5.2), and no general insecure switch exists.

---

## 17. Decisions recorded

| Decision | Chosen | Rejected | Reason |
|---|---|---|---|
| Harness home | `crates/probe`, unpublished | CLI subcommand; xtask | The CLI surface is a contract (`debloatmailctl.md` §14) and probe code must never ship |
| Guard location | Engine `ProbeScope` | Harness-side checks | A harness bug must not exceed the fence; same reasoning as `SECURITY.md` §5.2 |
| Recording point | Sans-I/O tee | TLS-intercepting proxy | No CA, no second TLS stack; `architecture.md` §5 already made the buffers available |
| Teardown | `DELETE` your own folder | Tag messages, then `EXPUNGE` | Needs no `UIDPLUS`; cannot touch a message you did not create |
| Execution trigger | `workflow_dispatch` + protected environment | Nightly cron | A standing mailbox token in CI is worse than a manual run |
| Result authority | The replayable capture | The probe run's log | A log is not a regression test; `architecture.md` §13 wants fixtures |
| Exit codes | 0–9 reused where identical, new codes from 20 | A fresh 0–15 table | One owner for 0–9 (`debloatmailctl.md` §7); no silent redefinition |

---

## 18. Repo pass

This document is inert until the following land, and they are listed here so the edit is
one commit rather than five discoveries:

1. **`GEMINI.md`** — a row in the Docs table:
   `| Live-provider probing, recorded corpora | docs/PROBES.md |`; a Testing bullet after
   "Never hit live providers": *"Live-provider work is a probe, not a test:
   `docs/PROBES.md`. It never gates a merge, and its captures become the fixtures this
   section wants."*; two Boundaries entries — **Ask first:** adding a provider to §10;
   **Never:** run a probe against a personal account or with write scope outside the run's
   own folder.
2. **`architecture.md` §13** — one sentence appended to the live-provider bullet:
   *"(Probing a live provider is a separate instrument with its own accounts, guard, and
   recording: `PROBES.md`. It exists to supply the real-world fixtures this section
   prefers, not to replace them.)"* The rule itself does not change.
3. **`SECURITY.md` §16** — three checklist items: the engine-level `ProbeScope`
   assertions, the sanitizer gate, and "no probe corpus contains a real address."
4. **`SYNC.md` §12** — a parenthetical noting that probe runs obey the same politeness
   table with a tighter window (§11 above), so nobody reads "probe" as "exempt."
5. **`.gitignore`** — `tests/imap/recorded/*/raw/`, so an unfiltered capture cannot be
   committed by accident even once.
6. **`.github/workflows/`** — a `workflow_dispatch`-only `probe.yml` (no schedule, no
   secrets in `pull_request` context), and the sanitizer gate added to `ci.yml`.

**One open question, flagged rather than decided:** whether `corpus-diff` should run in
`ci.yml` against the committed corpus (cheap, catches accidental fixture edits) or stay a
manual maintenance step. The former is more honest; the latter keeps CI from failing for a
reason that has nothing to do with the commit. Current answer: manual, revisited when
there is a second contributor.
```

Three things worth flagging about the choices I made, since they're judgement calls rather than transcription:

- **Recording at the sans-I/O boundary instead of via `mitmproxy`** is the one substantive departure from what we discussed earlier. `architecture.md` §5 already argues that sans-I/O exists partly so protocol logic is testable against in-memory buffers — so the recorder belongs there, and it removes the CA-install and second-TLS-stack problems entirely.
- **The guard lives in `engine`, not the harness.** This mirrors how `SECURITY.md` §5.2 puts the loopback check on the connected peer address rather than the configured hostname: check the thing that cannot lie.
- **Gmail/Workspace/M365 probes are marked post-beta** because `architecture.md` §7 ships password auth only, and there's no honest way to probe an OAuth-only provider before the OAuth phase exists. Faking it with an app password on a Workspace account won't authenticate anyway.
