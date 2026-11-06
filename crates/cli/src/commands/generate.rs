//! Generation of shell completions, man pages, and schema dump conforming to `docs/debloatmailctl.md` §8.

use clap::CommandFactory;
use std::fs;
use std::io::{self, Write};

use crate::args::{Cli, GenerateArgs, GenerateCommands};
use crate::error::CliError;

const SCHEMA_DDL: &str = include_str!("../../../store/src/migrations/0001_init.sql");

pub fn execute_generate(args: &GenerateArgs) -> Result<(), CliError> {
    match &args.command {
        GenerateCommands::Completions { shell, dir } => {
            let mut cmd = Cli::command();
            let bin_name = "debloatmailctl";

            if let Some(out_dir) = dir {
                fs::create_dir_all(out_dir)?;
                clap_complete::generate_to(*shell, &mut cmd, bin_name, out_dir)?;
                println!(
                    "Generated {:?} completion to {}",
                    shell,
                    out_dir.display()
                );
            } else {
                clap_complete::generate(*shell, &mut cmd, bin_name, &mut io::stdout());
            }
            Ok(())
        }
        GenerateCommands::Man { dir } => {
            let cmd = Cli::command();
            let man = clap_mangen::Man::new(cmd);

            if let Some(out_dir) = dir {
                fs::create_dir_all(out_dir)?;
                let file_path = out_dir.join("debloatmailctl.1");
                let mut f = fs::File::create(&file_path)?;
                man.render(&mut f)?;
                println!("Generated manual page to {}", file_path.display());
            } else {
                man.render(&mut io::stdout())?;
            }
            Ok(())
        }
        GenerateCommands::Schema { dir } => {
            if let Some(out_dir) = dir {
                fs::create_dir_all(out_dir)?;
                let file_path = out_dir.join("0001_init.sql");
                fs::write(&file_path, SCHEMA_DDL)?;
                println!("Generated schema DDL to {}", file_path.display());
            } else {
                io::stdout().write_all(SCHEMA_DDL.as_bytes())?;
            }
            Ok(())
        }
    }
}

pub fn execute_version() -> Result<(), CliError> {
    println!("debloatmailctl {}", env!("CARGO_PKG_VERSION"));
    Ok(())
}
