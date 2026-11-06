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
    Mark(MarkArgs),

    /// Move mail items to target folder.
    Move(MoveArgs),

    /// Copy mail items to target folder.
    Copy(CopyArgs),

    /// Delete mail items (moves to trash or expunges).
    Delete(DeleteArgs),

    /// Hide feed articles from all streams and search.
    Hide(HideArgs),

    /// Compose and queue a plaintext email.
    Compose(ComposeArgs),

    /// Reply to an existing mail item.
    Reply(ReplyArgs),

    /// Forward an existing mail item.
    Forward(ForwardArgs),

    /// Inspect and download attachments.
    Attachment(AttachmentArgs),

    /// Trigger synchronization for accounts, folders, or feeds.
    Sync(SyncArgs),

    /// Background daemon lifecycle and status control.
    Daemon(DaemonArgs),

    /// End-to-end local health and database diagnostics.
    Doctor(DoctorArgs),
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
        #[arg(long)]
        wait: bool,
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
        /// Account kind: 'imap' or 'feed' (default: imap).
        #[arg(long)]
        kind: Option<String>,

        /// Account identifier name (must be unique).
        #[arg(long)]
        name: Option<String>,

        /// Run interactive guided wizard.
        #[arg(long)]
        wizard: bool,

        /// User email address.
        #[arg(long)]
        email: Option<String>,

        /// Provider preset: 'fastmail', 'proton', or 'generic'.
        #[arg(long)]
        provider: Option<String>,

        /// IMAP/SMTP username (if different from email).
        #[arg(long)]
        username: Option<String>,

        /// Authentication mechanism kind ('password' or 'oauth2').
        #[arg(long)]
        auth_kind: Option<String>,

        /// IMAP server hostname.
        #[arg(long)]
        imap_host: Option<String>,

        /// IMAP server port (e.g. 993, 1143, 143).
        #[arg(long)]
        imap_port: Option<u16>,

        /// IMAP security ('tls', 'starttls', 'none').
        #[arg(long)]
        imap_security: Option<String>,

        /// SMTP server hostname.
        #[arg(long)]
        smtp_host: Option<String>,

        /// SMTP server port (e.g. 465, 587, 1025).
        #[arg(long)]
        smtp_port: Option<u16>,

        /// SMTP security ('tls', 'starttls', 'none').
        #[arg(long)]
        smtp_security: Option<String>,

        /// Read password/secret from standard input.
        #[arg(long, conflicts_with = "secret_command")]
        secret_stdin: bool,

        /// Command whose standard output produces the secret.
        #[arg(long, conflicts_with = "secret_stdin")]
        secret_command: Option<String>,
    },
    /// Update existing account settings.
    Update {
        /// Account name or ID to update.
        name: String,

        /// Rename account to a new unique name.
        #[arg(long)]
        name_new: Option<String>,

        /// Enable account.
        #[arg(long, conflicts_with = "disable")]
        enable: bool,

        /// Disable account.
        #[arg(long, conflicts_with = "enable")]
        disable: bool,

        /// Read updated password from standard input.
        #[arg(long, conflicts_with = "secret_command")]
        secret_stdin: bool,

        /// Command whose output produces the updated password.
        #[arg(long, conflicts_with = "secret_stdin")]
        secret_command: Option<String>,

        /// Update email address.
        #[arg(long)]
        email: Option<String>,

        /// Update username.
        #[arg(long)]
        username: Option<String>,

        /// Update IMAP host.
        #[arg(long)]
        imap_host: Option<String>,

        /// Update IMAP port.
        #[arg(long)]
        imap_port: Option<u16>,

        /// Update IMAP security.
        #[arg(long)]
        imap_security: Option<String>,

        /// Update SMTP host.
        #[arg(long)]
        smtp_host: Option<String>,

        /// Update SMTP port.
        #[arg(long)]
        smtp_port: Option<u16>,

        /// Update SMTP security.
        #[arg(long)]
        smtp_security: Option<String>,
    },
    /// Remove an account (disables by default, deletes data with --purge).
    Remove {
        /// Account name or ID.
        name: String,

        /// Permanently delete account and all associated mail/feed content.
        #[arg(long)]
        purge: bool,
    },
    /// Run diagnostic checks on account connectivity.
    Check {
        /// Specific account name or ID (defaults to all enabled accounts).
        name: Option<String>,
    },
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "lowercase")]
pub enum MarkMode {
    Read,
    Unread,
    Seen,
    Unseen,
    Starred,
    Unstarred,
}

#[derive(Debug, Args)]
pub struct MarkArgs {
    /// Flag mutation mode.
    #[arg(value_enum)]
    pub mode: MarkMode,

    /// Item identifiers (8-hex short prefix, UUIDv7, or '-' for stdin).
    #[arg(required = true, num_args = 1..)]
    pub ids: Vec<String>,
}

#[derive(Debug, Args)]
pub struct MoveArgs {
    /// Mail item identifiers to move (8-hex short prefix, UUIDv7, or '-' for stdin).
    #[arg(required = true, num_args = 1..)]
    pub ids: Vec<String>,

    /// Target mail folder name or role (e.g. inbox, sent, trash, archive).
    #[arg(long, required = true)]
    pub to: String,
}

#[derive(Debug, Args)]
pub struct CopyArgs {
    /// Mail item identifiers to copy (8-hex short prefix, UUIDv7, or '-' for stdin).
    #[arg(required = true, num_args = 1..)]
    pub ids: Vec<String>,

