//! debloatmail-extract: Pure, runtime-free MIME parsing and text extraction pipeline.
//!
//! Conforms to Invariant 4 (architecture.md §2, GEMINI.md, and MIME-AND-TEXT.md):
//! - Pure byte slices in, structures out.
//! - Strictly no sockets, filesystem, async runtime, or database.
//! - Depends on `core` only.

pub mod charset;
pub mod error;
pub mod flowed;
pub mod header;
pub mod html;
pub mod limits;
pub mod structure;

pub use error::ExtractError;
pub use html::html_to_plaintext;
pub use limits::Limits;
pub use structure::{
    derive_excerpt, derive_title, extract_message, ExtractedAttachment, ExtractedMessage,
};

/// The current extractor version, recorded in `item_text.extractor_version` (MIME-AND-TEXT.md §18).
pub const CURRENT_EXTRACTOR_VERSION: u32 = 1;

/// Extracts normalized plain text from a feed article per MIME-AND-TEXT.md §11.
///
/// Priority:
/// 1. `content` if present (HTML converted to text resolving relative links against `site_url`).
/// 2. `summary` if content is absent.
/// 3. Empty string if neither is present.
pub fn extract_feed_text(
    content: Option<&str>,
    summary: Option<&str>,
    site_url: Option<&str>,
) -> String {
    let source = match (content, summary) {
        (Some(c), _) if !c.trim().is_empty() => c,
        (_, Some(s)) if !s.trim().is_empty() => s,
        _ => return String::new(),
    };

    html_to_plaintext(source, site_url)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extractor_version_is_1() {
        assert_eq!(CURRENT_EXTRACTOR_VERSION, 1);
    }

    #[test]
    fn test_feed_text_content_preferred_over_summary() {
        let content = "<p>Full content with <a href=\"/link\">link</a></p>";
        let summary = "<p>Short summary</p>";
        let site_url = Some("https://example.com/site/");

        let res = extract_feed_text(Some(content), Some(summary), site_url);
        assert_eq!(res, "Full content with link <https://example.com/link>");
    }

    #[test]
    fn test_feed_text_summary_fallback() {
        let summary = "<p>Summary only</p>";
        let res = extract_feed_text(None, Some(summary), None);
        assert_eq!(res, "Summary only");
    }

    #[test]
    fn test_feed_text_empty() {
        assert_eq!(extract_feed_text(None, None, None), "");
        assert_eq!(extract_feed_text(Some("  "), Some(""), None), "");
    }
}
