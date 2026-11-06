//! The `doctor` diagnostic command conforming to `docs/debloatmailctl.md` §8.

use debloatmail_ffi::SecretStore;
use rusqlite::Connection;
use std::fs;
use std::path::Path;

use crate::args::{Cli, DoctorArgs, OutputFormat};
use crate::config::{AppConfig, AppPaths};
use crate::error::CliError;
use crate::executor::init_engine;

#[derive(Debug, serde::Serialize)]
pub struct DiagnosticCheck {
    pub name: String,
    pub status: String, // "ok", "warning", "error"
    pub message: String,
}

pub async fn execute_doctor(
    conn: &Connection,
    cli: &Cli,
    paths: &AppPaths,
    config: &AppConfig,
    args: &DoctorArgs,
) -> Result<(), CliError> {
    let mut checks = Vec::new();

    // 1. Config syntax and validation
    let config_ok = AppConfig::load_merged(&paths.config_paths).is_ok();
    checks.push(DiagnosticCheck {
        name: "configuration".to_string(),
        status: if config_ok {
            "ok".into()
        } else {
            "warning".into()
        },
        message: if config_ok {
            format!(
                "loaded and validated {} config file(s)",
                paths.config_paths.len()
            )
        } else {
            "one or more config files failed syntax validation".into()
        },
    });

    // 2. Database user_version
    let user_version: i64 = conn
        .query_row("PRAGMA user_version;", [], |r| r.get(0))
        .unwrap_or(0);
    if user_version > 3 {
        checks.push(DiagnosticCheck {
            name: "database_version".to_string(),
            status: "error".into(),
            message: format!(
                "database schema v{user_version} is newer than binary supported schema v3"
            ),
        });
    } else {
        checks.push(DiagnosticCheck {
            name: "database_version".to_string(),
            status: "ok".into(),
            message: format!("schema version v{user_version} (supported <= 3)"),
        });
    }

    // 3. Database integrity check & WAL mode
    let integrity: String = conn
        .query_row("PRAGMA integrity_check;", [], |r| r.get(0))
        .unwrap_or_else(|_| "failed".into());
    let journal_mode: String = conn
        .query_row("PRAGMA journal_mode;", [], |r| r.get(0))
        .unwrap_or_else(|_| "unknown".into());

    let db_ok = integrity == "ok" && journal_mode.eq_ignore_ascii_case("wal");
    checks.push(DiagnosticCheck {
        name: "sqlite_integrity".to_string(),
        status: if db_ok { "ok".into() } else { "warning".into() },
        message: format!("integrity={integrity}, journal_mode={journal_mode}"),
    });

    // 4. Keyring reachability and secret_ref resolution
    let secret_store = debloatmail_ffi::KeyringStore::new();
    let mut accounts_stmt =
        conn.prepare("SELECT name, secret_ref FROM accounts WHERE kind = 'imap' AND enabled = 1;")?;
    let imap_accounts = accounts_stmt
        .query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;

    let mut missing_secrets = Vec::new();
    for (name, sref) in &imap_accounts {
        match sref {
            Some(r) if secret_store.get_secret(r).is_ok() => {}
            _ => missing_secrets.push(name.clone()),
        }
    }

    if missing_secrets.is_empty() {
        checks.push(DiagnosticCheck {
            name: "keyring_credentials".to_string(),
            status: "ok".into(),
            message: format!(
                "all {} imap account credentials resolve in keyring",
                imap_accounts.len()
            ),
        });
    } else {
        checks.push(DiagnosticCheck {
            name: "keyring_credentials".to_string(),
            status: "warning".into(),
            message: format!(
                "missing keyring credentials for: {}",
                missing_secrets.join(", ")
            ),
        });
    }

    // 5. Blob refcount reconciliation
    let mut blob_stmt = conn.prepare("SELECT hash, refcount FROM blobs;")?;
    let blobs = blob_stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
        .collect::<Result<Vec<_>, _>>()?;

    let mut refcount_mismatches = 0;
    for (hash, stored_ref) in &blobs {
        let raw_cnt: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM mail_items WHERE raw_blob = ?1;",
                [hash],
                |r| r.get(0),
            )
            .unwrap_or(0);
        let att_cnt: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM attachments WHERE blob_hash = ?1;",
                [hash],
                |r| r.get(0),
            )
            .unwrap_or(0);
        let actual_ref = raw_cnt + att_cnt;
        if actual_ref != *stored_ref {
            refcount_mismatches += 1;
        }
    }

    checks.push(DiagnosticCheck {
        name: "blob_refcounts".to_string(),
        status: if refcount_mismatches == 0 {
            "ok".into()
        } else {
            "warning".into()
        },
        message: if refcount_mismatches == 0 {
            format!("{} blobs verified with consistent refcounts", blobs.len())
        } else {
            format!("{refcount_mismatches} blob refcount mismatches detected")
        },
    });

    // 6. FTS5 index consistency
    let fts_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM item_text;", [], |r| r.get(0))
        .unwrap_or(0);
    let items_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM visible_items;", [], |r| r.get(0))
        .unwrap_or(0);
    let fts_diff = (fts_count - items_count).abs();

    checks.push(DiagnosticCheck {
        name: "fts5_consistency".to_string(),
        status: if fts_diff == 0 { "ok".into() } else { "warning".into() },
        message: if fts_diff == 0 {
            format!("item_text index consistent with visible_items ({fts_count} rows)")
        } else {
            format!("fts5 row count ({fts_count}) differs from visible items ({items_count}); run: INSERT INTO item_text(item_text) VALUES('rebuild');")
        },
    });

    // 7. UIDVALIDITY residue
    let uidvalidity_resets: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM folders WHERE uidvalidity IS NOT NULL AND uidvalidity < 0;",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    checks.push(DiagnosticCheck {
        name: "uidvalidity_residue".to_string(),
        status: if uidvalidity_resets == 0 {
            "ok".into()
        } else {
            "warning".into()
        },
        message: format!("{uidvalidity_resets} uidvalidity reset anomalies"),
    });

    // 8. Operation queue health
    let failed_ops: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pending_ops WHERE state = 'failed';",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let aged_ops: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM pending_ops WHERE state IN ('pending','in_flight') AND created_at < unixepoch() - 86400;",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);

    checks.push(DiagnosticCheck {
        name: "pending_ops_queue".to_string(),
        status: if failed_ops == 0 && aged_ops == 0 {
            "ok".into()
        } else {
            "warning".into()
        },
        message: format!("failed_ops={failed_ops}, aged_in_flight_ops={aged_ops}"),
    });

    // 9. Unvalidated feeds
    let unvalidated_feeds: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM feeds WHERE last_fetch_at IS NULL AND removed_at IS NULL;",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    checks.push(DiagnosticCheck {
        name: "unvalidated_feeds".to_string(),
        status: if unvalidated_feeds == 0 {
            "ok".into()
        } else {
            "warning".into()
        },
        message: format!("{unvalidated_feeds} feeds pending initial validation fetch"),
    });

    // 10. Cache cap disk usage
    let cache_bytes = calculate_dir_size(&paths.blobs_dir);
    let cap_mb = config
        .get("cache.cap_mb")
        .and_then(|v| v.as_integer())
        .unwrap_or(500);
    let cap_bytes = (cap_mb as u64) * 1024 * 1024;
    let cache_mb_used = cache_bytes / (1024 * 1024);

    checks.push(DiagnosticCheck {
        name: "cache_capacity".to_string(),
        status: if cache_bytes <= cap_bytes {
            "ok".into()
        } else {
            "warning".into()
        },
        message: format!("{cache_mb_used} MB used of {cap_mb} MB configured cap"),
    });

    // 11. Daemon connectivity
    let daemon_check = match init_engine(cli, paths).await {
        Ok(eng) => match eng.get_daemon_status().await {
            Ok(st) => {
                if st.running {
                    DiagnosticCheck {
                        name: "daemon_service".to_string(),
                        status: "ok".into(),
                        message: format!(
                            "running (v{}, API v{}, state: {})",
                            st.daemon_version, st.api_version, st.state
                        ),
                    }
                } else {
                    DiagnosticCheck {
                        name: "daemon_service".to_string(),
                        status: "warning".into(),
                        message: "daemon not running (standalone mode active or service inactive)"
                            .into(),
                    }
                }
            }
            Err(_) => DiagnosticCheck {
                name: "daemon_service".to_string(),
                status: "warning".into(),
                message: "daemon not reachable on session D-Bus".into(),
            },
        },
        Err(_) => DiagnosticCheck {
            name: "daemon_service".to_string(),
            status: "warning".into(),
            message: "daemon not reachable on session D-Bus".into(),
        },
    };
    checks.push(daemon_check);

    // Format results
    let format = if args.json {
        OutputFormat::Json
    } else {
        cli.format
    };
    match format {
        OutputFormat::Human => {
            println!("=== debloatmail Doctor Diagnostics ===");
            for c in &checks {
                let badge = match c.status.as_str() {
                    "ok" => "[OK]     ",
                    "warning" => "[WARNING]",
                    _ => "[ERROR]  ",
                };
                println!("{badge} {:<22} {}", c.name, c.message);
            }
        }
        OutputFormat::Json | OutputFormat::Jsonl => {
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "diagnostics": checks
                }))
                .map_err(|e| CliError::Internal(e.to_string()))?
            );
        }
        OutputFormat::Plain => {
            for c in &checks {
                println!("{}\t{}\t{}", c.name, c.status, c.message);
            }
        }
    }

    Ok(())
}

fn calculate_dir_size(path: &Path) -> u64 {
    let mut total = 0;
    if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                total += calculate_dir_size(&p);
            } else if let Ok(meta) = p.metadata() {
                total += meta.len();
            }
        }
    }
    total
}
