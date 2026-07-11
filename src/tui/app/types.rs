use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use chrono::NaiveDate;
use gray_matter::engine::YAML;
use gray_matter::Matter;
use serde::Deserialize;

use crate::parse::{FetchedEmail, scan_mailbox_message_ids};
use crate::imap_client::{load_mailbox_states_cache, MailboxState, MessageIdIndex};

// ---------------------------------------------------------------------------
// EmailEntry (ported from beautifulmail's email.rs)
// ---------------------------------------------------------------------------

/// Parsed email entry for display in the list and preview.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct EmailEntry {
    pub path: PathBuf,
    pub from: String,
    pub to: String,
    pub cc: Option<String>,
    pub subject: String,
    pub status: String,
    pub date_display: String,
    pub date_sort: String,
    pub body: String,
    pub has_attachments: bool,
    pub read: bool,
}

impl EmailEntry {
    /// The contact to display depends on the mailbox kind:
    /// Inbox/Archive/Extra show `from`, Drafts/Sent show `to`.
    pub fn display_contact(&self, kind: MailboxKind) -> &str {
        match kind {
            MailboxKind::Inbox | MailboxKind::Archive | MailboxKind::Extra => &self.from,
            MailboxKind::Drafts | MailboxKind::Sent => &self.to,
        }
    }
}

/// Raw frontmatter fields (all optional to handle varying formats).
#[derive(Debug, Deserialize, Default)]
struct Frontmatter {
    from: Option<String>,
    to: Option<String>,
    cc: Option<String>,
    subject: Option<String>,
    status: Option<String>,
    date: Option<String>,
    sent_at: Option<String>,
    has_attachments: Option<bool>,
    #[allow(dead_code)]
    attachments: Option<Vec<String>>,
    read: Option<bool>,
}

/// Load all emails from a directory.
pub fn load_emails(dir: &Path) -> Vec<EmailEntry> {
    let mut entries = Vec::new();

    let walker = walkdir::WalkDir::new(dir)
        .max_depth(1)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file() && e.path().extension().is_some_and(|ext| ext == "md"));

    for entry in walker {
        match parse_email(entry.path()) {
            Ok(email) => entries.push(email),
            Err(_) => continue,
        }
    }

    // Sort by date descending (newest first)
    entries.sort_by(|a, b| b.date_sort.cmp(&a.date_sort));
    entries
}

/// Parse a single email markdown file.
fn parse_email(path: &Path) -> Result<EmailEntry> {
    let content = std::fs::read_to_string(path)?;
    let matter = Matter::<YAML>::new();
    let result = matter.parse(&content);

    let fm: Frontmatter = result
        .data
        .and_then(|d| d.deserialize().ok())
        .unwrap_or_default();

    let body = result.content;

    let from = fm.from.unwrap_or_default();
    let to = fm.to.unwrap_or_default();
    let subject = fm.subject.unwrap_or_else(|| "(no subject)".to_string());
    let status = fm.status.unwrap_or_else(|| "unknown".to_string());

    let (date_display, date_sort) = resolve_date(&fm.date, &fm.sent_at, path);

    Ok(EmailEntry {
        path: path.to_path_buf(),
        from: extract_display_name(&from),
        to: extract_display_name(&to),
        cc: fm.cc,
        subject,
        status,
        date_display,
        date_sort,
        body,
        has_attachments: fm.has_attachments.unwrap_or(false),
        read: fm.read.unwrap_or(false),
    })
}

/// Extract a short display name from an email address.
pub fn extract_display_name(addr: &str) -> String {
    let addr = addr.trim().trim_matches('"');
    if let Some(idx) = addr.find('<') {
        let name = addr[..idx].trim().trim_matches('"');
        if name.is_empty() {
            addr.trim_matches(|c| c == '<' || c == '>').to_string()
        } else {
            name.to_string()
        }
    } else {
        addr.to_string()
    }
}

