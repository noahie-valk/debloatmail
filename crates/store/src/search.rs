use rusqlite::Connection;

use debloatmail_core::{Item, ItemKind};

use crate::headers::row_to_item;
use crate::StoreError;

/// Structured parsed search query separating FTS5 terms and SQL filters.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ParsedSearch {
    /// FTS5 query expression (e.g. `(title:"foo") AND ("bar")`).
    pub fts_query: Option<String>,
    /// Filter on `items.kind`.
    pub kind_filter: Option<ItemKind>,
    /// Filter on `mail_items.from_addr`.
    pub from_filter: Option<String>,
}

/// Parse a user query string into a structured search specification.
///
/// Follows DATA-MODEL.md §10:
/// - Closed operator set: `title:`, `author:`, `from:`, `is:mail`, `is:feed`, and positional terms.
/// - Terms are safely quoted to prevent FTS5 syntax injection.
/// - Unrecognized or malformed queries safely degrade.
pub fn parse_search_query(input: &str) -> ParsedSearch {
    let mut parsed = ParsedSearch::default();
    let mut fts_clauses = Vec::new();

    for token in input.split_whitespace() {
        if token.is_empty() {
            continue;
        }

        if token.eq_ignore_ascii_case("is:mail") {
            parsed.kind_filter = Some(ItemKind::Mail);
        } else if token.eq_ignore_ascii_case("is:feed") {
            parsed.kind_filter = Some(ItemKind::Feed);
        } else if let Some(from_val) = token.strip_prefix("from:") {
            if !from_val.is_empty() {
                parsed.from_filter = Some(from_val.to_string());
            }
        } else if let Some(title_val) = token.strip_prefix("title:") {
            if !title_val.is_empty() {
                let safe = sanitize_fts_term(title_val);
                fts_clauses.push(format!("title:\"{safe}\""));
            }
        } else if let Some(author_val) = token.strip_prefix("author:") {
            if !author_val.is_empty() {
                let safe = sanitize_fts_term(author_val);
                fts_clauses.push(format!("author:\"{safe}\""));
            }
        } else {
            // Positional term: quote safely
            let safe = sanitize_fts_term(token);
            if !safe.is_empty() {
                fts_clauses.push(format!("\"{safe}\""));
            }
        }
    }

    if !fts_clauses.is_empty() {
        parsed.fts_query = Some(fts_clauses.join(" AND "));
    }

    parsed
}

/// Sanitize a term for FTS5 double-quote wrapping by doubling internal quotes.
fn sanitize_fts_term(term: &str) -> String {
    term.replace('"', "\"\"")
}

/// Execute a full-text search over visible items.
pub fn search_items(
    conn: &Connection,
    raw_query: &str,
    limit: u32,
) -> Result<Vec<Item>, StoreError> {
    let parsed = parse_search_query(raw_query);

    // If there is no search criteria at all, return empty
    if parsed.fts_query.is_none() && parsed.kind_filter.is_none() && parsed.from_filter.is_none() {
        return Ok(Vec::new());
    }

    let mut sql = String::from(
        "SELECT v.*, a.public_id AS account_public_id, t.public_id AS thread_public_id
         FROM visible_items v
         JOIN accounts a ON a.id = v.account_id
         LEFT JOIN threads t ON t.id = v.thread_id ",
    );

    if let Some(ref fts) = parsed.fts_query {
        // Safe FTS5 MATCH join with escaped query
        let safe_match = fts.replace('\'', "''");
        sql.push_str(&format!(
            " JOIN item_text_fts ON item_text_fts.rowid = v.id AND item_text_fts MATCH '{safe_match}' "
        ));
    }

    sql.push_str(" WHERE 1=1 ");

    if let Some(kind) = parsed.kind_filter {
        sql.push_str(&format!(" AND v.kind = '{}' ", kind.as_str()));
    }

    if let Some(ref from) = parsed.from_filter {
        let safe_from = from.replace('\'', "''");
        sql.push_str(&format!(
            " AND v.id IN (SELECT item_id FROM mail_items WHERE from_addr LIKE '%{safe_from}%') "
        ));
    }

    sql.push_str(&format!(
        " ORDER BY v.sort_at DESC, v.id DESC LIMIT {limit};"
    ));

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        row_to_item(row).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
    })?;

    let mut items = Vec::new();
    for r in rows {
        items.push(r?);
    }
    Ok(items)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_search_query() {
        let parsed =
            parse_search_query("hello world is:mail from:alice@example.com title:important");
        assert_eq!(
            parsed.fts_query,
            Some("\"hello\" AND \"world\" AND title:\"important\"".to_string())
        );
        assert_eq!(parsed.kind_filter, Some(ItemKind::Mail));
        assert_eq!(parsed.from_filter, Some("alice@example.com".to_string()));
    }

    #[test]
    fn test_adversarial_quotes() {
        let parsed = parse_search_query("hello\" OR \"1\"=\"1");
        assert_eq!(
            parsed.fts_query,
            Some("\"hello\"\"\" AND \"OR\" AND \"\"\"1\"\"=\"\"1\"".to_string())
        );
    }
}
