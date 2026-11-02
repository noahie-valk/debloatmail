//! Confined TCP and TLS stream I/O for IMAP and SMTP (`architecture.md` §5, `SYNC.md` §2).
//!
//! Also provides [`MemoryStream`] so every sans-I/O coroutine state machine in `imap.rs`
//! and `smtp.rs` can be driven against deterministic byte slices with zero network I/O.

use std::collections::VecDeque;
use std::io;
use std::net::IpAddr;

use debloatmail_core::Security;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream;

use crate::EngineError;
use crate::tls::{TlsVerifyMode, build_tls_connector, parse_server_name};

/// Trait abstracting an async byte transport for pumping `io-imap` and `io-smtp` coroutines.
pub trait AsyncByteStream: Send {
    fn read_chunk(
        &mut self,
        buf: &mut [u8],
    ) -> impl std::future::Future<Output = io::Result<usize>> + Send;

    fn write_all_bytes(
        &mut self,
        bytes: &[u8],
    ) -> impl std::future::Future<Output = io::Result<()>> + Send;
}

/// Live network transport wrapping a plain TCP socket (for STARTTLS pre-upgrade or
/// loopback-only cleartext) or a `rustls` TLS stream.
pub enum EngineStream {
    Tcp {
        stream: TcpStream,
        peer_ip: IpAddr,
    },
    Tls {
        stream: Box<TlsStream<TcpStream>>,
        peer_ip: IpAddr,
        verify_mode: TlsVerifyMode,
    },
}

impl EngineStream {
    /// Returns the connected socket's resolved peer IP address.
    pub fn peer_ip(&self) -> IpAddr {
        match self {
            Self::Tcp { peer_ip, .. } | Self::Tls { peer_ip, .. } => *peer_ip,
        }
    }

    /// Returns the active TLS verification mode for this stream.
    pub fn verify_mode(&self) -> TlsVerifyMode {
        match self {
            Self::Tcp { .. } => TlsVerifyMode::CleartextLoopback,
            Self::Tls { verify_mode, .. } => *verify_mode,
        }
    }

    /// Opens a plain TCP connection.
    ///
    /// If `for_cleartext_session` is `true` (`Security::None`), verifies that the connected
    /// socket's `peer_addr().ip()` is loopback (`127.0.0.1` or `::1`), aborting with
    /// [`EngineError::CleartextRefused`] otherwise (`SECURITY.md` §5.1).
    pub async fn connect_tcp(
        host: &str,
        port: u16,
        for_cleartext_session: bool,
    ) -> Result<Self, EngineError> {
        let stream = TcpStream::connect((host, port)).await?;
        let peer_ip = stream.peer_addr()?.ip();
        if for_cleartext_session && !peer_ip.is_loopback() {
            return Err(EngineError::CleartextRefused(peer_ip));
        }
        Ok(Self::Tcp { stream, peer_ip })
    }

    /// Opens an implicit TLS connection (`imaps://` on 993, `smtps://` on 465).
    pub async fn connect_tls(
        host: &str,
        port: u16,
        allow_invalid_cert_loopback: bool,
        alpn: Vec<Vec<u8>>,
    ) -> Result<Self, EngineError> {
        let tcp = TcpStream::connect((host, port)).await?;
        let peer_ip = tcp.peer_addr()?.ip();
        let (connector, verify_mode) =
            build_tls_connector(peer_ip, allow_invalid_cert_loopback, alpn)?;
        let server_name = parse_server_name(host)?;
        let tls = connector
            .connect(server_name, tcp)
            .await
            .map_err(|e| EngineError::Tls(e.to_string()))?;
        Ok(Self::Tls {
            stream: Box::new(tls),
            peer_ip,
            verify_mode,
        })
    }

    /// Upgrades an existing plain TCP connection to TLS after a clean `STARTTLS` exchange.
    pub async fn upgrade_tls(
        self,
        host: &str,
        allow_invalid_cert_loopback: bool,
        alpn: Vec<Vec<u8>>,
    ) -> Result<Self, EngineError> {
        let Self::Tcp { stream, peer_ip } = self else {
            return Err(EngineError::Tls(
                "attempted STARTTLS upgrade on an already-encrypted stream".to_string(),
            ));
        };
        let (connector, verify_mode) =
            build_tls_connector(peer_ip, allow_invalid_cert_loopback, alpn)?;
        let server_name = parse_server_name(host)?;
        let tls = connector
            .connect(server_name, stream)
            .await
            .map_err(|e| EngineError::Tls(e.to_string()))?;
        Ok(Self::Tls {
            stream: Box::new(tls),
            peer_ip,
            verify_mode,
        })
    }
}

impl AsyncByteStream for EngineStream {
    async fn read_chunk(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Tcp { stream, .. } => stream.read(buf).await,
            Self::Tls { stream, .. } => stream.read(buf).await,
        }
    }

    async fn write_all_bytes(&mut self, bytes: &[u8]) -> io::Result<()> {
        match self {
            Self::Tcp { stream, .. } => stream.write_all(bytes).await,
            Self::Tls { stream, .. } => stream.write_all(bytes).await,
        }
    }
}

