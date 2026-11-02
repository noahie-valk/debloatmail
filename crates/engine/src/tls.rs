//! Confined TLS configuration and handshakes for IMAP and SMTP (`SECURITY.md` §5.1–5.3).
//!
//! No other module outside `crates/engine/src/tls.rs` (and `crates/feeds` for HTTP) may configure
//! or negotiate TLS.

use std::net::IpAddr;
use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::{ClientConfig, DigitallySignedStruct, Error as RustlsError, SignatureScheme};
use rustls_pki_types::{CertificateDer, ServerName, UnixTime};
use rustls_platform_verifier::BuilderVerifierExt;
use tokio_rustls::TlsConnector;

use crate::EngineError;

/// Indicates how TLS server certificates were verified on a connection.
///
/// Surfaced by `account check` and `doctor` so the Proton Bridge loopback exception is
/// always reported explicitly and never silently tolerated (`SECURITY.md` §5.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TlsVerifyMode {
    /// Strict platform CA and hostname verification.
    StrictPlatform,
    /// Self-signed / untrusted certificate permitted because `allow_invalid_cert_loopback = true`
    /// AND the connected socket's `peer_addr()` is `127.0.0.1` or `::1`.
    LoopbackExceptionUsed,
    /// No TLS used (permitted only on loopback when explicitly configured).
    CleartextLoopback,
}

/// Custom certificate verifier scoped strictly to loopback connections (`127.0.0.1` / `::1`)
/// for local bridges such as Proton Mail Bridge (`architecture.md` §7, `SECURITY.md` §5.2).
///
/// While it accepts a self-signed certificate chain on loopback, it still cryptographically
/// verifies the TLS 1.2 / TLS 1.3 handshake signature against the certificate's public key
/// using `ring`.
#[derive(Debug)]
struct LoopbackSelfSignedVerifier {
    peer_ip: IpAddr,
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl LoopbackSelfSignedVerifier {
    fn new(peer_ip: IpAddr) -> Result<Self, EngineError> {
        if !peer_ip.is_loopback() {
            return Err(EngineError::LoopbackCertExceptionRefused(peer_ip));
        }
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        Ok(Self { peer_ip, provider })
    }
}

impl ServerCertVerifier for LoopbackSelfSignedVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, RustlsError> {
        // Defense in depth: re-assert loopback IP and non-empty DER certificate.
        if !self.peer_ip.is_loopback() || end_entity.is_empty() {
            return Err(RustlsError::General(
                "loopback certificate verifier invoked on non-loopback or empty cert".to_string(),
            ));
        }
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, RustlsError> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Parses a host string into a `rustls_pki_types::ServerName<'static>`.
pub fn parse_server_name(host: &str) -> Result<ServerName<'static>, EngineError> {
    let trimmed = host.trim().trim_start_matches('[').trim_end_matches(']');
    ServerName::try_from(trimmed)
        .map(|s| s.to_owned())
        .map_err(|e| EngineError::Tls(format!("invalid TLS server name '{host}': {e}")))
}

/// Builds a `TlsConnector` for the given connected socket's `peer_ip`.
///
/// # Security Invariant (`SECURITY.md` §5.2)
/// `allow_invalid_cert_loopback` is checked against the **connected socket's `peer_ip`**,
/// never against the configured hostname string. If `allow_invalid_cert_loopback` is `true`
/// but `peer_ip` is not `127.0.0.1` or `::1`, `EngineError::LoopbackCertExceptionRefused`
/// is returned immediately.
pub fn build_tls_connector(
    peer_ip: IpAddr,
    allow_invalid_cert_loopback: bool,
    alpn_protocols: Vec<Vec<u8>>,
) -> Result<(TlsConnector, TlsVerifyMode), EngineError> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());

    let (mut config, mode) = if allow_invalid_cert_loopback {
        if !peer_ip.is_loopback() {
            return Err(EngineError::LoopbackCertExceptionRefused(peer_ip));
        }
        let verifier = Arc::new(LoopbackSelfSignedVerifier::new(peer_ip)?);
        let cfg = ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|e| EngineError::Tls(e.to_string()))?
            .dangerous()
            .with_custom_certificate_verifier(verifier)
            .with_no_client_auth();
        (cfg, TlsVerifyMode::LoopbackExceptionUsed)
    } else {
        let cfg = ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(|e| EngineError::Tls(e.to_string()))?
            .with_platform_verifier()
            .map_err(|e| EngineError::Tls(e.to_string()))?
            .with_no_client_auth();
        (cfg, TlsVerifyMode::StrictPlatform)
    };

    config.alpn_protocols = alpn_protocols;

    Ok((TlsConnector::from(Arc::new(config)), mode))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    #[test]
    fn test_loopback_cert_exception_enforces_connected_peer_ip() {
        // IPv4 loopback (127.0.0.1) succeeds and reports LoopbackExceptionUsed
        let (_, mode_v4) = build_tls_connector(
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            true,
            vec![b"imap".to_vec()],
        )
        .unwrap();
        assert_eq!(mode_v4, TlsVerifyMode::LoopbackExceptionUsed);

        // IPv6 loopback (::1) succeeds and reports LoopbackExceptionUsed
        let (_, mode_v6) = build_tls_connector(
            IpAddr::V6(Ipv6Addr::LOCALHOST),
            true,
            vec![b"imap".to_vec()],
        )
        .unwrap();
        assert_eq!(mode_v6, TlsVerifyMode::LoopbackExceptionUsed);

        // Non-loopback IP (even private LAN 192.168.1.10 or public IP) with allow_invalid_cert_loopback = true
        // MUST be refused!
        let lan_ip = IpAddr::V4(Ipv4Addr::new(192, 168, 1, 10));
        let Err(err) = build_tls_connector(lan_ip, true, vec![]) else {
            panic!("expected LoopbackCertExceptionRefused error");
        };
        assert!(matches!(
            err,
            EngineError::LoopbackCertExceptionRefused(ip) if ip == lan_ip
        ));

        // Standard strict platform verifier works on any IP
        let (_, strict_mode) = build_tls_connector(lan_ip, false, vec![b"imap".to_vec()]).unwrap();
        assert_eq!(strict_mode, TlsVerifyMode::StrictPlatform);
    }
}
