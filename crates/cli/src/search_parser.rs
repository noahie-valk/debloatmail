//! Gmail-style search query parser conforming to `docs/debloatmailctl.md` §9 and `docs/DATA-MODEL.md` §10.
//!
//! Closed operator set:
//! - `word`, `"phrase"` -> FTS5 over `title`, `author`, `body` (with trailing `*` prefix match for bare words)
//! - `-word`, `NOT word` -> FTS5 NOT exclusion
//! - `from:<addr>` -> `mail_items.from_addr` substring match (mail only)
//! - `to:<addr>` -> `to_addrs` / `cc_addrs` substring match (mail only)
//! - `title:<text>` -> FTS5 `title:"..."`
//! - `author:<text>` -> FTS5 `author:"..."`
//! - `is:unread`, `is:starred` -> item state filters
//! - `is:mail`, `is:feed` -> item kind filters
//! - `has:attachment` -> `has_attachments = 1`
//! - `in:<folder>` -> mail folder by name or role
//! - `account:<name>` -> account scope
//! - `feed:<name>` -> feed scope
//! - `after:<date>`, `before:<date>` -> bounds on `sort_at` / `published_at`
//!
//! Any unknown `operator:` prefix produces exit code 2 (`CliError::Usage`).

use crate::commands::list::parse_date_filter;
use crate::error::CliError;

/// Compiled search expression separating FTS5 match query and SQL predicates.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct CompiledSearch {
    pub fts_query: Option<String>,
    pub sql_predicates: Vec<String>,
}

