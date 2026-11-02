//! IMAP session setup (`ImapSessionOpen` pump), capability extraction,
//! folder discovery (`LIST` + `SPECIAL-USE` + localized fallback),
//! `ImapClientAsync` implementation, and `UID EXPUNGE` guard (`SYNC.md` §5.2–5.3, §5.8, §8.3).

use std::collections::HashSet;
use std::num::NonZeroU32;

use debloatmail_core::{FolderRole, RoleSource, Security};
use io_imap::client::{ImapClientAsync, ImapClientError};
use io_imap::codec::fragmentizer::Fragmentizer;
use io_imap::coroutine::{ImapCoroutine, ImapCoroutineState, ImapYield};
use io_imap::rfc3501::list::ImapMailboxListing;
use io_imap::rfc4315::expunge_uid::ImapMessageExpungeUid;
use io_imap::session::{
    ImapSessionOpen, ImapSessionOpenOptions, ImapSessionOpenYield, ImapSessionTransport,
};
use io_imap::types::core::{IString, NString};
use io_imap::types::flag::FlagNameAttribute;
use io_imap::types::mailbox::Mailbox;
use io_imap::types::response::Capability;
use io_imap::types::sequence::SequenceSet;
use io_sasl::Sasl;

use crate::EngineError;
use crate::io::{AsyncByteStream, EngineStream, MemoryStream, open_imap_stream};
use crate::tls::TlsVerifyMode;

/// Default maximum message size for the connection-wide `Fragmentizer` (50 MiB).
pub const DEFAULT_FRAGMENTIZER_MAX_BYTES: usize = 50 * 1024 * 1024;

/// Capabilities negotiated on an authenticated IMAP connection (`SYNC.md` §5.2).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NegotiatedCapabilities {
    pub idle: bool,
    pub move_ext: bool,
    pub uidplus: bool,
    pub special_use: bool,
    pub condstore: bool,
    pub qresync: bool,
    pub utf8_accept: bool,
    pub sasl_mechanisms: Vec<String>,
    pub raw_names: Vec<String>,
}

/// Extracts [`NegotiatedCapabilities`] from a post-auth or post-TLS `CAPABILITY` list.
pub fn extract_capabilities(caps: &[Capability<'_>]) -> NegotiatedCapabilities {
    let mut out = NegotiatedCapabilities::default();
    for cap in caps {
        let formatted = cap.to_string();
        let upper = formatted.to_ascii_uppercase();
        out.raw_names.push(upper.clone());
        match cap {
            Capability::Idle => out.idle = true,
            Capability::Move => out.move_ext = true,
            Capability::UidPlus => out.uidplus = true,
            Capability::CondStore => out.condstore = true,
            Capability::QResync => out.qresync = true,
            Capability::Auth(mech) => {
                out.sasl_mechanisms.push(mech.to_string().to_ascii_uppercase());
            }
            _ => {
                match upper.as_str() {
                    "SPECIAL-USE" => out.special_use = true,
                    "UTF8=ACCEPT" | "UTF8=ONLY" => out.utf8_accept = true,
                    "CONDSTORE" => out.condstore = true,
                    "QRESYNC" => out.qresync = true,
                    _ => {}
                }
            }
        }
    }
    out
}

/// A folder discovered via `LIST "" "*"` with resolved [`FolderRole`] and [`RoleSource`]
/// (`SYNC.md` §5.3, §5.8; `DATA-MODEL.md` §5.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredFolder {
    /// Raw IMAP mailbox name used in `SELECT` (modified UTF-7 decoded by `io-imap`).
    pub name: String,
    /// Leaf display name for UI/CLI presentation.
    pub display_name: String,
    /// Hierarchy delimiter (e.g. `'/'` or `'.'`).
    pub delimiter: Option<char>,
    /// Assigned special role (`None` for custom folders or deduplicated collisions).
    pub role: Option<FolderRole>,
    /// Provenance of `role` (`None` when `role.is_none()`).
    pub role_source: Option<RoleSource>,
    /// `false` when `\Noselect` is present on the mailbox.
    pub selectable: bool,
    /// Whether the folder is synced by default (`false` for `\Noselect` and `\All` per `SYNC.md` §5.8).
    pub sync_by_default: bool,
}

