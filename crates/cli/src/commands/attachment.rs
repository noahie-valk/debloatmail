//! The `attachment` subcommands conforming to `docs/debloatmailctl.md` §8.

use debloatmail_ffi::reader::get_item_fn;
use rusqlite::Connection;
use std::fs;
use std::path::PathBuf;

use crate::args::{AttachmentArgs, AttachmentCommands, Cli, OutputFormat};
use crate::config::AppPaths;
use crate::error::CliError;
use crate::executor::init_engine;
use crate::resolve::resolve_item_id;

pub async fn execute_attachment(
    conn: &Connection,
    cli: &Cli,
    paths: &AppPaths,
    args: &AttachmentArgs,
) -> Result<(), CliError> {
    match &args.command {
        AttachmentCommands::List { id } => {
            let public_id = resolve_item_id(conn, id)?;
            let detail = get_item_fn(conn, &paths.blobs_dir, &public_id)?;

            match cli.format {
                OutputFormat::Json => {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&serde_json::json!({
                            "attachments": detail.attachments
                        }))
                        .map_err(|e| CliError::Internal(e.to_string()))?
                    );
                }
                OutputFormat::Jsonl => {
                    for att in &detail.attachments {
                        println!(
                            "{}",
                            serde_json::to_string(att)
                                .map_err(|e| CliError::Internal(e.to_string()))?
                        );
                    }
                }
                OutputFormat::Plain => {
                    for att in &detail.attachments {
                        println!(
                            "{}\t{}\t{}\t{}",
                            att.section_id,
                            att.filename.as_deref().unwrap_or("-"),
                            att.content_type,
                            att.size_bytes.unwrap_or(0)
                        );
                    }
                }
                OutputFormat::Human => {
                    if detail.attachments.is_empty() {
                        println!("No attachments found.");
                    } else {
                        println!(
                            "{:<8} {:<30} {:<25} {:>10}",
                            "SECTION", "FILENAME", "TYPE", "SIZE"
                        );
                        println!("{}", "-".repeat(76));
                        for att in &detail.attachments {
                            let name = att.filename.as_deref().unwrap_or("unnamed");
                            let size_str = att
                                .size_bytes
                                .map(|s| format!("{} B", s))
                                .unwrap_or_else(|| "-".into());
                            println!(
                                "{:<8} {:<30} {:<25} {:>10}",
                                att.section_id, name, att.content_type, size_str
                            );
                        }
                    }
                }
            }
            Ok(())
        }
        AttachmentCommands::Download {
            id,
            section_id,
            output,
        } => {
            let public_id = resolve_item_id(conn, id)?;
            let detail = get_item_fn(conn, &paths.blobs_dir, &public_id)?;

            let att = detail
                .attachments
                .iter()
                .find(|a| a.section_id == *section_id)
                .ok_or_else(|| {
                    CliError::NotFound(format!(
                        "attachment section '{section_id}' not found in item '{id}'"
                    ))
                })?;

            let default_name = att
                .filename
                .clone()
                .unwrap_or_else(|| format!("attachment_{section_id}.dat"));
            let dest_path = output
                .clone()
                .unwrap_or_else(|| PathBuf::from(default_name));

            if cli.offline {
                // In offline mode, verify blob is present locally
                let blob_path = paths.blobs_dir.join(&att.section_id);
                if !blob_path.exists() {
                    return Err(CliError::Offline(
                        "attachment not cached locally and --offline is active".into(),
                    ));
                }
            }

            let engine = init_engine(cli, paths).await?;
            let cached_blob = engine.fetch_attachment(&public_id, section_id).await?;

            fs::copy(&cached_blob, &dest_path).map_err(|e| {
                CliError::Internal(format!(
                    "failed to copy attachment to '{}': {e}",
                    dest_path.display()
                ))
            })?;

            match cli.format {
                OutputFormat::Human => println!("saved to {}", dest_path.display()),
                OutputFormat::Json | OutputFormat::Jsonl => {
                    println!(
                        "{}",
                        serde_json::json!({
                            "path": dest_path.to_string_lossy()
                        })
                    );
                }
                OutputFormat::Plain => println!("{}", dest_path.display()),
            }

            Ok(())
        }
        AttachmentCommands::Open { id, section_id } => {
            let public_id = resolve_item_id(conn, id)?;
            let engine = init_engine(cli, paths).await?;
            let cached_blob = engine.fetch_attachment(&public_id, section_id).await?;

            let status = std::process::Command::new("xdg-open")
                .arg(&cached_blob)
                .status();

            match status {
                Ok(s) if s.success() => Ok(()),
                _ => {
                    println!("cached at {}", cached_blob.display());
                    Ok(())
                }
            }
        }
    }
}