/// Tokenizes search input respecting double quotes and negative prefixes.
pub fn tokenize_query(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_quotes = false;

    for c in input.chars() {
        match c {
            '"' => {
                in_quotes = !in_quotes;
                current.push('"');
            }
            c if c.is_whitespace() && !in_quotes => {
                if !current.is_empty() {
                    tokens.push(current);
                    current = String::new();
                }
            }
            _ => {
                current.push(c);
            }
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }

    tokens
}

fn sanitize_fts_term(term: &str) -> String {
    term.replace('"', "\"\"")
}

/// Compiles a search query string into FTS5 expression and SQL predicates.
pub fn compile_search_query(input: &str) -> Result<CompiledSearch, CliError> {
    let raw_tokens = tokenize_query(input);
    if raw_tokens.is_empty() {
        return Ok(CompiledSearch::default());
    }

    let mut fts_positives = Vec::new();
    let mut fts_negatives = Vec::new();
    let mut sql_predicates = Vec::new();

    let mut i = 0;
    while i < raw_tokens.len() {
        let token = &raw_tokens[i];

        if token.eq_ignore_ascii_case("NOT") {
            i += 1;
            if i < raw_tokens.len() {
                let next_tok = &raw_tokens[i];
                let clean = next_tok.trim_matches('"');
                let safe = sanitize_fts_term(clean);
                if !safe.is_empty() {
                    fts_negatives.push(format!("\"{safe}\"*"));
                }
            }
            i += 1;
            continue;
        }

        let (is_negated, tok) = if let Some(stripped) = token.strip_prefix('-') {
            (true, stripped)
        } else {
            (false, token.as_str())
        };

        if let Some(colon_idx) = tok.find(':') {
            let op = &tok[..colon_idx].to_ascii_lowercase();
            let val = &tok[colon_idx + 1..];
            let clean_val = val.trim_matches('"');

            match op.as_str() {
                "from" => {
                    let safe = clean_val.replace('\'', "''");
                    if is_negated {
                        sql_predicates.push(format!(
                            "v.id NOT IN (SELECT item_id FROM mail_items WHERE from_addr LIKE '%{safe}%')"
                        ));
                    } else {
                        sql_predicates.push(format!(
                            "v.id IN (SELECT item_id FROM mail_items WHERE from_addr LIKE '%{safe}%')"
                        ));
                    }
                }
                "to" => {
                    let safe = clean_val.replace('\'', "''");
                    if is_negated {
                        sql_predicates.push(format!(
                            "v.id NOT IN (SELECT item_id FROM mail_items WHERE to_addrs LIKE '%{safe}%' OR cc_addrs LIKE '%{safe}%')"
                        ));
                    } else {
                        sql_predicates.push(format!(
                            "v.id IN (SELECT item_id FROM mail_items WHERE to_addrs LIKE '%{safe}%' OR cc_addrs LIKE '%{safe}%')"
                        ));
                    }
                }
                "title" => {
                    let safe = sanitize_fts_term(clean_val);
                    let clause = if clean_val.contains(' ') {
                        format!("title:\"{safe}\"")
                    } else {
                        format!("title:\"{safe}\"*")
                    };
                    if is_negated {
                        fts_negatives.push(clause);
                    } else {
                        fts_positives.push(clause);
                    }
                }
                "author" => {
                    let safe = sanitize_fts_term(clean_val);
                    let clause = if clean_val.contains(' ') {
                        format!("author:\"{safe}\"")
                    } else {
                        format!("author:\"{safe}\"*")
                    };
                    if is_negated {
                        fts_negatives.push(clause);
                    } else {
                        fts_positives.push(clause);
                    }
                }
                "is" => match clean_val.to_ascii_lowercase().as_str() {
                    "unread" => {
                        sql_predicates.push(if is_negated {
                            "v.unread = 0".to_string()
                        } else {
                            "v.unread = 1".to_string()
                        });
                    }
                    "starred" => {
                        sql_predicates.push(if is_negated {
                            "v.starred = 0".to_string()
                        } else {
                            "v.starred = 1".to_string()
                        });
                    }
                    "mail" => {
                        sql_predicates.push(if is_negated {
                            "v.kind != 'mail'".to_string()
                        } else {
                            "v.kind = 'mail'".to_string()
                        });
                    }
                    "feed" => {
                        sql_predicates.push(if is_negated {
                            "v.kind != 'feed'".to_string()
                        } else {
                            "v.kind = 'feed'".to_string()
                        });
                    }
                    unknown => {
                        return Err(CliError::Usage(format!(
                            "unknown search filter 'is:{unknown}'"
                        )));
                    }
                },
                "has" => match clean_val.to_ascii_lowercase().as_str() {
                    "attachment" | "attachments" => {
                        sql_predicates.push(if is_negated {
                            "v.has_attachments = 0".to_string()
                        } else {
                            "v.has_attachments = 1".to_string()
                        });
                    }
                    unknown => {
                        return Err(CliError::Usage(format!(
                            "unknown search filter 'has:{unknown}'"
                        )));
                    }
                },
                "in" => {
                    let safe = clean_val.replace('\'', "''");
                    if is_negated {
                        sql_predicates.push(format!(
                            "v.folder_id NOT IN (SELECT id FROM folders WHERE name = '{safe}' OR role = '{safe}')"
                        ));
                    } else {
                        sql_predicates.push(format!(
                            "v.folder_id IN (SELECT id FROM folders WHERE name = '{safe}' OR role = '{safe}')"
                        ));
                    }
                }
                "account" => {
                    let safe = clean_val.replace('\'', "''");
                    if is_negated {
                        sql_predicates.push(format!(
                            "v.account_id NOT IN (SELECT id FROM accounts WHERE name = '{safe}' OR public_id = '{safe}')"
                        ));
                    } else {
                        sql_predicates.push(format!(
                            "v.account_id IN (SELECT id FROM accounts WHERE name = '{safe}' OR public_id = '{safe}')"
                        ));
                    }
                }
                "feed" => {
                    let safe = clean_val.replace('\'', "''");
                    if is_negated {
                        sql_predicates.push(format!(
                            "v.feed_id NOT IN (SELECT id FROM feeds WHERE title = '{safe}' OR url = '{safe}' OR public_id = '{safe}')"
                        ));
                    } else {
                        sql_predicates.push(format!(
                            "v.feed_id IN (SELECT id FROM feeds WHERE title = '{safe}' OR url = '{safe}' OR public_id = '{safe}')"
                        ));
                    }
                }
                "after" => {
                    let ts = parse_date_filter(clean_val)?;
                    sql_predicates.push(format!("v.sort_at >= {ts}"));
                }
                "before" => {
                    let ts = parse_date_filter(clean_val)?;
                    sql_predicates.push(format!("v.sort_at <= {ts}"));
                }
                unknown => {
                    return Err(CliError::Usage(format!(
                        "unknown search operator '{unknown}:'"
                    )));
                }
            }
        } else {
            // Positional full-text term
            let is_phrase = tok.starts_with('"') && tok.ends_with('"') && tok.len() > 1;
            let clean = tok.trim_matches('"');
            let safe = sanitize_fts_term(clean);
            if !safe.is_empty() {
                let clause = if is_phrase {
                    format!("\"{safe}\"")
                } else {
                    format!("\"{safe}\"*")
                };

                if is_negated {
                    fts_negatives.push(clause);
                } else {
                    fts_positives.push(clause);
                }
            }
        }

        i += 1;
    }

    let fts_query = if !fts_positives.is_empty() || !fts_negatives.is_empty() {
        let mut parts = Vec::new();
        if !fts_positives.is_empty() {
            parts.push(fts_positives.join(" AND "));
        }
        if !fts_negatives.is_empty() {
            let neg_joined = fts_negatives.join(" OR ");
            parts.push(format!("NOT ({neg_joined})"));
        }
        Some(parts.join(" "))
    } else {
        None
    };

    Ok(CompiledSearch {
        fts_query,
        sql_predicates,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_search_query_compilation() {
        let compiled = compile_search_query(
            "borrow checker is:mail is:unread from:alice@example.com title:important -draft",
        )
        .unwrap();

        assert_eq!(
            compiled.fts_query,
            Some("\"borrow\"* AND \"checker\"* AND title:\"important\"* NOT (\"draft\"*)".into())
        );
        assert!(
            compiled
                .sql_predicates
                .contains(&"v.kind = 'mail'".to_string())
        );
        assert!(
            compiled
                .sql_predicates
                .contains(&"v.unread = 1".to_string())
        );
        assert!(
            compiled
                .sql_predicates
                .iter()
                .any(|p| p.contains("from_addr LIKE '%alice@example.com%'"))
        );
    }

    #[test]
    fn test_phrase_query() {
        let compiled = compile_search_query("\"release checklist\" author:\"Alice Chen\"").unwrap();
        assert_eq!(
            compiled.fts_query,
            Some("\"release checklist\" AND author:\"Alice Chen\"".into())
        );
    }

    #[test]
    fn test_unknown_operator_rejection() {
        let err = compile_search_query("searh:foo").unwrap_err();
        assert_eq!(err.exit_code(), 2);
        assert!(err.to_string().contains("unknown search operator 'searh:'"));

        let err2 = compile_search_query("is:invalid").unwrap_err();
        assert_eq!(err2.exit_code(), 2);
        assert!(
            err2.to_string()
                .contains("unknown search filter 'is:invalid'")
        );
    }

    #[test]
    fn test_adversarial_input_sanitization() {
        let compiled = compile_search_query("hello\" OR \"1\"=\"1").unwrap();
        assert!(compiled.fts_query.is_some());
        let fts = compiled.fts_query.unwrap();
        assert!(fts.contains("\"\""));
    }
}
