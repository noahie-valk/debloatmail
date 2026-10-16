use mail_parser::{Address, HeaderValue, Message};

/// Normalizes subject according to MIME-AND-TEXT.md §4.5 for threading.
///
/// Strips common reply/forward prefixes (`Re:`, `Re[2]:`, `Fwd:`, `FW:`, `AW:`, `SV:`, etc.),
/// collapses whitespace, and lowercases.
pub fn normalize_subject(subject: Option<&str>) -> String {
    let s = match subject {
        Some(s) => s.trim(),
        None => return String::new(),
    };

    let mut current = s;
    loop {
        let trimmed = current.trim_start();
        if let Some(rest) = strip_prefix_case_insensitive(trimmed) {
            current = rest;
        } else {
            break;
        }
    }

    // Collapse whitespace runs and lowercase
    let mut out = String::with_capacity(current.len());
    let mut in_whitespace = false;
    for c in current.chars() {
        if c.is_whitespace() {
            if !in_whitespace {
                out.push(' ');
                in_whitespace = true;
            }
        } else {
            for lc in c.to_lowercase() {
                out.push(lc);
            }
            in_whitespace = false;
        }
    }

    out.trim().to_string()
}

fn strip_prefix_case_insensitive(s: &str) -> Option<&str> {
    let prefixes = [
        "re:", "re :", "re-", "fwd:", "fwd :", "fw:", "fw :", "aw:", "sv:", "vs:",
    ];

    let lower = s.to_ascii_lowercase();
    for p in &prefixes {
        if lower.starts_with(p) {
            return Some(&s[p.len()..]);
        }
    }

    // Check for patterns like "re[2]:", "re(2):", "fwd[1]:"
    if lower.starts_with("re[")
        || lower.starts_with("re(")
        || lower.starts_with("fwd[")
        || lower.starts_with("fwd(")
    {
        if let Some(colon) = s.find(':') {
            let prefix_part = &s[..colon];
            if is_bracketed_prefix(prefix_part) {
                return Some(&s[colon + 1..]);
            }
        }
    }

    None
}

fn is_bracketed_prefix(p: &str) -> bool {
    let lower = p.to_ascii_lowercase();
    let digits_start = if lower.starts_with("re[") || lower.starts_with("re(") {
        3
    } else if lower.starts_with("fwd[") || lower.starts_with("fwd(") {
        4
    } else {
        return false;
    };

    if digits_start >= p.len() {
        return false;
    }

    let end_bracket = if p.contains('[') { ']' } else { ')' };
    if let Some(end_pos) = p.find(end_bracket) {
        let num_str = &p[digits_start..end_pos];
        num_str.chars().all(|c| c.is_ascii_digit())
    } else {
        false
    }
}

/// Derives `(author, from_addr)` per MIME-AND-TEXT.md §4.4 and §12.2.
pub fn derive_author(message: &Message<'_>) -> (Option<String>, Option<String>) {
    if let Some(from_header) = message.from() {
        if let Some(first_addr) = extract_first_address(from_header) {
            let addr = first_addr.address.map(|s| s.trim().to_string());
            let name = first_addr.name.map(|s| s.trim().to_string());

            let result = match (name, addr) {
                (Some(name), Some(addr)) if !name.is_empty() => Some((Some(name), Some(addr))),
                (None, Some(addr)) | (Some(_), Some(addr)) => {
                    Some((Some(addr.clone()), Some(addr)))
                }
                (Some(name), None) if !name.is_empty() => Some((Some(name), None)),
                _ => None,
            };
            if let Some(res) = result {
                return res;
            }
        }
    }

    // Fallback if parsing failed but header is present as raw text
    for header in message.headers() {
        if header.name.as_str().eq_ignore_ascii_case("from") {
            let raw = header.value.as_text().map(|s| s.trim().to_string());
            if let Some(r) = raw {
                if !r.is_empty() {
                    return (Some(r), None);
                }
            }
        }
    }

    (None, None)
}

struct SimpleAddr<'a> {
    name: Option<&'a str>,
    address: Option<&'a str>,
}

fn extract_first_address<'a>(addr: &'a Address<'a>) -> Option<SimpleAddr<'a>> {
    match addr {
        Address::List(list) => list.first().map(|a| SimpleAddr {
            name: a.name.as_deref(),
            address: a.address.as_deref(),
        }),
        Address::Group(groups) => {
            for g in groups {
                if let Some(first) = g.addresses.first() {
                    return Some(SimpleAddr {
                        name: first.name.as_deref(),
                        address: first.address.as_deref(),
                    });
                }
            }
            None
        }
    }
}

/// Extracts all email addresses from an `Address` header field.
pub fn extract_all_addresses(addr: Option<&Address<'_>>) -> Vec<String> {
    let mut results = Vec::new();
    let addr = match addr {
        Some(a) => a,
        None => return results,
    };

    match addr {
        Address::List(list) => {
            for a in list {
                if let Some(email) = &a.address {
                    let trimmed = email.trim();
                    if !trimmed.is_empty() {
                        results.push(trimmed.to_string());
                    }
                }
            }
        }
        Address::Group(groups) => {
            for g in groups {
                for a in &g.addresses {
                    if let Some(email) = &a.address {
                        let trimmed = email.trim();
                        if !trimmed.is_empty() {
                            results.push(trimmed.to_string());
                        }
                    }
                }
            }
        }
    }

    results
}

/// Cleans Message-IDs by stripping angle brackets.
pub fn clean_msg_id(id: &str) -> String {
    id.trim().trim_matches(['<', '>']).trim().to_string()
}

/// Extracts list of cleaned Message-IDs from header values.
pub fn extract_message_ids(val: &HeaderValue<'_>) -> Vec<String> {
    let mut ids = Vec::new();
    match val {
        HeaderValue::Text(s) => {
            for part in s.split_whitespace() {
                let cleaned = clean_msg_id(part);
                if !cleaned.is_empty() {
                    ids.push(cleaned);
                }
            }
        }
        HeaderValue::TextList(list) => {
            for s in list {
                for part in s.split_whitespace() {
                    let cleaned = clean_msg_id(part);
                    if !cleaned.is_empty() {
                        ids.push(cleaned);
                    }
                }
            }
        }
        _ => {}
    }
    ids
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_subject() {
        assert_eq!(normalize_subject(Some("Re: Hello World")), "hello world");
        assert_eq!(
            normalize_subject(Some("Fwd: Re: [Project] Update")),
            "[project] update"
        );
        assert_eq!(
            normalize_subject(Some("Re[2]: Multiple replies")),
            "multiple replies"
        );
        assert_eq!(normalize_subject(Some("AW: German reply")), "german reply");
        assert_eq!(
            normalize_subject(Some("SV: Swedish reply")),
            "swedish reply"
        );
        assert_eq!(normalize_subject(Some("   ")), "");
        assert_eq!(normalize_subject(None), "");
    }

    #[test]
    fn test_clean_msg_id() {
        assert_eq!(clean_msg_id("<abc@example.com>"), "abc@example.com");
        assert_eq!(clean_msg_id("  xyz@example.com  "), "xyz@example.com");
    }
}