/// Resolve date for display and sorting.
pub fn resolve_date(
    date_field: &Option<String>,
    sent_at_field: &Option<String>,
    path: &Path,
) -> (String, String) {
    // Sort key is always formatted in UTC so that emails from different
    // timezones on the same calendar day order by actual instant, not by
    // sender-local wallclock. Display stays in the sender's local time so
    // dates match what the user sees in other clients.
    if let Some(date_str) = date_field {
        if let Ok(dt) = chrono::DateTime::parse_from_rfc2822(date_str) {
            let display = dt.format("%Y-%m-%d").to_string();
            let sort = dt
                .with_timezone(&chrono::Utc)
                .format("%Y-%m-%dT%H:%M:%S")
                .to_string();
            return (display, sort);
        }
    }

    if let Some(sent_str) = sent_at_field {
        if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(sent_str) {
            let display = dt.format("%Y-%m-%d").to_string();
            let sort = dt
                .with_timezone(&chrono::Utc)
                .format("%Y-%m-%dT%H:%M:%S")
                .to_string();
            return (display, sort);
        }
        // Trailing 'Z' means UTC, so the naive value is already in UTC.
        if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(sent_str, "%Y-%m-%dT%H:%M:%SZ") {
            let display = dt.format("%Y-%m-%d").to_string();
            let sort = dt.format("%Y-%m-%dT%H:%M:%S").to_string();
            return (display, sort);
        }
    }

    let filename = path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();

    if filename.len() >= 10 {
        let date_part = &filename[..10];
        if NaiveDate::parse_from_str(date_part, "%Y-%m-%d").is_ok() {
            if filename.len() >= 15 && filename.as_bytes()[10] == b'-' {
                let time_part = &filename[11..15];
                if time_part.chars().all(|c| c.is_ascii_digit()) && time_part.len() == 4 {
                    let sort = format!("{}T{}:{}:00", date_part, &time_part[..2], &time_part[2..4]);
                    return (date_part.to_string(), sort);
                }
            }
            return (date_part.to_string(), format!("{date_part}T00:00:00"));
        }
    }

    ("".to_string(), "".to_string())
}

// ---------------------------------------------------------------------------
// App types
// ---------------------------------------------------------------------------

/// Per-account TUI state (config, caches, cursor positions).
pub struct AccountState {
    pub account_config: crate::config::AccountConfig,
    pub imap_config: Option<crate::config::ImapConfig>,
    pub smtp_config: Option<crate::config::SmtpConfig>,
    pub graph_config: Option<crate::config::GraphConfig>,
    pub signature_content: Option<String>,
    pub sent_dir: Option<PathBuf>,
    pub archive_dir: Option<PathBuf>,
    pub archive_server_name: String,
    pub drafts_dir: Option<PathBuf>,
    pub inbox_dir: Option<PathBuf>,
    pub mailboxes: Vec<MailboxInfo>,
    pub mailbox_counts: Vec<usize>,
    /// Per-mailbox cache of parsed entries. `Arc` so switching mailboxes
    /// or accounts shares the allocation with `App::emails` instead of
    /// deep-cloning thousands of entries (P2); see
    /// `App::with_emails_mut` for the mutation strategy.
    pub email_cache: Vec<Option<Arc<Vec<EmailEntry>>>>,
    pub sidebar_index: usize,
    pub active_mailbox: usize,
    pub list_index: usize,
    pub headers_scroll: u16,
    pub preview_scroll: u16,
    pub selection: std::collections::HashSet<PathBuf>,
    pub search_query: String,
    pub search_includes_body: bool,
    pub bg_mutations: usize,
    pub watcher_active: bool,
    pub has_unseen: bool,
    /// In-memory index: local_dir -> {message_id -> file_path}.
    /// Initialised empty at startup and populated asynchronously by a
    /// background indexing thread (see `BgResult::IndexReady`); updated
    /// incrementally during sync/mutations.
    pub message_id_index: MessageIdIndex,
    /// True between `AccountState::new` and the arrival of
    /// `BgResult::IndexReady` for this account. While true,
    /// `message_id_index` is empty (or partially populated) and
    /// sync/fetch actions are queued via the existing `bg_count` gate.
    pub indexing: bool,
    /// Last-seen IMAP mailbox states, keyed by role (e.g. "inbox", "archive").
    /// Used to decide whether reconciliation is needed on quick sync.
    pub mailbox_states: std::collections::HashMap<String, MailboxState>,
}