    /// Target mail folder name or role (e.g. inbox, sent, trash, archive).
    #[arg(long, required = true)]
    pub to: String,
}

#[derive(Debug, Args)]
pub struct DeleteArgs {
    /// Mail item identifiers to delete (moves to trash by default).
    #[arg(required = true, num_args = 1..)]
    pub ids: Vec<String>,

    /// Permanently expunge items from the server instead of moving to trash.
    #[arg(long)]
    pub expunge: bool,
}

#[derive(Debug, Args)]
pub struct HideArgs {
    /// Feed item identifiers to hide permanently from all streams and search.
    #[arg(required = true, num_args = 1..)]
    pub ids: Vec<String>,
}

#[derive(Debug, Args)]
pub struct ComposeArgs {
    /// Recipient email address (repeatable).
    #[arg(long)]
    pub to: Vec<String>,

    /// Carbon copy recipient (repeatable).
    #[arg(long)]
    pub cc: Vec<String>,

    /// Blind carbon copy recipient (repeatable).
    #[arg(long)]
    pub bcc: Vec<String>,

    /// Subject line.
    #[arg(short = 's', long)]
    pub subject: Option<String>,

    /// Read message body from standard input.
    #[arg(long)]
    pub body_stdin: bool,

    /// Attach a file (repeatable). Must be a regular readable file.
    #[arg(long)]
    pub attach: Vec<PathBuf>,

    /// Send a previously saved draft by ID (deletes draft upon queueing).
    #[arg(long)]
    pub draft: Option<String>,

    /// Save as draft to local drafts table without sending.
    #[arg(long)]
    pub save_draft: bool,

    /// Block until the queued operation reaches a terminal state.
    #[arg(long)]
    pub now: bool,

    /// Do not append a copy to the Sent folder.
    #[arg(long)]
    pub no_append_sent: bool,

    /// Open $EDITOR ($VISUAL, then vi) with a message template.
    #[arg(long)]
    pub edit: bool,
}

#[derive(Debug, Args)]
pub struct ReplyArgs {
    /// Mail item to reply to (short ID or UUIDv7).
    pub id: String,

    /// Reply to all original recipients (To + Cc).
    #[arg(long)]
    pub all: bool,

    /// Quote the original message body with '> '.
    #[arg(long)]
    pub quote: bool,

    /// Read reply body from standard input.
    #[arg(long)]
    pub body_stdin: bool,

    /// Open editor with prefilled template.
    #[arg(long)]
    pub edit: bool,

    /// Block until send completes.
    #[arg(long)]
    pub now: bool,

    /// Save as draft instead of sending.
    #[arg(long)]
    pub save_draft: bool,
}

#[derive(Debug, Args)]
pub struct ForwardArgs {
    /// Mail item to forward (short ID or UUIDv7).
    pub id: String,

    /// Forward recipient email address (repeatable).
    #[arg(long)]
    pub to: Vec<String>,

    /// Attach the original raw MIME message as a message/rfc822 attachment.
    #[arg(long)]
    pub attach_original: bool,

    /// Read commentary body from standard input.
    #[arg(long)]
    pub body_stdin: bool,

    /// Open editor with prefilled template.
    #[arg(long)]
    pub edit: bool,

    /// Block until send completes.
    #[arg(long)]
    pub now: bool,

    /// Save as draft instead of sending.
    #[arg(long)]
    pub save_draft: bool,
}

#[derive(Debug, Args)]
pub struct AttachmentArgs {
    #[command(subcommand)]
    pub command: AttachmentCommands,
}

#[derive(Debug, Subcommand)]
pub enum AttachmentCommands {
    /// List attachments for a message item.
    List {
        /// Item identifier.
        id: String,
    },
    /// Download an attachment part to disk.
    Download {
        /// Item identifier.
        id: String,
        /// MIME section ID (e.g. "2", "1.2").
        section_id: String,
        /// Output destination path (defaults to original filename in current directory).
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
    /// Download and open an attachment with the desktop default application (xdg-open).
    Open {
        /// Item identifier.
        id: String,
        /// MIME section ID (e.g. "2", "1.2").
        section_id: String,
    },
}

#[derive(Debug, Args)]
pub struct SyncArgs {
    /// Scope to a specific account name or ID.
    #[arg(short = 'a', long)]
    pub account: Option<String>,

    /// Scope to a specific mail folder name or role.
    #[arg(long)]
    pub folder: Option<String>,

    /// Scope to a specific feed name or URL.
    #[arg(long)]
    pub feed: Option<String>,

    /// Block and wait until sync job finishes.
    #[arg(long)]
    pub wait: bool,

    /// Force a full refetch, ignoring cached sync state.
    #[arg(long)]
    pub full: bool,
}

#[derive(Debug, Args)]
pub struct DaemonArgs {
    #[command(subcommand)]
    pub command: DaemonCommands,
}

#[derive(Debug, Subcommand)]
pub enum DaemonCommands {
    /// Report running status, accounts, and pending queue.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Stop the background daemon user service.
    Stop,
    /// Reload configuration and accounts in the background daemon.
    Reload,
    /// View systemd journal logs for the daemon service.
    Logs {
        /// Follow live journal output.
        #[arg(short = 'f', long)]
        follow: bool,
    },
}

#[derive(Debug, Args)]
pub struct DoctorArgs {
    #[arg(long)]
    pub json: bool,
}
