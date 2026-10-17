use debloatmail_core::item::BodyState;
use mail_parser::{Message, MessagePart, MimeHeaders};

use crate::charset;
use crate::error::ExtractError;
use crate::flowed;
use crate::header;
use crate::html;
use crate::limits::Limits;

/// Metadata describing an attachment discovered during the structure walk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedAttachment {
    /// RFC 3501 §6.4.5 section number, e.g. "1", "2", "1.2".
    pub section_id: String,
    pub filename: Option<String>,
    pub mime_type: String,
    pub size_bytes: Option<u64>,
    pub content_id: Option<String>,
    pub is_pdf: bool,
}

/// The result of extracting a full RFC 5322 message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedMessage {
    pub title: String,
    pub subject_norm: String,
    pub author: Option<String>,
    pub from_addr: Option<String>,
    pub to_addrs: Vec<String>,
    pub cc_addrs: Vec<String>,
    pub reply_to: Option<String>,
    pub message_id: Option<String>,
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
    pub date: Option<i64>,
    pub body: String,
    pub body_state: BodyState,
    pub excerpt: Option<String>,
    pub has_attachments: bool,
    pub attachments: Vec<ExtractedAttachment>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone)]
enum PartClassification {
    Container,
    Attachment,
    BodyCandidate { is_html: bool },
}

struct WalkState<'a> {
    message: &'a Message<'a>,
    limits: &'a Limits,
    attachments: Vec<ExtractedAttachment>,
    body_candidate: Option<(SelectedBodyPart<'a>, bool)>, // (part, is_html)
    is_encrypted: bool,
    warnings: Vec<String>,
}

struct SelectedBodyPart<'a> {
    part: &'a MessagePart<'a>,
    section_id: String,
}

