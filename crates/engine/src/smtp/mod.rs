//! Sans-I/O SMTP session open pump, `SmtpClientAsync` implementation, and
//! `send_email` state machine with mid-`DATA` `NoVerdict` detection (`SYNC.md` §8.4).

pub mod compose;

use std::borrow::Cow;

use debloatmail_core::Security;
use io_sasl::mechanism::Sasl;
use io_smtp::client::{SmtpClientAsync, SmtpClientError};
use io_smtp::coroutine::{SmtpCoroutine, SmtpCoroutineState, SmtpYield};
use io_smtp::message::{SmtpMessageSend, SmtpMessageSendError, SmtpMessageSendOptions};
use io_smtp::rfc5321::data::SmtpDataError;
use io_smtp::rfc5321::mail::SmtpMailError;
use io_smtp::rfc5321::rcpt::SmtpRcptError;
use io_smtp::rfc5321::{SmtpDomain, SmtpEhloDomain, SmtpForwardPath, SmtpReversePath};
use io_smtp::session::{
    SmtpSessionOpen, SmtpSessionOpenOptions, SmtpSessionOpenYield, SmtpSessionTransport,
};

use crate::EngineError;
use crate::io::{AsyncByteStream, EngineStream, MemoryStream, open_smtp_stream};
use crate::tls::TlsVerifyMode;

pub use compose::{OutgoingEmail, build_rfc5322_message, parse_smtp_mailbox, validate_no_crlf};

/// Outcome of an SMTP submission attempt (`SYNC.md` §8.4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendOutcome {
    /// Server accepted the message body with `250 OK`.
    Sent,
    /// Server returned an explicit 4xx/5xx rejection during `MAIL FROM`, `RCPT TO`, or `DATA`
    /// (counts toward `pending_ops.attempts`).
    Rejected { message: String },
    /// Connection failed or dropped before `DATA` body transmission began
    /// (does NOT increment `pending_ops.attempts`).
    ConnectionFailed { message: String },
    /// Connection dropped after `DATA` began but before the server's final `250 OK` verdict
    /// (does NOT increment `pending_ops.attempts`; caller must check the IMAP `\Sent` folder
    /// for `Message-ID` before resending to prevent duplicate delivery per `SYNC.md` §8.4).
    NoVerdict { message: String },
}

/// Active SMTP connection owning the byte stream and post-TLS `EHLO` capabilities.
pub struct SmtpConnection<S: AsyncByteStream> {
    pub stream: S,
    pub capabilities: Vec<String>,
    pub verify_mode: TlsVerifyMode,
}

