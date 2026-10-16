use html2text::render::{TaggedLine, TaggedLineElement, TextDecorator};

#[derive(Clone, Debug, Default)]
pub struct MailDecorator {
    current_link: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub enum MailAnnotation {
    #[default]
    Normal,
    Link(String),
    Preformat,
}

impl TextDecorator for MailDecorator {
    type Annotation = MailAnnotation;

    fn decorate_link_start(&mut self, url: &str) -> (String, Self::Annotation) {
        self.current_link = Some(url.to_string());
        ("".to_string(), MailAnnotation::Link(url.to_string()))
    }

    fn decorate_link_end(&mut self) -> String {
        self.current_link = None;
        "".to_string()
    }

    fn decorate_em_start(&self) -> (String, Self::Annotation) {
        ("".to_string(), MailAnnotation::Normal)
    }

    fn decorate_em_end(&self) -> String {
        "".to_string()
    }

    fn decorate_strong_start(&self) -> (String, Self::Annotation) {
        ("".to_string(), MailAnnotation::Normal)
    }

    fn decorate_strong_end(&self) -> String {
        "".to_string()
    }

    fn decorate_strikeout_start(&self) -> (String, Self::Annotation) {
        ("".to_string(), MailAnnotation::Normal)
    }

    fn decorate_strikeout_end(&self) -> String {
        "".to_string()
    }

    fn decorate_code_start(&self) -> (String, Self::Annotation) {
        ("".to_string(), MailAnnotation::Normal)
    }

    fn decorate_code_end(&self) -> String {
        "".to_string()
    }

    fn decorate_preformat_first(&self) -> Self::Annotation {
        MailAnnotation::Preformat
    }

    fn decorate_preformat_cont(&self) -> Self::Annotation {
        MailAnnotation::Preformat
    }

    fn decorate_image(&mut self, _src: &str, title: &str) -> (String, Self::Annotation) {
        let trimmed = title.trim();
        let text = if trimmed.is_empty() {
            "[image]".to_string()
        } else {
            format!("[image: {}]", trimmed)
        };
        (text, MailAnnotation::Normal)
    }

    fn header_prefix(&self, _level: usize) -> String {
        // MIME-AND-TEXT.md §10.3: h1–h6: Block, text on its own line, blank lines around. No '#' markers.
        "".to_string()
    }

    fn quote_prefix(&self) -> String {
        // blockquote: Each line prefixed '> ', nested as '> > '
        "> ".to_string()
    }

    fn unordered_item_prefix(&self) -> String {
        // ul: items prefixed '- '
        "- ".to_string()
    }

    fn ordered_item_prefix(&self, i: i64) -> String {
        format!("{}. ", i)
    }

    fn make_subblock_decorator(&self) -> Self {
        self.clone()
    }

    fn decorate_superscript_start(&self) -> (String, Self::Annotation) {
        ("".to_string(), MailAnnotation::Normal)
    }

    fn decorate_superscript_end(&self) -> String {
        "".to_string()
    }

