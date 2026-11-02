//! Diagnostic runner for `debloatmailctl account check` and `debloatmailctl doctor`
//! (`debloatmailctl.md` §3.1, §8; `SECURITY.md` §5.2).

use std::time::Instant;

use debloatmail_auth::{
    PasswordAuthPreference, SecretStore, build_sasl_credentials, select_password_mechanism,
    validate_account_security,
};
use debloatmail_core::{Account, Provider};
use io_imap::client::ImapClientAsync;
use io_imap::rfc3501::list::ImapMailboxList;
use io_imap::rfc3501::logout::ImapLogout;
use io_imap::rfc3501::select::ImapMailboxSelect;
use io_imap::types::mailbox::{ListMailbox, Mailbox};
use io_smtp::client::SmtpClientAsync;
use io_smtp::rfc5321::quit::SmtpQuit;

use crate::imap::{DiscoveredFolder, ImapConnection, discover_folders};
use crate::smtp::SmtpConnection;
use crate::tls::TlsVerifyMode;

/// Result of a single diagnostic step (`keyring`, `imap_connect`, `imap_list`, `smtp_connect`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckStepReport {
    pub step: &'static str,
    pub passed: bool,
    pub duration_ms: u64,
    pub detail: String,
    pub is_auth_or_network_failure: bool,
}

/// Complete report for an account connectivity & authentication check (`debloatmailctl.md` §3.1, §8).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountCheckReport {
    pub account_id: String,
    pub account_name: String,
    pub steps: Vec<CheckStepReport>,
    pub imap_capabilities: Vec<String>,
    pub discovered_folders: Vec<DiscoveredFolder>,
    /// Explicitly `true` whenever the Proton Bridge loopback self-signed TLS exception
    /// (`allow_invalid_cert_loopback` on `127.0.0.1` / `::1`) was used (`SECURITY.md` §5.2).
    pub loopback_cert_exception_used: bool,
}

impl AccountCheckReport {
    /// Returns `true` when every diagnostic step succeeded.
    pub fn all_passed(&self) -> bool {
        !self.steps.is_empty() && self.steps.iter().all(|s| s.passed)
    }

    /// Returns `true` when any step failed due to a network, TLS, or authentication error
    /// (maps to CLI exit code `3` per `debloatmailctl.md` §1.2).
    pub fn has_auth_or_network_failure(&self) -> bool {
        self.steps
            .iter()
            .any(|s| !s.passed && s.is_auth_or_network_failure)
    }
}

