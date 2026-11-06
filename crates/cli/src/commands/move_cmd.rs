//! The `move` command conforming to `docs/debloatmailctl.md` §8.

use rusqlite::Connection;

use crate::args::{Cli, MoveArgs};
use crate::config::AppPaths;
use crate::error::CliError;
use crate::executor::init_engine;
use crate::format::format_mutation_result;
use crate::resolve::{resolve_account, resolve_folder, resolve_item_ids};

pub async fn execute_move(
    conn: &Connection,
    cli: &Cli,
    paths: &AppPaths,
    args: &MoveArgs,
) -> Result<(), CliError> {
    let ids = resolve_item_ids(conn, &args.ids)?;
    if ids.is_empty() {
        return format_mutation_result("moved", 0, cli.format);
    }

    // Move is mail-only; reject feeds with exit code 2
    for id in &ids {
        let kind: String = conn
            .query_row("SELECT kind FROM items WHERE public_id = ?1;", [id], |r| {
                r.get(0)
            })
            .map_err(|e| CliError::NotFound(format!("item '{id}': {e}")))?;
        if kind == "feed" {
            return Err(CliError::Usage(
                "feed items cannot be moved; move is mail-only".to_string(),
            ));
        }
    }

    let acc_id = match cli.account.as_deref() {
        Some(acc) => Some(resolve_account(conn, acc)?.id),
        None => None,
    };
    let folder = resolve_folder(conn, acc_id.as_deref(), &args.to)?;

    if cli.dry_run {
        return format_mutation_result("moved", ids.len() as u32, cli.format);
    }

    let engine = init_engine(cli, paths).await?;
    let affected = engine.move_items(&ids, &folder.id).await?;

    format_mutation_result("moved", affected, cli.format)
}
