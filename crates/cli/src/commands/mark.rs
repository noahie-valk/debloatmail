//! The `mark` command conforming to `docs/debloatmailctl.md` §8.

use rusqlite::Connection;

use crate::args::{Cli, MarkArgs, MarkMode};
use crate::config::AppPaths;
use crate::error::CliError;
use crate::executor::init_engine;
use crate::format::format_mutation_result;
use crate::resolve::resolve_item_ids;

pub async fn execute_mark(
    conn: &Connection,
    cli: &Cli,
    paths: &AppPaths,
    args: &MarkArgs,
) -> Result<(), CliError> {
    let ids = resolve_item_ids(conn, &args.ids)?;
    if ids.is_empty() {
        return format_mutation_result("marked", 0, cli.format);
    }

    if cli.dry_run {
        return format_mutation_result("marked", ids.len() as u32, cli.format);
    }

    let engine = init_engine(cli, paths).await?;
    let affected = match args.mode {
        MarkMode::Read | MarkMode::Seen => engine.mark_read(&ids, true).await?,
        MarkMode::Unread | MarkMode::Unseen => engine.mark_read(&ids, false).await?,
        MarkMode::Starred => engine.set_starred(&ids, true).await?,
        MarkMode::Unstarred => engine.set_starred(&ids, false).await?,
    };

    format_mutation_result("marked", affected, cli.format)
}
