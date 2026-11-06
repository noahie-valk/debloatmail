//! Feed inspection and management subcommands conforming to `docs/debloatmailctl.md` §8.

use debloatmail_ffi::reader::list_feeds_fn;
use rusqlite::Connection;

use crate::args::{Cli, FeedArgs, FeedCommands, OutputFormat};
use crate::config::AppPaths;
use crate::error::CliError;
use crate::executor::{confirm_destructive, ensure_online, init_engine};
use crate::format::format_feeds;
use crate::format::json::print_single_json;
use crate::resolve::{resolve_account, resolve_feed};

pub async fn execute_feed(
    conn: &Connection,
    cli: &Cli,
    paths: &AppPaths,
    args: &FeedArgs,
) -> Result<(), CliError> {
    match &args.command {
        FeedCommands::List => {
            let acc_id = match cli.account.as_deref() {
                Some(acc_arg) => Some(resolve_account(conn, acc_arg)?.id),
                None => None,
            };
            let feeds = list_feeds_fn(conn, acc_id.as_deref())?;
            format_feeds(&feeds, cli.format)
        }
        FeedCommands::Show { name_or_id } => {
            let acc_id = match cli.account.as_deref() {
                Some(acc_arg) => Some(resolve_account(conn, acc_arg)?.id),
                None => None,
            };
            let feed = resolve_feed(conn, acc_id.as_deref(), name_or_id)?;

            match cli.format {
                OutputFormat::Json | OutputFormat::Jsonl => print_single_json(&feed)?,
                OutputFormat::Plain => {
                    println!(
                        "{}\t{}\t{}\t{}\t{}\t{}",
                        feed.id,
                        feed.name,
                        feed.url,
                        feed.interval_secs,
                        feed.last_fetch_at.as_deref().unwrap_or("-"),
                        feed.unread_count
                    );
                }
                OutputFormat::Human => {
                    println!("ID:          {}", feed.id);
                    println!(
                        "Title:       {}",
                        feed.title.as_deref().unwrap_or(&feed.name)
                    );
                    println!("URL:         {}", feed.url);
                    println!(
                        "Interval:    {}",
                        if feed.interval_secs == 0 {
                            "auto".to_string()
                        } else {
                            format!("{}s", feed.interval_secs)
                        }
                    );
                    println!(
                        "Last Fetch:  {}",
                        feed.last_fetch_at.as_deref().unwrap_or("never")
                    );
                    println!(
                        "Next Fetch:  {}",
                        feed.next_fetch_at.as_deref().unwrap_or("pending")
                    );
                    println!("Unread:      {}", feed.unread_count);
                }
            }
            Ok(())
        }
        FeedCommands::Add {
            url,
            account,
            name,
            interval,
        } => {
            ensure_online(cli)?;

            // Resolve feed account
            let account_id = match account.as_deref().or(cli.account.as_deref()) {
                Some(acc_arg) => {
                    let a = resolve_account(conn, acc_arg)?;
                    if a.kind != "feed" {
                        return Err(CliError::Usage(format!(
                            "account '{}' is a mail account; feeds require a feed account",
                            a.name
                        )));
                    }
                    a.id
                }
                None => {
                    let mut stmt =
                        conn.prepare("SELECT public_id, name FROM accounts WHERE kind = 'feed';")?;
                    let accounts: Vec<(String, String)> = stmt
                        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                        .collect::<Result<Vec<_>, _>>()?;

                    match accounts.len() {
                        0 => {
                            return Err(CliError::Usage(
                                "no feed accounts configured; add one first with 'debloatmailctl account add --kind feed --name <NAME>'".into(),
                            ));
                        }
                        1 => accounts[0].0.clone(),
                        _ => {
                            let names: Vec<String> = accounts.into_iter().map(|(_, n)| n).collect();
                            return Err(CliError::Usage(format!(
                                "multiple feed accounts exist, specify --account <NAME> (choices: {})",
                                names.join(", ")
                            )));
                        }
                    }
                }
            };

            let interval_secs = parse_interval_secs(interval.as_deref())?;
            let feed_name = name.as_deref().unwrap_or("");

            if cli.dry_run {
                println!("would add feed '{url}'");
                return Ok(());
            }

            let engine = init_engine(cli, paths).await?;
            let (feed_id, validated) = engine
                .add_feed(&account_id, url, feed_name, interval_secs)
                .await?;

            let display_name = if !feed_name.is_empty() {
                feed_name
            } else {
                url.as_str()
            };

            match cli.format {
                OutputFormat::Human => {
                    if validated {
                        println!("added feed '{display_name}'");
                    } else {
                        println!("added feed '{display_name}' — validation pending (slow server)");
                    }
                }
                OutputFormat::Json | OutputFormat::Jsonl => {
                    println!(
                        "{}",
                        serde_json::json!({
                            "feed_id": feed_id,
                            "name": display_name,
                            "validated": validated
                        })
                    );
                }
                OutputFormat::Plain => {
                    println!("{feed_id}\t{display_name}\t{validated}");
                }
            }

            Ok(())
        }
        FeedCommands::Update {
            name_or_id,
            name,
            interval,
        } => {
            let acc_id = match cli.account.as_deref() {
                Some(acc_arg) => Some(resolve_account(conn, acc_arg)?.id),
                None => None,
            };
            let feed = resolve_feed(conn, acc_id.as_deref(), name_or_id)?;
            let interval_secs = match interval {
                Some(s) => Some(parse_interval_secs(Some(s))?),
                None => None,
            };

            if cli.dry_run {
                println!("would update feed '{}'", feed.name);
                return Ok(());
            }

            let engine = init_engine(cli, paths).await?;
            engine
                .update_feed(&feed.id, name.as_deref(), interval_secs)
                .await?;

            match cli.format {
                OutputFormat::Human => println!("updated feed '{}'", feed.name),
                OutputFormat::Json | OutputFormat::Jsonl => {
                    println!(
                        "{}",
                        serde_json::json!({
                            "status": "updated",
                            "feed_id": feed.id
                        })
                    );
                }
                OutputFormat::Plain => println!("updated\t{}", feed.id),
            }

            Ok(())
        }
        FeedCommands::SetUrl {
            name_or_id,
            new_url,
        } => {
            ensure_online(cli)?;
            confirm_destructive(
                cli,
                "Change feed subscription URL? (Previous articles are retained)",
            )?;

            let acc_id = match cli.account.as_deref() {
                Some(acc_arg) => Some(resolve_account(conn, acc_arg)?.id),
                None => None,
            };
            let feed = resolve_feed(conn, acc_id.as_deref(), name_or_id)?;

            if cli.dry_run {
                println!(
                    "would change feed '{}' URL from '{}' to '{}'",
                    feed.name, feed.url, new_url
                );
                return Ok(());
            }

            let engine = init_engine(cli, paths).await?;
            // 1. Soft-remove old feed (purge = false keeps articles)
            engine.remove_feed(&feed.id, false).await?;

            // 2. Add new feed URL
            let (new_id, validated) = engine
                .add_feed(&feed.account_id, new_url, &feed.name, feed.interval_secs)
                .await?;

            match cli.format {
                OutputFormat::Human => {
                    println!(
                        "feed '{}' URL updated to {new_url} (previous articles retained)",
                        feed.name
                    );
                }
                OutputFormat::Json | OutputFormat::Jsonl => {
                    println!(
                        "{}",
                        serde_json::json!({
                            "status": "updated",
                            "old_feed_id": feed.id,
                            "feed_id": new_id,
                            "validated": validated
                        })
                    );
                }
                OutputFormat::Plain => println!("updated\t{new_id}"),
            }

            Ok(())
        }
        FeedCommands::Remove { name_or_id, purge } => {
            let acc_id = match cli.account.as_deref() {
                Some(acc_arg) => Some(resolve_account(conn, acc_arg)?.id),
                None => None,
            };
            let feed = resolve_feed(conn, acc_id.as_deref(), name_or_id)?;

            if *purge {
                confirm_destructive(
                    cli,
                    "Permanently purge feed subscription and all its articles from the database?",
                )?;
            }

            if cli.dry_run {
                println!("would remove feed '{}'", feed.name);
                return Ok(());
            }

            let engine = init_engine(cli, paths).await?;
            engine.remove_feed(&feed.id, *purge).await?;

            match cli.format {
                OutputFormat::Human => {
                    if *purge {
                        println!("purged feed '{}'", feed.name);
                    } else {
                        println!("removed feed '{}'", feed.name);
                    }
                }
                OutputFormat::Json | OutputFormat::Jsonl => {
                    println!(
                        "{}",
                        serde_json::json!({
                            "status": "removed",
                            "feed_id": feed.id
                        })
                    );
                }
                OutputFormat::Plain => println!("removed\t{}", feed.id),
            }

            Ok(())
        }
        FeedCommands::Refresh { name_or_id, wait } => {
            ensure_online(cli)?;

            let acc_id = match cli.account.as_deref() {
                Some(acc_arg) => Some(resolve_account(conn, acc_arg)?.id),
                None => None,
            };
            let feed = resolve_feed(conn, acc_id.as_deref(), name_or_id)?;

            if cli.dry_run {
                println!("would refresh feed '{}'", feed.name);
                return Ok(());
            }

            let engine = init_engine(cli, paths).await?;
            let job_id = engine.refresh_feed(&feed.id).await?;

            match cli.format {
                OutputFormat::Human => {
                    if *wait {
                        println!("refreshed feed '{}'", feed.name);
                    } else {
                        println!("refresh job {job_id}");
                    }
                }
                OutputFormat::Json | OutputFormat::Jsonl => {
                    println!(
                        "{}",
                        serde_json::json!({
                            "status": if *wait { "refreshed" } else { "pending" },
                            "feed_id": feed.id,
                            "job_id": job_id
                        })
                    );
                }
                OutputFormat::Plain => println!("job_id\t{job_id}"),
            }

            Ok(())
        }
    }
}

fn parse_interval_secs(s: Option<&str>) -> Result<u32, CliError> {
    match s {
        None => Ok(0),
        Some("0") | Some("auto") => Ok(0),
        Some(val) => {
            let val = val.trim();
            if let Some(num) = val.strip_suffix('s') {
                num.parse::<u32>()
                    .map_err(|_| CliError::Usage(format!("invalid interval '{val}'")))
            } else if let Some(num) = val.strip_suffix('m') {
                num.parse::<u32>()
                    .map(|m| m * 60)
                    .map_err(|_| CliError::Usage(format!("invalid interval '{val}'")))
            } else if let Some(num) = val.strip_suffix('h') {
                num.parse::<u32>()
                    .map(|h| h * 3600)
                    .map_err(|_| CliError::Usage(format!("invalid interval '{val}'")))
            } else if let Some(num) = val.strip_suffix('d') {
                num.parse::<u32>()
                    .map(|d| d * 86400)
                    .map_err(|_| CliError::Usage(format!("invalid interval '{val}'")))
            } else {
                val.parse::<u32>()
                    .map_err(|_| CliError::Usage(format!("invalid interval '{val}'")))
            }
        }
    }
}
