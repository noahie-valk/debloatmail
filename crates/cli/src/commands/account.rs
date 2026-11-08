//! Account inspection and configuration subcommands conforming to `docs/debloatmailctl.md` §8.

use debloatmail_ffi::FfiAccount;
use debloatmail_ffi::reader::list_accounts_fn;
use rusqlite::Connection;
use std::io::{self, Read, Write};

use crate::args::{AccountArgs, AccountCommands, Cli, OutputFormat};
use crate::config::AppPaths;
use crate::error::CliError;
use crate::executor::{confirm_destructive, ensure_online, init_engine};
use crate::format::format_accounts;
use crate::format::json::print_single_json;
use crate::format::plain::mask_email;
use crate::resolve::resolve_account;

pub async fn execute_account(
    conn: &Connection,
    cli: &Cli,
    paths: &AppPaths,
    args: &AccountArgs,
) -> Result<(), CliError> {
    match &args.command {
        AccountCommands::List { json } => {
            let mut accounts = list_accounts_fn(conn)?;
            if cli.redact {
                for a in &mut accounts {
                    if !a.email.is_empty() {
                        a.email = mask_email(&a.email);
                    }
                }
            }
            let format = if *json {
                OutputFormat::Json
            } else {
                cli.format
            };
            format_accounts(&accounts, format)
        }
        AccountCommands::Show { name_or_id } => {
            let mut account = resolve_account(conn, name_or_id)?;
            if cli.redact && !account.email.is_empty() {
                account.email = mask_email(&account.email);
            }

            match cli.format {
                OutputFormat::Json | OutputFormat::Jsonl => print_single_json(&account)?,
                OutputFormat::Plain => {
                    println!(
                        "{}\t{}\t{}\t{}\t{}\t{}",
                        account.id,
                        account.name,
                        account.kind,
                        account.email,
                        account.enabled,
                        account.unread_count
                    );
                }
                OutputFormat::Human => {
                    println!("ID:          {}", account.id);
                    println!("Name:        {}", account.name);
                    println!("Kind:        {}", account.kind);
                    println!("Email:       {}", account.email);
                    println!(
                        "Enabled:     {}",
                        if account.enabled { "yes" } else { "no" }
                    );
                    println!(
                        "Provider:    {}",
                        account.provider_preset.as_deref().unwrap_or("generic")
                    );
                    println!("Credentials: ••• (in keyring)");
                    println!(
                        "Last Sync:   {}",
                        account.last_sync_at.as_deref().unwrap_or("never")
                    );
                    println!("Unread:      {}", account.unread_count);
                }
            }
            Ok(())
        }
        AccountCommands::Add {
            kind,
            name,
            wizard,
            email,
            provider,
            username,
            auth_kind,
            imap_host,
            imap_port,
            imap_security,
            smtp_host,
            smtp_port,
            smtp_security,
            secret_stdin,
            secret_command,
        } => {
            let is_tty = unsafe { libc::isatty(libc::STDIN_FILENO) == 1 };
            let run_wizard =
                *wizard || (name.is_none() && is_tty && !*secret_stdin && secret_command.is_none());

            let (
                acc_name,
                acc_kind,
                acc_email,
                acc_provider,
                acc_user,
                acc_auth,
                acc_imap_host,
                acc_imap_port,
                acc_imap_sec,
                acc_smtp_host,
                acc_smtp_port,
                acc_smtp_sec,
                acc_password,
                acc_loopback,
            ) = if run_wizard {
                run_account_wizard()?
            } else {
                let n = name.as_deref().ok_or_else(|| {
                    CliError::Usage("--name <NAME> is required when adding an account".to_string())
                })?;
                let k = kind.as_deref().unwrap_or("imap");
                let em = email.as_deref().unwrap_or("");
                let prov = provider.as_deref();

                let is_proton = prov.is_some_and(|p| p.eq_ignore_ascii_case("proton"));

                let imap_h =
                    imap_host
                        .as_deref()
                        .or(if is_proton { Some("127.0.0.1") } else { None });
                let imap_p = imap_port.or(if is_proton { Some(1143) } else { None });
                let imap_s =
                    imap_security
                        .as_deref()
                        .or(if is_proton { Some("starttls") } else { None });

                let smtp_h =
                    smtp_host
                        .as_deref()
                        .or(if is_proton { Some("127.0.0.1") } else { None });
                let smtp_p = smtp_port.or(if is_proton { Some(1025) } else { None });
                let smtp_s =
                    smtp_security
                        .as_deref()
                        .or(if is_proton { Some("starttls") } else { None });

                let pass = if k == "imap" {
                    acquire_secret(*secret_stdin, secret_command.as_deref(), is_tty)?
                } else {
                    None
                };

                (
                    n.to_string(),
                    k.to_string(),
                    em.to_string(),
                    prov.map(|s| s.to_string()),
                    username.clone(),
                    auth_kind.clone().unwrap_or_else(|| "password".to_string()),
                    imap_h.map(|s| s.to_string()),
                    imap_p,
                    imap_s.map(|s| s.to_string()),
                    smtp_h.map(|s| s.to_string()),
                    smtp_p,
                    smtp_s.map(|s| s.to_string()),
                    pass,
                    is_proton,
                )
            };

            let mut params = serde_json::json!({
                "name": acc_name,
                "kind": acc_kind,
                "email": acc_email,
                "auth_kind": acc_auth,
                "allow_invalid_cert_loopback": acc_loopback,
            });

            if let Some(ref p) = acc_provider {
                params["provider"] = serde_json::Value::String(p.clone());
            }
            if let Some(ref u) = acc_user {
                params["username"] = serde_json::Value::String(u.clone());
            }
            if let Some(ref h) = acc_imap_host {
                params["imap_host"] = serde_json::Value::String(h.clone());
            }
            if let Some(p) = acc_imap_port {
                params["imap_port"] = serde_json::Value::Number(p.into());
            }
            if let Some(ref s) = acc_imap_sec {
                params["imap_security"] = serde_json::Value::String(s.clone());
            }
            if let Some(ref h) = acc_smtp_host {
                params["smtp_host"] = serde_json::Value::String(h.clone());
            }
            if let Some(p) = acc_smtp_port {
                params["smtp_port"] = serde_json::Value::Number(p.into());
            }
            if let Some(ref s) = acc_smtp_sec {
                params["smtp_security"] = serde_json::Value::String(s.clone());
            }
            if let Some(ref pass) = acc_password {
                params["password"] = serde_json::Value::String(pass.clone());
            }

            if cli.dry_run {
                println!("would add account '{acc_name}' of kind '{acc_kind}'");
                return Ok(());
            }

            let engine = init_engine(cli, paths).await?;
            let acc_id = engine.add_account(params).await?;

            match cli.format {
                OutputFormat::Human => println!("added account '{acc_name}' ({acc_id})"),
                OutputFormat::Json | OutputFormat::Jsonl => {
                    println!(
                        "{}",
                        serde_json::json!({
                            "account_id": acc_id,
                            "name": acc_name
                        })
                    );
                }
                OutputFormat::Plain => println!("{acc_id}\t{acc_name}"),
            }

            Ok(())
        }
        AccountCommands::Update {
            name,
            name_new,
            enable,
            disable,
            secret_stdin,
            secret_command,
            email,
            username,
            imap_host,
            imap_port,
            imap_security,
            smtp_host,
            smtp_port,
            smtp_security,
        } => {
            let account = resolve_account(conn, name)?;

            let mut params = serde_json::Map::new();

            if let Some(nn) = name_new {
                params.insert("name".to_string(), serde_json::Value::String(nn.clone()));
            }
            if *enable {
                params.insert("enabled".to_string(), serde_json::Value::Bool(true));
            } else if *disable {
                params.insert("enabled".to_string(), serde_json::Value::Bool(false));
            }
            if let Some(em) = email {
                params.insert("email".to_string(), serde_json::Value::String(em.clone()));
            }
            if let Some(u) = username {
                params.insert("username".to_string(), serde_json::Value::String(u.clone()));
            }
            if let Some(h) = imap_host {
                params.insert(
                    "imap_host".to_string(),
                    serde_json::Value::String(h.clone()),
                );
            }
            if let Some(p) = imap_port {
                params.insert(
                    "imap_port".to_string(),
                    serde_json::Value::Number((*p).into()),
                );
            }
            if let Some(s) = imap_security {
                params.insert(
                    "imap_security".to_string(),
                    serde_json::Value::String(s.clone()),
                );
            }
            if let Some(h) = smtp_host {
                params.insert(
                    "smtp_host".to_string(),
                    serde_json::Value::String(h.clone()),
                );
            }
            if let Some(p) = smtp_port {
                params.insert(
                    "smtp_port".to_string(),
                    serde_json::Value::Number((*p).into()),
                );
            }
            if let Some(s) = smtp_security {
                params.insert(
                    "smtp_security".to_string(),
                    serde_json::Value::String(s.clone()),
                );
            }

            let is_tty = unsafe { libc::isatty(libc::STDIN_FILENO) == 1 };
            if let Some(pass) = acquire_secret(
                *secret_stdin,
                secret_command.as_deref(),
                is_tty && (*secret_stdin || secret_command.is_some()),
            )? {
                params.insert("password".to_string(), serde_json::Value::String(pass));
            }

            if cli.dry_run {
                println!("would update account '{}'", account.name);
                return Ok(());
            }

            let engine = init_engine(cli, paths).await?;
            engine
                .update_account(&account.id, serde_json::Value::Object(params))
                .await?;

            match cli.format {
                OutputFormat::Human => println!("updated account '{}'", account.name),
                OutputFormat::Json | OutputFormat::Jsonl => {
                    println!(
                        "{}",
                        serde_json::json!({
                            "status": "updated",
                            "account_id": account.id
                        })
                    );
                }
                OutputFormat::Plain => println!("updated\t{}", account.id),
            }

            Ok(())
        }
        AccountCommands::Remove { name, purge } => {
            let account = resolve_account(conn, name)?;

            if *purge {
                confirm_destructive(
                    cli,
                    "Permanently delete account and all its folders and items from database?",
                )?;
            }

            if cli.dry_run {
                println!("would remove account '{}'", account.name);
                return Ok(());
            }

            let engine = init_engine(cli, paths).await?;
            engine.remove_account(&account.id, *purge).await?;

            match cli.format {
                OutputFormat::Human => {
                    if *purge {
                        println!("purged account '{}'", account.name);
                    } else {
                        println!("disabled account '{}'", account.name);
                    }
                }
                OutputFormat::Json | OutputFormat::Jsonl => {
                    println!(
                        "{}",
                        serde_json::json!({
                            "status": "removed",
                            "account_id": account.id
                        })
                    );
                }
                OutputFormat::Plain => println!("removed\t{}", account.id),
            }

            Ok(())
        }
        AccountCommands::Check { name } => {
            ensure_online(cli)?;

            let accounts_to_check: Vec<FfiAccount> = match name {
                Some(n) => vec![resolve_account(conn, n)?],
                None => {
                    let accs = list_accounts_fn(conn)?;
                    accs.into_iter().filter(|a| a.enabled).collect()
                }
            };

            if accounts_to_check.is_empty() {
                println!("No enabled accounts configured to check.");
                return Ok(());
            }

            let engine = init_engine(cli, paths).await?;
            let mut all_reports = Vec::new();

            for acc in &accounts_to_check {
                let report = engine.check_account(&acc.id).await?;
                all_reports.push((acc.name.clone(), report));
            }

            let all_failed = all_reports
                .iter()
                .all(|(_, r)| !r.steps.is_empty() && r.steps.iter().all(|s| !s.passed));

            match cli.format {
                OutputFormat::Human => {
                    for (acc_name, rep) in &all_reports {
                        println!("{acc_name}:");
                        for step in &rep.steps {
                            let status_str = if step.passed { "OK" } else { "FAIL" };
                            let detail_str = if !step.detail.is_empty() {
                                format!(" ({})", step.detail)
                            } else {
                                String::new()
                            };
                            println!("  {:<14} {status_str}{detail_str}", step.step);
                        }
                    }
                }
                OutputFormat::Json | OutputFormat::Jsonl => {
                    let json_reps: Vec<_> = all_reports
                        .iter()
                        .map(|(n, r)| {
                            serde_json::json!({
                                "account": n,
                                "account_id": r.account_id,
                                "passed": r.overall_success,
                                "steps": r.steps,
                            })
                        })
                        .collect();
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&json_reps)
                            .map_err(|e| CliError::Internal(e.to_string()))?
                    );
                }
                OutputFormat::Plain => {
                    for (acc_name, rep) in &all_reports {
                        for step in &rep.steps {
                            println!(
                                "{}\t{}\t{}\t{}",
                                acc_name,
                                step.step,
                                if step.passed { "OK" } else { "FAIL" },
                                step.detail
                            );
                        }
                    }
                }
            }

            if all_failed && !all_reports.is_empty() {
                return Err(CliError::AuthFailed(
                    "account check failed for all configured targets".into(),
                ));
            }

            Ok(())
        }
    }
}

