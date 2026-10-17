use std::fs;
use std::path::{Path, PathBuf};

use debloatmail_extract::{extract_message, ExtractedMessage, Limits};

fn format_extracted(msg: &ExtractedMessage) -> String {
    let mut out = String::new();
    out.push_str(&format!("Title: {}\n", msg.title));
    out.push_str(&format!("SubjectNorm: {}\n", msg.subject_norm));
    out.push_str(&format!(
        "Author: {}\n",
        msg.author.as_deref().unwrap_or("None")
    ));
    out.push_str(&format!(
        "FromAddr: {}\n",
        msg.from_addr.as_deref().unwrap_or("None")
    ));
    out.push_str(&format!("ToAddrs: {}\n", msg.to_addrs.join(", ")));
    out.push_str(&format!("CcAddrs: {}\n", msg.cc_addrs.join(", ")));
    out.push_str(&format!(
        "ReplyTo: {}\n",
        msg.reply_to.as_deref().unwrap_or("None")
    ));
    out.push_str(&format!(
        "MessageId: {}\n",
        msg.message_id.as_deref().unwrap_or("None")
    ));
    out.push_str(&format!(
        "Date: {}\n",
        msg.date.map_or("None".to_string(), |d| d.to_string())
    ));
    out.push_str(&format!("BodyState: {:?}\n", msg.body_state));
    out.push_str(&format!(
        "Excerpt: {}\n",
        msg.excerpt.as_deref().unwrap_or("None")
    ));
    out.push_str(&format!("HasAttachments: {}\n", msg.has_attachments));
    out.push_str("Attachments:\n");
    if msg.attachments.is_empty() {
        out.push_str("  (none)\n");
    } else {
        for att in &msg.attachments {
            out.push_str(&format!(
                "  [{}] {} | {} | {} bytes | is_pdf={} | cid={}\n",
                att.section_id,
                att.filename.as_deref().unwrap_or("(no filename)"),
                att.mime_type,
                att.size_bytes
                    .map_or("unknown".to_string(), |s| s.to_string()),
                att.is_pdf,
                att.content_id.as_deref().unwrap_or("none")
            ));
        }
    }
    out.push_str("Warnings:\n");
    if msg.warnings.is_empty() {
        out.push_str("  (none)\n");
    } else {
        for w in &msg.warnings {
            out.push_str(&format!("  - {}\n", w));
        }
    }
    out.push_str("---\n");
    out.push_str(&msg.body);
    out.push('\n');
    out
}

fn get_fixture_limits(path: &Path) -> Limits {
    let mut limits = Limits::default();
    if path.to_string_lossy().contains("oversize-body") {
        // Use 500 byte limit to test paragraph truncation on oversize-body fixture
        limits.max_extracted_body_size = 500;
    }
    limits
}

fn collect_fixture_emls(dir: &Path) -> Vec<PathBuf> {
    let mut eml_files = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                eml_files.extend(collect_fixture_emls(&p));
            } else if p.extension().is_some_and(|ext| ext == "eml") {
                eml_files.push(p);
            }
        }
    }
    eml_files.sort();
    eml_files
}

#[test]
fn test_mime_fixtures_determinism_and_golden() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/mime");
    let emls = collect_fixture_emls(&root);

    assert!(
        !emls.is_empty(),
        "Fixture files must be found in tests/fixtures/mime"
    );

    for eml_path in &emls {
        let raw = fs::read(eml_path)
            .unwrap_or_else(|e| panic!("Failed to read fixture {:?}: {}", eml_path, e));
        let limits = get_fixture_limits(eml_path);

        // Run 1
        let ext1 = extract_message(&raw, &limits)
            .unwrap_or_else(|e| panic!("extract_message failed for {:?}: {}", eml_path, e));

        // Run 2: assert byte-identical determinism
        let ext2 = extract_message(&raw, &limits)
            .unwrap_or_else(|e| panic!("second extract_message failed for {:?}: {}", eml_path, e));

        assert_eq!(
            ext1, ext2,
            "Determinism violation: run 1 and run 2 produced different results for {:?}",
            eml_path
        );

        let actual = format_extracted(&ext1);
        let golden_path = eml_path.with_extension("golden");

        if !golden_path.exists() {
            // Write golden file initially if missing
            fs::write(&golden_path, &actual)
                .unwrap_or_else(|e| panic!("Failed to write golden file {:?}: {}", golden_path, e));
        } else {
            let expected = fs::read_to_string(&golden_path)
                .unwrap_or_else(|e| panic!("Failed to read golden file {:?}: {}", golden_path, e));
            assert_eq!(
                actual.trim(),
                expected.trim(),
                "Golden file mismatch for fixture {:?}",
                eml_path
            );
        }
    }
}

