use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::NaiveDate;
use gray_matter::engine::YAML;
use gray_matter::Matter;
use serde::Deserialize;

use crate::parse::FetchedEmail;

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
        .filter(|e| {
            e.file_type().is_file()
                && e.path().extension().is_some_and(|ext| ext == "md")
        });

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
    if let Some(date_str) = date_field {
        if let Ok(dt) = chrono::DateTime::parse_from_rfc2822(date_str) {
            let display = dt.format("%Y-%m-%d").to_string();
            let sort = dt.format("%Y-%m-%dT%H:%M:%S").to_string();
            return (display, sort);
        }
    }

    if let Some(sent_str) = sent_at_field {
        if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(sent_str) {
            let display = dt.format("%Y-%m-%d").to_string();
            let sort = dt.format("%Y-%m-%dT%H:%M:%S").to_string();
            return (display, sort);
        }
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
                    let sort = format!(
                        "{}T{}:{}:00",
                        date_part,
                        &time_part[..2],
                        &time_part[2..4]
                    );
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
    pub signature_content: Option<String>,
    pub sent_dir: Option<PathBuf>,
    pub archive_dir: Option<PathBuf>,
    pub archive_server_name: String,
    pub drafts_dir: Option<PathBuf>,
    pub inbox_dir: Option<PathBuf>,
    pub mailboxes: Vec<MailboxInfo>,
    pub mailbox_counts: Vec<usize>,
    pub email_cache: Vec<Option<Vec<EmailEntry>>>,
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
}

impl AccountState {
    pub fn new(
        account_config: crate::config::AccountConfig,
        email_settings: &crate::config::EmailSettings,
    ) -> Self {
        let imap_config = crate::config::ImapConfig::load(&account_config).ok();
        let smtp_config = crate::config::SmtpConfig::load(&account_config).ok();

        let signature_content = if email_settings.include_signature {
            crate::config::load_signature(&account_config, None)
        } else {
            None
        };

        let sent_dir = account_config.mailboxes.sent.as_ref()
            .map(|m| crate::config::resolve_mailbox_local_path(&account_config, m));
        let archive_dir = account_config.mailboxes.archive.as_ref()
            .map(|m| crate::config::resolve_mailbox_local_path(&account_config, m));
        let archive_server_name = account_config.mailboxes.archive.as_ref()
            .map(|m| m.server.as_str())
            .unwrap_or("Archive")
            .to_string();
        let drafts_dir = crate::config::resolve_drafts_dir_from_config(&account_config);
        let inbox_dir = account_config.mailboxes.inbox.as_ref()
            .map(|m| crate::config::resolve_mailbox_local_path(&account_config, m));

        let mailboxes = build_mailboxes(&account_config);
        let n = mailboxes.len();
        let counts = count_all_emails(&mailboxes);

        Self {
            account_config,
            imap_config,
            smtp_config,
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
        }
    }
}

/// Result from a background CLI operation.
#[derive(Debug)]
pub enum BgResult {
    Fetch { account_index: usize, result: Result<String, String> },
    Sync { account_index: usize, result: Result<String, String> },
    Send { account_index: usize, result: Result<String, String> },
    SendApproved { account_index: usize, result: Result<String, String> },
    Archive { account_index: usize, result: Result<String, String> },
    Delete { account_index: usize, result: Result<String, String> },
    ToggleRead {
        account_index: usize,
        path: PathBuf,
        new_read_state: bool,
        result: Result<String, String>,
    },
    ServerSearch { result: Result<Vec<SearchHit>, String> },
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

/// Which pane currently has focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Sidebar,
    List,
    Headers,
    Preview,
    Search,
}

/// Messages that drive state transitions (TEA pattern).
#[derive(Debug)]
pub enum Message {
    Key(crossterm::event::KeyEvent),
    Resize(u16, u16),
    Quit,
    /// Background watcher detected new mail for a given account.
    MailboxChanged { account_index: usize },
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
    Archive,
    Delete,
    BatchArchive(Vec<PathBuf>),
    BatchDelete(Vec<PathBuf>),
    ToggleRead,
    MarkAsRead,
    BatchToggleRead(Vec<PathBuf>),
    CopyPath,
    OpenAttachment(PathBuf),
    Fetch,
    Sync,
    ServerSearch { query: String, targets: Vec<SearchTarget> },
    SearchResultOpen,
    SearchResultReply(bool),
    SearchResultForward,
    SearchResultArchive,
    SearchResultOpenInBrowser,
    OpenHtmlInBrowser(PathBuf),
}

/// Which destructive action a confirmation dialog is guarding.
#[derive(Debug, Clone)]
pub enum ConfirmAction {
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

/// Overlay state for choosing among multiple attachments.
pub struct AttachmentPicker {
    pub files: Vec<PathBuf>,
    pub selected: usize,
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
    let root = crate::config::resolve_root_dir(config)
        .unwrap_or_else(|| PathBuf::from(shellexpand::tilde("~/notes/email").into_owned()));

    let drafts_dir = crate::config::resolve_drafts_dir_from_config(config)
        .unwrap_or_else(|| root.join("drafts"));

    let mut result = Vec::new();

    let inbox_dir = config.mailboxes.inbox.as_ref()
        .map(|m| crate::config::resolve_mailbox_local_path(config, m))
        .unwrap_or_else(|| root.join("inbox"));
    result.push(MailboxInfo {
        label: "Inbox".to_string(),
        icon: "\u{f0172}",
        dir: inbox_dir,
        kind: MailboxKind::Inbox,
        server_name: config.mailboxes.inbox.as_ref().map(|m| m.server.clone()),
    });

    result.push(MailboxInfo {
        label: "Drafts".to_string(),
        icon: "\u{f03eb}",
        dir: drafts_dir,
        kind: MailboxKind::Drafts,
        server_name: None,
    });

    let sent_dir = config.mailboxes.sent.as_ref()
        .map(|m| crate::config::resolve_mailbox_local_path(config, m))
        .unwrap_or_else(|| root.join("sent"));
    result.push(MailboxInfo {
        label: "Sent".to_string(),
        icon: "\u{f046b}",
        dir: sent_dir,
        kind: MailboxKind::Sent,
        server_name: config.mailboxes.sent.as_ref().map(|m| m.server.clone()),
    });

    let archive_dir = config.mailboxes.archive.as_ref()
        .map(|m| crate::config::resolve_mailbox_local_path(config, m))
        .unwrap_or_else(|| root.join("archive"));
    result.push(MailboxInfo {
        label: "Archive".to_string(),
        icon: "\u{f013c}",
        dir: archive_dir,
        kind: MailboxKind::Archive,
        server_name: config.mailboxes.archive.as_ref().map(|m| m.server.clone()),
    });

    if let Some(ref extras) = config.mailboxes.extra {
        for m in extras {
            result.push(MailboxInfo {
                label: m.server.clone(),
                icon: "\u{f0247}",
                dir: crate::config::resolve_mailbox_local_path(config, m),
                kind: MailboxKind::Extra,
                server_name: Some(m.server.clone()),
            });
        }
    }

    result
}

pub fn count_all_emails(mailboxes: &[MailboxInfo]) -> Vec<usize> {
    mailboxes.iter().map(|mb| {
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
    }).collect()
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
        assert_eq!(extract_display_name("\"John Doe\" <john@x.com>"), "John Doe");
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
        assert_eq!(display, "2024-06-15");
        assert!(sort.starts_with("2024-06-15"));
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
}