fn acquire_secret(
    secret_stdin: bool,
    secret_command: Option<&str>,
    interactive: bool,
) -> Result<Option<String>, CliError> {
    if secret_stdin {
        let mut s = String::new();
        io::stdin()
            .read_to_string(&mut s)
            .map_err(|e| CliError::Internal(e.to_string()))?;
        let trimmed = s.trim_end_matches(&['\r', '\n'][..]).to_string();
        Ok(Some(trimmed))
    } else if let Some(cmd) = secret_command {
        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg(cmd)
            .output()
            .map_err(|e| {
                CliError::Usage(format!("failed to execute secret command '{cmd}': {e}"))
            })?;
        if !output.status.success() {
            return Err(CliError::Usage(format!(
                "secret command '{cmd}' exited with status {}",
                output.status
            )));
        }
        let s = String::from_utf8(output.stdout).map_err(|e| CliError::Internal(e.to_string()))?;
        let trimmed = s.trim_end_matches(&['\r', '\n'][..]).to_string();
        Ok(Some(trimmed))
    } else if interactive {
        let pass = rpassword::prompt_password("Enter account password: ")
            .map_err(|e| CliError::Internal(e.to_string()))?;
        Ok(Some(pass))
    } else {
        Ok(None)
    }
}

#[allow(clippy::type_complexity)]
fn run_account_wizard() -> Result<
    (
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        String,
        Option<String>,
        Option<u16>,
        Option<String>,
        Option<String>,
        Option<u16>,
        Option<String>,
        Option<String>,
        bool,
    ),
    CliError,
