// tests for parse module using RSS fixture
use debloatmail_feeds::parse::{parse_feed, ParsedEntry, ParseError};

#[test]
fn test_parse_rss_sample() {
    let data = include_bytes!("../tests/fixtures/rss_sample.xml");
    let entries = parse_feed(data).expect("parse should succeed");
    assert_eq!(entries.len(), 2);
    let first = &entries[0];
    assert_eq!(first.title.as_deref(), Some("First Item"));
    assert_eq!(first.guid.as_deref(), Some("item-1"));
    assert_eq!(first.link.as_deref(), Some("https://example.com/first"));
    assert!(first.published_at.is_some());
    let second = &entries[1];
    assert_eq!(second.title.as_deref(), Some("Second Item"));
    assert_eq!(second.guid.as_deref(), Some("item-2"));
    assert_eq!(second.link.as_deref(), Some("https://example.com/second"));
}
