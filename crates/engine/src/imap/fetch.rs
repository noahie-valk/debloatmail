//! IMAP `UID FETCH` helpers for windowed header sync, on-demand `BODY.PEEK[]`,
//! and on-demand attachment `BODY.PEEK[<section_id>]` (`SYNC.md` §5.5, §6.1, §6.2, §12).
//!
//! # Protocol Safety Invariant
//! All body and section fetches set `peek: true` (`BODY.PEEK[...]`) so fetching message text
//! or attachments never implicitly marks a message `\Seen` on the server.

use std::collections::BTreeMap;
use std::num::NonZeroU32;

use io_imap::client::ImapClientAsync;
use io_imap::rfc3501::fetch::{ImapMessageFetch, ImapMessageFetchFirst, ImapMessageFetchOptions};
use io_imap::types::core::Vec1;
use io_imap::types::fetch::{
    MacroOrMessageDataItemNames, MessageDataItem, MessageDataItemName, Part, Section,
};
use io_imap::types::sequence::{SeqOrUid, Sequence, SequenceSet};

use crate::EngineError;
use crate::imap::ImapConnection;
use crate::io::AsyncByteStream;

/// Maximum number of UIDs requested in a single `UID FETCH` header window (`SYNC.md` §5.5, §12).
pub const HEADER_FETCH_WINDOW_SIZE: u32 = 2_000;

/// Validates an RFC 3501 §6.4.5 MIME section identifier (`^[1-9][0-9]*(\.[1-9][0-9]*)*$`)
/// and converts it into an `imap_types::fetch::Part`.
///
/// Rejects empty strings, leading/trailing dots, leading zeros (`01`), non-digits, or
/// zero part numbers (`0`).
pub fn validate_section_id(section_id: &str) -> Result<Part, EngineError> {
    if section_id.is_empty() {
        return Err(EngineError::InvalidArgument(
            "empty MIME section_id".to_string(),
        ));
    }

    let mut parts = Vec::new();
    for segment in section_id.split('.') {
        if segment.is_empty() || segment.starts_with('0') {
            return Err(EngineError::InvalidArgument(format!(
                "invalid MIME section_id '{section_id}': must match ^[1-9][0-9]*(\\.[1-9][0-9]*)*$"
            )));
        }
        if !segment.bytes().all(|b| b.is_ascii_digit()) {
            return Err(EngineError::InvalidArgument(format!(
                "invalid MIME section_id '{section_id}': non-digit character"
            )));
        }
        let parsed: u32 = segment.parse().map_err(|_| {
            EngineError::InvalidArgument(format!(
                "invalid MIME section_id '{section_id}': integer overflow"
            ))
        })?;
        let nz = NonZeroU32::new(parsed).ok_or_else(|| {
            EngineError::InvalidArgument(format!(
                "invalid MIME section_id '{section_id}': zero part number"
            ))
        })?;
        parts.push(nz);
    }

    let vec1 = Vec1::try_from(parts).map_err(|_| {
        EngineError::InvalidArgument(format!("invalid MIME section_id '{section_id}'"))
    })?;
    Ok(Part(vec1))
}

/// Returns the standard `UID FETCH` item list for header sync (`SYNC.md` §5.5):
/// `(UID FLAGS INTERNALDATE RFC822.SIZE ENVELOPE BODYSTRUCTURE)`.
pub fn header_fetch_item_names() -> MacroOrMessageDataItemNames<'static> {
    MacroOrMessageDataItemNames::MessageDataItemNames(vec![
        MessageDataItemName::Uid,
        MessageDataItemName::Flags,
        MessageDataItemName::InternalDate,
        MessageDataItemName::Rfc822Size,
        MessageDataItemName::Envelope,
        MessageDataItemName::BodyStructure,
    ])
}

