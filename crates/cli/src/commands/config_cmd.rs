//! Config inspection and manipulation subcommands conforming to `docs/debloatmailctl.md` §8.

use crate::args::{ConfigArgs, ConfigCommands};
use crate::config::{AppConfig, AppPaths};
use crate::error::CliError;

pub fn execute_config(
    paths: &AppPaths,
    config: &AppConfig,
    args: &ConfigArgs,
) -> Result<(), CliError> {
    match &args.command {
        ConfigCommands::Path => {
            for p in &paths.config_paths {
                println!("{}", p.display());
            }
            Ok(())
        }
        ConfigCommands::Show => {
            let s = toml::to_string_pretty(&config.raw)?;
            print!("{s}");
            Ok(())
        }
        ConfigCommands::Validate => {
            // If we got here, config parsed cleanly in load_merged
            println!("Configuration syntax is valid.");
            Ok(())
        }
        ConfigCommands::Get { key } => {
            if let Some(val) = config.get(key) {
                println!("{val}");
            }
            Ok(())
        }
        ConfigCommands::Set { key, value } => {
            let primary = paths
                .config_paths
                .first()
                .ok_or_else(|| CliError::Internal("no configuration path configured".into()))?;
            AppConfig::set_and_save(primary, key, value)?;
            println!("Set {} = {}", key, value);
            Ok(())
        }
    }
}
