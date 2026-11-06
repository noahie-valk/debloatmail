//! The `forward` command conforming to `docs/debloatmailctl.md` §8.

use debloatmail_ffi::reader::get_item_fn;
use debloatmail_ffi::{FfiDraft, FfiItemKind};
use rusqlite::Connection;
use std::io::{self, Read};

use crate::args::{Cli, ForwardArgs};
use crate::config::AppPaths;
use crate::error::CliError;
use crate::executor::init_engine;
use crate::resolve::resolve_item_id;
use crate::rfc5322::Rfc5322Message;

pub async fn execute_forward(
    conn: &Connection,
    cli: &Cli,
    paths: &AppPaths,
    args: &ForwardArgs,
) -> Result<(), CliError> {
    let public_id = resolve_item_id(conn, &args.id)?;
    let mut detail = get_item_fn(conn, &paths.blobs_dir, &public_id)?;

    if detail.summary.kind == FfiItemKind::Feed {
        return Err(CliError::Usage(
            "feed items cannot be forwarded as email".to_string(),
        ));
    }

    // 1. Subject prefixed with "Fwd: "
    let subject = if detail
        .summary
        .title
        .to_ascii_lowercase()
        .starts_with("fwd: ")
    {
        detail.summary.title.clone()
    } else {
        format!("Fwd: {}", detail.summary.title)
    };

    // 2. Format forwarded message block
    let from = detail
        .from_addr
        .as_deref()
        .unwrap_or(&detail.summary.author);
    let date = if !detail.summary.published_at.is_empty() {
        &detail.summary.published_at
    } else {
        &detail.summary.sort_at
    };
    let to_hdr = if !detail.to_addrs.is_empty() {
        detail.to_addrs.join(", ")
    } else {
        String::new()
    };

    let mut forward_block = String::new();
    forward_block.push_str("---------- Forwarded message ---------\n");
    forward_block.push_str(&format!("From: {from}\n"));
    forward_block.push_str(&format!("Date: {date}\n"));
    forward_block.push_str(&format!("Subject: {}\n", detail.summary.title));
    if !to_hdr.is_empty() {
        forward_block.push_str(&format!("To: {to_hdr}\n"));
    }
    forward_block.push('\n');
    if let Some(ref body) = detail.body_text {
        forward_block.push_str(body);
        if !body.ends_with('\n') {
            forward_block.push('\n');
        }
    }

    // 3. User commentary body
    let mut user_body = String::new();
    if args.body_stdin {
        io::stdin()
            .read_to_string(&mut user_body)
            .map_err(|e| CliError::Internal(e.to_string()))?;
    }

    let full_body = if !user_body.is_empty() {
        format!("{user_body}\n\n{forward_block}")
    } else {
        forward_block
    };

    // 4. Attach original raw MIME if requested
    let mut attachments = Vec::new();
    if args.attach_original {
        let has_blob = detail.raw_blob_path.as_ref().is_some_and(|p| p.exists());
        if !has_blob {
            let engine = init_engine(cli, paths).await?;
            engine.fetch_body(&public_id).await?;
            detail = get_item_fn(conn, &paths.blobs_dir, &public_id)?;
        }
        if let Some(raw_path) = detail.raw_blob_path
            && raw_path.exists()
        {
            attachments.push(raw_path);
        }
    }

    let my_email: String = conn
        .query_row(
            "SELECT email FROM accounts WHERE public_id = ?1;",
            [&detail.summary.account_id],
            |r| r.get(0),
        )
        .unwrap_or_default();

    let draft = FfiDraft {
        account_id: detail.summary.account_id.clone(),
        to: args.to.clone(),
        cc: vec![],
        bcc: vec![],
        subject: subject.clone(),
        body: full_body,
        attachment_paths: attachments.clone(),
        in_reply_to: None,
        references: vec![],
    };

    if cli.dry_run {
        let rfc_msg = Rfc5322Message {
            from: my_email,
            to: draft.to,
            cc: draft.cc,
            bcc: draft.bcc,
            subject: draft.subject,
            in_reply_to: None,
            references: vec![],
            body: draft.body,
            attachment_paths: attachments,
        };
        let payload = rfc_msg.build_payload()?;
        print!("{payload}");
        return Ok(());
    }

    let engine = init_engine(cli, paths).await?;
    if args.save_draft {
        let draft_id = engine.save_draft(draft).await?;
        println!("saved draft {draft_id}");
        Ok(())
    } else {
        let op_id = engine.send_message(draft, None, true).await?;
        if args.now {
            println!("sent op {op_id}");
        } else {
            println!("queued op {op_id}");
        }
        Ok(())
    }
}