impl AccountState {
    pub fn new(
        account_config: crate::config::AccountConfig,
        email_settings: &crate::config::EmailSettings,
    ) -> Self {
        let imap_config = crate::config::ImapConfig::load(&account_config).ok();
        let smtp_config = crate::config::SmtpConfig::load(&account_config).ok();
        let graph_config = crate::config::GraphConfig::load(&account_config).ok();

        let signature_content = if email_settings.include_signature {
            crate::config::load_signature(&account_config, None)
        } else {
            None
        };

        let sent_dir = account_config
            .mailboxes
            .sent
            .as_ref()
            .map(|_| crate::config::mailbox_dir(&account_config.name, "sent"));
        let archive_dir = account_config
            .mailboxes
            .archive
            .as_ref()
            .map(|_| crate::config::mailbox_dir(&account_config.name, "archive"));
        let archive_server_name = account_config
            .mailboxes
            .archive
            .as_ref()
            .map(|m| m.server.as_str())
            .unwrap_or("Archive")
            .to_string();
        let drafts_dir = Some(crate::config::drafts_dir(&account_config.name));
        let inbox_dir = account_config
            .mailboxes
            .inbox
            .as_ref()
            .map(|_| crate::config::mailbox_dir(&account_config.name, "inbox"));

        let mut span = crate::timing::TimingSpan::with_context(
            "AccountState::new",
            account_config.name.clone(),
        );
        let mailboxes = build_mailboxes(&account_config);
        let n = mailboxes.len();
        let counts = count_all_emails(&mailboxes);
        span.mark(&format!("built {} mailbox(es)", n));

        // The in-memory message-ID index is built off the main thread
        // (see `build_message_id_index` and `BgResult::IndexReady`) so
        // first-frame render is not blocked by walking ~17 k frontmatter
        // files. Until the index arrives, sync/fetch actions are queued
        // via the existing `bg_count` gate in `tui/actions.rs`.
        let message_id_index: MessageIdIndex = std::collections::HashMap::new();

        // Load persisted IMAP mailbox states so the first quick sync after
        // launch can take the reconcile-skip branch (avoiding the ~14 s
        // full Message-ID scan that would otherwise fire when prev_states
        // is empty). `uid_validity` mismatch on the next SELECT falls back
        // to a full reconcile, so this is safe across other-client
        // mutations / mailbox renumbering.
        let account_root = crate::config::account_dir(&account_config.name);
        let mailbox_states = load_mailbox_states_cache(&account_root);
        span.mark(&format!("loaded {} mailbox state(s)", mailbox_states.len()));

        Self {
            account_config,
            imap_config,
            smtp_config,
            graph_config,
            signature_content,
            sent_dir,
            archive_dir,
            archive_server_name,
            drafts_dir,
            inbox_dir,
            mailboxes,
            mailbox_counts: counts,
            email_cache: vec![None; n],
            sidebar_index: 0,
            active_mailbox: 0,
            list_index: 0,
            headers_scroll: 0,
            preview_scroll: 0,
            selection: std::collections::HashSet::new(),
            search_query: String::new(),
            search_includes_body: false,
            bg_mutations: 0,
            watcher_active: false,
            has_unseen: false,
            message_id_index,
            indexing: true,
            mailbox_states,
        }
    }

    pub fn is_graph(&self) -> bool {
        self.account_config.auth_method == crate::config::AuthMethod::Graph
    }
}

/// Result from a background CLI operation.
#[derive(Debug)]
pub enum BgResult {
    Fetch {
        account_index: usize,
        result: Result<String, String>,
        /// Updated message ID index after sync (for merging back into AccountState).
        new_index: Option<MessageIdIndex>,
        /// Updated mailbox states from IMAP SELECT (for state-based reconciliation).
        new_mailbox_states: Option<std::collections::HashMap<String, MailboxState>>,
        /// Local mailbox directories the sync actually modified on disk.
        /// `Some(vec![])` means the sync was a no-op (skip cache invalidation
        /// and reload entirely); `None` means unknown (fall back to
        /// invalidating everything, the pre-optimization behavior).
        touched_dirs: Option<Vec<PathBuf>>,
        /// Sender + subject of every genuinely new inbox email the sync
        /// saved. Drives the opt-in desktop notification (#0009); empty
        /// on failure or when nothing new arrived.
        new_inbox_mail: Vec<crate::notify::NewMailMeta>,
    },
    Sync {
        account_index: usize,
        result: Result<String, String>,
        /// Updated message ID index after full sync.
        new_index: Option<MessageIdIndex>,
        /// Updated mailbox states from IMAP SELECT.
        new_mailbox_states: Option<std::collections::HashMap<String, MailboxState>>,
    },
    Send {
        account_index: usize,
        result: Result<String, String>,
    },
    SendApproved {
        account_index: usize,
        result: Result<String, String>,
    },
    Archive {
        account_index: usize,
        result: Result<String, String>,
    },
    /// A quick-move to an arbitrary mailbox finished (#0018). Carries
    /// the source + destination mailbox indices and the destination
    /// label so the handler can invalidate the right caches (the user
    /// may have switched mailboxes while the move was in flight) and
    /// report where the email went.
    Move {
        account_index: usize,
        source_mailbox_idx: usize,
        dest_mailbox_idx: usize,
        dest_label: String,
        result: Result<String, String>,
    },
    Delete {
        account_index: usize,
        result: Result<String, String>,
    },
    ToggleRead {
        account_index: usize,
        path: PathBuf,
        new_read_state: bool,
        result: Result<String, String>,
    },
    ServerSearch {
        result: Result<Vec<SearchHit>, String>,
    },
    /// The async startup index scan has finished for one account.
    /// Carries the freshly built `message_id_index` to be merged into
    /// `AccountState`. See `build_message_id_index` and ticket #0003.
    IndexReady {
        account_index: usize,
        index: MessageIdIndex,
    },
    /// A background `load_emails` walk of one mailbox directory has
    /// finished (P1 step 2: the walk blocks for seconds on large
    /// mailboxes, so it runs off the UI thread). Applied only while
    /// `generation` still matches `App::mailbox_load_generation` --
    /// stale/out-of-order results are dropped (see
    /// `mailbox_loaded_disposition` in `tui/bg.rs`).
    MailboxLoaded {
        account_index: usize,
        mailbox_idx: usize,
        generation: u64,
        entries: Vec<EmailEntry>,
    },
}

