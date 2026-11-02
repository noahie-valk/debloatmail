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

/// Simplified representation of a feed entry for upserting.
#[derive(Debug, Clone)]
pub struct ParsedEntry {
    pub guid: Option<String>,
    pub link: Option<String>,
    pub title: Option<String>,
    pub author: Option<String>,
    pub published_at: Option<i64>,
    pub body: String,
}

/// Parse raw feed bytes into a vector of `ParsedEntry`.
pub fn parse_feed(bytes: &[u8]) -> Result<Vec<ParsedEntry>, ParseError> {
    // Use the builder to sanitize content for security.
    let parser = parser::Builder::new().sanitize_content(true).build();
    let feed = parser.parse(bytes)?;
    let mut entries = Vec::new();
    for entry in feed.entries {
        entries.push(convert_entry(entry));
    }
    Ok(entries)
}

fn convert_entry(entry: Entry) -> ParsedEntry {
    // Helper to extract the first link.
    let link = entry.links.first().map(|l| l.href.clone());
    // GUID: prefer id, else link.
    let guid = entry.id.clone();
    // Author name if present.
    let author = entry.authors.first().and_then(|a| a.name.clone());
    // Published time to unix seconds.
    let published_at = entry.published.map(|dt| dt.timestamp());
    // Body: use content if present, otherwise summary.
    let body = entry
        .content
        .as_ref()
        .and_then(|c| c.body.clone())
        .or_else(|| entry.summary.map(|t| t.content))
        .unwrap_or_default();
    ParsedEntry {
        guid: Some(guid),
        link,
        title: entry.title.map(|t| t.content),
        author,
        published_at,
        body,
    }
}