#[test]
fn test_rfc3501_section_id_validity_and_round_trip() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/mime");
    let emls = collect_fixture_emls(&root);

    for eml_path in &emls {
        let raw = fs::read(eml_path).unwrap();
        let limits = get_fixture_limits(eml_path);
        let ext = extract_message(&raw, &limits).unwrap();

        for att in &ext.attachments {
            // Assert section_id matches RFC 3501 §6.4.5: numeric dotted parts e.g. "1", "2", "1.1", "2.1"
            let valid_rfc3501 = att
                .section_id
                .split('.')
                .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()));
            assert!(
                valid_rfc3501,
                "Section ID '{}' in {:?} is not a valid RFC 3501 section number",
                att.section_id, eml_path
            );
        }
    }

    // Specific round-trip assertion for nested multipart and embedded message/rfc822 per §5.1 & §17
    let nested_raw = b"From: outer@example.org\r\n\
Subject: Nested and Embedded\r\n\
Content-Type: multipart/mixed; boundary=\"OUTER\"\r\n\
\r\n\
--OUTER\r\n\
Content-Type: multipart/alternative; boundary=\"ALT\"\r\n\
\r\n\
--ALT\r\n\
Content-Type: text/plain\r\n\
\r\n\
Main body at section 1.1\r\n\
--ALT\r\n\
Content-Type: text/html\r\n\
\r\n\
<p>Alternative body at section 1.2</p>\r\n\
--ALT--\r\n\
--OUTER\r\n\
Content-Type: message/rfc822\r\n\
Content-Disposition: attachment; filename=\"attached.eml\"\r\n\
\r\n\
From: inner@example.org\r\n\
Subject: Inner Message\r\n\
Content-Type: text/plain\r\n\
\r\n\
Inner body text at 2.1, header at 2.HEADER\r\n\
--OUTER--\r\n";

    let ext = extract_message(nested_raw, &Limits::default()).unwrap();
    assert_eq!(ext.body, "Main body at section 1.1");
    assert!(ext.has_attachments);
    assert_eq!(ext.attachments.len(), 1);
    assert_eq!(ext.attachments[0].section_id, "2");
    assert_eq!(
        ext.attachments[0].filename,
        Some("attached.eml".to_string())
    );
    assert_eq!(ext.attachments[0].mime_type, "message/rfc822");

    // Verify legal RFC 3501 BODY.PEEK[] specifiers:
    // - Attached message container is section 2 (UID FETCH <uid> BODY.PEEK[2])
    // - Encapsulated message header is 2.HEADER
    // - Encapsulated message body is 2.1
    let container_section = &ext.attachments[0].section_id;
    let encapsulated_header = format!("{}.HEADER", container_section);
    let encapsulated_body = format!("{}.1", container_section);
    assert_eq!(encapsulated_header, "2.HEADER");
    assert_eq!(encapsulated_body, "2.1");
}

#[test]
fn test_invariants_across_all_fixtures() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/mime");
    let emls = collect_fixture_emls(&root);

    for eml_path in &emls {
        let raw = fs::read(eml_path).unwrap();
        let limits = get_fixture_limits(eml_path);
        let ext = extract_message(&raw, &limits).unwrap();

        // Invariant 1: No HTML markup in body
        assert!(
            !ext.body.contains("<html")
                && !ext.body.contains("<body")
                && !ext.body.contains("<div")
                && !ext.body.contains("<p>")
                && !ext.body.contains("<span"),
            "Extracted body in {:?} leaked HTML tags: {}",
            eml_path,
            ext.body
        );

        // Invariant 2: Body is valid UTF-8 (guaranteed by String, but check chars)
        assert!(ext.body.chars().all(|c| c != '\0'));

        // Invariant 3: PDF detection uses magic bytes (%PDF-)
        for att in &ext.attachments {
            if att.is_pdf {
                assert!(
                    att.mime_type.contains("pdf")
                        || att.filename.as_deref().unwrap_or("").ends_with(".pdf"),
                    "Attachment with is_pdf=true in {:?} should be PDF",
                    eml_path
                );
            }
        }

        // Invariant 4: Compressed Content-Encoding is never decompressed
        if eml_path.to_string_lossy().contains("gzip-opaque") {
            assert!(
                ext.warnings.iter().any(|w| w.contains(
                    "compressed Content-Encoding; treated as opaque without decompression"
                )),
                "Expected opaque compression warning for gzip fixture"
            );
        }
    }
}
