//! Plain TSV and NUL-separated format emitter conforming to `docs/debloatmailctl.md` §6.

use debloatmail_ffi::{FfiAccount, FfiFeed, FfiFolder, FfiItemSummary, FfiPage, FfiThreadSummary};
use std::io::{self, Write};

use crate::error::CliError;

pub fn print_item_page_plain(page: &FfiPage<FfiItemSummary>, print0: bool) -> Result<(), CliError> {
    let mut stdout = io::stdout();
    if print0 {
        for item in &page.items {
            stdout.write_all(item.id.as_bytes())?;
            stdout.write_all(b"\0")?;
        }
        stdout.flush()?;
    } else {
        for item in &page.items {
            let pub_date = if item.published_at.is_empty() {
                &item.sort_at
            } else {
                &item.published_at
            };
            println!(
                "{}\t{}\t{}\t{}",
                item.short_id, pub_date, item.author, item.title
            );
        }
    }
    Ok(())
}

pub fn print_thread_page_plain(page: &FfiPage<FfiThreadSummary>) -> Result<(), CliError> {
    for thread in &page.items {
        println!(
            "{}\t{}\t{}\t{}",
            thread.id, thread.last_activity, thread.unread_count, thread.display_subject
        );
    }
    Ok(())
}

pub fn print_folders_plain(folders: &[FfiFolder]) -> Result<(), CliError> {
    for f in folders {
        println!(
            "{}\t{}\t{}\t{}\t{}",
            f.id,
            f.role.as_deref().unwrap_or("-"),
            f.name,
            f.item_count,
            f.unread_count
        );
    }
    Ok(())
}

pub fn print_feeds_plain(feeds: &[FfiFeed]) -> Result<(), CliError> {
    for f in feeds {
        println!("{}\t{}\t{}\t{}", f.id, f.name, f.url, f.unread_count);
    }
    Ok(())
}

pub fn print_accounts_plain(accounts: &[FfiAccount]) -> Result<(), CliError> {
    for a in accounts {
        println!(
            "{}\t{}\t{}\t{}\t{}",
            a.id, a.name, a.kind, a.email, a.enabled
        );
    }
    Ok(())
}