/// Walk every mailbox directory of an account and build the
/// `message_id_index` (`local_dir -> {message_id -> file_path}`).
///
/// Extracted from `AccountState::new` so it can run on a background
/// thread at TUI launch (ticket #0003). Per-dir `[TIMING]` log lines
/// are preserved verbatim so cold-start observability is unchanged.
pub fn build_message_id_index(
    mailboxes: &[MailboxInfo],
    account_name: &str,
) -> MessageIdIndex {
    let mut span = crate::timing::TimingSpan::with_context(
        "build_message_id_index",
        account_name.to_string(),
    );
    let mut message_id_index: MessageIdIndex = std::collections::HashMap::new();
    let mut total_indexed = 0usize;
    for mb in mailboxes {
        if mb.dir.is_dir() {
            let t0 = std::time::Instant::now();
            if let Ok(ids) = scan_mailbox_message_ids(&mb.dir) {
                let n = ids.len();
                total_indexed += n;
                log::info!(
                    "[TIMING] AccountState::new [{}] scan {} ({} files): {} ms",
                    account_name,
                    mb.dir.display(),
                    n,
                    t0.elapsed().as_millis()
                );
                message_id_index.insert(mb.dir.clone(), ids);
            }
        }
    }
    span.mark(&format!("indexed {} files total", total_indexed));
    message_id_index
}

/// A mailbox target for server search.
#[derive(Debug, Clone)]
pub struct SearchTarget {
    pub server_name: String,
    pub local_dir: PathBuf,
    pub status: String,
    pub label: String,
}

/// A single search result with source metadata (returned from background task).
#[derive(Debug, Clone)]
pub struct SearchHit {
    pub entry: EmailEntry,
    pub fetched: FetchedEmail,
    pub source_label: String,
    pub source_local_dir: PathBuf,
    pub source_status: String,
}

/// A single server search result held in memory (with save state).
#[derive(Debug, Clone)]
pub struct SearchResultEntry {
    pub entry: EmailEntry,
    pub fetched: FetchedEmail,
    pub saved_path: Option<PathBuf>,
    pub source_label: String,
    pub source_local_dir: PathBuf,
    pub source_status: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchOverlayFocus {
    Input,
    List,
}

// ---------------------------------------------------------------------------
// Compose wizard
// ---------------------------------------------------------------------------

/// Which field of the compose wizard is currently focused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComposeField {
    To,
    Cc,
    Bcc,
    Subject,
}

impl ComposeField {
    pub fn label(&self) -> &'static str {
        match self {
            ComposeField::To => "To",
            ComposeField::Cc => "Cc",
            ComposeField::Bcc => "Bcc",
            ComposeField::Subject => "Subject",
        }
    }

    pub fn is_address(&self) -> bool {
        matches!(
            self,
            ComposeField::To | ComposeField::Cc | ComposeField::Bcc
        )
    }

    pub fn next(&self) -> Self {
        match self {
            ComposeField::To => ComposeField::Cc,
            ComposeField::Cc => ComposeField::Bcc,
            ComposeField::Bcc => ComposeField::Subject,
            ComposeField::Subject => ComposeField::To,
        }
    }

    pub fn prev(&self) -> Self {
        match self {
            ComposeField::To => ComposeField::Subject,
            ComposeField::Cc => ComposeField::To,
            ComposeField::Bcc => ComposeField::Cc,
            ComposeField::Subject => ComposeField::Bcc,
        }
    }
}

