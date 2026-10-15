# Implementation Progress

## Current Status: Step 1 Completed (`core` + `store`)

### 1. Toolchain & Workspace Configuration
- **Rust Toolchain**: Configured [rust-toolchain.toml](file:///home/nvalk1/debloatmail/rust-toolchain.toml) pinning `channel = "1.93.0"`.
- **Root Cargo Workspace**: Configured [Cargo.toml](file:///home/nvalk1/debloatmail/Cargo.toml) with resolver `2`, workspace inheritance, and shared dependencies (`rusqlite` bundled with FTS5, `thiserror`).
- **Ignored Artifacts**: Updated [.gitignore](file:///home/nvalk1/debloatmail/.gitignore) for build outputs and caches.

### 2. Domain Model (`crates/core`)
- **Invariant 1 Preserved**: Strictly **zero external dependencies** in [crates/core/Cargo.toml](file:///home/nvalk1/debloatmail/crates/core/Cargo.toml).
- **Identity & Tokens**:
  - Implemented [`PublicId`](file:///home/nvalk1/debloatmail/crates/core/src/item.rs#L10) (validated UUIDv7 string) and strongly-typed newtypes ([`AccountId`](file:///home/nvalk1/debloatmail/crates/core/src/item.rs#L94), [`FolderId`](file:///home/nvalk1/debloatmail/crates/core/src/item.rs#L95), [`FeedId`](file:///home/nvalk1/debloatmail/crates/core/src/item.rs#L96), [`ThreadId`](file:///home/nvalk1/debloatmail/crates/core/src/item.rs#L97), [`DraftId`](file:///home/nvalk1/debloatmail/crates/core/src/item.rs#L98), [`OpId`](file:///home/nvalk1/debloatmail/crates/core/src/item.rs#L99)).
  - Implemented standard lowercase snake_case enum wire tokens (`as_str` / `FromStr`) for [`ItemKind`](file:///home/nvalk1/debloatmail/crates/core/src/item.rs#L102), [`BodyState`](file:///home/nvalk1/debloatmail/crates/core/src/item.rs#L131), [`AccountKind`](file:///home/nvalk1/debloatmail/crates/core/src/account.rs#L7), [`AuthKind`](file:///home/nvalk1/debloatmail/crates/core/src/account.rs#L39), [`Security`](file:///home/nvalk1/debloatmail/crates/core/src/account.rs#L71), [`FolderRole`](file:///home/nvalk1/debloatmail/crates/core/src/mailbox.rs#L7), [`RoleSource`](file:///home/nvalk1/debloatmail/crates/core/src/mailbox.rs#L55), [`SortKey`](file:///home/nvalk1/debloatmail/crates/core/src/stream.rs#L8), [`OpKind`](file:///home/nvalk1/debloatmail/crates/core/src/message.rs#L7), and [`OpState`](file:///home/nvalk1/debloatmail/crates/core/src/message.rs#L45) per [docs/DATA-MODEL.md](file:///home/nvalk1/debloatmail/docs/DATA-MODEL.md) §14.
- **Unified Items & Content Stream**:
  - Implemented [`Item`](file:///home/nvalk1/debloatmail/crates/core/src/item.rs#L168) and [`Attachment`](file:///home/nvalk1/debloatmail/crates/core/src/item.rs#L186).
  - Implemented [`StreamSpec`](file:///home/nvalk1/debloatmail/crates/core/src/stream.rs#L61), [`StreamSource`](file:///home/nvalk1/debloatmail/crates/core/src/stream.rs#L50), and pagination [`Cursor`](file:///home/nvalk1/debloatmail/crates/core/src/stream.rs#L40).
  - Implemented [`Thread`](file:///home/nvalk1/debloatmail/crates/core/src/thread.rs#L5), [`Account`](file:///home/nvalk1/debloatmail/crates/core/src/account.rs#L85), [`Folder`](file:///home/nvalk1/debloatmail/crates/core/src/mailbox.rs#L80), [`Feed`](file:///home/nvalk1/debloatmail/crates/core/src/feed.rs#L5), [`PendingOp`](file:///home/nvalk1/debloatmail/crates/core/src/message.rs#L77), and [`Draft`](file:///home/nvalk1/debloatmail/crates/core/src/message.rs#L92).
  - Domain error handling via [`CoreError`](file:///home/nvalk1/debloatmail/crates/core/src/error.rs#L5) using pure standard library types.

### 3. Thin Cache & Storage (`crates/store`)
- **Authoritative DDL**:
  - Embedded schema in [crates/store/src/migrations/0001_init.sql](file:///home/nvalk1/debloatmail/crates/store/src/migrations/0001_init.sql) matching [docs/DATA-MODEL.md](file:///home/nvalk1/debloatmail/docs/DATA-MODEL.md) §5.
  - Setup `visible_items` view over the spine items, mail items, and feed items, enforcing suppression (`hidden_at IS NULL`).
  - Created `item_text` table with external-content FTS5 virtual table and synchronization triggers.
- **Connection Management & Pragmas**:
  - Implemented in [`schema.rs`](file:///home/nvalk1/debloatmail/crates/store/src/schema.rs): WAL mode, busy timeout (5000 ms), foreign keys enabled, normal synchronous mode, in-memory temp store.
  - Schema version check rejecting databases newer than `CURRENT_SCHEMA_VERSION = 1`.
  - Transaction-bound global revision incrementing ([`bump_revision`](file:///home/nvalk1/debloatmail/crates/store/src/schema.rs#L41) on `meta.revision`).
- **Mail Headers & Unified Stream**:
  - Implemented in [`headers.rs`](file:///home/nvalk1/debloatmail/crates/store/src/headers.rs): [`insert_mail_header`](file:///home/nvalk1/debloatmail/crates/store/src/headers.rs#L36) populating `items`, `mail_items`, and stub `item_text`.
  - Body eviction ([`evict_mail_body`](file:///home/nvalk1/debloatmail/crates/store/src/headers.rs#L125)) clearing body while preserving subject and sender searchable in FTS5.
  - Stream query execution ([`query_stream`](file:///home/nvalk1/debloatmail/crates/store/src/headers.rs#L182)) supporting all stream sources and cursor pagination across `Date`, `Received`, `Author`, and `Subject` sort keys.
- **Feed Articles**:
  - Implemented in [`feed_items.rs`](file:///home/nvalk1/debloatmail/crates/store/src/feed_items.rs): [`upsert_feed_item`](file:///home/nvalk1/debloatmail/crates/store/src/feed_items.rs#L51) matching on `(feed_id, guid)`, updating content while strictly preserving read, starred, and hidden states per §11.4.
  - Excerpt derivation ([`derive_excerpt`](file:///home/nvalk1/debloatmail/crates/store/src/feed_items.rs#L22)) and title fallback ([`derive_title`](file:///home/nvalk1/debloatmail/crates/store/src/feed_items.rs#L37)).
  - Soft-removal ([`soft_remove_feed`](file:///home/nvalk1/debloatmail/crates/store/src/feed_items.rs#L151)) keeping articles in stream and search; purge ([`purge_feed`](file:///home/nvalk1/debloatmail/crates/store/src/feed_items.rs#L161)) deleting spine and text rows.
- **Flags & Offline Queue**:
  - Implemented in [`flags.rs`](file:///home/nvalk1/debloatmail/crates/store/src/flags.rs): [`set_unread`](file:///home/nvalk1/debloatmail/crates/store/src/flags.rs#L7), [`set_starred`](file:///home/nvalk1/debloatmail/crates/store/src/flags.rs#L17), [`set_answered`](file:///home/nvalk1/debloatmail/crates/store/src/flags.rs#L27), [`set_hidden`](file:///home/nvalk1/debloatmail/crates/store/src/flags.rs#L37), and offline queueing via [`queue_flag_op`](file:///home/nvalk1/debloatmail/crates/store/src/flags.rs#L48).
- **Search**:
  - Implemented in [`search.rs`](file:///home/nvalk1/debloatmail/crates/store/src/search.rs): Query parser ([`parse_search_query`](file:///home/nvalk1/debloatmail/crates/store/src/search.rs#L26)) supporting closed operator set (`title:`, `author:`, `from:`, `is:mail`, `is:feed`, positional terms) with quote sanitization to prevent FTS5 syntax injection.
  - Query execution ([`search_items`](file:///home/nvalk1/debloatmail/crates/store/src/search.rs#L77)) joining `visible_items` and `item_text_fts`.

### 4. Verification & Quality Gates
- **Tests**: `cargo test --workspace` passes (14 tests: 8 core unit tests, 6 store integration tests).
- **Linting**: `cargo clippy --workspace --all-targets -- -D warnings` clean with zero warnings.
- **Formatting**: `cargo fmt --all -- --check` clean.

---

## Next Steps (Step 2: Engine Crates)

Per [docs/architecture.md](file:///home/nvalk1/debloatmail/docs/architecture.md) §14:
1. **[`crates/extract`](file:///home/nvalk1/debloatmail/crates/extract)**:
   - MIME parsing and plaintext extraction pipeline (pure byte slices in, structures out; depends on `core` only).
   - Approved dependencies per Invariant 4: `mail-parser`, `charset`, `chardetng`, `html2text`, `thiserror`.
2. **[`crates/feeds`](file:///home/nvalk1/debloatmail/crates/feeds)**:
   - Feed polling and fetch client using `reqwest` on `rustls` (no default features).
   - Feed parsing using `feed-rs`.
3. **[`crates/engine`](file:///home/nvalk1/debloatmail/crates/engine)**:
   - Sans-I/O IMAP and SMTP coroutine pumping using pimalaya (`io-imap`, `io-smtp`).
   - Sockets and TLS in `engine/io.rs` and `engine/tls.rs`.
4. **Step 3 (Validation Spike via [`crates/cli`](file:///home/nvalk1/debloatmail/crates/cli))**:
   - Exercising auth, sync, send, feed ingest, and FTS5 search end-to-end prior to any GUI scaffolding.
