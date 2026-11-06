//! `debloatmail-cli` library interface.

pub mod args;
pub mod commands;
pub mod config;
pub mod error;
pub mod executor;
pub mod format;
pub mod resolve;
pub mod rfc5322;
pub mod search_parser;

use rusqlite::{Connection, OpenFlags};

use crate::args::{Cli, Commands};
use crate::config::{AppConfig, AppPaths};
use crate::error::CliError;

pub async fn run(cli: Cli) -> Result<(), CliError> {
    let paths = AppPaths::resolve(Some(&cli.config), cli.db.as_deref())?;
    let config = AppConfig::load_merged(&paths.config_paths)?;

    // Commands that do not require a database connection
    match &cli.command {
        Commands::Config(args) => {
            return commands::config_cmd::execute_config(&paths, &config, args);
        }
        Commands::Generate(args) => return commands::generate::execute_generate(args),
        Commands::Version => return commands::generate::execute_version(),
        Commands::Daemon(args) => {
            return commands::daemon::execute_daemon(&cli, &paths, args).await;
        }
        _ => {}
    }

    // Ensure database exists or bootstrap if initial account creation
    if !paths.db_path.exists() {
        match &cli.command {
            Commands::Account(args) => match &args.command {
                crate::args::AccountCommands::Add { .. } => {
                    if cli.no_daemon {
                        let _ = debloatmail_store::schema::open_store(&paths.db_path)?;
                    }
                }
                _ => {
                    return Err(CliError::NotFound(format!(
                        "database file not found at {}",
                        paths.db_path.display()
                    )));
                }
            },
            Commands::Doctor(_) => {
                if cli.no_daemon {
                    let _ = debloatmail_store::schema::open_store(&paths.db_path)?;
                }
            }
            _ => {
                return Err(CliError::NotFound(format!(
                    "database file not found at {}",
                    paths.db_path.display()
                )));
            }
        }
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
        Commands::Folder(args) => commands::folder::execute_folder(&conn, &cli, &paths, args).await,
        Commands::Feed(args) => commands::feed::execute_feed(&conn, &cli, &paths, args).await,
        Commands::Account(args) => {
            commands::account::execute_account(&conn, &cli, &paths, args).await
        }
        Commands::Mark(args) => commands::mark::execute_mark(&conn, &cli, &paths, args).await,
        Commands::Move(args) => commands::move_cmd::execute_move(&conn, &cli, &paths, args).await,
        Commands::Copy(args) => commands::copy_cmd::execute_copy(&conn, &cli, &paths, args).await,
        Commands::Delete(args) => commands::delete::execute_delete(&conn, &cli, &paths, args).await,
        Commands::Hide(args) => commands::hide::execute_hide(&conn, &cli, &paths, args).await,
        Commands::Compose(args) => {
            commands::compose::execute_compose(&conn, &cli, &paths, args).await
        }
        Commands::Reply(args) => commands::reply::execute_reply(&conn, &cli, &paths, args).await,
        Commands::Forward(args) => {
            commands::forward::execute_forward(&conn, &cli, &paths, args).await
        }
        Commands::Attachment(args) => {
            commands::attachment::execute_attachment(&conn, &cli, &paths, args).await
        }
        Commands::Sync(args) => commands::sync_cmd::execute_sync(&conn, &cli, &paths, args).await,
        Commands::Daemon(args) => commands::daemon::execute_daemon(&cli, &paths, args).await,
        Commands::Doctor(args) => {
            commands::doctor::execute_doctor(&conn, &cli, &paths, &config, args).await
        }
        Commands::Config(_) | Commands::Generate(_) | Commands::Version => Ok(()),
    }
}