> {
    println!("=== debloatmail Account Configuration Wizard ===");

    let name = prompt_line("Account name (e.g. personal, fastmail, blogs): ")?;
    if name.is_empty() {
        return Err(CliError::Usage("account name cannot be empty".to_string()));
    }

    let kind_input = prompt_line("Account kind [imap/feed] (default: imap): ")?;
    let kind = if kind_input.eq_ignore_ascii_case("feed") {
        "feed"
    } else {
        "imap"
    };

    if kind == "feed" {
        return Ok((
            name,
            kind.to_string(),
            String::new(),
            None,
            None,
            "password".to_string(),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            false,
        ));
    }

    let email = prompt_line("Email address: ")?;
    let provider = prompt_line("Provider preset [generic/fastmail/proton] (default: generic): ")?;
    let prov_opt = if provider.is_empty() {
        None
    } else {
        Some(provider.clone())
    };

    let is_proton = provider.eq_ignore_ascii_case("proton");

    let imap_host = if is_proton {
        Some("127.0.0.1".to_string())
    } else {
        let h = prompt_line("IMAP host: ")?;
        if h.is_empty() { None } else { Some(h) }
    };

    let imap_port = if is_proton {
        Some(1143)
    } else {
        let p = prompt_line("IMAP port [default: 993]: ")?;
        if p.is_empty() {
            Some(993)
        } else {
            p.parse::<u16>().ok()
        }
    };

    let imap_security = if is_proton {
        Some("starttls".to_string())
    } else {
        let s = prompt_line("IMAP security [tls/starttls/none] (default: tls): ")?;
        if s.is_empty() {
            Some("tls".into())
        } else {
            Some(s)
        }
    };

    let smtp_host = if is_proton {
        Some("127.0.0.1".to_string())
    } else {
        let h = prompt_line("SMTP host: ")?;
        if h.is_empty() { None } else { Some(h) }
    };

    let smtp_port = if is_proton {
        Some(1025)
    } else {
        let p = prompt_line("SMTP port [default: 465]: ")?;
        if p.is_empty() {
            Some(465)
        } else {
            p.parse::<u16>().ok()
        }
    };

    let smtp_security = if is_proton {
        Some("starttls".to_string())
    } else {
        let s = prompt_line("SMTP security [tls/starttls/none] (default: tls): ")?;
        if s.is_empty() {
            Some("tls".into())
        } else {
            Some(s)
        }
    };

    let password = rpassword::prompt_password("Enter account password: ")
        .map_err(|e| CliError::Internal(e.to_string()))?;
    let pass_opt = if password.is_empty() {
        None
    } else {
        Some(password)
    };

    Ok((
        name,
        kind.to_string(),
        email,
        prov_opt,
        None,
        "password".to_string(),
        imap_host,
        imap_port,
        imap_security,
        smtp_host,
        smtp_port,
        smtp_security,
        pass_opt,
        is_proton,
    ))
}

fn prompt_line(prompt: &str) -> Result<String, CliError> {
    print!("{prompt}");
    io::stdout()
        .flush()
        .map_err(|e| CliError::Internal(e.to_string()))?;
    let mut line = String::new();
    io::stdin()
        .read_line(&mut line)
        .map_err(|e| CliError::Internal(e.to_string()))?;
    Ok(line.trim().to_string())
}
