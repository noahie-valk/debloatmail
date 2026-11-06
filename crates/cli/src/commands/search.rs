//! Full-text and filter search command conforming to `docs/debloatmailctl.md` §8 and §9.

use debloatmail_ffi::FfiPage;
use debloatmail_ffi::reader::item_to_summary;
use debloatmail_store::headers::row_to_item;
use rusqlite::Connection;

use crate::args::{Cli, SearchArgs};
use crate::error::CliError;
use crate::format::format_item_page;
use crate::search_parser::{CompiledSearch, compile_search_query};

pub fn execute_search(conn: &Connection, cli: &Cli, args: &SearchArgs) -> Result<(), CliError> {
    let compiled = compile_search_query(&args.query)?;

    let limit = if args.list.all {
        10_000
    } else {
        args.list.limit.unwrap_or(25)
    };

    let sql = build_search_sql(&compiled, limit);

    if args.explain {
        println!("FTS5 Expression:");
        match &compiled.fts_query {
            Some(fts) => println!("  {fts}"),
            None => println!("  (none)"),
        }
        println!("\nSQL Query:");
        println!("{sql}");
        return Ok(());
    }

    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        row_to_item(row).map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
    })?;

    let mut items = Vec::new();
    for r in rows {
        items.push(r?);
    }

    let mut summaries = Vec::with_capacity(items.len());
    for it in items {
        summaries.push(item_to_summary(conn, it)?);
    }

    let page = FfiPage {
        items: summaries,
        next_cursor: None,
        has_more: false,
    };

    format_item_page(&page, cli.format, cli.color, args.list.print0)
}

fn build_search_sql(compiled: &CompiledSearch, limit: u32) -> String {
    let mut sql = String::from(
        "SELECT v.*, a.public_id AS account_public_id, t.public_id AS thread_public_id\n\
         FROM visible_items v\n\
         JOIN accounts a ON a.id = v.account_id\n\
         LEFT JOIN threads t ON t.id = v.thread_id\n",
    );

    if let Some(ref fts) = compiled.fts_query {
        let safe_fts = fts.replace('\'', "''");
        sql.push_str(&format!(
            "JOIN item_text_fts ON item_text_fts.rowid = v.id AND item_text_fts MATCH '{safe_fts}'\n"
        ));
    }

    sql.push_str("WHERE 1=1\n");

    for pred in &compiled.sql_predicates {
        sql.push_str(&format!("  AND {pred}\n"));
    }

    sql.push_str(&format!(
        "ORDER BY v.sort_at DESC, v.id DESC\nLIMIT {limit};"
    ));

    sql
}
