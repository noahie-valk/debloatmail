//! The plaintext `read` command conforming to `docs/debloatmailctl.md` §8 and §10.

use debloatmail_ffi::reader::get_item_fn;
use debloatmail_ffi::{
    DbusEngine, Engine, FfiBodyState, FfiItemDetail, FfiItemKind, InProcessEngine,
};
use rusqlite::Connection;
use std::fs;
use std::io::{self, Write};
use std::path::Path;

use crate::args::{Cli, ReadArgs};
use crate::config::AppPaths;
use crate::error::CliError;
use crate::resolve::resolve_item_id;

pub async fn execute_read(
    conn: &Connection,
    cli: &Cli,
    paths: &AppPaths,
    args: &ReadArgs,
) -> Result<(), CliError> {
    let public_id = resolve_item_id(conn, &args.id)?;

    // If reading whole conversation thread
    if args.thread {
        let mut thread_stmt = conn.prepare(
            "SELECT public_id FROM visible_items
             WHERE thread_id = (SELECT thread_id FROM items WHERE public_id = ?1)
             ORDER BY sort_at ASC, id ASC;",
        )?;
        let thread_ids = thread_stmt
            .query_map([&public_id], |r| r.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;

        let ids_to_read = if thread_ids.is_empty() {
            vec![public_id]
        } else {
            thread_ids
        };

        for (idx, tid) in ids_to_read.iter().enumerate() {
            if idx > 0 {
                println!("\n{}\n", "=".repeat(60));
            }
            read_single_item(
                conn,
                cli,
                paths,
                tid,
                args.headers,
                args.raw,
                args.mark_read,
            )
            .await?;
        }
        return Ok(());
    }

    read_single_item(
        conn,
        cli,
        paths,
        &public_id,
        args.headers,
        args.raw,
        args.mark_read,
    )
    .await
}

async fn read_single_item(
    conn: &Connection,
    cli: &Cli,
    paths: &AppPaths,
    public_id: &str,
    show_headers: bool,
    show_raw: bool,
    mark_read: bool,
) -> Result<(), CliError> {
    let mut detail = get_item_fn(conn, &paths.blobs_dir, public_id)?;

    if show_raw && detail.summary.kind == FfiItemKind::Feed {
        return Err(CliError::Usage("--raw is mail-only".to_string()));
    }

    // Check if body is cached
    let needs_fetch = if show_raw {
        detail.raw_blob_path.as_ref().is_none_or(|p| !p.exists())
    } else {
        detail.summary.body_state != FfiBodyState::Cached
    };

    if needs_fetch {
        if cli.offline {
            return Err(CliError::Offline(
                "body not cached and --offline specified".to_string(),
            ));
        }

        let engine = init_engine(cli, &paths.db_path, &paths.blobs_dir, &paths.lock_path).await?;
        engine.fetch_body(public_id).await?;

        // Re-read from SQLite
        detail = get_item_fn(conn, &paths.blobs_dir, public_id)?;
    }

    if mark_read && !cli.dry_run {
        let engine = init_engine(cli, &paths.db_path, &paths.blobs_dir, &paths.lock_path).await?;
        let _ = engine.mark_read(&[public_id.to_string()], true).await;
    }

    if show_raw {
        if let Some(ref path) = detail.raw_blob_path
            && path.exists()
        {
            let bytes = fs::read(path)?;
            io::stdout().write_all(&bytes)?;
            return Ok(());
        }
        return Err(CliError::NotFound(
            "raw blob not found in cache".to_string(),
        ));
    }

    if show_headers {
        print_headers(&detail);
    }

    if let Some(ref body) = detail.body_text {
        print!("{body}");
        if !body.ends_with('\n') {
            println!();
        }
    }

    Ok(())
}

fn print_headers(detail: &FfiItemDetail) {
    if let Some(ref from) = detail.from_addr {
        println!("From: {from}");
    } else if !detail.summary.author.is_empty() {
        println!("Author: {}", detail.summary.author);
    }

    if !detail.to_addrs.is_empty() {
        println!("To: {}", detail.to_addrs.join(", "));
    }
    if !detail.cc_addrs.is_empty() {
        println!("Cc: {}", detail.cc_addrs.join(", "));
    }
    if let Some(ref reply_to) = detail.reply_to {
        println!("Reply-To: {reply_to}");
    }

    let date = if !detail.summary.published_at.is_empty() {
        &detail.summary.published_at
    } else {
        &detail.summary.sort_at
    };
    println!("Date: {date}");
    println!("Subject: {}", detail.summary.title);

    if !detail.attachments.is_empty() {
        let att_names: Vec<String> = detail
            .attachments
            .iter()
            .map(|a| {
                let name = a.filename.as_deref().unwrap_or("unnamed");
                let size_str = a
                    .size_bytes
                    .map(|s| format!(" ({} bytes)", s))
                    .unwrap_or_default();
                format!("{name}{size_str}")
            })
            .collect();
        println!("Attachments: {}", att_names.join(", "));
    }
    println!();
}

async fn init_engine(
    cli: &Cli,
    db_path: &Path,
    blobs_dir: &Path,
    lock_path: &Path,
) -> Result<Box<dyn Engine>, CliError> {
    if cli.no_daemon {
        let in_proc = InProcessEngine::open_standalone(
            db_path.to_path_buf(),
            blobs_dir.to_path_buf(),
            Some(lock_path.to_path_buf()),
        )?;
        Ok(Box::new(in_proc))
    } else {
        let dbus_eng = DbusEngine::connect(db_path, blobs_dir.to_path_buf()).await?;
        Ok(Box::new(dbus_eng))
    }
}
