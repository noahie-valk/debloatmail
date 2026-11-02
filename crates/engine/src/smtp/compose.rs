//! RFC 5322 plaintext email assembly and CRLF header-injection guards (`SECURITY.md` §7.2, `SYNC.md` §8.4).

use std::borrow::Cow;

use io_smtp::rfc5321::{SmtpDomain, SmtpEhloDomain, SmtpLocalPart, SmtpMailbox};

use crate::EngineError;

/// Outgoing plaintext email parameters (`SYNC.md` §8.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutgoingEmail {
    pub from: String,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
    pub subject: String,
    pub message_id: String,
    pub in_reply_to: Option<String>,
    pub references: Option<String>,
    pub body: String,
}

/// Rejects any value containing `\r`, `\n`, or `\0` before it can be interpolated into an
/// RFC 5322 header or SMTP envelope address (`SECURITY.md` §7.2).
pub fn validate_no_crlf(field_name: &str, value: &str) -> Result<(), EngineError> {
    if value.bytes().any(|b| matches!(b, b'\r' | b'\n' | b'\0')) {
        return Err(EngineError::InvalidArgument(format!(
            "CRLF or NUL byte rejected in '{field_name}' header/address field"
        )));
    }
    Ok(())
}

/// Extracts and validates an `SmtpMailbox<'static>` (`local@domain`) from an address or
/// display-name + angle-addr string, enforcing [`validate_no_crlf`].
pub fn parse_smtp_mailbox(addr: &str) -> Result<SmtpMailbox<'static>, EngineError> {
    validate_no_crlf("address", addr)?;
    let trimmed = addr.trim();
    let bare = if let (Some(start), Some(end)) = (trimmed.rfind('<'), trimmed.rfind('>')) {
        if start < end {
            trimmed[start + 1..end].trim()
        } else {
            trimmed
        }
    } else {
        trimmed
    };

    let (local, domain) = bare.rsplit_once('@').ok_or_else(|| {
        EngineError::InvalidArgument(format!("invalid email address '{addr}': missing '@'"))
    })?;

    if local.is_empty() || domain.is_empty() || domain.chars().any(char::is_whitespace) {
        return Err(EngineError::InvalidArgument(format!(
            "invalid email address '{addr}'"
        )));
    }

    Ok(SmtpMailbox {
        local_part: SmtpLocalPart(Cow::Owned(local.to_string())),
        domain: SmtpEhloDomain::SmtpDomain(SmtpDomain(Cow::Owned(domain.to_string()))),
    })
}

/// Builds an RFC 5322 plaintext UTF-8 message from [`OutgoingEmail`].
///
/// - Enforces [`validate_no_crlf`] on every header and recipient field (`SECURITY.md` §7.2).
/// - Never includes `Bcc` in the serialized RFC 5322 header block (`Bcc` recipients only appear
///   in the SMTP `RCPT TO` forward paths).
/// - Normalizes body line endings to `\r\n`.
pub fn build_rfc5322_message(email: &OutgoingEmail) -> Result<Vec<u8>, EngineError> {
    validate_no_crlf("From", &email.from)?;
    let _ = parse_smtp_mailbox(&email.from)?;

    if email.to.is_empty() && email.cc.is_empty() && email.bcc.is_empty() {
        return Err(EngineError::InvalidArgument(
            "outgoing email must have at least one recipient (To, Cc, or Bcc)".to_string(),
        ));
    }

    for r in &email.to {
        validate_no_crlf("To", r)?;
        let _ = parse_smtp_mailbox(r)?;
    }
    for r in &email.cc {
        validate_no_crlf("Cc", r)?;
        let _ = parse_smtp_mailbox(r)?;
    }
    for r in &email.bcc {
        validate_no_crlf("Bcc", r)?;
        let _ = parse_smtp_mailbox(r)?;
    }

    validate_no_crlf("Subject", &email.subject)?;
    validate_no_crlf("Message-ID", &email.message_id)?;
    if email.message_id.trim().is_empty() {
        return Err(EngineError::InvalidArgument(
            "Message-ID must not be empty".to_string(),
        ));
    }

    if let Some(irt) = &email.in_reply_to {
        validate_no_crlf("In-Reply-To", irt)?;
    }
    if let Some(refs) = &email.references {
        validate_no_crlf("References", refs)?;
    }

    let mut out = String::with_capacity(email.body.len() + 256);
    out.push_str("From: ");
    out.push_str(email.from.trim());
    out.push_str("\r\n");

    if !email.to.is_empty() {
        out.push_str("To: ");
        out.push_str(&email.to.join(", "));
        out.push_str("\r\n");
    }

    if !email.cc.is_empty() {
        out.push_str("Cc: ");
        out.push_str(&email.cc.join(", "));
        out.push_str("\r\n");
    }

    out.push_str("Subject: ");
    out.push_str(&email.subject);
    out.push_str("\r\n");

    let msg_id_trimmed = email.message_id.trim();
    out.push_str("Message-ID: ");
    if msg_id_trimmed.starts_with('<') && msg_id_trimmed.ends_with('>') {
        out.push_str(msg_id_trimmed);
    } else {
        out.push('<');
        out.push_str(msg_id_trimmed);
        out.push('>');
    }
    out.push_str("\r\n");

    if let Some(irt) = &email.in_reply_to {
        let trimmed = irt.trim();
        if !trimmed.is_empty() {
            out.push_str("In-Reply-To: ");
            out.push_str(trimmed);
            out.push_str("\r\n");
        }
    }

    if let Some(refs) = &email.references {
        let trimmed = refs.trim();
        if !trimmed.is_empty() {
            out.push_str("References: ");
            out.push_str(trimmed);
            out.push_str("\r\n");
        }
    }

    out.push_str("MIME-Version: 1.0\r\n");
    out.push_str("Content-Type: text/plain; charset=utf-8\r\n");
    out.push_str("Content-Transfer-Encoding: 8bit\r\n");
    out.push_str("\r\n");

    // Normalize body line endings to CRLF.
    let mut chars = email.body.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\r' {
            if chars.peek() == Some(&'\n') {
                let _ = chars.next();
            }
            out.push_str("\r\n");
        } else if ch == '\n' {
            out.push_str("\r\n");
        } else {
            out.push(ch);
        }
    }

    if !out.ends_with("\r\n") {
        out.push_str("\r\n");
    }

    Ok(out.into_bytes())
}
