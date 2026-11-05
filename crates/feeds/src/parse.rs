use feed_rs::model::Entry;
use feed_rs::parser;
use feed_rs::parser::ParseFeedError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ParseError {
    #[error("Feed parse error: {0}")]
    Feed(#[from] ParseFeedError),
    #[error("Missing required fields")]
    Missing,
}

/// Feed-level metadata and entries parsed from an RSS, Atom, or JSON Feed document (`SYNC.md` §7.1–7.4).
#[derive(Debug, Clone)]
pub struct ParsedFeed {
    pub title: Option<String>,
    pub site_url: Option<String>,
    pub entries: Vec<ParsedEntry>,
}

/// Simplified representation of a feed entry for upserting.
#[derive(Debug, Clone)]
pub struct ParsedEntry {
    pub guid: Option<String>,
    pub link: Option<String>,
    pub title: Option<String>,
    pub author: Option<String>,
    pub published_at: Option<i64>,
    pub updated_at: Option<i64>,
    pub content: Option<String>,
    pub summary: Option<String>,
    pub body: String,
}

/// Parse raw feed bytes into a [`ParsedFeed`] containing feed-level `title`, `site_url`, and `entries`.
pub fn parse_feed_document(bytes: &[u8]) -> Result<ParsedFeed, ParseError> {
    let parser = parser::Builder::new().sanitize_content(true).build();
    let feed = parser.parse(bytes)?;

    let title = feed
        .title
        .map(|t| t.content.trim().to_string())
        .filter(|s| !s.is_empty());

    let site_url = feed
        .links
        .iter()
        .find(|l| {
            l.rel
                .as_deref()
                .is_none_or(|rel| rel.eq_ignore_ascii_case("alternate"))
        })
        .or_else(|| feed.links.first())
        .map(|l| l.href.trim().to_string())
        .filter(|s| !s.is_empty());

    let entries = feed.entries.into_iter().map(convert_entry).collect();

    Ok(ParsedFeed {
        title,
        site_url,
        entries,
    })
}

/// Parse raw feed bytes into a vector of `ParsedEntry`.
pub fn parse_feed(bytes: &[u8]) -> Result<Vec<ParsedEntry>, ParseError> {
    Ok(parse_feed_document(bytes)?.entries)
}

fn convert_entry(entry: Entry) -> ParsedEntry {
    let link = entry
        .links
        .first()
        .map(|l| l.href.trim().to_string())
        .filter(|s| !s.is_empty());
    let guid = {
        let trimmed = entry.id.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    };
    let author = entry
        .authors
        .first()
        .and_then(|a| a.name.clone())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let published_at = entry.published.map(|dt| dt.timestamp());
    let updated_at = entry.updated.map(|dt| dt.timestamp());
    let content = entry
        .content
        .as_ref()
        .and_then(|c| c.body.clone())
        .filter(|s| !s.trim().is_empty());
    let summary = entry
        .summary
        .map(|t| t.content)
        .filter(|s| !s.trim().is_empty());
    let body = content
        .clone()
        .or_else(|| summary.clone())
        .unwrap_or_default();

    ParsedEntry {
        guid,
        link,
        title: entry
            .title
            .map(|t| t.content.trim().to_string())
            .filter(|s| !s.is_empty()),
        author,
        published_at,
        updated_at,
        content,
        summary,
        body,
    }
}