/// Wizard mode: blank new draft, forward from an existing email, or
/// edit the recipient/subject fields of an existing draft in place.
#[derive(Debug, Clone)]
pub enum ComposeMode {
    New,
    Forward { source_path: PathBuf },
    EditDraft { source_path: PathBuf },
}

/// A fuzzy-matched contact suggestion under the focused address field.
#[derive(Debug, Clone)]
pub struct ComposeSuggestion {
    pub address: String,
    pub display_name: String,
    /// Tier marker for visual: 0=received, 1=sent-cc, 2=sent-to.
    pub tier: u8,
}

/// State for the compose wizard overlay.
pub struct ComposeWizard {
    pub mode: ComposeMode,
    pub to: String,
    pub cc: String,
    pub bcc: String,
    pub subject: String,
    pub focus: ComposeField,
    /// Fuzzy-matched suggestions for the currently-focused field
    /// (empty for Subject or when no cache exists).
    pub suggestions: Vec<ComposeSuggestion>,
    pub suggestion_idx: usize,
    /// The contact index for the active account, loaded once when the
    /// wizard opens. `None` means "no cache yet — run rebuild first".
    pub contacts: Option<crate::contacts::ContactIndex>,
}

/// Which pane currently has focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Sidebar,
    List,
    Headers,
    Preview,
    Search,
    ComposeWizard,
}

/// Messages that drive state transitions (TEA pattern).
#[derive(Debug)]
pub enum Message {
    Key(crossterm::event::KeyEvent),
    Resize(u16, u16),
    Quit,
    /// Background watcher detected new mail for a given account.
    MailboxChanged {
        account_index: usize,
    },
}

/// Behavioral kind of a mailbox (used for action differentiation).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailboxKind {
    Inbox,
    Drafts,
    Sent,
    Archive,
    Extra,
}

/// A mailbox entry with its metadata and resolved path.
#[derive(Debug, Clone)]
pub struct MailboxInfo {
    pub label: String,
    pub icon: &'static str,
    pub dir: PathBuf,
    pub kind: MailboxKind,
    pub server_name: Option<String>,
}

/// Side-effects that the main loop must execute (keeps update pure).
#[derive(Debug)]
pub enum Action {
    EditCurrent,
    Reply(bool),
    Forward,
    Send,
    SendApproved,
    NewDraft,
    Approve,
    BatchApprove(Vec<PathBuf>),
    /// Demote a single approved draft back to `draft` status (#0021).
    MarkDraft,
    /// Batch variant for `Action::MarkDraft` -- run mark_as_draft over
    /// the current selection.
    BatchMarkDraft(Vec<PathBuf>),
    Archive,
    Delete,
    BatchArchive(Vec<PathBuf>),
    BatchDelete(Vec<PathBuf>),
    /// Quick-move emails to another mailbox (#0018). `paths` is the
    /// selection (or the cursor email); `dest_idx` indexes
    /// `App::mailboxes`.
    MoveToMailbox {
        paths: Vec<PathBuf>,
        dest_idx: usize,
    },
    ToggleRead,
    MarkAsRead,
    BatchToggleRead(Vec<PathBuf>),
    CopyPath,
    /// Open the newest log file from `logs_dir()` in `$EDITOR` (#0025).
    OpenLogFile,
    /// Open the global config file (`config_path()`) in `$EDITOR`. Changes
    /// apply on restart (theme is `OnceLock`; no hot-reload).
    OpenConfigFile,
    OpenAttachment(PathBuf),
    SaveAttachments {
        sources: Vec<PathBuf>,
        dest_dir: PathBuf,
    },
    Fetch,
    /// Walk one mailbox directory of the active account off the UI
    /// thread (P1 step 2). Queued by `App::request_mailbox_load` on
    /// every cache-miss switch/reload; the result arrives as
    /// `BgResult::MailboxLoaded` carrying the same `generation`.
    LoadMailbox {
        mailbox_idx: usize,
        generation: u64,
    },
    /// Quick-sync a specific account by index. Used for the per-account
    /// startup auto-fetch (#0001) -- caller must guarantee that account's
    /// `message_id_index` is ready (i.e. its `BgResult::IndexReady`
    /// has already been handled).
    FetchAccount(usize),
    Sync,
    ServerSearch {
        query: String,
        targets: Vec<SearchTarget>,
    },
    SearchResultOpen,
    SearchResultReply(bool),
    SearchResultForward,
    SearchResultArchive,
    SearchResultOpenInBrowser,
    OpenHtmlInBrowser(PathBuf),
    /// Open the compose wizard overlay (new or forward).
    OpenComposeWizard(ComposeMode),
    /// Commit the wizard: write the draft file and launch $EDITOR.
    ComposeWizardSubmit,
    /// Close the wizard without writing anything.
    ComposeWizardCancel,
}

