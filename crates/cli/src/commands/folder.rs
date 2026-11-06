//! Mail folder subcommands conforming to `docs/debloatmailctl.md` §8.

use debloatmail_ffi::reader::list_folders_fn;
use rusqlite::Connection;

use crate::args::{Cli, FolderArgs, FolderCommands};
use crate::error::CliError;
use crate::format::format_folders;
use crate::resolve::resolve_account;

pub fn execute_folder(
    conn: &Connection,
    cli: &Cli,
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
        FolderCommands::Sync { .. } => {
            Err(CliError::Usage("folder sync is a write mutation and is implemented in Phase 3C".into()))
        }
    }
}
