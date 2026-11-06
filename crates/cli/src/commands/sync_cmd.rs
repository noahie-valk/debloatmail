//! The `sync` command conforming to `docs/debloatmailctl.md` §8.

use rusqlite::Connection;

use crate::args::{Cli, OutputFormat, SyncArgs};
use crate::config::AppPaths;
use crate::error::CliError;
use crate::executor::{ensure_online, init_engine};
use crate::resolve::{resolve_account, resolve_feed, resolve_folder};

pub async fn execute_sync(
    conn: &Connection,
    cli: &Cli,
    paths: &AppPaths,
    args: &SyncArgs,
) -> Result<(), CliError> {
    ensure_online(cli)?;

    let account_id = match args.account.as_deref().or(cli.account.as_deref()) {
        Some(a) => Some(resolve_account(conn, a)?.id),
        None => None,
    };

    let folder_id = match args.folder.as_deref() {
        Some(f) => Some(resolve_folder(conn, account_id.as_deref(), f)?.id),
        None => None,
    };

    let feed_id = match args.feed.as_deref() {
        Some(fd) => Some(resolve_feed(conn, account_id.as_deref(), fd)?.id),
        None => None,
    };

    if cli.dry_run {
        println!(
            "would trigger sync (account: {:?}, folder: {:?}, feed: {:?}, full: {})",
            account_id, folder_id, feed_id, args.full
        );
        return Ok(());
    }

    let engine = init_engine(cli, paths).await?;
    let job_id = engine
        .trigger_sync(account_id, folder_id, feed_id, args.full)
        .await?;

    match cli.format {
        OutputFormat::Human => {
            if args.wait {
                println!("synced");
            } else {
                println!("sync job {job_id}");
            }
        }
        OutputFormat::Json | OutputFormat::Jsonl => {
            println!(
                "{}",
                serde_json::json!({
                    "job_id": job_id,
                    "status": if args.wait { "synced" } else { "pending" }
                })
            );
        }
        OutputFormat::Plain => {
            println!("{job_id}\t{}", if args.wait { "synced" } else { "pending" });
        }
    }

    Ok(())
}
