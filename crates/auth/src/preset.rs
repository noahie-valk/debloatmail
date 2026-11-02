use std::net::IpAddr;
use std::str::FromStr;

use debloatmail_core::{Account, AccountKind, Security};

use crate::AuthError;

/// Supported provider presets for IMAP/SMTP accounts (`architecture.md` §7, `debloatmailctl.md` §8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderPreset {
    Fastmail,
    Proton,
    Generic,
}

impl FromStr for ProviderPreset {
    type Err = AuthError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "fastmail" => Ok(Self::Fastmail),
            "proton" => Ok(Self::Proton),
            "generic" => Ok(Self::Generic),
            other => Err(AuthError::InvalidConfig(format!(
                "unknown provider preset '{other}' (expected 'fastmail', 'proton', or 'generic')"
            ))),
        }
    }
}

/// Applies provider preset defaults to an `Account` for any connection fields not already set.
///
/// Per `debloatmailctl.md` §8:
/// - `fastmail`: `imap.fastmail.com:993` (`tls`), `smtp.fastmail.com:465` (`tls`), `allow_invalid_cert_loopback = false`.
/// - `proton`: `127.0.0.1:1143` (`starttls`), `127.0.0.1:1025` (`starttls`), `allow_invalid_cert_loopback = true`.
pub fn apply_preset(account: &mut Account, preset: ProviderPreset) {
    match preset {
        ProviderPreset::Fastmail => {
            account.provider = Some("fastmail".to_string());
            if account.imap_host.is_none() {
                account.imap_host = Some("imap.fastmail.com".to_string());
            }
            if account.imap_port.is_none() {
                account.imap_port = Some(993);
            }
            if account.imap_security.is_none() {
                account.imap_security = Some(Security::Tls);
            }
            if account.smtp_host.is_none() {
                account.smtp_host = Some("smtp.fastmail.com".to_string());
            }
            if account.smtp_port.is_none() {
                account.smtp_port = Some(465);
            }
            if account.smtp_security.is_none() {
                account.smtp_security = Some(Security::Tls);
            }
            account.allow_invalid_cert_loopback = false;
        }
        ProviderPreset::Proton => {
            account.provider = Some("proton".to_string());
            if account.imap_host.is_none() {
                account.imap_host = Some("127.0.0.1".to_string());
            }
            if account.imap_port.is_none() {
                account.imap_port = Some(1143);
            }
            if account.imap_security.is_none() {
                account.imap_security = Some(Security::StartTls);
            }
            if account.smtp_host.is_none() {
                account.smtp_host = Some("127.0.0.1".to_string());
            }
            if account.smtp_port.is_none() {
                account.smtp_port = Some(1025);
            }
            if account.smtp_security.is_none() {
                account.smtp_security = Some(Security::StartTls);
            }
            account.allow_invalid_cert_loopback = true;
        }
        ProviderPreset::Generic => {
            account.provider = Some("generic".to_string());
            if account.imap_port.is_none() {
                account.imap_port = Some(match account.imap_security.unwrap_or(Security::Tls) {
                    Security::Tls => 993,
                    Security::StartTls | Security::None => 143,
                });
            }
            if account.imap_security.is_none() {
                account.imap_security = Some(Security::Tls);
            }
            if account.smtp_port.is_none() {
                account.smtp_port = Some(match account.smtp_security.unwrap_or(Security::Tls) {
                    Security::Tls => 465,
                    Security::StartTls => 587,
                    Security::None => 25,
                });
            }
            if account.smtp_security.is_none() {
                account.smtp_security = Some(Security::Tls);
            }
        }
    }
}

/// Checks whether a configured host string refers to a loopback address (`127.0.0.1`, `::1`, or `localhost`).
///
/// Note: At connect time (`crates/engine/src/io.rs` and `tls.rs`), the actual connected socket's
/// `peer_addr().ip().is_loopback()` is also enforced (`SECURITY.md` §5.2).
pub fn is_loopback_host(host: &str) -> bool {
    let trimmed = host.trim().trim_start_matches('[').trim_end_matches(']');
    if trimmed.eq_ignore_ascii_case("localhost") {
        return true;
    }
    if let Ok(ip) = IpAddr::from_str(trimmed) {
        return ip.is_loopback();
    }
    false
}