pub fn extract_message(
    raw_bytes: &[u8],
    limits: &Limits,
) -> Result<ExtractedMessage, ExtractError> {
    let mut warnings = Vec::new();

    // Check raw size limit per MIME-AND-TEXT.md §13 & §14
    if raw_bytes.len() > limits.max_raw_message_size {
        warnings.push(format!(
            "Raw message size ({} bytes) exceeds limit ({} bytes)",
            raw_bytes.len(),
            limits.max_raw_message_size
        ));
        return Ok(ExtractedMessage {
            title: "(no subject)".to_string(),
            subject_norm: String::new(),
            author: None,
            from_addr: None,
            to_addrs: Vec::new(),
            cc_addrs: Vec::new(),
            reply_to: None,
            message_id: None,
            in_reply_to: None,
            references: Vec::new(),
            date: None,
            body: String::new(),
            body_state: BodyState::Failed,
            excerpt: None,
            has_attachments: false,
            attachments: Vec::new(),
            warnings,
        });
    }

    let parsed = mail_parser::MessageParser::default().parse(raw_bytes);
    let message = match parsed {
        Some(m) => m,
        None => {
            warnings.push("Failed to parse RFC 5322 message structure".to_string());
            return Ok(ExtractedMessage {
                title: "(no subject)".to_string(),
                subject_norm: String::new(),
                author: None,
                from_addr: None,
                to_addrs: Vec::new(),
                cc_addrs: Vec::new(),
                reply_to: None,
                message_id: None,
                in_reply_to: None,
                references: Vec::new(),
                date: None,
                body: String::new(),
                body_state: BodyState::Failed,
                excerpt: None,
                has_attachments: false,
                attachments: Vec::new(),
                warnings,
            });
        }
    };

    let title = derive_title(message.subject());
    let subject_norm = header::normalize_subject(message.subject());
    let (author, from_addr) = header::derive_author(&message);
    let to_addrs = header::extract_all_addresses(message.to());
    let cc_addrs = header::extract_all_addresses(message.cc());
    let reply_to = header::extract_all_addresses(message.reply_to())
        .into_iter()
        .next();
    let message_id = message.message_id().map(header::clean_msg_id);
    let in_reply_to = message.in_reply_to().as_text().map(header::clean_msg_id);

    let mut references = Vec::new();
    for h in message.headers() {
        if h.name.as_str().eq_ignore_ascii_case("references") {
            references.extend(header::extract_message_ids(&h.value));
        }
    }

    let date = message.date().map(|d| d.to_timestamp());

    let mut state = WalkState {
        message: &message,
        limits,
        attachments: Vec::new(),
        body_candidate: None,
        is_encrypted: false,
        warnings,
    };

    if !message.parts.is_empty() {
        let root_part = &message.parts[0];
        if root_part.is_multipart() {
            walk_multipart(0, "", &mut state, 1);
        } else {
            walk_single_part(0, "1", &mut state);
        }
    }

    // Process selected body
    let (body_text, body_state) = if state.is_encrypted {
        ("[encrypted message]".to_string(), BodyState::Cached)
    } else if let Some((selected, is_html)) = state.body_candidate {
        let recovered_bytes;
        let raw_part_bytes = if let mail_parser::PartType::Text(ref text) = selected.part.body {
            if text.chars().all(|c| (c as u32) <= 0xFF) {
                let candidate: Vec<u8> = text.chars().map(|c| c as u8).collect();
                if std::str::from_utf8(&candidate).is_ok() {
                    recovered_bytes = candidate;
                    &recovered_bytes[..]
                } else {
                    selected.part.contents()
                }
            } else {
                selected.part.contents()
            }
        } else {
            selected.part.contents()
        };

        // Check for compressed Content-Encoding per §7 & §14 (treat as opaque, do not decompress)
        let has_compression = selected.part.headers.iter().any(|h| {
            h.name.as_str().eq_ignore_ascii_case("content-encoding")
                && (h
                    .value
                    .as_text()
                    .unwrap_or("")
                    .to_ascii_lowercase()
                    .contains("gzip")
                    || h.value
                        .as_text()
                        .unwrap_or("")
                        .to_ascii_lowercase()
                        .contains("deflate"))
        });

        if has_compression {
            state.warnings.push(format!(
                "Part {} uses compressed Content-Encoding; treated as opaque without decompression",
                selected.section_id
            ));
            (String::new(), BodyState::Cached)
        } else {
            let declared_charset = selected.part.content_type().and_then(|ct| {
                ct.attributes().and_then(|attrs| {
                    attrs
                        .iter()
                        .find(|a| a.name.eq_ignore_ascii_case("charset"))
                        .map(|a| a.value.as_ref())
                })
            });

            let decoded_text = charset::decode_bytes(raw_part_bytes, declared_charset);

            let rendered = if is_html {
                html::html_to_plaintext(&decoded_text, None)
            } else {
                let is_flowed = selected.part.content_type().is_some_and(|ct| {
                    ct.attributes().is_some_and(|attrs| {
                        attrs.iter().any(|a| {
                            a.name.eq_ignore_ascii_case("format")
                                && a.value.eq_ignore_ascii_case("flowed")
                        })
                    })
                });
                let delsp = selected.part.content_type().is_some_and(|ct| {
                    ct.attributes().is_some_and(|attrs| {
                        attrs.iter().any(|a| {
                            a.name.eq_ignore_ascii_case("delsp")
                                && a.value.eq_ignore_ascii_case("yes")
                        })
                    })
                });
                normalize_plaintext_whitespace(&flowed::unwrap_flowed(
                    &decoded_text,
                    is_flowed,
                    delsp,
                ))
            };

            // Truncate at paragraph boundary if body exceeds limit per §13
            let truncated = if rendered.len() > limits.max_extracted_body_size {
                state.warnings.push(format!(
                    "Extracted body ({} bytes) exceeds limit ({} bytes); truncated at paragraph boundary",
                    rendered.len(),
                    limits.max_extracted_body_size
                ));
                truncate_at_paragraph_boundary(&rendered, limits.max_extracted_body_size)
            } else {
                rendered
            };

            (truncated, BodyState::Cached)
        }
    } else {
        // No body candidate per §6.2 / §14 -> body_state = Cached with empty text
        (String::new(), BodyState::Cached)
    };

    let excerpt = derive_excerpt(&body_text, 200);
    let has_attachments = !state.attachments.is_empty();

    Ok(ExtractedMessage {
        title,
        subject_norm,
        author,
        from_addr,
        to_addrs,
        cc_addrs,
        reply_to,
        message_id,
        in_reply_to,
        references,
        date,
        body: body_text,
        body_state,
        excerpt,
        has_attachments,
        attachments: state.attachments,
        warnings: state.warnings,
    })
}