/// Which destructive action a confirmation dialog is guarding.
#[derive(Debug, Clone)]
pub enum ConfirmAction {
    Approve,
    MarkDraft,
    Archive,
    Delete,
    Send,
    SendApproved,
}

/// Data for rendering the confirmation dialog overlay.
#[derive(Debug, Clone)]
pub struct ConfirmDialog {
    pub title: String,
    pub detail: String,
    pub action: ConfirmAction,
}

/// Persistent error notification (requires user action to dismiss).
pub struct PersistentError {
    pub message: String,
}

/// Whether the attachment picker is opening or saving.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachmentPickerMode {
    Open,
    Save,
}

/// Overlay state for choosing among multiple attachments.
pub struct AttachmentPicker {
    pub files: Vec<PathBuf>,
    pub selected: usize,
    pub mode: AttachmentPickerMode,
    /// Set of selected indices (used in Save mode for multi-select).
    pub selected_set: HashSet<usize>,
}

/// Overlay state for the quick-move mailbox picker (#0018): a fuzzy
/// type-to-filter list of destination mailboxes, opened with `M` from
/// the email list.
pub struct MailboxPicker {
    /// Type-to-filter query.
    pub query: String,
    /// Candidate destinations: `(mailbox index into App::mailboxes,
    /// label)`. Excludes the active mailbox and any mailbox without a
    /// server-side folder.
    pub candidates: Vec<(usize, String)>,
    /// Indices into `candidates` matching the current query.
    pub filtered: Vec<usize>,
    /// Cursor position in `filtered`.
    pub selected: usize,
    /// Emails to move (current selection, or the cursor email).
    pub paths: Vec<PathBuf>,
}

/// Whether the directory picker is in zoxide or browser mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirPickerMode {
    Zoxide,
    Browser,
}

/// Overlay for picking a target directory (for saving attachments).
pub struct DirPicker {
    pub mode: DirPickerMode,
    /// Text input for zoxide query.
    pub query: String,
    /// Results from `zoxide query --list`.
    pub zoxide_results: Vec<PathBuf>,
    /// Whether zoxide is available on this system.
    pub zoxide_available: bool,
    /// Current directory in browser mode.
    pub current_dir: PathBuf,
    /// Subdirectories of `current_dir` (browser mode listing).
    pub dir_entries: Vec<PathBuf>,
    /// Cursor position in the result/entry list.
    pub selected: usize,
    /// The attachment files to save (carried from the attachment picker).
    pub sources: Vec<PathBuf>,
}

pub const STATUS_LOG_CAPACITY: usize = 100;

#[derive(Debug, Clone)]
pub enum StatusLevel {
    Info,
    Success,
    Warning,
    Error,
    Progress,
}

#[derive(Debug, Clone)]
pub struct StatusEntry {
    pub timestamp: chrono::DateTime<chrono::Local>,
    pub message: String,
    pub level: StatusLevel,
}

// ---------------------------------------------------------------------------
// Mailbox helpers (free functions)
// ---------------------------------------------------------------------------

pub fn kind_to_status(kind: MailboxKind) -> String {
    match kind {
        MailboxKind::Inbox | MailboxKind::Extra => "inbox".to_string(),
        MailboxKind::Archive => "archived".to_string(),
        MailboxKind::Sent => "sent".to_string(),
        MailboxKind::Drafts => "draft".to_string(),
    }
}

