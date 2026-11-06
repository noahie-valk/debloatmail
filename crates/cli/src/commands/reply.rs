//! The `reply` command conforming to `docs/debloatmailctl.md` §8.

use debloatmail_ffi::reader::get_item_fn;
use debloatmail_ffi::{FfiDraft, FfiItemKind};
use rusqlite::Connection;
use std::io::{self, Read};

use crate::args::{Cli, ReplyArgs};
use crate::commands::compose::execute_compose;
use crate::config::AppPaths;
use crate::error::CliError;
use crate::resolve::resolve_item_id;

pub async fn execute_reply(
    conn: &Connection,
    cli: &Cli,
    paths: &AppPaths,
    args: &ReplyArgs,
) -> Result<(), CliError> {
    let public_id = resolve_item_id(conn, &args.id)?;
    let detail = get_item_fn(conn, &paths.blobs_dir, &public_id)?;

    if detail.summary.kind == FfiItemKind::Feed {
        return Err(CliError::Usage(
            "feed items have no sender and cannot be replied to".to_string(),
        ));
    }

    // 1. Prefill To from reply_to or from_addr
    let to_addr = detail
        .reply_to
        .clone()
        .or_else(|| detail.from_addr.clone())
        .unwrap_or_default();

    let to = if to_addr.is_empty() {
        vec![]
    } else {
        vec![to_addr.clone()]
    };

    // 2. Query account email to filter out self when --all is used
    let my_email: String = conn
        .query_row(
            "SELECT email FROM accounts WHERE public_id = ?1;",
            [&detail.summary.account_id],
            |r| r.get(0),
        )
        .unwrap_or_default();

    let mut cc = Vec::new();
    if args.all {
        for addr in detail.to_addrs.iter().chain(detail.cc_addrs.iter()) {
            let trimmed = addr.trim();
            if !trimmed.is_empty()
                && !trimmed.eq_ignore_ascii_case(&my_email)
                && !to.iter().any(|t| t.eq_ignore_ascii_case(trimmed))
            {
                cc.push(trimmed.to_string());
            }
        }
    }

    // 3. Subject: prefix "Re: " on item's title (not subject_norm)
    let subject = if detail
        .summary
        .title
        .to_ascii_lowercase()
        .starts_with("re: ")
    {
        detail.summary.title.clone()
    } else {
        format!("Re: {}", detail.summary.title)
    };

    // 4. In-Reply-To and References
    let (message_id, references_raw): (Option<String>, Option<String>) = conn
        .query_row(
            "SELECT message_id, references_hdr FROM mail_items
             WHERE item_id = (SELECT id FROM items WHERE public_id = ?1);",
            [&public_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap_or((None, None));

    let mut references = Vec::new();
    if let Some(r_raw) = references_raw {
        for part in r_raw.split_whitespace() {
            references.push(part.to_string());
        }
    }
    if let Some(ref mid) = message_id
        && !references.contains(mid)
    {
        references.push(mid.clone());
    }

    // 5. Quote body
    let mut quote_block = String::new();
    if args.quote {
        let author = if !detail.summary.author.is_empty() {
            &detail.summary.author
        } else {
            detail.from_addr.as_deref().unwrap_or("Unknown")
        };
        let date = if !detail.summary.published_at.is_empty() {
            &detail.summary.published_at
        } else {
            &detail.summary.sort_at
        };

        quote_block.push_str(&format!("On {date}, {author} wrote:\n"));
        if let Some(ref body) = detail.body_text {
            for line in body.lines() {
                quote_block.push_str(&format!("> {line}\n"));
            }
        }
    }

    // 6. User reply body
    let mut user_body = String::new();
    if args.body_stdin {
        io::stdin()
            .read_to_string(&mut user_body)
            .map_err(|e| CliError::Internal(e.to_string()))?;
    }

    let full_body = if !user_body.is_empty() && !quote_block.is_empty() {
        format!("{user_body}\n\n{quote_block}")
    } else if !user_body.is_empty() {
        user_body
    } else {
        quote_block
    };

    let draft = FfiDraft {
        account_id: detail.summary.account_id.clone(),
        to: to.clone(),
        cc: cc.clone(),
        bcc: vec![],
        subject: subject.clone(),
        body: full_body,
        attachment_paths: vec![],
        in_reply_to: message_id,
        references,
    };

    // Forward to compose execution helper
    let compose_args = crate::args::ComposeArgs {
        to,
        cc,
        bcc: vec![],
        subject: Some(subject),
        body_stdin: false,
        attach: vec![],
        draft: None,
        save_draft: args.save_draft,
        now: args.now,
        no_append_sent: false,
        edit: args.edit,
    };

    if args.edit {
        execute_compose(conn, cli, paths, &compose_args).await
    } else if cli.dry_run {
        let rfc_msg = crate::rfc5322::Rfc5322Message {
            from: my_email,
            to: draft.to,
            cc: draft.cc,
            bcc: draft.bcc,
            subject: draft.subject,
            in_reply_to: draft.in_reply_to,
            references: draft.references,
            body: draft.body,
            attachment_paths: vec![],
        };
        let payload = rfc_msg.build_payload()?;
        print!("{payload}");
        Ok(())
    } else if args.save_draft {
        let engine = crate::executor::init_engine(cli, paths).await?;
        let draft_id = engine.save_draft(draft).await?;
        println!("saved draft {draft_id}");
        Ok(())
    } else {
        let engine = crate::executor::init_engine(cli, paths).await?;
        let op_id = engine.send_message(draft, None, true).await?;
        if args.now {
            println!("sent op {op_id}");
        } else {
            println!("queued op {op_id}");
        }
        Ok(())
    }
}