/// Executes the end-to-end diagnostic sequence for `account`:
/// 1. Validates security invariants (`validate_account_security`).
/// 2. Looks up the password in `secret_store`.
/// 3. Connects to IMAP, negotiates TLS/STARTTLS, authenticates, runs `LIST "" "*"` and
///    `SELECT INBOX`, and sends `LOGOUT`.
/// 4. Connects to SMTP, negotiates TLS/STARTTLS, authenticates, and sends `QUIT`.
pub async fn run_account_check(
    account: &Account,
    secret_store: &dyn SecretStore,
) -> AccountCheckReport {
    let mut report = AccountCheckReport {
        account_id: account.id.to_string(),
        account_name: account.name.clone(),
        steps: Vec::new(),
        imap_capabilities: Vec::new(),
        discovered_folders: Vec::new(),
        loopback_cert_exception_used: false,
    };

    // Step 1: Security policy validation
    let t0 = Instant::now();
    if let Err(err) = validate_account_security(account) {
        report.steps.push(CheckStepReport {
            step: "security_policy",
            passed: false,
            duration_ms: t0.elapsed().as_millis() as u64,
            detail: err.to_string(),
            is_auth_or_network_failure: false,
        });
        return report;
    }

    // Step 2: Keyring secret lookup
    let t_keyring = Instant::now();
    let password = match secret_store.get_secret(&account.id) {
        Ok(secret) => {
            report.steps.push(CheckStepReport {
                step: "keyring",
                passed: true,
                duration_ms: t_keyring.elapsed().as_millis() as u64,
                detail: format!("loaded secret for {}", account.secret_ref),
                is_auth_or_network_failure: false,
            });
            secret
        }
        Err(err) => {
            report.steps.push(CheckStepReport {
                step: "keyring",
                passed: false,
                duration_ms: t_keyring.elapsed().as_millis() as u64,
                detail: err.to_string(),
                is_auth_or_network_failure: true,
            });
            return report;
        }
    };

    let mech = select_password_mechanism(&[], PasswordAuthPreference::Auto);
    let imap_sasl = match build_sasl_credentials(&account.username, &password, mech, None) {
        Ok(creds) => creds,
        Err(err) => {
            report.steps.push(CheckStepReport {
                step: "imap_auth_prep",
                passed: false,
                duration_ms: 0,
                detail: err.to_string(),
                is_auth_or_network_failure: true,
            });
            return report;
        }
    };
    let smtp_sasl = match build_sasl_credentials(&account.username, &password, mech, None) {
        Ok(creds) => creds,
        Err(err) => {
            report.steps.push(CheckStepReport {
                step: "smtp_auth_prep",
                passed: false,
                duration_ms: 0,
                detail: err.to_string(),
                is_auth_or_network_failure: true,
            });
            return report;
        }
    };

    let auto_id = account.provider == Provider::Fastmail;

    // Step 3: IMAP connect + TLS + authenticate + LIST + SELECT INBOX + LOGOUT
    let t_imap = Instant::now();
    match ImapConnection::connect(
        &account.imap_host,
        account.imap_port,
        account.imap_security,
        account.allow_invalid_cert_loopback,
        Some(imap_sasl),
        auto_id,
    )
    .await
    {
        Ok(mut imap_conn) => {
            if imap_conn.verify_mode == TlsVerifyMode::LoopbackExceptionUsed {
                report.loopback_cert_exception_used = true;
            }
            report.imap_capabilities = imap_conn.capabilities.raw_names.clone();
            report.steps.push(CheckStepReport {
                step: "imap_auth",
                passed: true,
                duration_ms: t_imap.elapsed().as_millis() as u64,
                detail: format!(
                    "connected and authenticated to {}:{} ({:?})",
                    account.imap_host, account.imap_port, imap_conn.verify_mode
                ),
                is_auth_or_network_failure: false,
            });

            // LIST "" "*" and SELECT INBOX
            let t_list = Instant::now();
            let wildcard = ListMailbox::try_from("*").unwrap_or_else(|_| {
                ListMailbox::Token(io_imap::types::mailbox::ListCharString::unvalidated("*"))
            });
            let reference = Mailbox::try_from("").unwrap_or(Mailbox::Inbox);
            match imap_conn
                .run(ImapMailboxList::new(reference, wildcard))
                .await
            {
                Ok(listing) => {
                    let folders = discover_folders(listing);
                    let count = folders.len();
                    report.discovered_folders = folders;

                    match imap_conn
                        .run(ImapMailboxSelect::new(Mailbox::Inbox, None))
                        .await
                    {
                        Ok(_) => {
                            report.steps.push(CheckStepReport {
                                step: "imap_mailbox",
                                passed: true,
                                duration_ms: t_list.elapsed().as_millis() as u64,
                                detail: format!(
                                    "discovered {count} folder(s) and selected INBOX"
                                ),
                                is_auth_or_network_failure: false,
                            });
                        }
                        Err(err) => {
                            report.steps.push(CheckStepReport {
                                step: "imap_mailbox",
                                passed: false,
                                duration_ms: t_list.elapsed().as_millis() as u64,
                                detail: format!("SELECT INBOX failed: {err}"),
                                is_auth_or_network_failure: true,
                            });
                        }
                    }
                }
                Err(err) => {
                    report.steps.push(CheckStepReport {
                        step: "imap_mailbox",
                        passed: false,
                        duration_ms: t_list.elapsed().as_millis() as u64,
                        detail: format!("LIST failed: {err}"),
                        is_auth_or_network_failure: true,
                    });
                }
            }

            let _ = imap_conn.run(ImapLogout::new()).await;
        }
        Err(err) => {
            report.steps.push(CheckStepReport {
                step: "imap_auth",
                passed: false,
                duration_ms: t_imap.elapsed().as_millis() as u64,
                detail: err.to_string(),
                is_auth_or_network_failure: true,
            });
        }
    }

    // Step 4: SMTP connect + TLS + EHLO + authenticate + QUIT
    let t_smtp = Instant::now();
    match SmtpConnection::connect(
        &account.smtp_host,
        account.smtp_port,
        account.smtp_security,
        account.allow_invalid_cert_loopback,
        Some(smtp_sasl),
    )
    .await
    {
        Ok(mut smtp_conn) => {
            if smtp_conn.verify_mode == TlsVerifyMode::LoopbackExceptionUsed {
                report.loopback_cert_exception_used = true;
            }
            report.steps.push(CheckStepReport {
                step: "smtp_auth",
                passed: true,
                duration_ms: t_smtp.elapsed().as_millis() as u64,
                detail: format!(
                    "connected and authenticated to {}:{} ({:?})",
                    account.smtp_host, account.smtp_port, smtp_conn.verify_mode
                ),
                is_auth_or_network_failure: false,
            });
            let _ = smtp_conn.run(SmtpQuit::new()).await;
        }
        Err(err) => {
            report.steps.push(CheckStepReport {
                step: "smtp_auth",
                passed: false,
                duration_ms: t_smtp.elapsed().as_millis() as u64,
                detail: err.to_string(),
                is_auth_or_network_failure: true,
            });
        }
    }

    report
}

#[cfg(test)]
mod tests {
    use debloatmail_auth::{MemorySecretStore, apply_preset};
    use debloatmail_core::{AccountId, Provider};

    use super::*;

    #[tokio::test]
    async fn test_run_account_check_reports_missing_keyring_secret() {
        let store = MemorySecretStore::new();
        let account = apply_preset(
            AccountId::new("01J8X6Z4M9R3W2Q7V5N8K1P0T2"),
            "Fastmail",
            "alice@fastmail.com",
            Provider::Fastmail,
            None,
        );

        let report = run_account_check(&account, &store).await;
        assert!(!report.all_passed());
        assert!(report.has_auth_or_network_failure());
        assert_eq!(report.steps.len(), 1);
        assert_eq!(report.steps[0].step, "keyring");
        assert!(!report.steps[0].passed);
    }

    #[tokio::test]
    async fn test_run_account_check_rejects_invalid_security_policy() {
        let store = MemorySecretStore::new();
        let mut account = apply_preset(
            AccountId::new("01J8X6Z4M9R3W2Q7V5N8K1P0T2"),
            "Bad",
            "alice@fastmail.com",
            Provider::Fastmail,
            None,
        );
        account.allow_invalid_cert_loopback = true; // Invalid for non-loopback Fastmail host

        let report = run_account_check(&account, &store).await;
        assert!(!report.all_passed());
        assert!(!report.has_auth_or_network_failure());
        assert_eq!(report.steps[0].step, "security_policy");
    }
}