pub fn build_mailboxes(config: &crate::config::AccountConfig) -> Vec<MailboxInfo> {
    let name = &config.name;
    let mut result = Vec::new();

    result.push(MailboxInfo {
        label: "Inbox".to_string(),
        icon: "\u{f0172}",
        dir: crate::config::mailbox_dir(name, "inbox"),
        kind: MailboxKind::Inbox,
        server_name: config.mailboxes.inbox.as_ref().map(|m| m.server.clone()),
    });

    result.push(MailboxInfo {
        label: "Drafts".to_string(),
        icon: "\u{f03eb}",
        dir: crate::config::drafts_dir(name),
        kind: MailboxKind::Drafts,
        server_name: None,
    });

    result.push(MailboxInfo {
        label: "Sent".to_string(),
        icon: "\u{f046b}",
        dir: crate::config::mailbox_dir(name, "sent"),
        kind: MailboxKind::Sent,
        server_name: config.mailboxes.sent.as_ref().map(|m| m.server.clone()),
    });

    result.push(MailboxInfo {
        label: "Archive".to_string(),
        icon: "\u{f013c}",
        dir: crate::config::mailbox_dir(name, "archive"),
        kind: MailboxKind::Archive,
        server_name: config.mailboxes.archive.as_ref().map(|m| m.server.clone()),
    });

    if let Some(ref extras) = config.mailboxes.extra {
        for m in extras {
            result.push(MailboxInfo {
                label: m.server.clone(),
                icon: "\u{f0247}",
                dir: crate::config::mailbox_dir(name, &m.server),
                kind: MailboxKind::Extra,
                server_name: Some(m.server.clone()),
            });
        }
    }

    result
}