impl SmtpConnection<EngineStream> {
    /// Opens an SMTP session, optionally upgrades via `STARTTLS` (re-issuing `EHLO`),
    /// and authenticates via `SmtpSessionOpen`.
    pub async fn connect(
        host: &str,
        port: u16,
        security: Security,
        allow_invalid_cert_loopback: bool,
        sasl: Option<Sasl>,
    ) -> Result<Self, EngineError> {
        let (transport, starttls) = open_smtp_stream(host, port, security)?;
        let for_cleartext = security == Security::None;

        let domain = SmtpEhloDomain::SmtpDomain(SmtpDomain(Cow::Borrowed("localhost")));
        let opts = SmtpSessionOpenOptions { starttls };

        let mut coroutine = SmtpSessionOpen::new(transport, domain, sasl, opts);
        let mut stream: Option<EngineStream> = None;
        let mut buf = [0u8; 8192];
        let mut arg: Option<Vec<u8>> = None;

        let open_data = loop {
            let state = coroutine.resume(arg.as_deref());
            arg = None;

            match state {
                SmtpCoroutineState::Yielded(yielded) => match yielded {
                    SmtpSessionOpenYield::WantsTcpConnect {
                        host: conn_host,
                        port: conn_port,
                    } => {
                        stream = Some(
                            EngineStream::connect_tcp(&conn_host, conn_port, for_cleartext).await?,
                        );
                    }
                    SmtpSessionOpenYield::WantsTlsConnect {
                        host: conn_host,
                        port: conn_port,
                    } => {
                        stream = Some(
                            EngineStream::connect_tls(
                                &conn_host,
                                conn_port,
                                allow_invalid_cert_loopback,
                                Vec::new(),
                            )
                            .await?,
                        );
                    }
                    SmtpSessionOpenYield::WantsUnixConnect(path) => {
                        return Err(EngineError::InvalidArgument(format!(
                            "unix domain socket SMTP transport is not supported ({path})"
                        )));
                    }
                    SmtpSessionOpenYield::WantsTlsUpgrade => {
                        let existing = stream.take().ok_or_else(|| {
                            EngineError::Tls(
                                "SMTP STARTTLS requested before TCP connection was opened"
                                    .to_string(),
                            )
                        })?;
                        stream = Some(
                            existing
                                .upgrade_tls(host, allow_invalid_cert_loopback, Vec::new())
                                .await?,
                        );
                    }
                    SmtpSessionOpenYield::WantsWrite(bytes) => {
                        let s = stream.as_mut().ok_or_else(|| {
                            EngineError::Io(std::io::Error::new(
                                std::io::ErrorKind::NotConnected,
                                "SMTP coroutine requested write before connecting",
                            ))
                        })?;
                        s.write_all_bytes(&bytes).await?;
                    }
                    SmtpSessionOpenYield::WantsRead => {
                        let s = stream.as_mut().ok_or_else(|| {
                            EngineError::Io(std::io::Error::new(
                                std::io::ErrorKind::NotConnected,
                                "SMTP coroutine requested read before connecting",
                            ))
                        })?;
                        let n = s.read_chunk(&mut buf).await?;
                        arg = Some(buf[..n].to_vec());
                    }
                },
                SmtpCoroutineState::Complete(res) => break res?,
            }
        };

        let active_stream = stream.ok_or_else(|| {
            EngineError::Io(std::io::Error::new(
                std::io::ErrorKind::NotConnected,
                "SMTP session completed without opening a stream",
            ))
        })?;
        let verify_mode = active_stream.verify_mode();
        let capabilities = open_data
            .capabilities
            .into_iter()
            .map(|c| c.into_owned())
            .collect();

        Ok(Self {
            stream: active_stream,
            capabilities,
            verify_mode,
        })
    }
}

impl SmtpConnection<MemoryStream> {
    /// Drives `SmtpSessionOpen` over a deterministic in-memory byte stream for zero-socket unit tests.
    pub async fn connect_memory(
        mut stream: MemoryStream,
        starttls: bool,
        sasl: Option<Sasl>,
    ) -> Result<Self, EngineError> {
        let transport = SmtpSessionTransport::Tcp {
            host: "127.0.0.1".to_string(),
            port: 1025,
        };
        let domain = SmtpEhloDomain::SmtpDomain(SmtpDomain(Cow::Borrowed("localhost")));
        let opts = SmtpSessionOpenOptions { starttls };

        let mut coroutine = SmtpSessionOpen::new(transport, domain, sasl, opts);
        let mut buf = [0u8; 4096];
        let mut arg: Option<Vec<u8>> = None;

        let open_data = loop {
            let state = coroutine.resume(arg.as_deref());
            arg = None;

            match state {
                SmtpCoroutineState::Yielded(yielded) => match yielded {
                    SmtpSessionOpenYield::WantsTcpConnect { .. }
                    | SmtpSessionOpenYield::WantsTlsConnect { .. }
                    | SmtpSessionOpenYield::WantsUnixConnect(_) => {}
                    SmtpSessionOpenYield::WantsTlsUpgrade => {
                        stream.mark_tls_upgraded();
                    }
                    SmtpSessionOpenYield::WantsWrite(bytes) => {
                        stream.write_all_bytes(&bytes).await?;
                    }
                    SmtpSessionOpenYield::WantsRead => {
                        let n = stream.read_chunk(&mut buf).await?;
                        arg = Some(buf[..n].to_vec());
                    }
                },
                SmtpCoroutineState::Complete(res) => break res?,
            }
        };

        let verify_mode = if stream.is_tls_upgraded() {
            TlsVerifyMode::StrictPlatform
        } else {
            TlsVerifyMode::CleartextLoopback
        };
        let capabilities = open_data
            .capabilities
            .into_iter()
            .map(|c| c.into_owned())
            .collect();

        Ok(Self {
            stream,
            capabilities,
            verify_mode,
        })
    }

