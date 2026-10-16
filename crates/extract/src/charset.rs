/// Decodes raw bytes to a UTF-8 String according to MIME-AND-TEXT.md §8.
///
/// Precedence:
/// 1. BOM detection (overrides contradicting declaration).
/// 2. If valid UTF-8, accept as UTF-8 (catches mislabelled UTF-8).
/// 3. Declared charset resolved through the `charset` crate (handles UTF-7, JavaMail names, legacy charsets).
/// 4. `chardetng` sniffing on the first ~1 KiB.
/// 5. `windows-1252` fallback with replacement chars (never fails).
pub fn decode_bytes(bytes: &[u8], declared_charset: Option<&str>) -> String {
    if bytes.is_empty() {
        return String::new();
    }

    // 1. BOM detection
    if let Some((bom_encoding, stripped)) = check_bom(bytes) {
        if let Some(cs) = charset::Charset::for_label(bom_encoding.as_bytes()) {
            let (decoded, _, _) = cs.decode(stripped);
            return decoded.into_owned();
        }
        return String::from_utf8_lossy(stripped).into_owned();
    }

    // 2. Check if valid UTF-8 directly.
    if let Ok(s) = std::str::from_utf8(bytes) {
        let is_utf7 = declared_charset
            .map(|c| c.trim().eq_ignore_ascii_case("utf-7"))
            .unwrap_or(false);

        if !is_utf7 {
            let has_non_ascii = bytes.iter().any(|&b| b >= 0x80);
            if has_non_ascii || declared_charset.is_none() || is_ascii_or_utf8(declared_charset) {
                return s.to_string();
            }
        }
    }

    // 3. The declared charset resolved through `charset`
    if let Some(declared) = declared_charset {
        let trimmed = declared.trim().trim_matches('"').trim_matches('\'');
        if !trimmed.is_empty() {
            if let Some(cs) = charset::Charset::for_label(trimmed.as_bytes()) {
                let (decoded, _, _) = cs.decode(bytes);
                return decoded.into_owned();
            }
        }
    }

    // Check UTF-8 validity if not checked above
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_string();
    }

    // 4. `chardetng` on the first ~1 KiB
    let sniff_sample = if bytes.len() > 1024 {
        &bytes[..1024]
    } else {
        bytes
    };

    let mut detector = chardetng::EncodingDetector::new(chardetng::Iso2022JpDetection::Allow);
    detector.feed(sniff_sample, sniff_sample.len() == bytes.len());
    let encoding = detector.guess(None, chardetng::Utf8Detection::Allow);
    let (decoded, _, _) = encoding.decode(bytes);
    if !decoded.contains('\u{FFFD}') || bytes.iter().all(|&b| b < 0x80) {
        return decoded.into_owned();
    }

    // 5. Fallback to windows-1252
    if let Some(cs) = charset::Charset::for_label(b"windows-1252") {
        let (decoded, _, _) = cs.decode(bytes);
        return decoded.into_owned();
    }

    // Last resort lossy UTF-8
    String::from_utf8_lossy(bytes).into_owned()
}

fn check_bom(bytes: &[u8]) -> Option<(&'static str, &[u8])> {
    if bytes.starts_with(b"\xEF\xBB\xBF") {
        Some(("utf-8", &bytes[3..]))
    } else if bytes.starts_with(b"\xFF\xFE") {
        Some(("utf-16le", &bytes[2..]))
    } else if bytes.starts_with(b"\xFE\xFF") {
        Some(("utf-16be", &bytes[2..]))
    } else {
        None
    }
}

fn is_ascii_or_utf8(charset_name: Option<&str>) -> bool {
    match charset_name {
        Some(name) => {
            let n = name.trim().trim_matches('"').trim_matches('\'');
            n.eq_ignore_ascii_case("utf-8")
                || n.eq_ignore_ascii_case("utf8")
                || n.eq_ignore_ascii_case("us-ascii")
                || n.eq_ignore_ascii_case("ascii")
                || n.eq_ignore_ascii_case("iso-8859-1")
        }
        None => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_utf8_plain() {
        let s = "Hello, world! 🚀";
        assert_eq!(decode_bytes(s.as_bytes(), Some("utf-8")), s);
        assert_eq!(decode_bytes(s.as_bytes(), None), s);
    }

    #[test]
    fn test_utf8_bom_overrides_declared() {
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice("Bonjour le monde".as_bytes());
        let res = decode_bytes(&bytes, Some("iso-8859-1"));
        assert_eq!(res, "Bonjour le monde");
    }

    #[test]
    fn test_mislabelled_utf8() {
        let text = "Café résumé";
        let bytes = text.as_bytes();
        let res = decode_bytes(bytes, Some("iso-8859-1"));
        assert_eq!(res, text);
    }

    #[test]
    fn test_windows_1252_decode() {
        let bytes = [0x93, b'H', b'i', 0x94];
        let res = decode_bytes(&bytes, Some("windows-1252"));
        assert_eq!(res, "“Hi”");
    }

    #[test]
    fn test_utf7_decode() {
        let bytes = b"Hello +JgM-!";
        let res = decode_bytes(bytes, Some("utf-7"));
        assert_eq!(res, "Hello ☃!");
    }

    #[test]
    fn test_invalid_bytes_replacement() {
        let bytes = b"Bad \xFF byte";
        let res = decode_bytes(bytes, Some("utf-8"));
        assert!(res.contains("Bad ") && res.contains(" byte"));
    }
}