/// Validates and maps `(host, port, Security)` for opening an IMAP session.
pub fn open_imap_stream(
    host: &str,
    port: u16,
    security: Security,
) -> Result<(io_imap::session::ImapSessionTransport, bool), EngineError> {
    if security == Security::None && !debloatmail_auth::is_loopback_host(host) {
        return Err(EngineError::InvalidArgument(format!(
            "cleartext IMAP (Security::None) is refused for non-loopback host '{host}'"
        )));
    }
    let transport = match security {
        Security::Tls => io_imap::session::ImapSessionTransport::Tls {
            host: host.to_string(),
            port,
        },
        Security::StartTls | Security::None => io_imap::session::ImapSessionTransport::Tcp {
            host: host.to_string(),
            port,
        },
    };
    let starttls = security == Security::StartTls;
    Ok((transport, starttls))
}

/// Validates and maps `(host, port, Security)` for opening an SMTP session.
pub fn open_smtp_stream(
    host: &str,
    port: u16,
    security: Security,
) -> Result<(io_smtp::session::SmtpSessionTransport, bool), EngineError> {
    if security == Security::None && !debloatmail_auth::is_loopback_host(host) {
        return Err(EngineError::InvalidArgument(format!(
            "cleartext SMTP (Security::None) is refused for non-loopback host '{host}'"
        )));
    }
    let transport = match security {
        Security::Tls => io_smtp::session::SmtpSessionTransport::Tls {
            host: host.to_string(),
            port,
        },
        Security::StartTls | Security::None => io_smtp::session::SmtpSessionTransport::Tcp {
            host: host.to_string(),
            port,
        },
    };
    let starttls = security == Security::StartTls;
    Ok((transport, starttls))
}

/// In-memory scripted byte stream for testing sans-I/O IMAP and SMTP coroutines
/// with zero sockets (`SYNC.md` §2, §15).
///
/// Each entry in `read_chunks` is returned by one `read_chunk` call (enabling tests to
/// simulate arbitrary TCP fragmentation / mid-line splits or sudden EOFs).
#[derive(Debug, Default)]
pub struct MemoryStream {
    read_chunks: VecDeque<Vec<u8>>,
    written_chunks: Vec<Vec<u8>>,
    upgraded_to_tls: bool,
}

impl MemoryStream {
    pub fn new<I, B>(chunks: I) -> Self
    where
        I: IntoIterator<Item = B>,
        B: Into<Vec<u8>>,
    {
        Self {
            read_chunks: chunks.into_iter().map(Into::into).collect(),
            written_chunks: Vec::new(),
            upgraded_to_tls: false,
        }
    }

    pub fn push_read_chunk(&mut self, chunk: impl Into<Vec<u8>>) {
        self.read_chunks.push_back(chunk.into());
    }

    pub fn written_chunks(&self) -> &[Vec<u8>] {
        &self.written_chunks
    }

    pub fn all_written_bytes(&self) -> Vec<u8> {
        self.written_chunks
            .iter()
            .flat_map(|c| c.iter().copied())
            .collect()
    }

    pub fn mark_tls_upgraded(&mut self) {
        self.upgraded_to_tls = true;
    }

    pub fn is_tls_upgraded(&self) -> bool {
        self.upgraded_to_tls
    }
}

impl AsyncByteStream for MemoryStream {
    async fn read_chunk(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.read_chunks.pop_front() {
            Some(chunk) => {
                if chunk.is_empty() {
                    return Ok(0);
                }
                let n = chunk.len().min(buf.len());
                buf[..n].copy_from_slice(&chunk[..n]);
                if n < chunk.len() {
                    self.read_chunks.push_front(chunk[n..].to_vec());
                }
                Ok(n)
            }
            None => Ok(0),
        }
    }

    async fn write_all_bytes(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.written_chunks.push(bytes.to_vec());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_open_stream_refuses_non_loopback_cleartext() {
        assert!(open_imap_stream("imap.fastmail.com", 143, Security::None).is_err());
        assert!(open_smtp_stream("smtp.fastmail.com", 25, Security::None).is_err());

        let (_, starttls) = open_imap_stream("127.0.0.1", 1143, Security::StartTls).unwrap();
        assert!(starttls);

        let (_, starttls_tls) = open_imap_stream("imap.fastmail.com", 993, Security::Tls).unwrap();
        assert!(!starttls_tls);
    }

    #[tokio::test]
    async fn test_memory_stream_fragmented_reads() {
        let mut mem = MemoryStream::new([b"hello ".to_vec(), b"world".to_vec()]);
        let mut buf = [0u8; 4];

        let n1 = mem.read_chunk(&mut buf).await.unwrap();
        assert_eq!(&buf[..n1], b"hell");

        let n2 = mem.read_chunk(&mut buf).await.unwrap();
        assert_eq!(&buf[..n2], b"o ");

        let n3 = mem.read_chunk(&mut buf).await.unwrap();
        assert_eq!(&buf[..n3], b"worl");

        let n4 = mem.read_chunk(&mut buf).await.unwrap();
        assert_eq!(&buf[..n4], b"d");

        let n5 = mem.read_chunk(&mut buf).await.unwrap();
        assert_eq!(n5, 0);
    }
}
