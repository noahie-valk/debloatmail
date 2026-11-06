//! The `compose` command conforming to `docs/debloatmailctl.md` §8.

use debloatmail_ffi::{FfiDraft, FfiOpState};
use rusqlite::Connection;
use std::io::{self, Read};
use std::time::{Duration, Instant};

use crate::args::{Cli, ComposeArgs, OutputFormat};
use crate::config::AppPaths;
use crate::error::CliError;
use crate::executor::init_engine;
use crate::resolve::resolve_account;
use crate::rfc5322::Rfc5322Message;

pub async fn execute_compose(
    conn: &Connection,
    cli: &Cli,
    paths: &AppPaths,
    args: &ComposeArgs,
) -> Result<(), CliError> {
    // 1. Resolve sender account
    let sender_account = match cli.account.as_deref() {
        Some(acc) => {
            let a = resolve_account(conn, acc)?;
            if a.kind != "imap" {
                return Err(CliError::Usage(format!(
                    "account '{}' is a feed account; cannot send mail",
                    a.name
                )));
            }
            a
        }
        None => {
            let accounts = debloatmail_ffi::reader::list_accounts_fn(conn)?;
            accounts
                .into_iter()
                .find(|a| a.kind == "imap" && a.enabled)
                .ok_or_else(|| {
                    CliError::Usage(
                        "no enabled mail accounts found; add one with 'debloatmailctl account add'"
                            .into(),
                    )
                })?
        }
    };

    // 2. Validate attachments exist and are readable
    for path in &args.attach {
        if !path.is_file() {
            return Err(CliError::Usage(format!(
                "attachment '{}' is not a regular file",
                path.display()
            )));
        }
    }

    // 3. Acquire body
    let mut to = args.to.clone();
    let mut cc = args.cc.clone();
    let mut subject = args.subject.clone().unwrap_or_default();
    let mut body = String::new();

    if args.body_stdin {
        io::stdin()
            .read_to_string(&mut body)
            .map_err(|e| CliError::Internal(e.to_string()))?;
    } else if args.edit {
        let template = format!(
            "To: {}\nCc: {}\nSubject: {}\n\n",
            to.join(", "),
            cc.join(", "),
            subject
        );
        let edited = open_in_editor(&template)?;
        let (parsed_to, parsed_cc, parsed_subject, parsed_body) = parse_edited_template(&edited);
        if !parsed_to.is_empty() {
            to = parsed_to;
        }
        if !parsed_cc.is_empty() {
            cc = parsed_cc;
        }
        if !parsed_subject.is_empty() {
            subject = parsed_subject;
        }
        body = parsed_body;
    }

    // 4. Construct draft
    let draft = FfiDraft {
        account_id: sender_account.id.clone(),
        to: to.clone(),
        cc: cc.clone(),
        bcc: args.bcc.clone(),
        subject: subject.clone(),
        body: body.clone(),
        attachment_paths: args.attach.clone(),
        in_reply_to: None,
        references: vec![],
    };

    // 5. Handle --dry-run
    if cli.dry_run {
        let rfc_msg = Rfc5322Message {
            from: sender_account.email.clone(),
            to,
            cc,
            bcc: args.bcc.clone(),
            subject,
            in_reply_to: None,
            references: vec![],
            body,
            attachment_paths: args.attach.clone(),
        };
        let payload = rfc_msg.build_payload()?;
        print!("{payload}");
        return Ok(());
    }

    // 6. Handle --save-draft
    if args.save_draft {
        let engine = init_engine(cli, paths).await?;
        let draft_id = engine.save_draft(draft).await?;
        match cli.format {
            OutputFormat::Human => println!("saved draft {draft_id}"),
            OutputFormat::Json => {
                println!(
                    "{}",
                    serde_json::json!({
                        "draft_id": draft_id
                    })
                );
            }
            OutputFormat::Jsonl => {
                println!("{}", serde_json::json!({ "draft_id": draft_id }));
            }
            OutputFormat::Plain => println!("draft_id\t{draft_id}"),
        }
        return Ok(());
    }

    // 7. Enqueue send operation
    if cli.offline && args.now {
        return Err(CliError::Offline(
            "--now requires network connectivity to send and --offline is active".into(),
        ));
    }

    let engine = init_engine(cli, paths).await?;
    let op_id = engine
        .send_message(draft, args.draft.clone(), !args.no_append_sent)
        .await?;

    if args.now {
        let timeout = parse_duration_cli(&cli.timeout).unwrap_or(Duration::from_secs(60));
        let start = Instant::now();
        loop {
            let status = engine.get_op_status(&op_id)?;
            match status.state {
                FfiOpState::Done => {
                    match cli.format {
                        OutputFormat::Human => println!("sent op {op_id}"),
                        OutputFormat::Json | OutputFormat::Jsonl => {
                            println!(
                                "{}",
                                serde_json::json!({ "op_id": op_id, "status": "sent" })
                            );
                        }
                        OutputFormat::Plain => println!("op_id\t{op_id}\tsent"),
                    }
                    return Ok(());
                }
                FfiOpState::Failed => {
                    let err_msg = status
                        .last_error
                        .unwrap_or_else(|| "message send failed".into());
                    return Err(CliError::Internal(err_msg));
                }
                _ => {
                    if start.elapsed() >= timeout {
                        return Err(CliError::NetworkError(format!(
                            "timed out waiting for op '{op_id}' to complete"
                        )));
                    }
                    tokio::time::sleep(Duration::from_millis(150)).await;
                }
            }
        }
    } else {
        match cli.format {
            OutputFormat::Human => println!("queued op {op_id}"),
            OutputFormat::Json => {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "op_id": op_id,
                        "status": "queued"
                    }))
                    .map_err(|e| CliError::Internal(e.to_string()))?
                );
            }
            OutputFormat::Jsonl => {
                println!(
                    "{}",
                    serde_json::json!({
                        "op_id": op_id,
                        "status": "queued"
                    })
                );
            }
            OutputFormat::Plain => println!("op_id\t{op_id}\tqueued"),
        }
    }

    Ok(())
}