fn mailbox_to_string(mailbox: &Mailbox<'_>) -> String {
    match mailbox {
        Mailbox::Inbox => "INBOX".to_string(),
        Mailbox::Other(other) => String::from_utf8_lossy(other.as_ref()).into_owned(),
    }
}

fn leaf_display_name(name: &str, delimiter: Option<char>) -> String {
    if let Some(delim) = delimiter {
        if let Some((_, leaf)) = name.rsplit_once(delim) {
            if !leaf.is_empty() {
                return leaf.to_string();
            }
        }
    }
    name.to_string()
}

fn match_special_use_attrs(
    attrs: &[FlagNameAttribute<'_>],
) -> (Option<FolderRole>, bool, bool) {
    let mut role = None;
    let mut is_noselect = false;
    let mut is_all_mail = false;

    for attr in attrs {
        if matches!(attr, FlagNameAttribute::Noselect) {
            is_noselect = true;
            continue;
        }
        let s = attr.to_string();
        match s.to_ascii_lowercase().as_str() {
            "\\noselect" | "\\nonexistent" => is_noselect = true,
            "\\inbox" => {
                if role.is_none() {
                    role = Some(FolderRole::Inbox);
                }
            }
            "\\sent" => {
                if role.is_none() {
                    role = Some(FolderRole::Sent);
                }
            }
            "\\drafts" => {
                if role.is_none() {
                    role = Some(FolderRole::Drafts);
                }
            }
            "\\trash" => {
                if role.is_none() {
                    role = Some(FolderRole::Trash);
                }
            }
            "\\archive" => {
                if role.is_none() {
                    role = Some(FolderRole::Archive);
                }
            }
            "\\junk" => {
                if role.is_none() {
                    role = Some(FolderRole::Junk);
                }
            }
            "\\all" => {
                is_all_mail = true;
                if role.is_none() {
                    role = Some(FolderRole::Archive);
                }
            }
            _ => {}
        }
    }

    (role, is_noselect, is_all_mail)
}

/// Case-insensitive localized name fallback table (`SYNC.md` §5.3).
fn match_fallback_role_by_name(name: &str, leaf: &str) -> Option<FolderRole> {
    if name.eq_ignore_ascii_case("INBOX") {
        return Some(FolderRole::Inbox);
    }

    for candidate in [name, leaf] {
        let lower = candidate.trim().to_lowercase();
        match lower.as_str() {
            "sent" | "sent items" | "sent messages" | "gesendet" | "envoyés" => {
                return Some(FolderRole::Sent);
            }
            "drafts" | "draft" | "entwürfe" | "brouillons" => {
                return Some(FolderRole::Drafts);
            }
            "trash" | "deleted" | "deleted items" | "deleted messages" | "bin" | "papierkorb"
            | "corbeille" => {
                return Some(FolderRole::Trash);
            }
            "archive" | "archives" | "all mail" | "archiv" => {
                return Some(FolderRole::Archive);
            }
            "junk" | "spam" | "junk e-mail" | "bulk mail" | "unerwünscht" | "indésirables" => {
                return Some(FolderRole::Junk);
            }
            _ => {}
        }
    }

    None
}

/// Maps raw `LIST` entries into [`DiscoveredFolder`] items following `SYNC.md` §5.3 and §5.8:
/// 1. `INBOX` is always `FolderRole::Inbox`.
/// 2. RFC 6154 `SPECIAL-USE` attributes (`\Sent`, `\Drafts`, `\Trash`, `\Archive`, `\Junk`, `\All`)
///    assign roles with `RoleSource::SpecialUse` (prioritized before fallback names).
/// 3. Case-insensitive localized name matching assigns roles with `RoleSource::Name` for any role
///    not yet claimed.
/// 4. At most one folder per account may hold a given `FolderRole` (first wins; duplicates get `role = None`).
/// 5. `\Noselect` folders are marked `selectable = false` and `sync_by_default = false`.
/// 6. `\All` (`[Gmail]/All Mail`) is marked `sync_by_default = false` to avoid double-counting (`SYNC.md` §5.8).
pub fn discover_folders(listing: ImapMailboxListing) -> Vec<DiscoveredFolder> {
    struct RawEntry {
        name: String,
        display_name: String,
        delimiter: Option<char>,
        special_role: Option<FolderRole>,
        selectable: bool,
        is_all_mail: bool,
    }

    let mut raw_entries = Vec::with_capacity(listing.len());
    for (mailbox, delim_opt, attrs) in listing {
        let name = mailbox_to_string(&mailbox);
        let delimiter = delim_opt.map(|q| q.inner());
        let display_name = leaf_display_name(&name, delimiter);
        let (special_role, is_noselect, is_all_mail) = match_special_use_attrs(&attrs);
        let selectable = !is_noselect;
        raw_entries.push(RawEntry {
            name,
            display_name,
            delimiter,
            special_role,
            selectable,
            is_all_mail,
        });
    }

    let mut claimed_roles: HashSet<FolderRole> = HashSet::new();
    let mut assigned: Vec<Option<(FolderRole, RoleSource)>> = vec![None; raw_entries.len()];

    // Pass 1: INBOX and RFC 6154 SPECIAL-USE attributes take priority over name fallbacks.
    for (idx, entry) in raw_entries.iter().enumerate() {
        if !entry.selectable {
            continue;
        }
        if entry.name.eq_ignore_ascii_case("INBOX") {
            if claimed_roles.insert(FolderRole::Inbox) {
                let src = if entry.special_role == Some(FolderRole::Inbox) {
                    RoleSource::SpecialUse
                } else {
                    RoleSource::Name
                };
                assigned[idx] = Some((FolderRole::Inbox, src));
            }
        } else if let Some(role) = entry.special_role {
            if claimed_roles.insert(role) {
                assigned[idx] = Some((role, RoleSource::SpecialUse));
            }
        }
    }

    // Pass 2: Fallback localized name matching for remaining unclaimed roles.
    for (idx, entry) in raw_entries.iter().enumerate() {
        if !entry.selectable || assigned[idx].is_some() {
            continue;
        }
        if let Some(role) = match_fallback_role_by_name(&entry.name, &entry.display_name) {
            if claimed_roles.insert(role) {
                assigned[idx] = Some((role, RoleSource::Name));
            }
        }
    }

    raw_entries
        .into_iter()
        .zip(assigned)
        .map(|(entry, role_pair)| {
            let (role, role_source) = match role_pair {
                Some((r, s)) => (Some(r), Some(s)),
                None => (None, None),
            };
            let is_gmail_all_mail =
                entry.is_all_mail || entry.name.eq_ignore_ascii_case("[Gmail]/All Mail");
            let sync_by_default = entry.selectable && !is_gmail_all_mail;

            DiscoveredFolder {
                name: entry.name,
                display_name: entry.display_name,
                delimiter: entry.delimiter,
                role,
                role_source,
                selectable: entry.selectable,
                sync_by_default,
            }
        })
        .collect()
}

/// Active IMAP connection owning the byte stream and a connection-wide [`Fragmentizer`]
/// (`SYNC.md` §5.1).
pub struct ImapConnection<S: AsyncByteStream> {
    pub stream: S,
    pub fragmentizer: Fragmentizer,
    pub capabilities: NegotiatedCapabilities,
    pub verify_mode: TlsVerifyMode,
}

impl ImapConnection<EngineStream> {
    /// Connects, optionally upgrades via `STARTTLS`, and authenticates an IMAP session
    /// over a live network socket by pumping `ImapSessionOpen`.
    pub async fn connect(
        host: &str,
        port: u16,
        security: Security,
        allow_invalid_cert_loopback: bool,
        sasl: Option<Sasl>,
        auto_id: bool,
    ) -> Result<Self, EngineError> {
        let (transport, starttls) = open_imap_stream(host, port, security)?;
        let for_cleartext = security == Security::None;

        let opts = ImapSessionOpenOptions {
            starttls,
            auto_id: build_auto_id_params(auto_id)?,
            sasl_ir: None,
        };

        let mut coroutine = ImapSessionOpen::new(transport, sasl, opts);
        let mut fragmentizer = Fragmentizer::new(DEFAULT_FRAGMENTIZER_MAX_BYTES);
        let mut stream: Option<EngineStream> = None;
        let mut buf = [0u8; 8192];
        let mut arg: Option<Vec<u8>> = None;

        let open_data = loop {
            let state = coroutine.resume(&mut fragmentizer, arg.as_deref());
            arg = None;

            match state {
                ImapCoroutineState::Yielded(yielded) => match yielded {
                    ImapSessionOpenYield::WantsTcpConnect {
                        host: conn_host,
                        port: conn_port,
                    } => {
                        stream = Some(
                            EngineStream::connect_tcp(&conn_host, conn_port, for_cleartext)
                                .await?,
                        );
                    }
                    ImapSessionOpenYield::WantsTlsConnect {
                        host: conn_host,
                        port: conn_port,
                    } => {
                        stream = Some(
                            EngineStream::connect_tls(
                                &conn_host,
                                conn_port,
                                allow_invalid_cert_loopback,
                                vec![b"imap".to_vec()],
                            )
                            .await?,
                        );
                    }
                    ImapSessionOpenYield::WantsUnixConnect(path) => {
                        return Err(EngineError::InvalidArgument(format!(
                            "unix domain socket IMAP transport is not supported ({path})"
                        )));
                    }
                    ImapSessionOpenYield::WantsTlsUpgrade => {
                        let existing = stream.take().ok_or_else(|| {
                            EngineError::Tls(
                                "IMAP STARTTLS requested before TCP connection was opened"
                                    .to_string(),
                            )
                        })?;
                        stream = Some(
                            existing
                                .upgrade_tls(host, allow_invalid_cert_loopback, vec![])
                                .await?,
                        );
                    }
                    ImapSessionOpenYield::WantsWrite(bytes) => {
                        let s = stream.as_mut().ok_or_else(|| {
                            EngineError::Io(std::io::Error::new(
                                std::io::ErrorKind::NotConnected,
                                "IMAP coroutine requested write before connecting",
                            ))
                        })?;
                        s.write_all_bytes(&bytes).await?;
                    }
                    ImapSessionOpenYield::WantsRead => {
                        let s = stream.as_mut().ok_or_else(|| {
                            EngineError::Io(std::io::Error::new(
                                std::io::ErrorKind::NotConnected,
                                "IMAP coroutine requested read before connecting",
                            ))
                        })?;
                        let n = s.read_chunk(&mut buf).await?;
                        arg = Some(buf[..n].to_vec());
                    }
                },
                ImapCoroutineState::Complete(res) => break res?,
            }
        };

        let active_stream = stream.ok_or_else(|| {
            EngineError::Io(std::io::Error::new(
                std::io::ErrorKind::NotConnected,
                "IMAP session completed without opening a stream",
            ))
        })?;
        let verify_mode = active_stream.verify_mode();
        let capabilities = extract_capabilities(&open_data.capability);

        Ok(Self {
            stream: active_stream,
            fragmentizer,
            capabilities,
            verify_mode,
        })
    }
}

fn build_auto_id_params(
    auto_id: bool,
) -> Result<Option<Vec<(IString<'static>, NString<'static>)>>, EngineError> {
    if !auto_id {
        return Ok(None);
    }
    let name_key = IString::try_from("name")
        .map_err(|e| EngineError::InvalidArgument(e.to_string()))?;
    let name_val = NString::from(
        IString::try_from("debloatmail")
            .map_err(|e| EngineError::InvalidArgument(e.to_string()))?,
    );
    Ok(Some(vec![(name_key, name_val)]))
}

impl ImapConnection<MemoryStream> {
    /// Drives `ImapSessionOpen` over a deterministic in-memory byte stream for zero-socket unit tests.
    pub async fn connect_memory(
        mut stream: MemoryStream,
        starttls: bool,
        sasl: Option<Sasl>,
        auto_id: bool,
    ) -> Result<Self, EngineError> {
        let transport = ImapSessionTransport::Tcp {
            host: "127.0.0.1".to_string(),
            port: 1143,
        };
        let opts = ImapSessionOpenOptions {
            starttls,
            auto_id: build_auto_id_params(auto_id)?,
            sasl_ir: None,
        };

        let mut coroutine = ImapSessionOpen::new(transport, sasl, opts);
        let mut fragmentizer = Fragmentizer::new(DEFAULT_FRAGMENTIZER_MAX_BYTES);
        let mut buf = [0u8; 4096];
        let mut arg: Option<Vec<u8>> = None;

        let open_data = loop {
            let state = coroutine.resume(&mut fragmentizer, arg.as_deref());
            arg = None;

            match state {
                ImapCoroutineState::Yielded(yielded) => match yielded {
                    ImapSessionOpenYield::WantsTcpConnect { .. }
                    | ImapSessionOpenYield::WantsTlsConnect { .. }
                    | ImapSessionOpenYield::WantsUnixConnect(_) => {}
                    ImapSessionOpenYield::WantsTlsUpgrade => {
                        stream.mark_tls_upgraded();
                    }
                    ImapSessionOpenYield::WantsWrite(bytes) => {
                        stream.write_all_bytes(&bytes).await?;
                    }
                    ImapSessionOpenYield::WantsRead => {
                        let n = stream.read_chunk(&mut buf).await?;
                        arg = Some(buf[..n].to_vec());
                    }
                },
                ImapCoroutineState::Complete(res) => break res?,
            }
        };

        let capabilities = extract_capabilities(&open_data.capability);
        let verify_mode = if stream.is_tls_upgraded() {
            TlsVerifyMode::StrictPlatform
        } else {
            TlsVerifyMode::CleartextLoopback
        };

        Ok(Self {
            stream,
            fragmentizer,
            capabilities,
            verify_mode,
        })
    }

    /// Creates an authenticated in-memory connection directly for testing post-auth commands.
    pub fn from_authenticated_memory(
        stream: MemoryStream,
        capabilities: NegotiatedCapabilities,
    ) -> Self {
        Self {
            stream,
            fragmentizer: Fragmentizer::new(DEFAULT_FRAGMENTIZER_MAX_BYTES),
            capabilities,
            verify_mode: TlsVerifyMode::StrictPlatform,
        }
    }
}

impl<S: AsyncByteStream> ImapClientAsync for ImapConnection<S> {
    async fn run<C, T, E>(&mut self, mut coroutine: C) -> Result<T, ImapClientError>
    where
        C: ImapCoroutine<Yield = ImapYield, Return = Result<T, E>> + Send,
        T: Send,
        E: Send,
        ImapClientError: From<E>,
    {
        let mut buf = [0u8; 8192];
        let mut arg: Option<Vec<u8>> = None;

        loop {
            let state = coroutine.resume(&mut self.fragmentizer, arg.as_deref());
            arg = None;

            match state {
                ImapCoroutineState::Yielded(ImapYield::WantsWrite(bytes)) => {
                    self.stream.write_all_bytes(&bytes).await?;
                }
                ImapCoroutineState::Yielded(ImapYield::WantsRead) => {
                    let n = self.stream.read_chunk(&mut buf).await?;
                    arg = Some(buf[..n].to_vec());
                }
                ImapCoroutineState::Complete(res) => return Ok(res?),
            }
        }
    }
}

/// Expunges specific message UIDs using RFC 4315 `UID EXPUNGE`.
///
/// # Protocol Safety Invariant (`SYNC.md` §8.3)
/// Never issues plain `EXPUNGE` (which would permanently purge unrelated `\Deleted` messages
/// in the mailbox). If `UIDPLUS` is not negotiated on the connection, returns
/// [`EngineError::UnsupportedCapability`] (`"UIDPLUS"`).
pub async fn uid_expunge_messages<S: AsyncByteStream>(
    conn: &mut ImapConnection<S>,
    sequence_set: SequenceSet,
) -> Result<Vec<NonZeroU32>, EngineError> {
    if !conn.capabilities.uidplus {
        return Err(EngineError::UnsupportedCapability("UIDPLUS"));
    }
    let expunged = conn.run(ImapMessageExpungeUid::new(sequence_set)).await?;
    Ok(expunged)
}