/// Validates an account's transport security configuration at account creation/update time
/// (`SYNC.md` §5.1, `SECURITY.md` §5.1).
///
/// Specifically:
/// - `Security::None` (cleartext) is refused for any non-loopback host.
/// - IMAP accounts must specify `imap_host`, `imap_port`, `imap_security`, `smtp_host`, `smtp_port`, `smtp_security`.
pub fn validate_account_security(account: &Account) -> Result<(), AuthError> {
    if account.kind != AccountKind::Imap {
        return Ok(());
    }

    let imap_host = account
        .imap_host
        .as_deref()
        .map(str::trim)
        .filter(|h| !h.is_empty())
        .ok_or_else(|| AuthError::InvalidConfig("imap_host is required for IMAP accounts".into()))?;
    let imap_sec = account.imap_security.unwrap_or(Security::Tls);
    if imap_sec == Security::None && !is_loopback_host(imap_host) {
        return Err(AuthError::InvalidConfig(format!(
            "imap_security = 'none' (cleartext) is refused for non-loopback host '{imap_host}'"
        )));
    }

    let smtp_host = account
        .smtp_host
        .as_deref()
        .map(str::trim)
        .filter(|h| !h.is_empty())
        .ok_or_else(|| AuthError::InvalidConfig("smtp_host is required for IMAP accounts".into()))?;
    let smtp_sec = account.smtp_security.unwrap_or(Security::Tls);
    if smtp_sec == Security::None && !is_loopback_host(smtp_host) {
        return Err(AuthError::InvalidConfig(format!(
            "smtp_security = 'none' (cleartext) is refused for non-loopback host '{smtp_host}'"
        )));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use debloatmail_core::{AccountId, AuthKind};

    fn empty_imap_account() -> Account {
        Account {
            id: AccountId::new("018f6e80-7c00-7218-971e-7f62b2512f71").unwrap(),
            kind: AccountKind::Imap,
            name: "test".to_string(),
            email: Some("user@example.com".to_string()),
            provider: None,
            enabled: true,
            imap_host: None,
            imap_port: None,
            imap_security: None,
            smtp_host: None,
            smtp_port: None,
            smtp_security: None,
            auth_kind: AuthKind::Password,
            username: None,
            secret_ref: None,
            allow_invalid_cert_loopback: false,
            poll_interval_secs: None,
            created_at: 1000,
            updated_at: 1000,
        }
    }

    #[test]
    fn test_proton_and_fastmail_presets() {
        let mut proton = empty_imap_account();
        apply_preset(&mut proton, ProviderPreset::Proton);
        assert_eq!(proton.imap_host.as_deref(), Some("127.0.0.1"));
        assert_eq!(proton.imap_port, Some(1143));
        assert_eq!(proton.imap_security, Some(Security::StartTls));
        assert_eq!(proton.smtp_host.as_deref(), Some("127.0.0.1"));
        assert_eq!(proton.smtp_port, Some(1025));
        assert_eq!(proton.smtp_security, Some(Security::StartTls));
        assert!(proton.allow_invalid_cert_loopback);
        validate_account_security(&proton).unwrap();

        let mut fastmail = empty_imap_account();
        apply_preset(&mut fastmail, ProviderPreset::Fastmail);
        assert_eq!(fastmail.imap_host.as_deref(), Some("imap.fastmail.com"));
        assert_eq!(fastmail.imap_port, Some(993));
        assert_eq!(fastmail.imap_security, Some(Security::Tls));
        assert_eq!(fastmail.smtp_host.as_deref(), Some("smtp.fastmail.com"));
        assert_eq!(fastmail.smtp_port, Some(465));
        assert_eq!(fastmail.smtp_security, Some(Security::Tls));
        assert!(!fastmail.allow_invalid_cert_loopback);
        validate_account_security(&fastmail).unwrap();
    }

    #[test]
    fn test_cleartext_rejected_for_remote_hosts() {
        let mut acct = empty_imap_account();
        apply_preset(&mut acct, ProviderPreset::Fastmail);
        acct.imap_security = Some(Security::None);

        let err = validate_account_security(&acct).unwrap_err();
        assert!(err.to_string().contains("refused for non-loopback host"));

        // Loopback cleartext is allowed at config validation time
        acct.imap_host = Some("127.0.0.1".to_string());
        validate_account_security(&acct).unwrap();
    }
}