fn walk_single_part<'a>(part_id: usize, section_id: &str, state: &mut WalkState<'a>) {
    let part = &state.message.parts[part_id];
    let class = classify_part(part);

    match class {
        PartClassification::BodyCandidate { is_html } => {
            state.body_candidate = Some((
                SelectedBodyPart {
                    part,
                    section_id: section_id.to_string(),
                },
                is_html,
            ));
        }
        PartClassification::Attachment => {
            record_attachment(part, section_id, state);
        }
        PartClassification::Container => {
            walk_multipart(part_id, section_id, state, 1);
        }
    }
}

fn walk_multipart<'a>(
    part_id: usize,
    parent_section: &str,
    state: &mut WalkState<'a>,
    depth: usize,
) {
    if depth > state.limits.max_mime_nesting_depth {
        state.warnings.push(format!(
            "MIME nesting depth limit exceeded at section {}",
            parent_section
        ));
        return;
    }

    let part = &state.message.parts[part_id];

    // Check if multipart/encrypted per §6.2
    if is_subtype(part, "multipart", "encrypted") {
        state.is_encrypted = true;
        return;
    }

    let is_alt_or_report =
        is_subtype(part, "multipart", "alternative") || is_subtype(part, "multipart", "report");
    let is_signed = is_subtype(part, "multipart", "signed");

    let subparts = match part.sub_parts() {
        Some(sp) => sp,
        None => return,
    };

    if is_alt_or_report {
        // multipart/alternative (§6.2):
        // 1. Prefer text/plain if present and not empty/whitespace-only.
        // 2. Otherwise fall through to text/html.
        let mut text_candidate: Option<(usize, String)> = None; // (part_idx, section_id)
        let mut html_candidate: Option<(usize, String)> = None;

        for (idx, &sub_id) in subparts.iter().enumerate() {
            let section = format_sub_section(parent_section, idx + 1);
            let sub_part = &state.message.parts[sub_id as usize];
            if sub_part.is_multipart() {
                // Nested container in alternative
                walk_multipart(sub_id as usize, &section, state, depth + 1);
            } else {
                let class = classify_part(sub_part);
                match class {
                    PartClassification::BodyCandidate { is_html: false } => {
                        let text = charset::decode_bytes(sub_part.contents(), None);
                        if !text.trim().is_empty() {
                            text_candidate = Some((sub_id as usize, section));
                        }
                    }
                    PartClassification::BodyCandidate { is_html: true } => {
                        if html_candidate.is_none() {
                            html_candidate = Some((sub_id as usize, section));
                        }
                    }
                    PartClassification::Attachment => {
                        record_attachment(sub_part, &section, state);
                    }
                    _ => {}
                }
            }
        }

        if let Some((pid, sec)) = text_candidate {
            if state.body_candidate.is_none() {
                state.body_candidate = Some((
                    SelectedBodyPart {
                        part: &state.message.parts[pid],
                        section_id: sec,
                    },
                    false,
                ));
            }
        } else if let Some((pid, sec)) = html_candidate {
            if state.body_candidate.is_none() {
                state.body_candidate = Some((
                    SelectedBodyPart {
                        part: &state.message.parts[pid],
                        section_id: sec,
                    },
                    true,
                ));
            }
        }
    } else if is_signed {
        // multipart/signed (§6.2): first part is content, signature part is discarded
        if let Some(&first_id) = subparts.first() {
            let section = format_sub_section(parent_section, 1);
            let sub_part = &state.message.parts[first_id as usize];
            if sub_part.is_multipart() {
                walk_multipart(first_id as usize, &section, state, depth + 1);
            } else {
                walk_single_part(first_id as usize, &section, state);
            }
        }
    } else {
        // multipart/mixed, multipart/related, or standard multipart
        for (idx, &sub_id) in subparts.iter().enumerate() {
            let section = format_sub_section(parent_section, idx + 1);
            let sub_part = &state.message.parts[sub_id as usize];

            if sub_part.is_multipart() {
                walk_multipart(sub_id as usize, &section, state, depth + 1);
            } else {
                let class = classify_part(sub_part);
                match class {
                    PartClassification::BodyCandidate { is_html } => {
                        if state.body_candidate.is_none() {
                            state.body_candidate = Some((
                                SelectedBodyPart {
                                    part: sub_part,
                                    section_id: section,
                                },
                                is_html,
                            ));
                        } else {
                            // Subsequent body candidate in multipart/mixed becomes attachment (§6.3)
                            record_attachment(sub_part, &section, state);
                        }
                    }
                    PartClassification::Attachment => {
                        record_attachment(sub_part, &section, state);
                    }
                    PartClassification::Container => {
                        walk_multipart(sub_id as usize, &section, state, depth + 1);
                    }
                }
            }
        }
    }
}