fn open_in_editor(template: &str) -> Result<String, CliError> {
    let editor = std::env::var("VISUAL")
        .or_else(|_| std::env::var("EDITOR"))
        .unwrap_or_else(|_| "vi".to_string());

    let tmp_path = std::env::temp_dir().join(format!("debloatmail_compose_{}.eml", uuid_short()));
    std::fs::write(&tmp_path, template.as_bytes())
        .map_err(|e| CliError::Internal(e.to_string()))?;

    let status = std::process::Command::new(&editor)
        .arg(&tmp_path)
        .status()
        .map_err(|e| CliError::Usage(format!("failed to run editor '{editor}': {e}")))?;

    if !status.success() {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(CliError::Usage(
            "editor exited with non-zero exit code".to_string(),
        ));
    }

    let content =
        std::fs::read_to_string(&tmp_path).map_err(|e| CliError::Internal(e.to_string()))?;
    let _ = std::fs::remove_file(&tmp_path);
    Ok(content)
}

fn uuid_short() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{now:x}")
}

fn parse_edited_template(edited: &str) -> (Vec<String>, Vec<String>, String, String) {
    let mut to = Vec::new();
    let mut cc = Vec::new();
    let mut subject = String::new();
    let mut body = String::new();
    let mut in_headers = true;

    for line in edited.lines() {
        if in_headers {
            if line.is_empty() {
                in_headers = false;
                continue;
            }
            if let Some(val) = line
                .strip_prefix("To:")
                .or_else(|| line.strip_prefix("to:"))
            {
                to = val
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            } else if let Some(val) = line
                .strip_prefix("Cc:")
                .or_else(|| line.strip_prefix("cc:"))
            {
                cc = val
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            } else if let Some(val) = line
                .strip_prefix("Subject:")
                .or_else(|| line.strip_prefix("subject:"))
            {
                subject = val.trim().to_string();
            }
        } else {
            body.push_str(line);
            body.push('\n');
        }
    }

    (to, cc, subject, body)
}

pub fn parse_duration_cli(s: &str) -> Option<Duration> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    if let Some(num) = s.strip_suffix("ms") {
        num.parse::<u64>().ok().map(Duration::from_millis)
    } else if let Some(num) = s.strip_suffix('s') {
        num.parse::<u64>().ok().map(Duration::from_secs)
    } else if let Some(num) = s.strip_suffix('m') {
        num.parse::<u64>().ok().map(|m| Duration::from_secs(m * 60))
    } else if let Some(num) = s.strip_suffix('h') {
        num.parse::<u64>()
            .ok()
            .map(|h| Duration::from_secs(h * 3600))
    } else {
        s.parse::<u64>().ok().map(Duration::from_secs)
    }
}