    fn finalise(&mut self, _urls: Vec<String>) -> Vec<TaggedLine<Self::Annotation>> {
        // MIME-AND-TEXT.md §10.4: Inline links, no footnotes.
        Vec::new()
    }
}

/// Converts HTML bytes or string to normalized plain text according to MIME-AND-TEXT.md §10.
pub fn html_to_plaintext(html: &str, base_url: Option<&str>) -> String {
    if html.trim().is_empty() {
        return String::new();
    }

    // Width large enough to disable wrapping per §10.2 and §10.5
    let width = 100_000;
    let conf = html2text::config::with_decorator(MailDecorator::default())
        .empty_img_mode(html2text::config::ImageRenderMode::ShowAlways);

    let lines = match conf.lines_from_read(html.as_bytes(), width) {
        Ok(lines) => lines,
        Err(_) => return fallback_strip_html(html),
    };

    let mut rendered_lines: Vec<(String, bool)> = Vec::with_capacity(lines.len());

    for line in &lines {
        let mut line_buf = String::new();
        let mut is_preformat = false;

        // Group consecutive segments with the same link target
        let mut current_link: Option<(String, String)> = None; // (url, text)

        for el in line.iter() {
            if let TaggedLineElement::Str(ts) = el {
                let is_pre = ts
                    .tag
                    .iter()
                    .any(|a| matches!(a, MailAnnotation::Preformat));
                if is_pre {
                    is_preformat = true;
                }

                let link_target = ts.tag.iter().find_map(|a| match a {
                    MailAnnotation::Link(url) => Some(url.clone()),
                    _ => None,
                });

                if let Some(url) = link_target {
                    if let Some((cur_url, text)) = &mut current_link {
                        if cur_url == &url {
                            text.push_str(&ts.s);
                        } else {
                            let (old_url, old_text) = current_link.take().unwrap();
                            line_buf.push_str(&format_link(&old_text, &old_url, base_url));
                            current_link = Some((url, ts.s.clone()));
                        }
                    } else {
                        current_link = Some((url, ts.s.clone()));
                    }
                } else {
                    if let Some((url, text)) = current_link.take() {
                        line_buf.push_str(&format_link(&text, &url, base_url));
                    }
                    line_buf.push_str(&ts.s);
                }
            }
        }

        if let Some((url, text)) = current_link.take() {
            line_buf.push_str(&format_link(&text, &url, base_url));
        }

        rendered_lines.push((line_buf, is_preformat));
    }

    normalize_rendered_lines(&rendered_lines)
}

/// Formats a link according to MIME-AND-TEXT.md §10.4.
fn format_link(text: &str, href: &str, base_url: Option<&str>) -> String {
    let resolved_url = resolve_url(href, base_url);
    let trimmed_text = text.trim();

    // Check for mailto:
    if let Some(mail_addr) = href.strip_prefix("mailto:") {
        let clean_addr = mail_addr.split('?').next().unwrap_or(mail_addr).trim();
        if trimmed_text.is_empty() || trimmed_text.eq_ignore_ascii_case(clean_addr) {
            return clean_addr.to_string();
        }
        return format!("{} <{}>", trimmed_text, clean_addr);
    }

    if trimmed_text.is_empty() {
        return resolved_url;
    }

    if trimmed_text == href || trimmed_text == resolved_url {
        return resolved_url;
    }

    format!("{} <{}>", trimmed_text, resolved_url)
}

fn resolve_url(url: &str, base_url: Option<&str>) -> String {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    // Absolute URLs or non-HTTP schemes (mailto, tel, urn, cid, etc.)
    if trimmed.starts_with("http://")
        || trimmed.starts_with("https://")
        || trimmed.starts_with("mailto:")
        || trimmed.starts_with("cid:")
        || trimmed.starts_with("data:")
        || trimmed.starts_with("tel:")
        || trimmed.starts_with('#')
    {
        return trimmed.to_string();
    }

    if let Some(base) = base_url {
        let base_clean = base.trim_end_matches('/');
        if trimmed.starts_with('/') {
            // Find host in base URL
            if let Some(pos) = base_clean.find("://") {
                let rest = &base_clean[pos + 3..];
                let host = match rest.find('/') {
                    Some(slash) => &base_clean[..pos + 3 + slash],
                    None => base_clean,
                };
                return format!("{}{}", host, trimmed);
            }
            return format!("{}{}", base_clean, trimmed);
        } else {
            return format!("{}/{}", base_clean, trimmed);
        }
    }

    // Mail: emit relative URL as-is per §10.4
    trimmed.to_string()
}

/// Whitespace normalisation per MIME-AND-TEXT.md §10.5.
pub fn normalize_rendered_lines(lines: &[(String, bool)]) -> String {
    let mut normalized_lines: Vec<String> = Vec::with_capacity(lines.len());

    for (line, is_pre) in lines {
        if *is_pre {
            // Preformatted: protect whitespace runs, strip trailing whitespace
            let trimmed = line.trim_end_matches([' ', '\t', '\r']);
            normalized_lines.push(trimmed.to_string());
        } else {
            // Collapse runs of spaces and tabs within the line to one space
            let mut buf = String::with_capacity(line.len());
            let mut in_space = false;
            for c in line.chars() {
                if c == ' ' || c == '\t' {
                    if !in_space {
                        buf.push(' ');
                        in_space = true;
                    }
                } else {
                    buf.push(c);
                    in_space = false;
                }
            }
            let trimmed = buf.trim_end_matches(' ');
            normalized_lines.push(trimmed.to_string());
        }
    }

    // Collapse multiple blank lines: at most one blank line between paragraphs (max 2 consecutive newlines)
    let mut out = String::new();
    let mut consecutive_blanks = 0;

    for line in normalized_lines {
        if line.is_empty() {
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
            out.push_str(&line);
        }
    }

    // MIME-AND-TEXT.md §10.5 Step 5: Strip leading and trailing blank lines
    out.trim_matches(['\n', '\r']).to_string()
}

/// Fallback HTML tag stripper if html2text encounters an unrecoverable failure.
fn fallback_strip_html(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for c in html.chars() {
        if c == '<' {
            in_tag = true;
        } else if c == '>' {
            in_tag = false;
        } else if !in_tag {
            out.push(c);
        }
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_worked_example_newsletter() {
        let input = r#"<div><p>Hello <b>Alice</b>,</p>
<blockquote><p>Can you review this?</p></blockquote>
<p>See <a href="https://example.com/pr/42">the PR</a>.</p></div>"#;

        let res = html_to_plaintext(input, None);
        let expected =
            "Hello Alice,\n\n> Can you review this?\n\nSee the PR <https://example.com/pr/42>.";
        assert_eq!(res, expected);
    }

    #[test]
    fn test_link_equals_url() {
        let input = r#"<p><a href="https://example.com">https://example.com</a></p>"#;
        let res = html_to_plaintext(input, None);
        assert_eq!(res, "https://example.com");
    }

    #[test]
    fn test_link_mailto() {
        let input = r#"<p>Contact <a href="mailto:bob@example.com">Bob</a></p>"#;
        let res = html_to_plaintext(input, None);
        assert_eq!(res, "Contact Bob <bob@example.com>");

        let input_same = r#"<p><a href="mailto:bob@example.com">bob@example.com</a></p>"#;
        let res_same = html_to_plaintext(input_same, None);
        assert_eq!(res_same, "bob@example.com");
    }

    #[test]
    fn test_headers_have_no_hashes() {
        let input = "<h1>Title One</h1><p>Body</p><h2>Subtitle</h2>";
        let res = html_to_plaintext(input, None);
        assert!(!res.contains('#'));
        assert!(res.contains("Title One"));
        assert!(res.contains("Subtitle"));
    }

    #[test]
    fn test_images_render() {
        let input = r#"<img src="foo.png" alt="Photo of cat"> and <img src="bar.png">"#;
        let res = html_to_plaintext(input, None);
        assert_eq!(res, "[image: Photo of cat] and [image]");
    }

    #[test]
    fn test_relative_url_resolution_with_base() {
        let input = r#"<a href="/articles/1">First article</a>"#;
        let res = html_to_plaintext(input, Some("https://blog.example.com/feed.xml"));
        assert_eq!(res, "First article <https://blog.example.com/articles/1>");
    }

    #[test]
    fn test_relative_url_without_base() {
        let input = r#"<a href="/articles/1">First article</a>"#;
        let res = html_to_plaintext(input, None);
        assert_eq!(res, "First article </articles/1>");
    }

    #[test]
    fn test_pre_preserves_whitespace() {
        let input = "<pre>  let x = 1;\n    let y = 2;</pre>";
        let res = html_to_plaintext(input, None);
        assert!(res.contains("  let x = 1;"));
        assert!(res.contains("    let y = 2;"));
    }

    #[test]
    fn test_collapsing_multiple_blank_lines() {
        let input = "<p>P1</p><br><br><br><br><p>P2</p>";
        let res = html_to_plaintext(input, None);
        assert_eq!(res, "P1\n\nP2");
    }
}
