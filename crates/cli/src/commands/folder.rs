//! Mail folder subcommands conforming to `docs/debloatmailctl.md` §8.

use debloatmail_ffi::reader::list_folders_fn;
use rusqlite::Connection;

use crate::args::{Cli, FolderArgs, FolderCommands, OutputFormat};
use crate::config::AppPaths;
use crate::error::CliError;
use crate::executor::{ensure_online, init_engine};
use crate::format::format_folders;
use crate::resolve::{resolve_account, resolve_folder};

pub async fn execute_folder(
    conn: &Connection,
    cli: &Cli,
    paths: &AppPaths,
    args: &FolderArgs,
) -> Result<(), CliError> {
    match &args.command {
        FolderCommands::List { account } => {
            let acc_id = match account.as_deref().or(cli.account.as_deref()) {
                Some(acc_arg) => Some(resolve_account(conn, acc_arg)?.id),
                None => None,
            };
            let folders = list_folders_fn(conn, acc_id.as_deref())?;
            format_folders(&folders, cli.format)
        }
        FolderCommands::Stats { account } => {
            let acc_id = match account.as_deref().or(cli.account.as_deref()) {
                Some(acc_arg) => Some(resolve_account(conn, acc_arg)?.id),
                None => None,
            };
            let folders = list_folders_fn(conn, acc_id.as_deref())?;
            format_folders(&folders, cli.format)
        }
        FolderCommands::Sync { name, full, wait } => {
            ensure_online(cli)?;

            let acc_id = match cli.account.as_deref() {
                Some(acc_arg) => Some(resolve_account(conn, acc_arg)?.id),
                None => None,
            };
            let folder = resolve_folder(conn, acc_id.as_deref(), name)?;

            if cli.dry_run {
                println!("would sync folder '{}'", folder.name);
                return Ok(());
            }

            let engine = init_engine(cli, paths).await?;
            let job_id = engine
                .trigger_sync(Some(folder.account_id), Some(folder.id), None, *full)
                .await?;

            match cli.format {
                OutputFormat::Human => {
                    if *wait {
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
                            "folder": folder.name,
                            "status": if *wait { "synced" } else { "pending" }
                        })
                    );
                }
                OutputFormat::Plain => {
                    println!("{job_id}\t{}", if *wait { "synced" } else { "pending" });
                }
            }

            Ok(())
        }
    }
}