fn format_sub_section(parent: &str, index: usize) -> String {
    if parent.is_empty() {
        format!("{}", index)
    } else {
        format!("{}.{}", parent, index)
    }
}

fn classify_part(part: &MessagePart<'_>) -> PartClassification {
    // 1. Content-Disposition: attachment -> Attachment (§6.1 #1)
    if let Some(cd) = part.content_disposition() {
        if cd.c_type.eq_ignore_ascii_case("attachment") {
            return PartClassification::Attachment;
        }
    }

    // 2. A filename/name parameter is present -> Attachment (§6.1 #2)
    if part.attachment_name().is_some() {
        return PartClassification::Attachment;
    }

    // 3. Type is multipart/* -> Container (§6.1 #3)
    if part.is_multipart() {
        return PartClassification::Container;
    }

    // 4. Type is message/rfc822 -> Attachment (§6.1 #4)
    if part.is_message() || is_type(part, "message") {
        return PartClassification::Attachment;
    }

    // 5. Type is text/calendar -> Attachment (§6.1 #5)
    if is_subtype(part, "text", "calendar") {
        return PartClassification::Attachment;
    }

    // 6. Type is text/plain or text/html -> Body candidate (§6.1 #6)
    if is_subtype(part, "text", "plain") || (part.is_text() && !part.is_text_html()) {
        return PartClassification::BodyCandidate { is_html: false };
    }
    if is_subtype(part, "text", "html") || part.is_text_html() {
        return PartClassification::BodyCandidate { is_html: true };
    }

    // 7 & 8. Other text or application/image/etc -> Attachment
    PartClassification::Attachment
}

fn record_attachment(part: &MessagePart<'_>, section_id: &str, state: &mut WalkState<'_>) {
    if state.attachments.len() >= state.limits.max_attachment_count {
        state.warnings.push(format!(
            "Attachment limit ({}) exceeded; dropping part at section {}",
            state.limits.max_attachment_count, section_id
        ));
        return;
    }

    let raw_bytes = part.contents();
    let is_pdf = raw_bytes.starts_with(b"%PDF-");

    let filename = part.attachment_name().map(|s| s.trim().to_string());
    let mime_type = part
        .content_type()
        .map_or("application/octet-stream".to_string(), |ct| {
            let subtype = ct.c_subtype.as_deref().unwrap_or("octet-stream");
            format!(
                "{}/{}",
                ct.c_type.to_ascii_lowercase(),
                subtype.to_ascii_lowercase()
            )
        });

    let content_id = part
        .content_id()
        .map(|cid| cid.trim().trim_matches(['<', '>']).to_string());
    let size_bytes = Some(raw_bytes.len() as u64);

    state.attachments.push(ExtractedAttachment {
        section_id: section_id.to_string(),
        filename,
        mime_type,
        size_bytes,
        content_id,
        is_pdf,
    });
}

