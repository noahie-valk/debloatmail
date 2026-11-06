//! The `delete` command conforming to `docs/debloatmailctl.md` §8.

use rusqlite::Connection;

use crate::args::{Cli, DeleteArgs};
use crate::config::AppPaths;
use crate::error::CliError;
use crate::executor::{confirm_destructive, init_engine};
use crate::format::format_mutation_result;
use crate::resolve::resolve_item_ids;

pub async fn execute_delete(
    conn: &Connection,
    cli: &Cli,
    paths: &AppPaths,
    args: &DeleteArgs,
) -> Result<(), CliError> {
    let ids = resolve_item_ids(conn, &args.ids)?;
    if ids.is_empty() {
        return format_mutation_result("deleted", 0, cli.format);
    }

    // Delete is mail-only; reject feeds with exit code 2
    for id in &ids {
        let kind: String = conn
            .query_row("SELECT kind FROM items WHERE public_id = ?1;", [id], |r| {
                r.get(0)
            })
            .map_err(|e| CliError::NotFound(format!("item '{id}': {e}")))?;
        if kind == "feed" {
            return Err(CliError::Usage(
                "feed items cannot be deleted; use hide for feed items".to_string(),
            ));
        }
    }

    if args.expunge {
        confirm_destructive(cli, "Permanently expunge mail items from server?")?;
    }

    if cli.dry_run {
        return format_mutation_result("deleted", ids.len() as u32, cli.format);
    }

    let engine = init_engine(cli, paths).await?;
    let affected = engine.delete_items(&ids, args.expunge).await?;

    format_mutation_result("deleted", affected, cli.format)
}
