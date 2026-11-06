//! CLI argument parsing definitions for `debloatmailctl`.
//!
//! Conforms strictly to `docs/debloatmailctl.md` §3, §8, §14.

use clap::{Args, Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

/// `debloatmailctl` — The command-line interface to debloatmail.
#[derive(Debug, Parser)]
#[command(
    name = "debloatmailctl",
    about = "Scriptable, stateless command-line interface to debloatmail",
    version,
    subcommand_required = true,
    arg_required_else_help = true
)]
pub struct Cli {
    /// Scope to one account (resolved client-side).
    #[arg(short = 'a', long, global = true)]
    pub account: Option<String>,

    /// Config file path. Repeatable; multiple paths are deep-merged left-to-right.
    #[arg(short = 'c', long, global = true)]
    pub config: Vec<String>,

    /// Override the database path (primarily for tests).
    #[arg(long, global = true)]
    pub db: Option<PathBuf>,

    /// Never touch the network. Missing data exits 7.
    #[arg(long, global = true)]
    pub offline: bool,

    /// Do not use D-Bus; run operations in-process taking standalone exclusive lock.
    #[arg(long, global = true)]
    pub no_daemon: bool,

    /// Output format.
    #[arg(long, global = true, value_enum, default_value_t = OutputFormat::Human)]
    pub format: OutputFormat,

    /// Terminal color policy.
    #[arg(long, global = true, value_enum, default_value_t = ColorPolicy::Auto)]
    pub color: ColorPolicy,

    /// Suppress non-essential output. Errors still print to stderr.
    #[arg(short = 'q', long, global = true)]
    pub quiet: bool,

    /// Increase logging verbosity (-v, -vv, -vvv).
    #[arg(short = 'v', long, global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,

    /// Log at debug level, including protocol traces with credentials redacted.
    #[arg(long, global = true)]
    pub debug: bool,

    /// Overall operation timeout (e.g. 30s, 5m). Default 60s.
    #[arg(long, global = true, default_value = "60s")]
    pub timeout: String,

    /// Assume yes for destructive confirmations.
    #[arg(short = 'y', long, global = true)]
    pub yes: bool,

    /// Print what would happen; change nothing.
    #[arg(long, global = true)]
    pub dry_run: bool,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum OutputFormat {
    #[default]
    Human,
    Json,
    Jsonl,
    Plain,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum ColorPolicy {
    #[default]
    Auto,
    Always,
    Never,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    /// The unified stream — emails and feed articles in one ordered list.
    List(ListArgs),

    /// Display plaintext body of a message or feed item.
    Read(ReadArgs),

    /// Full-text search over extracted text using Gmail-style filter queries.
    Search(SearchArgs),

    /// Mail folder management and statistics.
    Folder(FolderArgs),

    /// Feed management, inspection, and updates.
    Feed(FeedArgs),

    /// Account configuration and diagnostics.
    Account(AccountArgs),

    /// Configuration file inspection and settings.
    Config(ConfigArgs),

    /// Shell completion, man page, and schema generator.
    Generate(GenerateArgs),

    /// Print detailed binary version and build information.
    Version,

    // --- Stubs for Phase 3C commands ---
    /// Mark items as read, unread, starred, or unstarred.
    Mark(MarkStubArgs),

    /// Move mail items to target folder.
    Move(MoveStubArgs),

    /// Copy mail items to target folder.
    Copy(CopyStubArgs),

    /// Delete mail items (moves to trash or expunges).
    Delete(DeleteStubArgs),

    /// Hide feed articles from all streams and search.
    Hide(HideStubArgs),

    /// Compose and queue a plaintext email.
    Compose(ComposeStubArgs),

    /// Reply to an existing mail item.
    Reply(ReplyStubArgs),

    /// Forward an existing mail item.
    Forward(ForwardStubArgs),

    /// Inspect and download attachments.
    Attachment(AttachmentStubArgs),

    /// Trigger synchronization for accounts, folders, or feeds.
    Sync(SyncStubArgs),

    /// Background daemon lifecycle and status control.
    Daemon(DaemonStubArgs),

    /// End-to-end local health and database diagnostics.
    Doctor(DoctorStubArgs),
}

#[derive(Debug, Args, Clone)]
pub struct ListArgs {
    /// Restrict to email only.
    #[arg(long, conflicts_with = "feed")]
    pub mail: bool,

    /// Restrict to feeds only, or scope to one feed name.
    #[arg(long, num_args(0..=1), default_missing_value = "")]
    pub feed: Option<String>,

    /// Filter by unread status.
    #[arg(long)]
    pub unread: bool,

    /// Filter by starred status.
    #[arg(long)]
    pub starred: bool,

    /// Filter by mail folder (name or role: inbox, sent, drafts, trash, archive, junk, all).
    #[arg(long)]
    pub folder: Option<String>,

    /// Lower bound on published_at date (ISO 8601 or relative: 7d, 2w).
    #[arg(long)]
    pub since: Option<String>,

    /// Upper bound on published_at date (ISO 8601 or relative: 7d, 2w).
    #[arg(long)]
    pub before: Option<String>,

    /// Collapse rows to one per conversation thread.
    #[arg(long)]
    pub threads: bool,

    /// Ordering tuple key.
    #[arg(long, value_enum, default_value_t = SortOption::Date)]
    pub sort: SortOption,

    /// Limit maximum rows returned (default: 25).
    #[arg(short = 'n', long, conflicts_with = "all")]
    pub limit: Option<u32>,

    /// Return all matching items without pagination.
    #[arg(long)]
    pub all: bool,

    /// Keyset pagination cursor from previous response.
    #[arg(long)]
    pub cursor: Option<String>,

    /// In plain format, print item UUIDs NUL-separated.
    #[arg(long)]
    pub print0: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, ValueEnum)]
pub enum SortOption {
    #[default]
    Date,
    Received,
    Author,
    Subject,
}

#[derive(Debug, Args)]
pub struct ReadArgs {
    /// Item identifier (8-hex short ID prefix or full UUIDv7).
    pub id: String,

