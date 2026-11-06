//! Output format dispatching for `debloatmailctl`.

pub mod human;
pub mod json;
pub mod jsonl;
pub mod plain;

use debloatmail_ffi::{FfiAccount, FfiFeed, FfiFolder, FfiItemSummary, FfiPage, FfiThreadSummary};
use serde::Serialize;

use crate::args::{ColorPolicy, OutputFormat};
use crate::error::CliError;

pub fn format_item_page(
    page: &FfiPage<FfiItemSummary>,
    format: OutputFormat,
    color: ColorPolicy,
    print0: bool,
) -> Result<(), CliError> {
    match format {
        OutputFormat::Human => human::print_item_page_human(page, color),
        OutputFormat::Json => json::print_item_page_json(page),
        OutputFormat::Jsonl => jsonl::print_item_page_jsonl(page),
        OutputFormat::Plain => plain::print_item_page_plain(page, print0),
    }
}

pub fn format_thread_page(
    page: &FfiPage<FfiThreadSummary>,
    format: OutputFormat,
    color: ColorPolicy,
) -> Result<(), CliError> {
    match format {
        OutputFormat::Human => human::print_thread_page_human(page, color),
        OutputFormat::Json => json::print_thread_page_json(page),
        OutputFormat::Jsonl => jsonl::print_thread_page_jsonl(page),
        OutputFormat::Plain => plain::print_thread_page_plain(page),
    }
}

pub fn format_folders(
    folders: &[FfiFolder],
    format: OutputFormat,
) -> Result<(), CliError> {
    match format {
        OutputFormat::Human => human::print_folders_human(folders),
        OutputFormat::Json => json::print_folders_json(folders),
        OutputFormat::Jsonl => jsonl::print_folders_jsonl(folders),
        OutputFormat::Plain => plain::print_folders_plain(folders),
    }
}

pub fn format_feeds(
    feeds: &[FfiFeed],
    format: OutputFormat,
) -> Result<(), CliError> {
    match format {
        OutputFormat::Human => human::print_feeds_human(feeds),
        OutputFormat::Json => json::print_feeds_json(feeds),
        OutputFormat::Jsonl => jsonl::print_feeds_jsonl(feeds),
        OutputFormat::Plain => plain::print_feeds_plain(feeds),
    }
}

pub fn format_accounts(
    accounts: &[FfiAccount],
    format: OutputFormat,
) -> Result<(), CliError> {
    match format {
        OutputFormat::Human => human::print_accounts_human(accounts),
        OutputFormat::Json => json::print_accounts_json(accounts),
        OutputFormat::Jsonl => jsonl::print_accounts_jsonl(accounts),
        OutputFormat::Plain => plain::print_accounts_plain(accounts),
    }
}

pub fn format_single<T: Serialize + std::fmt::Display>(
    item: &T,
    format: OutputFormat,
) -> Result<(), CliError> {
    match format {
        OutputFormat::Human | OutputFormat::Plain => {
            println!("{item}");
            Ok(())
        }
        OutputFormat::Json => json::print_single_json(item),
        OutputFormat::Jsonl => jsonl::print_single_jsonl(item),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use debloatmail_ffi::{FfiBodyState, FfiItemKind};

    #[test]
    fn test_json_page_serialization() {
        let summary = FfiItemSummary {
            id: "018f3a2b-7c41-7e2a-9d3f-1a2b3c4d5e6f".into(),
            short_id: "3f2a9c1e".into(),
            kind: FfiItemKind::Mail,
            account_id: "018f3a9c-e4f2-7a1b-8c3d-9f0a1b2c3d4e".into(),
            account_name: "fastmail".into(),
            folder_id: Some("018f3aa1-0000-7000-8000-000000000001".into()),
            folder_name: Some("INBOX".into()),
            title: "Re: release checklist".into(),
            author: "Alice Chen".into(),
            published_at: "2026-10-07T14:22:11Z".into(),
            sort_at: "2026-10-07T14:25:02Z".into(),
            unread: true,
            starred: false,
            has_attachments: true,
            thread_id: Some("019f3a2b-7d52-7e1c-9a4b-2c3d4e5f6a7b".into()),
            body_state: FfiBodyState::Cached,
        };

        let page = FfiPage {
            items: vec![summary],
            next_cursor: Some("eyJ0IjoxNzYwMDEx…".into()),
            has_more: true,
        };

        let env = json::JsonItemPageEnvelope {
            items: &page.items,
            cursor: page.next_cursor.as_deref(),
            has_more: page.has_more,
        };

        let json_str = serde_json::to_string(&env).unwrap();
        assert!(json_str.contains("\"id\":\"018f3a2b-7c41-7e2a-9d3f-1a2b3c4d5e6f\""));
        assert!(json_str.contains("\"short_id\":\"3f2a9c1e\""));
        assert!(json_str.contains("\"account\":\"018f3a9c-e4f2-7a1b-8c3d-9f0a1b2c3d4e\"") || json_str.contains("\"account_id\""));
        assert!(json_str.contains("\"cursor\":\"eyJ0IjoxNzYwMDEx…\""));
        assert!(json_str.contains("\"has_more\":true"));
    }
}
