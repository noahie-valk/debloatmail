//! Clean RFC 5322 plaintext email assembler with MIME attachment support.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use chrono::Utc;
use std::fs;
use std::path::{Path, PathBuf};

use crate::error::CliError;

/// Message structure for building RFC 5322 compliant email payloads.
#[derive(Debug, Clone)]
pub struct Rfc5322Message {
    pub from: String,
    pub to: Vec<String>,
    pub cc: Vec<String>,
    pub bcc: Vec<String>,
    pub subject: String,
    pub in_reply_to: Option<String>,
    pub references: Vec<String>,
    pub body: String,
    pub attachment_paths: Vec<PathBuf>,
}

impl Rfc5322Message {
    /// Generates the standard RFC 5322 formatted message string.
    pub fn build_payload(&self) -> Result<String, CliError> {
        let mut out = String::new();
        let date_str = Utc::now().to_rfc2822();
        let msg_id = format!("<{}@debloatmail>", uuid_short());

        out.push_str(&format!("Date: {date_str}\r\n"));
        out.push_str(&format!("From: {}\r\n", self.from));
        if !self.to.is_empty() {
            out.push_str(&format!("To: {}\r\n", self.to.join(", ")));
        }
        if !self.cc.is_empty() {
            out.push_str(&format!("Cc: {}\r\n", self.cc.join(", ")));
        }
        out.push_str(&format!("Subject: {}\r\n", self.subject));
        out.push_str(&format!("Message-ID: {msg_id}\r\n"));

        if let Some(ref in_reply) = self.in_reply_to {
            out.push_str(&format!("In-Reply-To: {in_reply}\r\n"));
        }
        if !self.references.is_empty() {
            out.push_str(&format!("References: {}\r\n", self.references.join(" ")));
        }

        out.push_str("MIME-Version: 1.0\r\n");

        if self.attachment_paths.is_empty() {
            out.push_str("Content-Type: text/plain; charset=utf-8; format=flowed\r\n");
            out.push_str("Content-Transfer-Encoding: 8bit\r\n");
            out.push_str("\r\n");
            out.push_str(&self.body);
            if !self.body.ends_with('\n') {
                out.push_str("\r\n");
            }
        } else {
            let boundary = format!("_=_boundary_debloatmail_{}_=_", uuid_short());
            out.push_str(&format!(
                "Content-Type: multipart/mixed; boundary=\"{boundary}\"\r\n\r\n"
            ));

            // Plaintext part
            out.push_str(&format!("--{boundary}\r\n"));
            out.push_str("Content-Type: text/plain; charset=utf-8; format=flowed\r\n");
            out.push_str("Content-Transfer-Encoding: 8bit\r\n\r\n");
            out.push_str(&self.body);
            if !self.body.ends_with('\n') {
                out.push_str("\r\n");
            }

            // Attachment parts
            for path in &self.attachment_paths {
                if !path.is_file() {
                    return Err(CliError::Usage(format!(
                        "attachment '{}' is not a regular file",
                        path.display()
                    )));
                }

                let filename = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("attachment.dat");
                let content_type = guess_mime_type(path);
                let bytes = fs::read(path).map_err(|e| {
                    CliError::Usage(format!(
                        "failed to read attachment '{}': {e}",
                        path.display()
                    ))
                })?;

                let b64 = STANDARD.encode(&bytes);

                out.push_str(&format!("\r\n--{boundary}\r\n"));
                out.push_str(&format!(
                    "Content-Type: {content_type}; name=\"{filename}\"\r\n"
                ));
                out.push_str(&format!(
                    "Content-Disposition: attachment; filename=\"{filename}\"\r\n"
                ));
                out.push_str("Content-Transfer-Encoding: base64\r\n\r\n");

                // Wrap base64 at 76 chars
                for chunk in b64.as_bytes().chunks(76) {
                    if let Ok(s) = std::str::from_utf8(chunk) {
                        out.push_str(s);
                        out.push_str("\r\n");
                    }
                }
            }

            out.push_str(&format!("--{boundary}--\r\n"));
        }

        Ok(out)
    }
}

fn uuid_short() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{now:x}")
}

fn guess_mime_type(path: &Path) -> &'static str {
    match path.extension().and_then(|s| s.to_str()).unwrap_or("") {
        "txt" => "text/plain",
        "html" | "htm" => "text/html",
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "json" => "application/json",
        "xml" => "application/xml",
        "zip" => "application/zip",
        "tar" => "application/x-tar",
        "gz" => "application/gzip",
        "eml" => "message/rfc822",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[test]
    fn test_rfc5322_simple_plaintext() {
        let msg = Rfc5322Message {
            from: "alice@example.com".into(),
            to: vec!["bob@example.com".into()],
            cc: vec![],
            bcc: vec![],
            subject: "Hello World".into(),
            in_reply_to: None,
            references: vec![],
            body: "This is a test message.".into(),
            attachment_paths: vec![],
        };

        let payload = msg.build_payload().unwrap();
        assert!(payload.contains("From: alice@example.com\r\n"));
        assert!(payload.contains("To: bob@example.com\r\n"));
        assert!(payload.contains("Subject: Hello World\r\n"));
        assert!(payload.contains("Content-Type: text/plain"));
        assert!(payload.contains("This is a test message."));
    }

    #[test]
    fn test_rfc5322_with_attachment() {
        let mut tmp = NamedTempFile::new().unwrap();
        use std::io::Write;
        writeln!(tmp, "attachment content").unwrap();

        let msg = Rfc5322Message {
            from: "alice@example.com".into(),
            to: vec!["bob@example.com".into()],
            cc: vec![],
            bcc: vec![],
            subject: "With Attachment".into(),
            in_reply_to: None,
            references: vec![],
            body: "Here is an attachment.".into(),
            attachment_paths: vec![tmp.path().to_path_buf()],
        };

        let payload = msg.build_payload().unwrap();
        assert!(
            payload.contains("Content-Type: multipart/mixed; boundary=\"_=_boundary_debloatmail_")
        );
        assert!(payload.contains("Content-Disposition: attachment; filename="));
        assert!(payload.contains("Here is an attachment."));
    }
}