pub fn count_all_emails(mailboxes: &[MailboxInfo]) -> Vec<usize> {
    mailboxes
        .iter()
        .map(|mb| {
            if mb.dir.is_dir() {
                walkdir::WalkDir::new(&mb.dir)
                    .max_depth(1)
                    .into_iter()
                    .filter_map(|e| e.ok())
                    .filter(|e| {
                        e.file_type().is_file()
                            && e.path().extension().is_some_and(|ext| ext == "md")
                    })
                    .count()
            } else {
                0
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    // -----------------------------------------------------------------------
    // extract_display_name
    // -----------------------------------------------------------------------

    #[test]
    fn test_extract_display_name_full_address() {
        assert_eq!(extract_display_name("John Doe <john@x.com>"), "John Doe");
    }

    #[test]
    fn test_extract_display_name_bare_email() {
        assert_eq!(extract_display_name("john@x.com"), "john@x.com");
    }

    #[test]
    fn test_extract_display_name_no_name() {
        assert_eq!(extract_display_name("<john@x.com>"), "john@x.com");
    }

    #[test]
    fn test_extract_display_name_quoted() {
        assert_eq!(
            extract_display_name("\"John Doe\" <john@x.com>"),
            "John Doe"
        );
    }

    #[test]
    fn test_extract_display_name_empty() {
        assert_eq!(extract_display_name(""), "");
    }

    // -----------------------------------------------------------------------
    // resolve_date
    // -----------------------------------------------------------------------

    #[test]
    fn test_resolve_date_rfc2822() {
        let date = Some("Mon, 01 Jan 2024 12:00:00 +0000".to_string());
        let (display, sort) = resolve_date(&date, &None, Path::new("test.md"));
        assert_eq!(display, "2024-01-01");
        assert_eq!(sort, "2024-01-01T12:00:00");
    }

    #[test]
    fn test_resolve_date_rfc3339_sent_at() {
        let sent = Some("2024-06-15T14:30:00+02:00".to_string());
        let (display, sort) = resolve_date(&None, &sent, Path::new("test.md"));
        // Display is in sender-local time, sort key is normalised to UTC.
        assert_eq!(display, "2024-06-15");
        assert_eq!(sort, "2024-06-15T12:30:00");
    }

    /// Two emails on the same day from different timezones must sort by the
    /// actual instant, not by sender-local wallclock. Regression test for
    /// ticket #0024.
    #[test]
    fn test_resolve_date_sort_normalises_timezone() {
        // 10:00 +0200 == 08:00 UTC (earlier instant)
        let early = Some("Mon, 06 May 2024 10:00:00 +0200".to_string());
        // 09:30 +0000 == 09:30 UTC (later instant)
        let late = Some("Mon, 06 May 2024 09:30:00 +0000".to_string());
        let (_, sort_early) = resolve_date(&early, &None, Path::new("a.md"));
        let (_, sort_late) = resolve_date(&late, &None, Path::new("b.md"));
        assert!(
            sort_late > sort_early,
            "expected later UTC instant to sort higher: late={sort_late} early={sort_early}"
        );

        // Same check for the RFC3339 sent_at branch.
        let early_rfc3339 = Some("2024-05-06T10:00:00+02:00".to_string());
        let late_rfc3339 = Some("2024-05-06T09:30:00+00:00".to_string());
        let (_, sort_early) = resolve_date(&None, &early_rfc3339, Path::new("a.md"));
        let (_, sort_late) = resolve_date(&None, &late_rfc3339, Path::new("b.md"));
        assert!(
            sort_late > sort_early,
            "expected later UTC instant to sort higher: late={sort_late} early={sort_early}"
        );
    }

    #[test]
    fn test_resolve_date_naive_sent_at() {
        let sent = Some("2024-06-15T14:30:00Z".to_string());
        let (display, sort) = resolve_date(&None, &sent, Path::new("test.md"));
        assert_eq!(display, "2024-06-15");
        assert_eq!(sort, "2024-06-15T14:30:00");
    }

    #[test]
    fn test_resolve_date_filename_fallback() {
        let path = Path::new("2024-03-15-1430_sender_subject.md");
        let (display, sort) = resolve_date(&None, &None, path);
        assert_eq!(display, "2024-03-15");
        assert_eq!(sort, "2024-03-15T14:30:00");
    }

    #[test]
    fn test_resolve_date_filename_date_only() {
        let path = Path::new("2024-03-15_sender_subject.md");
        let (display, sort) = resolve_date(&None, &None, path);
        assert_eq!(display, "2024-03-15");
        assert_eq!(sort, "2024-03-15T00:00:00");
    }

    #[test]
    fn test_resolve_date_all_missing() {
        let path = Path::new("random-name.md");
        let (display, sort) = resolve_date(&None, &None, path);
        assert_eq!(display, "");
        assert_eq!(sort, "");
    }

    // -----------------------------------------------------------------------
    // build_message_id_index (ticket #0003)
    // -----------------------------------------------------------------------

    fn write_msg(dir: &Path, filename: &str, message_id: &str) {
        std::fs::write(
            dir.join(filename),
            format!(
                "---\nmessage_id: {}\nsubject: test\n---\n\nbody\n",
                message_id
            ),
        )
        .unwrap();
    }

    fn mb(label: &str, dir: PathBuf, kind: MailboxKind) -> MailboxInfo {
        MailboxInfo {
            label: label.to_string(),
            icon: "",
            dir,
            kind,
            server_name: None,
        }
    }

    #[test]
    fn test_build_message_id_index_collects_per_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let inbox = tmp.path().join("inbox");
        let archive = tmp.path().join("archive");
        std::fs::create_dir_all(&inbox).unwrap();
        std::fs::create_dir_all(&archive).unwrap();

        write_msg(&inbox, "a.md", "<a@example.com>");
        write_msg(&inbox, "b.md", "<b@example.com>");
        write_msg(&archive, "c.md", "<c@example.com>");

        let mailboxes = vec![
            mb("Inbox", inbox.clone(), MailboxKind::Inbox),
            mb("Archive", archive.clone(), MailboxKind::Archive),
        ];

        let index = build_message_id_index(&mailboxes, "test-account");

        assert_eq!(index.len(), 2);
        assert_eq!(index.get(&inbox).unwrap().len(), 2);
        assert_eq!(index.get(&archive).unwrap().len(), 1);
        assert!(index.get(&inbox).unwrap().contains_key("<a@example.com>"));
        assert!(index.get(&archive).unwrap().contains_key("<c@example.com>"));
    }

    #[test]
    fn test_build_message_id_index_skips_missing_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let exists = tmp.path().join("inbox");
        std::fs::create_dir_all(&exists).unwrap();
        write_msg(&exists, "a.md", "<a@example.com>");
        let missing = tmp.path().join("does-not-exist");

        let mailboxes = vec![
            mb("Inbox", exists.clone(), MailboxKind::Inbox),
            mb("Archive", missing.clone(), MailboxKind::Archive),
        ];

        let index = build_message_id_index(&mailboxes, "test-account");
        // Missing dirs are silently skipped (no panic, no entry).
        assert!(index.contains_key(&exists));
        assert!(!index.contains_key(&missing));
    }

    #[test]
    fn test_build_message_id_index_empty_mailboxes() {
        let index = build_message_id_index(&[], "test-account");
        assert!(index.is_empty());
    }
}