    /// Creates an authenticated in-memory SMTP connection directly for testing post-auth commands.
    pub fn from_authenticated_memory(stream: MemoryStream) -> Self {
        Self {
            stream,
            capabilities: Vec::new(),
            verify_mode: TlsVerifyMode::StrictPlatform,
        }
    }
}

impl<S: AsyncByteStream> SmtpClientAsync for SmtpConnection<S> {
    async fn run<C, T, E>(&mut self, mut coroutine: C) -> Result<T, SmtpClientError>
    where
        C: SmtpCoroutine<Yield = SmtpYield, Return = Result<T, E>> + Send,
        T: Send,
        E: Send,
        SmtpClientError: From<E>,
    {
        let mut buf = [0u8; 8192];
        let mut arg: Option<Vec<u8>> = None;

        loop {
            let state = coroutine.resume(arg.as_deref());
            arg = None;

            match state {
                SmtpCoroutineState::Yielded(SmtpYield::WantsWrite(bytes)) => {
                    self.stream.write_all_bytes(&bytes).await?;
                }
                SmtpCoroutineState::Yielded(SmtpYield::WantsRead) => {
                    let n = self.stream.read_chunk(&mut buf).await?;
                    arg = Some(buf[..n].to_vec());
                }
                SmtpCoroutineState::Complete(res) => return Ok(res?),
            }
        }
    }
}

/// Submits an [`OutgoingEmail`] over an authenticated [`SmtpConnection`] (`SYNC.md` §8.4).
///
/// Tracks the exact protocol phase (`MAIL FROM` -> `RCPT TO` -> `DATA\r\n` -> body) so that:
/// - Server 4xx/5xx rejections return [`SendOutcome::Rejected`].
/// - Pre-`DATA` transport drops return [`SendOutcome::ConnectionFailed`].
/// - Transport drops after `DATA` starts (before `250 OK`) return [`SendOutcome::NoVerdict`].
pub async fn send_email<S: AsyncByteStream>(
    conn: &mut SmtpConnection<S>,
    email: &OutgoingEmail,
) -> Result<SendOutcome, EngineError> {
    let raw_message = build_rfc5322_message(email)?;
    let sender = parse_smtp_mailbox(&email.from)?;

    let mut forward_paths = Vec::with_capacity(email.to.len() + email.cc.len() + email.bcc.len());
    for addr in email.to.iter().chain(&email.cc).chain(&email.bcc) {
        forward_paths.push(SmtpForwardPath(parse_smtp_mailbox(addr)?));
    }

    let mut coroutine = SmtpMessageSend::new(
        SmtpReversePath::SmtpMailbox(sender),
        forward_paths,
        raw_message,
        SmtpMessageSendOptions {
            keep_bcc: false,
            mail_parameters: Vec::new(),
            rcpt_parameters: Vec::new(),
        },
    );

    let mut buf = [0u8; 8192];
    let mut arg: Option<Vec<u8>> = None;
    let mut data_command_sent = false;
    let mut data_body_sent = false;

    loop {
        let state = coroutine.resume(arg.as_deref());
        arg = None;

        match state {
            SmtpCoroutineState::Yielded(SmtpYield::WantsWrite(bytes)) => {
                if data_command_sent {
                    data_body_sent = true;
                } else if bytes == b"DATA\r\n" {
                    data_command_sent = true;
                }

                if let Err(io_err) = conn.stream.write_all_bytes(&bytes).await {
                    return Ok(if data_body_sent {
                        SendOutcome::NoVerdict {
                            message: io_err.to_string(),
                        }
                    } else {
                        SendOutcome::ConnectionFailed {
                            message: io_err.to_string(),
                        }
                    });
                }
            }
            SmtpCoroutineState::Yielded(SmtpYield::WantsRead) => {
                match conn.stream.read_chunk(&mut buf).await {
                    Ok(n) => {
                        arg = Some(buf[..n].to_vec());
                    }
                    Err(io_err) => {
                        return Ok(if data_body_sent {
                            SendOutcome::NoVerdict {
                                message: io_err.to_string(),
                            }
                        } else {
                            SendOutcome::ConnectionFailed {
                                message: io_err.to_string(),
                            }
                        });
                    }
                }
            }
            SmtpCoroutineState::Complete(Ok(())) => return Ok(SendOutcome::Sent),
            SmtpCoroutineState::Complete(Err(err)) => {
                return Ok(classify_send_error(err, data_body_sent));
            }
        }
    }
}