/// Fetches a window of message metadata (`UID`, `FLAGS`, `INTERNALDATE`, `RFC822.SIZE`,
/// `ENVELOPE`, `BODYSTRUCTURE`) starting at `uid_start` and bounded to at most
/// [`HEADER_FETCH_WINDOW_SIZE`] (2,000) UIDs (`SYNC.md` §5.5, §12).
pub async fn fetch_header_window<S: AsyncByteStream>(
    conn: &mut ImapConnection<S>,
    uid_start: u32,
    uid_end: Option<u32>,
) -> Result<BTreeMap<NonZeroU32, Vec1<MessageDataItem<'static>>>, EngineError> {
    let start_nz = NonZeroU32::new(uid_start)
        .ok_or_else(|| EngineError::InvalidArgument("uid_start must be >= 1".to_string()))?;

    let capped_end_val = match uid_end {
        Some(end) => {
            if end < uid_start {
                return Err(EngineError::InvalidArgument(format!(
                    "uid_end ({end}) must be >= uid_start ({uid_start})"
                )));
            }
            end.min(uid_start.saturating_add(HEADER_FETCH_WINDOW_SIZE - 1))
        }
        None => uid_start.saturating_add(HEADER_FETCH_WINDOW_SIZE - 1),
    };

    let end_nz = NonZeroU32::new(capped_end_val)
        .ok_or_else(|| EngineError::InvalidArgument("computed uid_end must be >= 1".to_string()))?;

    let seq = if start_nz == end_nz {
        Sequence::Single(SeqOrUid::Value(start_nz))
    } else {
        Sequence::Range(SeqOrUid::Value(start_nz), SeqOrUid::Value(end_nz))
    };
    let sequence_set = SequenceSet(Vec1::from(seq));

    let opts = ImapMessageFetchOptions {
        uid: true,
        modifiers: Vec::new(),
    };
    let coroutine = ImapMessageFetch::new(sequence_set, header_fetch_item_names(), opts);
    let rows = conn.run(coroutine).await?;
    Ok(rows)
}

/// Fetches the full RFC 5322 message bytes on-demand via `UID FETCH <uid> (BODY.PEEK[])`
/// (`SYNC.md` §6.1).
///
/// Uses `peek: true` (`BODY.PEEK[]`) so the message is never marked `\Seen` as a side effect.
pub async fn fetch_body_peek<S: AsyncByteStream>(
    conn: &mut ImapConnection<S>,
    uid: NonZeroU32,
) -> Result<Vec<u8>, EngineError> {
    let items = MacroOrMessageDataItemNames::MessageDataItemNames(vec![
        MessageDataItemName::Uid,
        MessageDataItemName::BodyExt {
            section: None,
            partial: None,
            peek: true,
        },
    ]);
    let opts = ImapMessageFetchOptions {
        uid: true,
        modifiers: Vec::new(),
    };
    let fetched = conn
        .run(ImapMessageFetchFirst::new(uid, items, opts))
        .await?;
    extract_body_bytes(fetched.as_ref(), None)
}

/// Fetches a single MIME body section on-demand via `UID FETCH <uid> (BODY.PEEK[<section_id>])`
/// (`SYNC.md` §6.2).
///
/// Validates `section_id` against `^[1-9][0-9]*(\.[1-9][0-9]*)*$` before building the command,
/// and uses `peek: true` (`BODY.PEEK[...]`).
pub async fn fetch_attachment_section<S: AsyncByteStream>(
    conn: &mut ImapConnection<S>,
    uid: NonZeroU32,
    section_id: &str,
) -> Result<Vec<u8>, EngineError> {
    let part = validate_section_id(section_id)?;
    let expected_section = Section::Part(part.clone());
    let items = MacroOrMessageDataItemNames::MessageDataItemNames(vec![
        MessageDataItemName::Uid,
        MessageDataItemName::BodyExt {
            section: Some(Section::Part(part)),
            partial: None,
            peek: true,
        },
    ]);
    let opts = ImapMessageFetchOptions {
        uid: true,
        modifiers: Vec::new(),
    };
    let fetched = conn
        .run(ImapMessageFetchFirst::new(uid, items, opts))
        .await?;
    extract_body_bytes(fetched.as_ref(), Some(&expected_section))
}

fn extract_body_bytes(
    items: &[MessageDataItem<'_>],
    expected_section: Option<&Section<'_>>,
) -> Result<Vec<u8>, EngineError> {
    for item in items {
        match item {
            MessageDataItem::BodyExt { section, data, .. } => {
                if section.as_ref() == expected_section {
                    return Ok(data
                        .0
                        .as_ref()
                        .map(|s| s.as_ref().to_vec())
                        .unwrap_or_default());
                }
            }
            MessageDataItem::Rfc822(data) if expected_section.is_none() => {
                return Ok(data
                    .0
                    .as_ref()
                    .map(|s| s.as_ref().to_vec())
                    .unwrap_or_default());
            }
            _ => {}
        }
    }

    Err(EngineError::InvalidArgument(
        "server FETCH response did not contain the requested BODY[]/BODY[<section>] data"
            .to_string(),
    ))
}
