/// Unwraps RFC 3676 / RFC 2646 `format=flowed` text according to MIME-AND-TEXT.md §9.
///
/// If `is_flowed` is false, returns the input text normalized to LF line endings.
pub fn unwrap_flowed(text: &str, is_flowed: bool, delsp: bool) -> String {
    if !is_flowed {
        return normalize_line_endings(text);
    }

    let raw_lines: Vec<&str> = text
        .split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .collect();
    if raw_lines.is_empty() {
        return String::new();
    }

    struct ParsedLine<'a> {
        quote_depth: usize,
        content: &'a str,
        is_flowed: bool,
    }

    let mut parsed: Vec<ParsedLine> = Vec::with_capacity(raw_lines.len());

    for line in raw_lines {
        // 1. Calculate quote depth
        let mut depth = 0;
        let bytes = line.as_bytes();
        while depth < bytes.len() && bytes[depth] == b'>' {
            depth += 1;
        }

        let mut rem = &line[depth..];

        // An optional space immediately following '>' is the quote separator (part of quoting syntax)
        if depth > 0 && rem.starts_with(' ') {
            rem = &rem[1..];
        }

        // 2. Space-stuffing: if the line content starts with a space, strip it
        if rem.starts_with(' ') {
            rem = &rem[1..];
        }

        // 3. Signature separator: exactly "-- " at quote depth 0
        let is_sig_sep = depth == 0 && rem == "-- ";

        // 4. Flowed test: ends with space and is NOT the signature separator
        let is_flowed = !is_sig_sep && rem.ends_with(' ');

        parsed.push(ParsedLine {
            quote_depth: depth,
            content: rem,
            is_flowed,
        });
    }

    let mut result = String::new();
    let mut i = 0;

    while i < parsed.len() {
        let current_depth = parsed[i].quote_depth;

        // Emit quote prefixes for the start of the output line
        if current_depth > 0 {
            for _ in 0..current_depth {
                result.push('>');
            }
            result.push(' ');
        }

        let mut line_content = String::new();

        loop {
            let p = &parsed[i];

            if p.is_flowed && i + 1 < parsed.len() && parsed[i + 1].quote_depth == current_depth {
                // Flowed line onto the next line
                if delsp {
                    // DelSp=yes: drop the trailing space before joining
                    let trimmed = p.content.strip_suffix(' ').unwrap_or(p.content);
                    line_content.push_str(trimmed);
                } else {
                    // Keep the trailing space
                    line_content.push_str(p.content);
                }
                i += 1;
            } else {
                // Hard break or end of paragraph
                line_content.push_str(p.content);
                break;
            }
        }

        result.push_str(&line_content);
        if i + 1 < parsed.len() {
            result.push('\n');
        }
        i += 1;
    }

    result
}

fn normalize_line_endings(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\r' {
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
            out.push('\n');
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_flowed_basic_example() {
        let input = "This line flows onto the next \none and ends here.\n-- \nNo flowed signature.";
        let res = unwrap_flowed(input, true, false);
        let expected =
            "This line flows onto the next one and ends here.\n-- \nNo flowed signature.";
        assert_eq!(res, expected);
    }

    #[test]
    fn test_flowed_delsp_yes() {
        // DelSp=yes deletes the trailing space
        let input = "Soft break with delsp \nyes.";
        let res = unwrap_flowed(input, true, true);
        assert_eq!(res, "Soft break with delspyes.");
    }

    #[test]
    fn test_space_stuffing() {
        // Line starting with space has space removed
        let input = " Leading space stuffed\nAnother line";
        let res = unwrap_flowed(input, true, false);
        assert_eq!(res, "Leading space stuffed\nAnother line");
    }

    #[test]
    fn test_flowed_quotes() {
        let input = "> Quoted line flows \n> onto next quoted.\nNon-quoted.";
        let res = unwrap_flowed(input, true, false);
        assert_eq!(res, "> Quoted line flows onto next quoted.\nNon-quoted.");
    }

    #[test]
    fn test_flowed_differing_quotes_do_not_flow() {
        let input = "> Quoted line with space \n>> Different quote depth.";
        let res = unwrap_flowed(input, true, false);
        assert_eq!(res, "> Quoted line with space \n>> Different quote depth.");
    }

    #[test]
    fn test_non_flowed_leaves_whitespace_intact() {
        let input = "Line with trailing space \r\nSecond line";
        let res = unwrap_flowed(input, false, false);
        assert_eq!(res, "Line with trailing space \nSecond line");
    }
}
