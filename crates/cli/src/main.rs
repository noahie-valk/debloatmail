//! `debloatmailctl` — Command-line interface to debloatmail.
//!
//! Conforms strictly to `docs/debloatmailctl.md` and `docs/DATA-MODEL.md`.

pub mod args;
pub mod commands;
pub mod config;
pub mod error;
pub mod format;
pub mod resolve;
pub mod search_parser;

use clap::Parser;
use rusqlite::{Connection, OpenFlags};
use std::process::ExitCode;

use crate::args::{Cli, Commands, OutputFormat};
use crate::config::{AppConfig, AppPaths};
use crate::error::CliError;

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let is_json = cli.format == OutputFormat::Json;

    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            err.render(is_json);
            ExitCode::from(err.exit_code() as u8)
        }
    }
}

async fn run(cli: Cli) -> Result<(), CliError> {
    let paths = AppPaths::resolve(Some(&cli.config), cli.db.as_deref())?;
    let config = AppConfig::load_merged(&paths.config_paths)?;

    // Commands that do not require a database connection
    match &cli.command {
        Commands::Config(args) => return commands::config_cmd::execute_config(&paths, &config, args),
        Commands::Generate(args) => return commands::generate::execute_generate(args),
        Commands::Version => return commands::generate::execute_version(),
        _ => {}
    }

    // Ensure database exists before attempting read-only connection
    if !paths.db_path.exists() {
        return Err(CliError::NotFound(format!(
            "database file not found at {}",
            paths.db_path.display()
        )));
    }

    let conn = Connection::open_with_flags(
        &paths.db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_URI,
    )?;

    match &cli.command {
        Commands::List(args) => commands::list::execute_list(&conn, &cli, args),
        Commands::Read(args) => commands::read::execute_read(&conn, &cli, &paths, args).await,
        Commands::Search(args) => commands::search::execute_search(&conn, &cli, args),
        Commands::Folder(args) => commands::folder::execute_folder(&conn, &cli, args),
        Commands::Feed(args) => commands::feed::execute_feed(&conn, &cli, args),
        Commands::Account(args) => commands::account::execute_account(&conn, &cli, args),
        Commands::Config(_) | Commands::Generate(_) | Commands::Version => Ok(()),

        // Phase 3C mutation and action stubs
        Commands::Mark(_)
        | Commands::Move(_)
        | Commands::Copy(_)
        | Commands::Delete(_)
        | Commands::Hide(_)
        | Commands::Compose(_)
        | Commands::Reply(_)
        | Commands::Forward(_)
        | Commands::Attachment(_)
        | Commands::Sync(_)
        | Commands::Daemon(_)
        | Commands::Doctor(_) => {
            Err(CliError::Usage("this action command is implemented in Phase 3C".to_string()))
        }
    }
}
