//! Human-readable aligned terminal table formatter conforming to `docs/debloatmailctl.md` §6.

use debloatmail_ffi::{FfiAccount, FfiFeed, FfiFolder, FfiItemSummary, FfiPage, FfiThreadSummary};
use std::io::Write;

use crate::args::ColorPolicy;
use crate::error::CliError;

fn should_colorize(policy: ColorPolicy) -> bool {
    match policy {
        ColorPolicy::Never => false,
        ColorPolicy::Always => true,
        ColorPolicy::Auto => {
            if std::env::var_os("NO_COLOR").is_some() {
                false
            } else {
                anstream::stdout().is_terminal()
            }
        }
    }
}

pub fn print_item_page_human(
    page: &FfiPage<FfiItemSummary>,
    color: ColorPolicy,
) -> Result<(), CliError> {
    if page.items.is_empty() {
        println!("No items found.");
        return Ok(());
    }

    let colorize = should_colorize(color);
    let mut stdout = anstream::stdout();

    let bold = if colorize {
        anstyle::Style::new().bold()
    } else {
        anstyle::Style::new()
    };
    let yellow = if colorize {
        anstyle::Style::new().fg_color(Some(anstyle::Color::Ansi(anstyle::AnsiColor::Yellow)))
    } else {
        anstyle::Style::new()
    };
    let dim = if colorize {
        anstyle::Style::new().dimmed()
    } else {
        anstyle::Style::new()
    };

    for item in &page.items {
        let date_str = if item.published_at.len() >= 10 {
            &item.published_at[..10]
        } else if item.sort_at.len() >= 10 {
            &item.sort_at[..10]
        } else {
            "-"
        };

        let star_marker = if item.starred {
            format!("{yellow}*{yellow:#}")
        } else {
            " ".to_string()
        };

        let author_padded = if item.author.len() > 18 {
            format!("{}…", &item.author[..17])
        } else {
            format!("{:18}", item.author)
        };

        let row_style = if item.unread { bold } else { dim };

        writeln!(
            stdout,
            " {star_marker} {:8}  {:10}  {}  {row_style}{}{row_style:#}",
            item.short_id, date_str, author_padded, item.title
        )?;
    }

    if let Some(cursor) = &page.next_cursor {
        writeln!(stdout, "\n{dim}Next page cursor: {cursor}{dim:#}")?;
    }

    Ok(())
}

pub fn print_thread_page_human(
    page: &FfiPage<FfiThreadSummary>,
    color: ColorPolicy,
) -> Result<(), CliError> {
    if page.items.is_empty() {
        println!("No threads found.");
        return Ok(());
    }

    let colorize = should_colorize(color);
    let mut stdout = anstream::stdout();

    let bold = if colorize {
        anstyle::Style::new().bold()
    } else {
        anstyle::Style::new()
    };
    let dim = if colorize {
        anstyle::Style::new().dimmed()
    } else {
        anstyle::Style::new()
    };

    for thread in &page.items {
        let date_str = if thread.last_activity.len() >= 10 {
            &thread.last_activity[..10]
        } else {
            "-"
        };

        let count_str = format!("[{}/{}]", thread.unread_count, thread.reply_count);
        let short_id = if thread.id.len() >= 8 {
            &thread.id[..8]
        } else {
            &thread.id
        };

        let row_style = if thread.unread_count > 0 { bold } else { dim };

        writeln!(
            stdout,
            "  {:8}  {:10}  {:8}  {row_style}{}{row_style:#}",
            short_id, date_str, count_str, thread.display_subject
        )?;
    }

    Ok(())
}

pub fn print_folders_human(folders: &[FfiFolder]) -> Result<(), CliError> {
    if folders.is_empty() {
        println!("No folders found.");
        return Ok(());
    }

    println!(
        "{:<24} {:<12} {:<12} {:>8} {:>8}",
        "NAME", "ROLE", "SOURCE", "TOTAL", "UNREAD"
    );
    println!("{}", "-".repeat(68));

    for f in folders {
        let role = f.role.as_deref().unwrap_or("-");
        let src = f.role_source.map(|s| s.as_str()).unwrap_or("-");
        println!(
            "{:<24} {:<12} {:<12} {:>8} {:>8}",
            f.name, role, src, f.item_count, f.unread_count
        );
    }
    Ok(())
}

pub fn print_feeds_human(feeds: &[FfiFeed]) -> Result<(), CliError> {
    if feeds.is_empty() {
        println!("No feeds configured.");
        return Ok(());
    }

    println!(
        "{:<24} {:<36} {:>8} {:>8}",
        "TITLE", "URL", "INTERVAL", "UNREAD"
    );
    println!("{}", "-".repeat(80));

    for f in feeds {
        let title = f.title.as_deref().unwrap_or(&f.name);
        let url = if f.url.len() > 36 {
            format!("{}…", &f.url[..35])
        } else {
            f.url.clone()
        };
        let interval_str = if f.interval_secs == 0 {
            "auto".to_string()
        } else {
            format!("{}s", f.interval_secs)
        };
        println!(
            "{:<24} {:<36} {:>8} {:>8}",
            title, url, interval_str, f.unread_count
        );
    }
    Ok(())
}

pub fn print_accounts_human(accounts: &[FfiAccount]) -> Result<(), CliError> {
    if accounts.is_empty() {
        println!("No accounts configured.");
        return Ok(());
    }

    println!(
        "{:<18} {:<8} {:<28} {:<8} {:>8}",
        "NAME", "KIND", "EMAIL", "ENABLED", "UNREAD"
    );
    println!("{}", "-".repeat(74));

    for a in accounts {
        let enabled = if a.enabled { "yes" } else { "no" };
        println!(
            "{:<18} {:<8} {:<28} {:<8} {:>8}",
            a.name, a.kind, a.email, enabled, a.unread_count
        );
    }
    Ok(())
}