fn is_type(part: &MessagePart<'_>, main_type: &str) -> bool {
    part.content_type()
        .is_some_and(|ct| ct.c_type.eq_ignore_ascii_case(main_type))
}

fn is_subtype(part: &MessagePart<'_>, main_type: &str, sub_type: &str) -> bool {
    part.content_type().is_some_and(|ct| {
        ct.c_type.eq_ignore_ascii_case(main_type)
            && ct
                .c_subtype
                .as_deref()
                .is_some_and(|s| s.eq_ignore_ascii_case(sub_type))
    })
}

/// Truncates body at the closest paragraph boundary (double newline) under max_len.
fn truncate_at_paragraph_boundary(text: &str, max_len: usize) -> String {
    if text.len() <= max_len {
        return text.to_string();
    }

    let slice = &text[..max_len];
    if let Some(pos) = slice.rfind("\n\n") {
        text[..pos].to_string()
    } else if let Some(pos) = slice.rfind('\n') {
        text[..pos].to_string()
    } else {
        slice.to_string()
    }
}

/// Whitespace normalisation for plaintext bodies per MIME-AND-TEXT.md §9 and §10.5.
pub fn normalize_plaintext_whitespace(text: &str) -> String {
    // 1. Normalise CRLF and lone CR to LF
    let mut normalized = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\r' {
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
            normalized.push('\n');
        } else {
            normalized.push(c);
        }
    }

    // 4. Collapse three or more consecutive newlines to two (one blank line)
    // 5. Strip leading and trailing blank lines
    let mut out = String::new();
    let mut consecutive_blanks = 0;

    for line in normalized.split('\n') {
        if line.trim().is_empty() {
            consecutive_blanks += 1;
            if consecutive_blanks <= 1 && !out.is_empty() {
                out.push('\n');
            }
        } else {
            if consecutive_blanks > 0 && !out.is_empty() {
                out.push('\n');
            }
            consecutive_blanks = 0;
            if !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(line);
        }
    }

    out.trim_matches(['\n', '\r']).to_string()
}

/// Derives title per MIME-AND-TEXT.md §12.1.
pub fn derive_title(subject: Option<&str>) -> String {
    match subject {
        Some(s) if !s.trim().is_empty() => s.trim().to_string(),
        _ => "(no subject)".to_string(),
    }
}

