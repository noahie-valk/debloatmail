//! JSON format emitter conforming to `docs/debloatmailctl.md` §6.

use debloatmail_ffi::{FfiAccount, FfiFeed, FfiFolder, FfiItemSummary, FfiPage, FfiThreadSummary};
use serde::Serialize;

use crate::error::CliError;

#[derive(Serialize)]
pub struct JsonItemPageEnvelope<'a> {
    pub items: &'a [FfiItemSummary],
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<&'a str>,
    pub has_more: bool,
}

#[derive(Serialize)]
pub struct JsonThreadPageEnvelope<'a> {
    pub threads: &'a [FfiThreadSummary],
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<&'a str>,
    pub has_more: bool,
}

pub fn print_item_page_json(page: &FfiPage<FfiItemSummary>) -> Result<(), CliError> {
    let env = JsonItemPageEnvelope {
        items: &page.items,
        cursor: page.next_cursor.as_deref(),
        has_more: page.has_more,
    };
    let s = serde_json::to_string_pretty(&env)?;
    println!("{s}");
    Ok(())
}

pub fn print_thread_page_json(page: &FfiPage<FfiThreadSummary>) -> Result<(), CliError> {
    let env = JsonThreadPageEnvelope {
        threads: &page.items,
        cursor: page.next_cursor.as_deref(),
        has_more: page.has_more,
    };
    let s = serde_json::to_string_pretty(&env)?;
    println!("{s}");
    Ok(())
}

pub fn print_folders_json(folders: &[FfiFolder]) -> Result<(), CliError> {
    let s = serde_json::to_string_pretty(folders)?;
    println!("{s}");
    Ok(())
}

pub fn print_feeds_json(feeds: &[FfiFeed]) -> Result<(), CliError> {
    let s = serde_json::to_string_pretty(feeds)?;
    println!("{s}");
    Ok(())
}

pub fn print_accounts_json(accounts: &[FfiAccount]) -> Result<(), CliError> {
    let s = serde_json::to_string_pretty(accounts)?;
    println!("{s}");
    Ok(())
}

pub fn print_single_json<T: Serialize>(val: &T) -> Result<(), CliError> {
    let s = serde_json::to_string_pretty(val)?;
    println!("{s}");
    Ok(())
}
