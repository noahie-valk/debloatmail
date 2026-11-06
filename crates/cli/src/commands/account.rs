//! Account inspection and configuration subcommands conforming to `docs/debloatmailctl.md` §8.

use debloatmail_ffi::reader::list_accounts_fn;
use rusqlite::Connection;

use crate::args::{AccountArgs, AccountCommands, Cli, OutputFormat};
use crate::error::CliError;
use crate::format::format_accounts;
use crate::format::json::print_single_json;
use crate::resolve::resolve_account;

pub fn execute_account(conn: &Connection, cli: &Cli, args: &AccountArgs) -> Result<(), CliError> {
    match &args.command {
        AccountCommands::List { json } => {
            let accounts = list_accounts_fn(conn)?;
            let format = if *json {
                OutputFormat::Json
            } else {
                cli.format
            };
            format_accounts(&accounts, format)
        }
        AccountCommands::Show { name_or_id } => {
            let account = resolve_account(conn, name_or_id)?;

            match cli.format {
                OutputFormat::Json | OutputFormat::Jsonl => print_single_json(&account)?,
                OutputFormat::Plain => {
                    println!(
                        "{}\t{}\t{}\t{}\t{}\t{}",
                        account.id,
                        account.name,
                        account.kind,
                        account.email,
                        account.enabled,
                        account.unread_count
                    );
                }
                OutputFormat::Human => {
                    println!("ID:          {}", account.id);
                    println!("Name:        {}", account.name);
                    println!("Kind:        {}", account.kind);
                    println!("Email:       {}", account.email);
                    println!(
                        "Enabled:     {}",
                        if account.enabled { "yes" } else { "no" }
                    );
                    println!(
                        "Provider:    {}",
                        account.provider_preset.as_deref().unwrap_or("generic")
                    );
                    println!("Credentials: ••• (in keyring)");
                    println!(
                        "Last Sync:   {}",
                        account.last_sync_at.as_deref().unwrap_or("never")
                    );
                    println!("Unread:      {}", account.unread_count);
                }
            }
            Ok(())
        }
        AccountCommands::Add { .. }
        | AccountCommands::Update { .. }
        | AccountCommands::Remove { .. }
        | AccountCommands::Check { .. } => Err(CliError::Usage(
            "account mutating and diagnostic actions are implemented in Phase 3C".into(),
        )),
    }
}
