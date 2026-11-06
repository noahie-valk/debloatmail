//! Feed inspection and management subcommands conforming to `docs/debloatmailctl.md` §8.

use debloatmail_ffi::reader::list_feeds_fn;
use rusqlite::Connection;

use crate::args::{Cli, FeedArgs, FeedCommands, OutputFormat};
use crate::error::CliError;
use crate::format::format_feeds;
use crate::format::json::print_single_json;
use crate::resolve::{resolve_account, resolve_feed};

pub fn execute_feed(
    conn: &Connection,
    cli: &Cli,
    args: &FeedArgs,
) -> Result<(), CliError> {
    match &args.command {
        FeedCommands::List => {
            let acc_id = match cli.account.as_deref() {
                Some(acc_arg) => Some(resolve_account(conn, acc_arg)?.id),
                None => None,
            };
            let feeds = list_feeds_fn(conn, acc_id.as_deref())?;
            format_feeds(&feeds, cli.format)
        }
        FeedCommands::Show { name_or_id } => {
            let acc_id = match cli.account.as_deref() {
                Some(acc_arg) => Some(resolve_account(conn, acc_arg)?.id),
                None => None,
            };
            let feed = resolve_feed(conn, acc_id.as_deref(), name_or_id)?;

            match cli.format {
                OutputFormat::Json | OutputFormat::Jsonl => print_single_json(&feed)?,
                OutputFormat::Plain => {
                    println!(
                        "{}\t{}\t{}\t{}\t{}\t{}",
                        feed.id,
                        feed.name,
                        feed.url,
                        feed.interval_secs,
                        feed.last_fetch_at.as_deref().unwrap_or("-"),
                        feed.unread_count
                    );
                }
                OutputFormat::Human => {
                    println!("ID:          {}", feed.id);
                    println!("Title:       {}", feed.title.as_deref().unwrap_or(&feed.name));
                    println!("URL:         {}", feed.url);
                    println!(
                        "Interval:    {}",
                        if feed.interval_secs == 0 {
                            "auto".to_string()
                        } else {
                            format!("{}s", feed.interval_secs)
                        }
                    );
                    println!(
                        "Last Fetch:  {}",
                        feed.last_fetch_at.as_deref().unwrap_or("never")
                    );
                    println!(
                        "Next Fetch:  {}",
                        feed.next_fetch_at.as_deref().unwrap_or("pending")
                    );
                    println!("Unread:      {}", feed.unread_count);
                }
            }
            Ok(())
        }
        FeedCommands::Add { .. }
        | FeedCommands::Update { .. }
        | FeedCommands::SetUrl { .. }
        | FeedCommands::Remove { .. }
        | FeedCommands::Refresh { .. } => {
            Err(CliError::Usage("feed mutating actions are implemented in Phase 3C".into()))
        }
    }
}
