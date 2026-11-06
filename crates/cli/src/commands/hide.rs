//! The `hide` command conforming to `docs/debloatmailctl.md` §8.

use rusqlite::Connection;

use crate::args::{Cli, HideArgs};
use crate::config::AppPaths;
use crate::error::CliError;
use crate::executor::{confirm_destructive, init_engine};
use crate::format::format_mutation_result;
use crate::resolve::resolve_item_ids;

pub async fn execute_hide(
    conn: &Connection,
    cli: &Cli,
    paths: &AppPaths,
    args: &HideArgs,
) -> Result<(), CliError> {
    let ids = resolve_item_ids(conn, &args.ids)?;
    if ids.is_empty() {
        return format_mutation_result("hidden", 0, cli.format);
    }

    // Hide is feed-only; reject mail with exit code 2
    for id in &ids {
        let kind: String = conn
            .query_row("SELECT kind FROM items WHERE public_id = ?1;", [id], |r| {
                r.get(0)
            })
            .map_err(|e| CliError::NotFound(format!("item '{id}': {e}")))?;
        if kind == "mail" {
            return Err(CliError::Usage(
                "mail items cannot be hidden; hide is feed-only (use delete)".to_string(),
            ));
        }
    }

    // Hidden items cannot be unhidden; confirmation required
    confirm_destructive(cli, "Permanently hide feed items from all streams?")?;

    if cli.dry_run {
        return format_mutation_result("hidden", ids.len() as u32, cli.format);
    }

    let engine = init_engine(cli, paths).await?;
    let affected = engine.hide_items(&ids).await?;

    format_mutation_result("hidden", affected, cli.format)
}