/// Derives excerpt per MIME-AND-TEXT.md §12.3:
/// First 200 characters of the first non-empty paragraph of extracted body.
pub fn derive_excerpt(body: &str, max_len: usize) -> Option<String> {
    if body.trim().is_empty() {
        return None;
    }

    for paragraph in body.split("\n\n") {
        let trimmed = paragraph.trim();
        if !trimmed.is_empty() {
            // Collapse whitespace within paragraph
            let mut out = String::with_capacity(trimmed.len());
            let mut in_space = false;
            for c in trimmed.chars() {
                if c.is_whitespace() {
                    if !in_space {
                        out.push(' ');
                        in_space = true;
                    }
                } else {
                    out.push(c);
                    in_space = false;
                }
            }
            let collapsed = out.trim();
            if collapsed.len() <= max_len {
                return Some(collapsed.to_string());
            } else {
                // Truncate at character boundary up to max_len
                let mut end = max_len;
                while !collapsed.is_char_boundary(end) && end > 0 {
                    end -= 1;
                }
                return Some(collapsed[..end].trim_end().to_string());
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_plain_message() {
        let raw = b"From: Alice <alice@example.com>\r\nSubject: Hi\r\nContent-Type: text/plain\r\n\r\nHello World\r\n";
        let ext = extract_message(raw, &Limits::default()).unwrap();
        assert_eq!(ext.title, "Hi");
        assert_eq!(ext.subject_norm, "hi");
        assert_eq!(ext.author, Some("Alice".to_string()));
        assert_eq!(ext.from_addr, Some("alice@example.com".to_string()));
        assert_eq!(ext.body, "Hello World");
        assert_eq!(ext.body_state, BodyState::Cached);
        assert!(!ext.has_attachments);
        assert_eq!(ext.excerpt, Some("Hello World".to_string()));
    }

    #[test]
    fn test_multipart_alternative_prefers_plain() {
        let raw = b"From: a@b.com\r\nSubject: Alt\r\nContent-Type: multipart/alternative; boundary=\"B\"\r\n\r\n--B\r\nContent-Type: text/plain\r\n\r\nPlain text body\r\n--B\r\nContent-Type: text/html\r\n\r\n<p>HTML body</p>\r\n--B--\r\n";
        let ext = extract_message(raw, &Limits::default()).unwrap();
        assert_eq!(ext.body, "Plain text body");
        assert!(!ext.has_attachments);
    }

    #[test]
    fn test_multipart_alternative_falls_back_to_html_if_plain_empty() {
        let raw = b"From: a@b.com\r\nSubject: Alt\r\nContent-Type: multipart/alternative; boundary=\"B\"\r\n\r\n--B\r\nContent-Type: text/plain\r\n\r\n   \r\n--B\r\nContent-Type: text/html\r\n\r\n<p>Real content</p>\r\n--B--\r\n";
        let ext = extract_message(raw, &Limits::default()).unwrap();
        assert_eq!(ext.body, "Real content");
    }

    #[test]
    fn test_attachments_section_ids_and_magic_pdf() {
        let raw = b"From: a@b.com\r\nSubject: Files\r\nContent-Type: multipart/mixed; boundary=\"MIX\"\r\n\r\n--MIX\r\nContent-Type: text/plain\r\n\r\nMain body\r\n--MIX\r\nContent-Type: application/pdf; name=\"doc.pdf\"\r\nContent-Disposition: attachment; filename=\"doc.pdf\"\r\n\r\n%PDF-1.4 Fake PDF content\r\n--MIX\r\nContent-Type: application/pdf; name=\"not_pdf.bin\"\r\nContent-Disposition: attachment\r\n\r\nNot actually a pdf\r\n--MIX--\r\n";
        let ext = extract_message(raw, &Limits::default()).unwrap();
        assert_eq!(ext.body, "Main body");
        assert!(ext.has_attachments);
        assert_eq!(ext.attachments.len(), 2);

        // Section numbers RFC 3501 §6.4.5
        assert_eq!(ext.attachments[0].section_id, "2");
        assert_eq!(ext.attachments[0].filename, Some("doc.pdf".to_string()));
        assert!(ext.attachments[0].is_pdf); // Magic %PDF- present

        assert_eq!(ext.attachments[1].section_id, "3");
        assert!(!ext.attachments[1].is_pdf); // Magic %PDF- missing despite application/pdf declaration!
    }

    #[test]
    fn test_encrypted_message_placeholder() {
        let raw = b"From: a@b.com\r\nSubject: Secret\r\nContent-Type: multipart/encrypted; boundary=\"ENC\"\r\n\r\n--ENC\r\nContent-Type: application/pgp-encrypted\r\n\r\nVersion: 1\r\n--ENC--\r\n";
        let ext = extract_message(raw, &Limits::default()).unwrap();
        assert_eq!(ext.body, "[encrypted message]");
        assert_eq!(ext.body_state, BodyState::Cached);
    }
}