fn classify_send_error(err: SmtpMessageSendError, data_body_sent: bool) -> SendOutcome {
    match err {
        SmtpMessageSendError::MailFrom(mail_err) => match mail_err {
            SmtpMailError::Rejected { code, message } => SendOutcome::Rejected {
                message: format!("MAIL FROM rejected ({code}): {message}"),
            },
            SmtpMailError::Send(send_err) => SendOutcome::ConnectionFailed {
                message: send_err.to_string(),
            },
        },
        SmtpMessageSendError::RcptTo(rcpt_err) => match rcpt_err {
            SmtpRcptError::Rejected { code, message } => SendOutcome::Rejected {
                message: format!("RCPT TO rejected ({code}): {message}"),
            },
            SmtpRcptError::Send(send_err) => SendOutcome::ConnectionFailed {
                message: send_err.to_string(),
            },
        },
        SmtpMessageSendError::Data(data_err) => match data_err {
            SmtpDataError::CommandRejected { code, message } => SendOutcome::Rejected {
                message: format!("DATA command rejected ({code}): {message}"),
            },
            SmtpDataError::BodyRejected { code, message } => SendOutcome::Rejected {
                message: format!("DATA body rejected ({code}): {message}"),
            },
            SmtpDataError::Send(send_err) => {
                if data_body_sent {
                    SendOutcome::NoVerdict {
                        message: send_err.to_string(),
                    }
                } else {
                    SendOutcome::ConnectionFailed {
                        message: send_err.to_string(),
                    }
                }
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use io_sasl::{mechanism::Sasl, rfc4616::plain::SaslPlainCreds};
    use io_smtp::session::SmtpSessionOpenError;

    use super::*;

    fn sample_email() -> OutgoingEmail {
        OutgoingEmail {
            from: "Alice <alice@example.org>".to_string(),
            to: vec!["bob@example.org".to_string()],
            cc: vec![],
            bcc: vec!["secret@example.org".to_string()],
            subject: "Hello from debloatmail".to_string(),
            message_id: "<msg-01@example.org>".to_string(),
            in_reply_to: None,
            references: None,
            body: "Plaintext body line 1\n.Dot-prefixed line 2".to_string(),
        }
    }

    #[tokio::test]
    async fn test_smtp_starttls_and_post_tls_ehlo_auth() {
        let mem = MemoryStream::new([
            b"220 smtp.example.org ESMTP ready\r\n".to_vec(),
            b"250-smtp.example.org\r\n250 STARTTLS\r\n".to_vec(),
            b"220 2.0.0 Ready to start TLS\r\n".to_vec(),
            b"250-smtp.example.org\r\n250 AUTH PLAIN LOGIN\r\n".to_vec(),
            b"235 2.7.0 Authentication successful\r\n".to_vec(),
        ]);

        let sasl = Sasl::Plain(SaslPlainCreds {
            authzid: None,
            authcid: "alice@example.org".to_string(),
            passwd: "secret".to_string().into(),
        });

        let conn = SmtpConnection::connect_memory(mem, true, Some(sasl))
            .await
            .unwrap();
        assert!(conn.stream.is_tls_upgraded());
        assert!(
            conn.capabilities
                .iter()
                .any(|c| c.contains("AUTH PLAIN LOGIN"))
        );
    }

    #[tokio::test]
    async fn test_smtp_starttls_injection_aborts_upgrade() {
        let mem = MemoryStream::new([
            b"220 smtp.example.org ESMTP ready\r\n".to_vec(),
            b"250-smtp.example.org\r\n250 STARTTLS\r\n".to_vec(),
            b"220 Ready to start TLS\r\n250 Injected reply\r\n".to_vec(),
        ]);

        let res = SmtpConnection::connect_memory(mem, true, None).await;
        assert!(matches!(
            res,
            Err(EngineError::SmtpSession(
                SmtpSessionOpenError::StartTlsInjection
            ))
        ));
    }

    #[tokio::test]
    async fn test_send_email_success_strips_bcc_and_dot_stuffs() {
        let mem = MemoryStream::new([
            b"250 2.1.0 Mail Ok\r\n".to_vec(),
            b"250 2.1.5 Rcpt to bob Ok\r\n".to_vec(),
            b"250 2.1.5 Rcpt to secret Ok\r\n".to_vec(),
            b"354 End data with <CR><LF>.<CR><LF>\r\n".to_vec(),
            b"250 2.0.0 Queued as 12345\r\n".to_vec(),
        ]);

        let mut conn = SmtpConnection::from_authenticated_memory(mem);
        let outcome = send_email(&mut conn, &sample_email()).await.unwrap();
        assert_eq!(outcome, SendOutcome::Sent);

        let written = String::from_utf8(conn.stream.all_written_bytes()).unwrap();
        // Both To and Bcc recipients receive RCPT TO commands:
        assert!(written.contains("RCPT TO:<bob@example.org>"));
        assert!(written.contains("RCPT TO:<secret@example.org>"));
        // Bcc is NEVER present in the message header section:
        assert!(!written.to_ascii_lowercase().contains("\r\nbcc:"));
        // Dot-stuffing applied to `.Dot-prefixed line 2`:
        assert!(written.contains("\r\n..Dot-prefixed line 2\r\n.\r\n"));
    }

    #[tokio::test]
    async fn test_send_email_mid_data_eof_returns_no_verdict() {
        // Server accepts MAIL FROM, RCPT TO, and 354 DATA, then drops the connection (EOF)
        // while or right after the client transmits the DATA body before 250 OK arrives.
        let mem = MemoryStream::new([
            b"250 2.1.0 Mail Ok\r\n".to_vec(),
            b"250 2.1.5 Rcpt to bob Ok\r\n".to_vec(),
            b"250 2.1.5 Rcpt to secret Ok\r\n".to_vec(),
            b"354 End data with <CR><LF>.<CR><LF>\r\n".to_vec(),
            // No 250 OK chunk -> EOF during DATA body verdict wait!
        ]);

        let mut conn = SmtpConnection::from_authenticated_memory(mem);
        let outcome = send_email(&mut conn, &sample_email()).await.unwrap();
        assert!(matches!(outcome, SendOutcome::NoVerdict { .. }));
    }

    #[tokio::test]
    async fn test_send_email_pre_data_eof_returns_connection_failed() {
        // Server drops connection during RCPT TO before DATA starts.
        let mem = MemoryStream::new([b"250 2.1.0 Mail Ok\r\n".to_vec()]);

        let mut conn = SmtpConnection::from_authenticated_memory(mem);
        let outcome = send_email(&mut conn, &sample_email()).await.unwrap();
        assert!(matches!(outcome, SendOutcome::ConnectionFailed { .. }));
    }

    #[tokio::test]
    async fn test_send_email_server_5xx_returns_rejected() {
        let mem = MemoryStream::new([
            b"250 2.1.0 Mail Ok\r\n".to_vec(),
            b"550 5.1.1 <bob@example.org>: Recipient address rejected\r\n".to_vec(),
        ]);

        let mut conn = SmtpConnection::from_authenticated_memory(mem);
        let outcome = send_email(&mut conn, &sample_email()).await.unwrap();
        assert!(matches!(outcome, SendOutcome::Rejected { .. }));
    }

    #[test]
    fn test_crlf_header_injection_rejected() {
        let mut email = sample_email();
        email.subject = "Innocent\r\nBcc: attacker@evil.example".to_string();
        assert!(build_rfc5322_message(&email).is_err());

        let mut email2 = sample_email();
        email2.to = vec!["bob@example.org\r\nRCPT TO:<attacker@evil.example>".to_string()];
        assert!(build_rfc5322_message(&email2).is_err());
    }
}
