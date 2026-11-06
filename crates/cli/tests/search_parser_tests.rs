use debloatmail_cli::search_parser::compile_search_query;

#[test]
fn test_all_closed_operators() {
    let query = "borrow checker from:alice@example.com to:bob@example.com title:release author:alice is:unread is:starred is:mail has:attachment in:INBOX account:fastmail feed:rust after:2026-01-01 before:2026-12-31";
    let compiled = compile_search_query(query).unwrap();

    let fts = compiled.fts_query.expect("should have fts query");
    assert!(fts.contains("\"borrow\"*"));
    assert!(fts.contains("\"checker\"*"));
    assert!(fts.contains("title:\"release\"*"));
    assert!(fts.contains("author:\"alice\"*"));

    assert!(
        compiled
            .sql_predicates
            .iter()
            .any(|p| p.contains("from_addr LIKE '%alice@example.com%'"))
    );
    assert!(
        compiled
            .sql_predicates
            .iter()
            .any(|p| p.contains("to_addrs LIKE '%bob@example.com%'"))
    );
    assert!(
        compiled
            .sql_predicates
            .contains(&"v.unread = 1".to_string())
    );
    assert!(
        compiled
            .sql_predicates
            .contains(&"v.starred = 1".to_string())
    );
    assert!(
        compiled
            .sql_predicates
            .contains(&"v.kind = 'mail'".to_string())
    );
    assert!(
        compiled
            .sql_predicates
            .contains(&"v.has_attachments = 1".to_string())
    );
    assert!(
        compiled
            .sql_predicates
            .iter()
            .any(|p| p.contains("folders WHERE name = 'INBOX'"))
    );
    assert!(
        compiled
            .sql_predicates
            .iter()
            .any(|p| p.contains("accounts WHERE name = 'fastmail'"))
    );
    assert!(
        compiled
            .sql_predicates
            .iter()
            .any(|p| p.contains("feeds WHERE title = 'rust'"))
    );
    assert!(
        compiled
            .sql_predicates
            .iter()
            .any(|p| p.contains("v.sort_at >="))
    );
    assert!(
        compiled
            .sql_predicates
            .iter()
            .any(|p| p.contains("v.sort_at <="))
    );
}

#[test]
fn test_negated_operators() {
    let query = "-from:spam.com -is:starred -is:feed NOT invoice";
    let compiled = compile_search_query(query).unwrap();

    let fts = compiled.fts_query.expect("should have fts query");
    assert!(fts.contains("NOT (\"invoice\"*)"));

    assert!(
        compiled
            .sql_predicates
            .iter()
            .any(|p| p.contains("from_addr LIKE '%spam.com%'") && p.contains("NOT IN"))
    );
    assert!(
        compiled
            .sql_predicates
            .contains(&"v.starred = 0".to_string())
    );
    assert!(
        compiled
            .sql_predicates
            .contains(&"v.kind != 'feed'".to_string())
    );
}

#[test]
fn test_adversarial_inputs_do_not_panic() {
    let inputs = [
        "\"\"\"\"\"\"",
        "''''; DROP TABLE items; --",
        "title:\"unclosed quote",
        "is: from: to: title: author:",
        "NEAR/2 AND OR NOT",
        "- - -",
        ":::",
        "author:\"Alice \\\"The Boss\\\" Chen\"",
    ];

    for input in inputs {
        let res = compile_search_query(input);
        // Should either succeed safely or return exit 2 usage error, never panic
        if let Err(e) = res {
            assert_eq!(e.exit_code(), 2);
        }
    }
}

#[test]
fn test_unknown_operator_exits_2() {
    let err = compile_search_query("tag:important").unwrap_err();
    assert_eq!(err.exit_code(), 2);
    assert_eq!(err.code_str(), "usage");

    let err2 = compile_search_query("kind:mail").unwrap_err();
    assert_eq!(err2.exit_code(), 2);
}
