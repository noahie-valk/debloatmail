//! Engine executor and confirmation guards for CLI write/mutation operations.

use debloatmail_ffi::{DbusEngine, Engine, InProcessEngine};
use std::io::{self, BufRead, Write};

use crate::args::Cli;
use crate::config::AppPaths;
use crate::error::CliError;

/// Initializes the Qt-free engine instance based on `--no-daemon` and paths.
pub async fn init_engine(cli: &Cli, paths: &AppPaths) -> Result<Box<dyn Engine>, CliError> {
    if cli.no_daemon {
        let in_proc = InProcessEngine::open_standalone(
            paths.db_path.clone(),
            paths.blobs_dir.clone(),
            Some(paths.lock_path.clone()),
        )?;
        Ok(Box::new(in_proc))
    } else {
        let dbus_eng = DbusEngine::connect(&paths.db_path, paths.blobs_dir.clone()).await?;
        Ok(Box::new(dbus_eng))
    }
}

/// Enforces destructive confirmation guards per `SECURITY.md` §7.3 and `docs/debloatmailctl.md` §8.
///
/// If stdin is not a TTY, `--yes` / `-y` is strictly required.
/// If stdin is a TTY, prompts interactively.
pub fn confirm_destructive(cli: &Cli, prompt: &str) -> Result<(), CliError> {
    if cli.yes {
        return Ok(());
    }

    let is_tty = unsafe { libc::isatty(libc::STDIN_FILENO) == 1 };
    if !is_tty {
        return Err(CliError::Usage(
            "destructive command requires -y when stdin is not a TTY".to_string(),
        ));
    }

    eprint!("{prompt} [y/N]: ");
    io::stderr()
        .flush()
        .map_err(|e| CliError::Internal(e.to_string()))?;

    let mut line = String::new();
    let stdin = io::stdin();
    stdin
        .lock()
        .read_line(&mut line)
        .map_err(|e| CliError::Internal(e.to_string()))?;

    let trimmed = line.trim();
    if trimmed.eq_ignore_ascii_case("y") || trimmed.eq_ignore_ascii_case("yes") {
        Ok(())
    } else {
        Err(CliError::Usage("operation aborted by user".to_string()))
    }
}

/// Asserts that network operations are allowed (i.e. `--offline` is not active).
pub fn ensure_online(cli: &Cli) -> Result<(), CliError> {
    if cli.offline {
        Err(CliError::Offline(
            "command requires network connectivity and --offline is active".to_string(),
        ))
    } else {
        Ok(())
    }
}