    /// Print header block before the body text.
    #[arg(long)]
    pub headers: bool,

    /// Print raw MIME bytes from blob store (mail only).
    #[arg(long)]
    pub raw: bool,

    /// Mark item as read upon displaying.
    #[arg(long)]
    pub mark_read: bool,

    /// Print the entire conversation thread, oldest first.
    #[arg(long)]
    pub thread: bool,
}

#[derive(Debug, Args)]
pub struct SearchArgs {
    /// Gmail-style search query string.
    pub query: String,

    /// Print compiled FTS5 expression and SQL query instead of executing.
    #[arg(long)]
    pub explain: bool,

    #[command(flatten)]
    pub list: ListArgs,
}

#[derive(Debug, Args)]
pub struct FolderArgs {
    #[command(subcommand)]
    pub command: FolderCommands,
}

#[derive(Debug, Subcommand)]
pub enum FolderCommands {
    /// List mail folders, roles, and message counts.
    List {
        #[arg(short = 'a', long)]
        account: Option<String>,
    },
    /// Print per-folder sync statistics.
    Stats {
        #[arg(short = 'a', long)]
        account: Option<String>,
    },
    /// Synchronize a single folder.
    Sync {
        name: String,
        #[arg(long)]
        full: bool,
    },
}

#[derive(Debug, Args)]
pub struct FeedArgs {
    #[command(subcommand)]
    pub command: FeedCommands,
}

#[derive(Debug, Subcommand)]
pub enum FeedCommands {
    /// List configured feed subscriptions.
    List,
    /// Display detailed metadata for a single feed subscription.
    Show { name_or_id: String },
    /// Subscribe to a new RSS or Atom feed.
    Add {
        url: String,
        #[arg(short = 'a', long)]
        account: Option<String>,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        interval: Option<String>,
    },
    /// Update feed metadata or poll interval.
    Update {
        name_or_id: String,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        interval: Option<String>,
    },
    /// Change subscription URL (soft-remove + re-add).
    SetUrl { name_or_id: String, new_url: String },
    /// Remove feed subscription.
    Remove {
        name_or_id: String,
        #[arg(long)]
        purge: bool,
    },
    /// Trigger an immediate fetch of feed.
    Refresh {
        name_or_id: String,
        #[arg(long)]
        wait: bool,
    },
}

#[derive(Debug, Args)]
pub struct AccountArgs {
    #[command(subcommand)]
    pub command: AccountCommands,
}

#[derive(Debug, Subcommand)]
pub enum AccountCommands {
    /// List all configured accounts.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Show account details and configuration.
    Show { name_or_id: String },
    /// Add a new mail or feed account.
    Add {
        #[arg(long)]
        kind: Option<String>,
        #[arg(long)]
        name: Option<String>,
    },
    /// Update existing account settings.
    Update { name: String },
    /// Remove an account.
    Remove {
        name: String,
        #[arg(long)]
        purge: bool,
    },
    /// Run diagnostic checks on account connectivity.
    Check { name: Option<String> },
}

#[derive(Debug, Args)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub command: ConfigCommands,
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommands {
    /// Print active configuration file path(s).
    Path,
    /// Print merged configuration TOML.
    Show,
    /// Validate configuration syntax.
    Validate,
    /// Retrieve a configuration value by dotted key path.
    Get { key: String },
    /// Set a configuration value and write to config file.
    Set { key: String, value: String },
}

#[derive(Debug, Args)]
pub struct GenerateArgs {
    #[command(subcommand)]
    pub command: GenerateCommands,
}

#[derive(Debug, Subcommand)]
pub enum GenerateCommands {
    /// Generate shell completions for bash, zsh, fish, elvish, or powershell.
    Completions {
        shell: clap_complete::Shell,
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// Generate manual pages.
    Man {
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// Dump the authoritative SQLite database DDL schema.
    Schema {
        #[arg(long)]
        dir: Option<PathBuf>,
    },
}

// Stubs for Phase 3C commands
#[derive(Debug, Args)]
pub struct MarkStubArgs {
    pub mode: String,
    pub ids: Vec<String>,
}

#[derive(Debug, Args)]
pub struct MoveStubArgs {
    pub ids: Vec<String>,
    #[arg(long)]
    pub to: String,
}

#[derive(Debug, Args)]
pub struct CopyStubArgs {
    pub ids: Vec<String>,
    #[arg(long)]
    pub to: String,
}

#[derive(Debug, Args)]
pub struct DeleteStubArgs {
    pub ids: Vec<String>,
    #[arg(long)]
    pub expunge: bool,
}

#[derive(Debug, Args)]
pub struct HideStubArgs {
    pub ids: Vec<String>,
}

#[derive(Debug, Args)]
pub struct ComposeStubArgs {
    #[arg(long)]
    pub to: Option<String>,
    #[arg(long)]
    pub subject: Option<String>,
}

#[derive(Debug, Args)]
pub struct ReplyStubArgs {
    pub id: String,
}

#[derive(Debug, Args)]
pub struct ForwardStubArgs {
    pub id: String,
}

#[derive(Debug, Args)]
pub struct AttachmentStubArgs {
    pub action: String,
}

#[derive(Debug, Args)]
pub struct SyncStubArgs {
    #[arg(long)]
    pub wait: bool,
}

#[derive(Debug, Args)]
pub struct DaemonStubArgs {
    pub action: String,
}

#[derive(Debug, Args)]
pub struct DoctorStubArgs {
    #[arg(long)]
    pub json: bool,
}
