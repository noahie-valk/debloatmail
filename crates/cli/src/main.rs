//! `debloatmailctl` binary entrypoint.

use clap::Parser;
use debloatmail_cli::args::{Cli, OutputFormat};
use debloatmail_cli::run;
use std::process::ExitCode;

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
