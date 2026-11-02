use debloatmail_core::{Account, AccountKind, AuthKind};
use io_sasl::{
    login::SaslLoginCreds,
    mechanism::{Sasl, SaslMechanism},
    rfc4616::plain::SaslPlainCreds,
    rfc5801::SaslGs2ChannelBinding,
    rfc5802::SaslScramCreds,
};
use secrecy::{ExposeSecret, SecretString};

use crate::AuthError;

/// Password authentication mechanism preference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PasswordAuthPreference {
    /// Standard SASL PLAIN (RFC 4616).
    Plain,
    /// Legacy SASL LOGIN / IMAP LOGIN.
    Login,
    /// Salted challenge-response SCRAM-SHA-256 (RFC 7677) with caller-supplied nonce bytes.
    ScramSha256 { nonce: Vec<u8> },
}

/// Selects the strongest mutually supported password mechanism from the server's
/// advertised SASL mechanisms (`SCRAM-SHA-256` -> `PLAIN` -> `LOGIN`).
///
/// If the server advertises no explicit `AUTH=...` capability (e.g. basic IMAP `LOGIN`),
/// falls back to `PLAIN` (or `LOGIN` if `LOGINDISABLED` is not present and only `LOGIN` is used).
pub fn select_password_mechanism(
    advertised: &[SaslMechanism],
    scram_nonce: Option<Vec<u8>>,
) -> PasswordAuthPreference {
    if advertised.contains(&SaslMechanism::ScramSha256)
        && let Some(nonce) = scram_nonce
        && !nonce.is_empty()
    {
        return PasswordAuthPreference::ScramSha256 { nonce };
    }
    if advertised.contains(&SaslMechanism::Plain) {
        return PasswordAuthPreference::Plain;
    }
    if advertised.contains(&SaslMechanism::Login) {
        return PasswordAuthPreference::Login;
    }
    PasswordAuthPreference::Plain
}

/// Builds a `Sasl` credential payload for `io-imap` or `io-smtp` session opening.
///
/// Per `SECURITY.md` §9.4, secrets are passed via `SecretString` and wrapped into
/// `io_sasl`'s `SecretString` without logging or exposing plaintext in `Debug`.
pub fn build_sasl_credentials(
    account: &Account,
    secret: &SecretString,
    preference: PasswordAuthPreference,
) -> Result<Sasl, AuthError> {
    if account.kind != AccountKind::Imap {
        return Err(AuthError::InvalidConfig(
            "cannot build SASL credentials for a feed account".to_string(),
        ));
    }

    match account.auth_kind {
        AuthKind::OAuth2 => Err(AuthError::OAuthDeferred),
        AuthKind::Password => {
            let username = account
                .username
                .as_deref()
                .or(account.email.as_deref())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| {
                    AuthError::InvalidConfig(
                        "account is missing both username and email for authentication".to_string(),
                    )
                })?;

            let passwd = SecretString::from(secret.expose_secret().to_string());

            let sasl = match preference {
                PasswordAuthPreference::Plain => Sasl::Plain(SaslPlainCreds {
                    authzid: None,
                    authcid: username.to_string(),
                    passwd,
                }),
                PasswordAuthPreference::Login => Sasl::Login(SaslLoginCreds {
                    username: username.to_string(),
                    password: passwd,
                }),
                PasswordAuthPreference::ScramSha256 { nonce } => {
                    if nonce.is_empty() {
                        return Err(AuthError::InvalidConfig(
                            "SCRAM-SHA-256 requires a non-empty client nonce".to_string(),
                        ));
                    }
                    Sasl::ScramSha256(SaslScramCreds {
                        username: username.to_string(),
                        password: passwd,
                        nonce,
                        channel_binding: SaslGs2ChannelBinding::Unsupported,
                    })
                }
            };

            Ok(sasl)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use debloatmail_core::{AccountId, Security};

    fn sample_imap_account() -> Account {
        Account {
            id: AccountId::new("018f6e80-7c00-7218-971e-7f62b2512f71").unwrap(),
            kind: AccountKind::Imap,
            name: "fastmail".to_string(),
            email: Some("alice@fastmail.com".to_string()),
            provider: Some("fastmail".to_string()),
            enabled: true,
            imap_host: Some("imap.fastmail.com".to_string()),
            imap_port: Some(993),
            imap_security: Some(Security::Tls),
            smtp_host: Some("smtp.fastmail.com".to_string()),
            smtp_port: Some(465),
            smtp_security: Some(Security::Tls),
            auth_kind: AuthKind::Password,
            username: Some("alice@fastmail.com".to_string()),
            secret_ref: Some("debloatmail:018f6e80-7c00-7218-971e-7f62b2512f71".to_string()),
            allow_invalid_cert_loopback: false,
            poll_interval_secs: None,
            created_at: 1700000000,
            updated_at: 1700000000,
        }
    }

    #[test]
    fn test_select_password_mechanism_precedence() {
        let adv = [SaslMechanism::Login, SaslMechanism::Plain, SaslMechanism::ScramSha256];
        let pref = select_password_mechanism(&adv, Some(b"nonce12345".to_vec()));
        assert_eq!(
            pref,
            PasswordAuthPreference::ScramSha256 {
                nonce: b"nonce12345".to_vec()
            }
        );

        let pref_no_nonce = select_password_mechanism(&adv, None);
        assert_eq!(pref_no_nonce, PasswordAuthPreference::Plain);

        let login_only = [SaslMechanism::Login];
        assert_eq!(
            select_password_mechanism(&login_only, None),
            PasswordAuthPreference::Login
        );
    }

    #[test]
    fn test_build_sasl_credentials_plain_and_scram() {
        let acct = sample_imap_account();
        let secret = SecretString::from("my-app-pass".to_string());

        let plain =
            build_sasl_credentials(&acct, &secret, PasswordAuthPreference::Plain).unwrap();
        assert_eq!(plain.mechanism(), SaslMechanism::Plain);
        assert!(!format!("{plain:?}").contains("my-app-pass"));

        let scram = build_sasl_credentials(
            &acct,
            &secret,
            PasswordAuthPreference::ScramSha256 {
                nonce: b"client-nonce-xyz".to_vec(),
            },
        )
        .unwrap();
        assert_eq!(scram.mechanism(), SaslMechanism::ScramSha256);
        assert!(!format!("{scram:?}").contains("my-app-pass"));
    }

    #[test]
    fn test_oauth_deferred_error() {
        let mut acct = sample_imap_account();
        acct.auth_kind = AuthKind::OAuth2;
        let secret = SecretString::from("token".to_string());
        let res = build_sasl_credentials(&acct, &secret, PasswordAuthPreference::Plain);
        assert!(matches!(res, Err(AuthError::OAuthDeferred)));
    }
}
