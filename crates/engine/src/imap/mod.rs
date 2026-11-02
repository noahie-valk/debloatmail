//! Sans-I/O IMAP protocol module (`SYNC.md` §5, §6, §8).

pub mod fetch;
pub mod idle;
pub mod session;

pub use fetch::{
    HEADER_FETCH_WINDOW_SIZE, fetch_attachment_section, fetch_body_peek, fetch_header_window,
    header_fetch_item_names, validate_section_id,
};
pub use idle::{IDLE_MAX_DURATION, run_idle_once};
pub use session::{
    DEFAULT_FRAGMENTIZER_MAX_BYTES, DiscoveredFolder, ImapConnection, NegotiatedCapabilities,
    discover_folders, extract_capabilities, uid_expunge_messages,
};

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;

    use debloatmail_core::{FolderRole, RoleSource};
    use io_imap::client::ImapClientAsync;
    use io_imap::rfc3501::noop::ImapNoop;
    use io_imap::session::ImapSessionOpenError;
    use io_imap::types::core::{Atom, QuotedChar, Vec1};
    use io_imap::types::flag::FlagNameAttribute;
    use io_imap::types::mailbox::Mailbox;
    use io_imap::types::sequence::{SeqOrUid, Sequence, SequenceSet};
    use io_sasl::{Sasl, SaslPlainCreds};

    use super::*;
    use crate::EngineError;
    use crate::io::MemoryStream;

    #[tokio::test]
    async fn test_starttls_upgrade_and_post_tls_capability_refresh() {
        // Pre-TLS greeting does NOT advertise IDLE or UIDPLUS; post-TLS CAPABILITY DOES.
        let mem = MemoryStream::new([
            b"* OK [CAPABILITY IMAP4rev1 STARTTLS] pre-tls ready\r\n".to_vec(),
            b"<TAG> OK Begin TLS negotiation now\r\n".to_vec(),
            b"* CAPABILITY IMAP4rev1 IDLE MOVE UIDPLUS SPECIAL-USE CONDSTORE QRESYNC AUTH=PLAIN SASL-IR\r\n<TAG> OK CAPABILITY completed\r\n".to_vec(),
            b"<TAG> OK [CAPABILITY IMAP4rev1 IDLE MOVE UIDPLUS SPECIAL-USE CONDSTORE QRESYNC] Logged in\r\n".to_vec(),
        ]);

        let sasl = Sasl::Plain(SaslPlainCreds {
            authzid: None,
            authcid: "alice@example.com".to_string(),
            passwd: "app-password".to_string().into(),
        });

        let conn = ImapConnection::connect_memory(mem, true, Some(sasl), false)
            .await
            .unwrap();

        assert!(conn.stream.is_tls_upgraded());
        assert!(conn.capabilities.idle);
        assert!(conn.capabilities.move_ext);
        assert!(conn.capabilities.uidplus);
        assert!(conn.capabilities.special_use);
        assert!(conn.capabilities.condstore);
        assert!(conn.capabilities.qresync);
    }

    #[tokio::test]
    async fn test_starttls_injection_trailing_bytes_aborts_upgrade() {
        // Server (or MITM) appends injected response bytes in the same segment as STARTTLS OK.
        let mem = MemoryStream::new([
            b"* OK [CAPABILITY IMAP4rev1 STARTTLS] ready\r\n".to_vec(),
            b"<TAG> OK Begin TLS\r\n* BYE injected plaintext after starttls\r\n".to_vec(),
        ]);

        let res = ImapConnection::connect_memory(mem, true, None, false).await;
        assert!(matches!(
            res,
            Err(EngineError::ImapSession(
                ImapSessionOpenError::StartTlsInjection
            ))
        ));
    }

    #[tokio::test]
    async fn test_fragmentizer_persists_partial_reads_across_commands() {
        // A single TCP chunk contains the tagged OK of NOOP #1 AND the first half of NOOP #2's
        // untagged + tagged response; the second chunk finishes NOOP #2's line.
        let mem = MemoryStream::new([
            b"<TAG> OK first noop done\r\n* 3 EXI".to_vec(),
            b"STS\r\n<TAG> OK second noop done\r\n".to_vec(),
        ]);

        let mut conn = ImapConnection::from_authenticated_memory(
            mem,
            NegotiatedCapabilities::default(),
        );

        conn.run(ImapNoop::new()).await.unwrap();
        conn.run(ImapNoop::new()).await.unwrap();
    }

    #[test]
    fn test_discover_folders_special_use_fallback_uniqueness_and_gmail_all_mail() {
        let listing = vec![
            (
                Mailbox::Inbox,
                Some(QuotedChar::try_from('/').unwrap()),
                vec![],
            ),
            // Two folders claiming Sent: first via SPECIAL-USE (\Sent), second via name ("Sent Items").
            // SPECIAL-USE wins; second gets role = None to enforce role uniqueness.
            (
                Mailbox::try_from("Sent Items").unwrap(),
                Some(QuotedChar::try_from('/').unwrap()),
                vec![],
            ),
            (
                Mailbox::try_from("Outbox/Sent").unwrap(),
                Some(QuotedChar::try_from('/').unwrap()),
                vec![FlagNameAttribute::from(Atom::try_from("Sent").unwrap())],
            ),
            // Localized fallback name ("Papierkorb" -> Trash with RoleSource::Name)
            (
                Mailbox::try_from("Papierkorb").unwrap(),
                Some(QuotedChar::try_from('/').unwrap()),
                vec![],
            ),
            // \Noselect container folder -> selectable = false, sync_by_default = false
            (
                Mailbox::try_from("[Gmail]").unwrap(),
                Some(QuotedChar::try_from('/').unwrap()),
                vec![FlagNameAttribute::Noselect],
            ),
            // \All folder -> Archive role, but sync_by_default = false (Gmail quirk SYNC.md §5.8)
            (
                Mailbox::try_from("[Gmail]/All Mail").unwrap(),
                Some(QuotedChar::try_from('/').unwrap()),
                vec![FlagNameAttribute::from(Atom::try_from("All").unwrap())],
            ),
        ];

        let discovered = discover_folders(listing);
        assert_eq!(discovered.len(), 6);

        // 0: INBOX
        assert_eq!(discovered[0].name, "INBOX");
        assert_eq!(discovered[0].role, Some(FolderRole::Inbox));
        assert_eq!(discovered[0].role_source, Some(RoleSource::Name));
        assert!(discovered[0].selectable);
        assert!(discovered[0].sync_by_default);

        // 1: "Sent Items" lost the Sent role because "Outbox/Sent" had \Sent SPECIAL-USE
        assert_eq!(discovered[1].name, "Sent Items");
        assert_eq!(discovered[1].role, None);
        assert_eq!(discovered[1].role_source, None);

        // 2: "Outbox/Sent" won Sent via SPECIAL-USE and leaf display name is "Sent"
        assert_eq!(discovered[2].name, "Outbox/Sent");
        assert_eq!(discovered[2].display_name, "Sent");
        assert_eq!(discovered[2].role, Some(FolderRole::Sent));
        assert_eq!(discovered[2].role_source, Some(RoleSource::SpecialUse));

        // 3: "Papierkorb" -> Trash via RoleSource::Name
        assert_eq!(discovered[3].name, "Papierkorb");
        assert_eq!(discovered[3].role, Some(FolderRole::Trash));
        assert_eq!(discovered[3].role_source, Some(RoleSource::Name));

        // 4: "[Gmail]" (\Noselect)
        assert!(!discovered[4].selectable);
        assert!(!discovered[4].sync_by_default);
        assert_eq!(discovered[4].role, None);

        // 5: "[Gmail]/All Mail" (\All) -> Archive role, sync_by_default = false
        assert_eq!(discovered[5].role, Some(FolderRole::Archive));
        assert_eq!(discovered[5].role_source, Some(RoleSource::SpecialUse));
        assert!(discovered[5].selectable);
        assert!(!discovered[5].sync_by_default);
    }

    #[test]
    fn test_validate_section_id() {
        assert!(validate_section_id("1").is_ok());
        assert!(validate_section_id("1.2.3").is_ok());
        assert!(validate_section_id("10.4").is_ok());

        for invalid in ["", "0", "01", "1.0", "1..2", ".1", "1.", "1.a", "1] BODY["] {
            assert!(
                validate_section_id(invalid).is_err(),
                "expected section_id '{invalid}' to be rejected"
            );
        }
    }

    #[tokio::test]
    async fn test_fetch_body_and_attachment_always_use_body_peek() {
        let mem = MemoryStream::new([
            b"* 1 FETCH (UID 42 BODY[] {11}\r\nHello world)\r\n<TAG> OK UID FETCH completed\r\n"
                .to_vec(),
            b"* 1 FETCH (UID 42 BODY[1.2] {4}\r\nDATA)\r\n<TAG> OK UID FETCH completed\r\n"
                .to_vec(),
        ]);

        let mut conn = ImapConnection::from_authenticated_memory(
            mem,
            NegotiatedCapabilities::default(),
        );
        let uid = NonZeroU32::new(42).unwrap();

        let body = fetch_body_peek(&mut conn, uid).await.unwrap();
        assert_eq!(body, b"Hello world");

        let part = fetch_attachment_section(&mut conn, uid, "1.2")
            .await
            .unwrap();
        assert_eq!(part, b"DATA");

        let written = String::from_utf8(conn.stream.all_written_bytes()).unwrap();
        assert!(written.contains("BODY.PEEK[]"));
        assert!(written.contains("BODY.PEEK[1.2]"));
        assert!(!written.contains("BODY[]"));
        assert!(!written.contains("BODY[1.2]"));
    }

    #[tokio::test]
    async fn test_uid_expunge_requires_uidplus_and_never_issues_plain_expunge() {
        let seq = SequenceSet(Vec1::from(Sequence::Single(SeqOrUid::Value(
            NonZeroU32::new(99).unwrap(),
        ))));

        // Without UIDPLUS capability: refused before writing anything to the wire!
        let mut conn_no_uidplus = ImapConnection::from_authenticated_memory(
            MemoryStream::default(),
            NegotiatedCapabilities {
                uidplus: false,
                ..Default::default()
            },
        );
        let err = uid_expunge_messages(&mut conn_no_uidplus, seq.clone())
            .await
            .unwrap_err();
        assert!(matches!(err, EngineError::UnsupportedCapability("UIDPLUS")));
        assert!(conn_no_uidplus.stream.all_written_bytes().is_empty());

        // With UIDPLUS capability: issues `UID EXPUNGE 99`
        let mem = MemoryStream::new([b"* 1 EXPUNGE\r\n<TAG> OK UID EXPUNGE completed\r\n".to_vec()]);
        let mut conn_with_uidplus = ImapConnection::from_authenticated_memory(
            mem,
            NegotiatedCapabilities {
                uidplus: true,
                ..Default::default()
            },
        );
        let expunged = uid_expunge_messages(&mut conn_with_uidplus, seq)
            .await
            .unwrap();
        assert_eq!(expunged.len(), 1);
        let written = String::from_utf8(conn_with_uidplus.stream.all_written_bytes()).unwrap();
        assert!(written.contains("UID EXPUNGE 99"));
    }

    #[tokio::test]
    async fn test_run_idle_once_returns_event_after_clean_done() {
        let mem = MemoryStream::new([
            b"+ idling\r\n".to_vec(),
            b"* 5 EXISTS\r\n".to_vec(),
            b"<TAG> OK IDLE terminated\r\n".to_vec(),
        ]);

        let mut conn = ImapConnection::from_authenticated_memory(
            mem,
            NegotiatedCapabilities {
                idle: true,
                ..Default::default()
            },
        );

        let stop = Arc::new(AtomicBool::new(false));
        let ev = run_idle_once(&mut conn, stop, Some(Duration::from_secs(5)))
            .await
            .unwrap();
        assert!(ev.is_some());
        let written = String::from_utf8(conn.stream.all_written_bytes()).unwrap();
        assert!(written.contains("IDLE\r\n"));
        assert!(written.contains("DONE\r\n"));
    }
}
