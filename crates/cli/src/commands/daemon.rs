//! Daemon lifecycle and status commands conforming to `docs/debloatmailctl.md` §8.

use debloatmail_ffi::FfiError;

use crate::args::{Cli, DaemonArgs, DaemonCommands, OutputFormat};
use crate::config::AppPaths;
use crate::error::CliError;
use crate::executor::init_engine;

pub async fn execute_daemon(
    cli: &Cli,
    paths: &AppPaths,
    args: &DaemonArgs,
) -> Result<(), CliError> {
    match &args.command {
        DaemonCommands::Status { json } => {
            let format = if *json {
                OutputFormat::Json
            } else {
                cli.format
            };

            let daemon_status = match init_engine(cli, paths).await {
                Ok(engine) => match engine.get_daemon_status().await {
                    Ok(st) => Some(st),
                    Err(FfiError::DaemonUnavailable(_)) => None,
                    Err(e) => return Err(CliError::from(e)),
                },
                Err(CliError::DaemonUnavailable(_)) => None,
                Err(e) => return Err(e),
            };

            match daemon_status {
                Some(st) if st.running => match format {
                    OutputFormat::Human => {
                        println!("Daemon:       running");
                        println!(
                            "Version:      {} (API v{})",
                            st.daemon_version, st.api_version
                        );
                        println!("State:        {}", st.state);
                        println!("Unread Items: {}", st.unread_count);
                        println!(
                            "Last Sync:    {}",
                            st.last_sync_at.as_deref().unwrap_or("never")
                        );
                        println!("Pending Ops:  {}", st.pending_ops_count);
                    }
                    OutputFormat::Json | OutputFormat::Jsonl => {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&serde_json::json!({
                                "daemon": {
                                    "running": true,
                                    "version": st.daemon_version,
                                    "api_version": st.api_version,
                                    "state": st.state,
                                    "unread_items": st.unread_count,
                                    "last_sync_at": st.last_sync_at,
                                    "pending_ops": st.pending_ops_count,
                                }
                            }))
                            .map_err(|e| CliError::Internal(e.to_string()))?
                        );
                    }
                    OutputFormat::Plain => {
                        println!("running\t{}\t{}", st.daemon_version, st.state);
                    }
                },
                _ => {
                    // Daemon is stopped: per docs/debloatmailctl.md §8, this exits 0
                    match format {
                        OutputFormat::Human => println!("Daemon: stopped"),
                        OutputFormat::Json => {
                            println!(
                                "{}",
                                serde_json::to_string_pretty(&serde_json::json!({
                                    "daemon": {
                                        "running": false
                                    }
                                }))
                                .map_err(|e| CliError::Internal(e.to_string()))?
                            );
                        }
                        OutputFormat::Jsonl => {
                            println!(
                                "{}",
                                serde_json::to_string(&serde_json::json!({
                                    "daemon": {
                                        "running": false
                                    }
                                }))
                                .map_err(|e| CliError::Internal(e.to_string()))?
                            );
                        }
                        OutputFormat::Plain => println!("stopped"),
                    }
                }
            }

            Ok(())
        }
        DaemonCommands::Stop => {
            let status = std::process::Command::new("systemctl")
                .args(["--user", "stop", "debloatmaild.service"])
                .status();

            match status {
                Ok(s) if s.success() => {
                    println!("stopped debloatmaild.service");
                    Ok(())
                }
                _ => Err(CliError::DaemonUnavailable(
                    "failed to stop debloatmaild.service via systemctl --user; ensure systemd is running or stop the process manually".into(),
                )),
            }
        }
        DaemonCommands::Reload => {
            let engine = init_engine(cli, paths).await?;
            engine.reload_accounts().await?;
            println!("accounts reloaded");
            Ok(())
        }
        DaemonCommands::Logs { follow } => {
            let mut cmd = std::process::Command::new("journalctl");
            cmd.args(["--user", "-u", "debloatmaild", "-n", "100"]);
            if *follow {
                cmd.arg("-f");
            }
            let status = cmd
                .status()
                .map_err(|e| CliError::Internal(format!("failed to run journalctl: {e}")))?;

            if !status.success() {
                return Err(CliError::Internal(
                    "journalctl exited with non-zero exit code".into(),
                ));
            }

            Ok(())
        }
    }
}
