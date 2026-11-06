//! JSON Lines format emitter conforming to `docs/debloatmailctl.md` §6.

use debloatmail_ffi::{FfiAccount, FfiFeed, FfiFolder, FfiItemSummary, FfiPage, FfiThreadSummary};
use serde::Serialize;

use crate::error::CliError;

pub fn print_item_page_jsonl(page: &FfiPage<FfiItemSummary>) -> Result<(), CliError> {
    for item in &page.items {
        let s = serde_json::to_string(item)?;
        println!("{s}");
    }
    Ok(())
}

pub fn print_thread_page_jsonl(page: &FfiPage<FfiThreadSummary>) -> Result<(), CliError> {
    for thread in &page.items {
        let s = serde_json::to_string(thread)?;
        println!("{s}");
    }
    Ok(())
}

pub fn print_folders_jsonl(folders: &[FfiFolder]) -> Result<(), CliError> {
    for f in folders {
        let s = serde_json::to_string(f)?;
        println!("{s}");
    }
    Ok(())
}

pub fn print_feeds_jsonl(feeds: &[FfiFeed]) -> Result<(), CliError> {
    for f in feeds {
        let s = serde_json::to_string(f)?;
        println!("{s}");
    }
    Ok(())
}

pub fn print_accounts_jsonl(accounts: &[FfiAccount]) -> Result<(), CliError> {
    for a in accounts {
        let s = serde_json::to_string(a)?;
        println!("{s}");
    }
    Ok(())
}

pub fn print_single_jsonl<T: Serialize>(val: &T) -> Result<(), CliError> {
    let s = serde_json::to_string(val)?;
    println!("{s}");
    Ok(())
}
