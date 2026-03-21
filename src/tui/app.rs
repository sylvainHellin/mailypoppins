use std::collections::HashSet;
use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::NaiveDate;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use gray_matter::engine::YAML;
use gray_matter::Matter;
use serde::Deserialize;

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
    })
}

/// Extract a short display name from an email address.
fn extract_display_name(addr: &str) -> String {
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
fn resolve_date(
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
// App types and state
// ---------------------------------------------------------------------------

/// Result from a background CLI operation.
#[derive(Debug)]
pub enum BgResult {
    Fetch { result: Result<String, String> },
    Sync { result: Result<String, String> },
    Send { result: Result<String, String> },
    SendApproved { result: Result<String, String> },
    Archive { result: Result<String, String> },
    Delete { result: Result<String, String> },
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
    Key(KeyEvent),
    Resize(u16, u16),
    Quit,
    /// Background watcher detected new mail.
    MailboxChanged,
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
    CopyPath,
    Fetch,
    Sync,
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

/// Top-level application state.
pub struct App {
    pub focus: Focus,
    pub running: bool,
    pub terminal_width: u16,
    pub terminal_height: u16,

    pub mailboxes: Vec<MailboxInfo>,
    pub sidebar_index: usize,
    pub active_mailbox: usize,
    pub mailbox_counts: Vec<usize>,

    pub emails: Vec<EmailEntry>,
    pub list_index: usize,
    pub g_pending: bool,
    pub headers_scroll: u16,
    pub preview_scroll: u16,
    pub selection: HashSet<PathBuf>,
    email_cache: Vec<Option<Vec<EmailEntry>>>,

    pub pending_action: Option<Action>,
    pub confirm_dialog: Option<ConfirmDialog>,
    pub status_message: Option<String>,
    pub status_ticks: u8,
    pub search_query: String,
    pub search_includes_body: bool,
    pub show_help: bool,
    pub help_scroll: u16,
    pub help_filter: String,
    pub help_filter_active: bool,
    pub watcher_active: bool,

    pub bg_count: usize,
    pub bg_mutations: usize,
    pub bg_spin_tick: usize,
    pub queued_action: Option<Action>,
    pub persistent_error: Option<PersistentError>,

    // Config (loaded once at startup)
    pub global_config: crate::config::GlobalConfig,
    pub imap_config: Option<crate::config::ImapConfig>,
    pub smtp_config: Option<crate::config::SmtpConfig>,
    pub signature_content: Option<String>,
    pub sent_dir: Option<PathBuf>,
    pub archive_dir: Option<PathBuf>,
    pub archive_server_name: String,
    pub drafts_dir: Option<PathBuf>,
    pub inbox_dir: Option<PathBuf>,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    pub fn new() -> Self {
        let global_config = crate::config::load_global_config().unwrap_or_default();
        let imap_config = crate::config::ImapConfig::load(&global_config).ok();
        let smtp_config = crate::config::SmtpConfig::load(&global_config).ok();

        let signature_content = if global_config.email.include_signature {
            crate::config::load_signature(&global_config, None)
        } else {
            None
        };

        let sent_dir = global_config.mailboxes.sent.as_ref()
            .map(|m| crate::config::resolve_mailbox_local_path(&global_config, m));
        let archive_dir = global_config.mailboxes.archive.as_ref()
            .map(|m| crate::config::resolve_mailbox_local_path(&global_config, m));
        let archive_server_name = global_config.mailboxes.archive.as_ref()
            .map(|m| m.server.as_str())
            .unwrap_or("Archive")
            .to_string();
        let drafts_dir = crate::config::resolve_drafts_dir_from_config(&global_config);
        let inbox_dir = global_config.mailboxes.inbox.as_ref()
            .map(|m| crate::config::resolve_mailbox_local_path(&global_config, m));

        let mailboxes = build_mailboxes(&global_config);
        let n = mailboxes.len();
        let counts = count_all_emails(&mailboxes);

        let emails = if !mailboxes.is_empty() {
            load_emails(&mailboxes[0].dir)
        } else {
            Vec::new()
        };

        let mut cache: Vec<Option<Vec<EmailEntry>>> = vec![None; n];
        if !cache.is_empty() {
            cache[0] = Some(emails.clone());
        }

        Self {
            focus: Focus::List,
            running: true,
            terminal_width: 0,
            terminal_height: 0,
            mailboxes,
            sidebar_index: 0,
            active_mailbox: 0,
            mailbox_counts: counts,
            emails,
            list_index: 0,
            g_pending: false,
            headers_scroll: 0,
            preview_scroll: 0,
            selection: HashSet::new(),
            email_cache: cache,
            pending_action: None,
            confirm_dialog: None,
            status_message: None,
            status_ticks: 0,
            search_query: String::new(),
            search_includes_body: false,
            show_help: false,
            help_scroll: 0,
            help_filter: String::new(),
            help_filter_active: false,
            watcher_active: false,
            bg_count: 0,
            bg_mutations: 0,
            bg_spin_tick: 0,
            queued_action: None,
            persistent_error: None,
            global_config,
            imap_config,
            smtp_config,
            signature_content,
            sent_dir,
            archive_dir,
            archive_server_name,
            drafts_dir,
            inbox_dir,
        }
    }

    pub fn active_kind(&self) -> MailboxKind {
        self.mailboxes.get(self.active_mailbox)
            .map(|m| m.kind)
            .unwrap_or(MailboxKind::Inbox)
    }

    pub fn active_label(&self) -> &str {
        self.mailboxes.get(self.active_mailbox)
            .map(|m| m.label.as_str())
            .unwrap_or("Mail")
    }

    pub fn active_dir(&self) -> Option<&PathBuf> {
        self.mailboxes.get(self.active_mailbox).map(|m| &m.dir)
    }

    pub fn find_mailbox_by_kind(&self, kind: MailboxKind) -> Option<usize> {
        self.mailboxes.iter().position(|m| m.kind == kind)
    }

    pub fn update(&mut self, msg: Message) -> Option<Message> {
        match msg {
            Message::Key(key) => self.handle_key(key),
            Message::Resize(w, h) => {
                self.terminal_width = w;
                self.terminal_height = h;
                None
            }
            Message::MailboxChanged => {
                self.pending_action = Some(Action::Fetch);
                None
            }
            Message::Quit => {
                self.running = false;
                None
            }
        }
    }

    pub fn set_status(&mut self, msg: String) {
        self.status_message = Some(msg);
        self.status_ticks = 12;
    }

    pub fn tick_status(&mut self) {
        if self.bg_count > 0 {
            return;
        }
        if self.status_ticks > 0 {
            self.status_ticks -= 1;
            if self.status_ticks == 0 {
                self.status_message = None;
            }
        }
    }

    pub fn selected_email(&self) -> Option<&EmailEntry> {
        self.emails.get(self.list_index)
    }

    pub fn selected_email_path(&self) -> Option<PathBuf> {
        self.selected_email().map(|e| e.path.clone())
    }

    pub fn remove_selected_from_list(&mut self) -> Option<PathBuf> {
        if self.emails.is_empty() {
            return None;
        }
        let path = self.emails[self.list_index].path.clone();
        self.emails.remove(self.list_index);

        if let Some(Some(cached)) = self.email_cache.get_mut(self.active_mailbox) {
            cached.retain(|e| e.path != path);
        }

        if !self.emails.is_empty() {
            self.list_index = self.list_index.min(self.emails.len() - 1);
        } else {
            self.list_index = 0;
        }

        if let Some(count) = self.mailbox_counts.get_mut(self.active_mailbox) {
            *count = self.emails.len();
        }

        self.headers_scroll = 0;
        self.preview_scroll = 0;

        Some(path)
    }

    pub fn remove_selected_from_list_batch(&mut self, paths: &HashSet<PathBuf>) -> Vec<PathBuf> {
        let removed: Vec<PathBuf> = self.emails
            .iter()
            .filter(|e| paths.contains(&e.path))
            .map(|e| e.path.clone())
            .collect();

        self.emails.retain(|e| !paths.contains(&e.path));

        if let Some(Some(cached)) = self.email_cache.get_mut(self.active_mailbox) {
            cached.retain(|e| !paths.contains(&e.path));
        }

        if !self.emails.is_empty() {
            self.list_index = self.list_index.min(self.emails.len() - 1);
        } else {
            self.list_index = 0;
        }

        if let Some(count) = self.mailbox_counts.get_mut(self.active_mailbox) {
            *count = self.emails.len();
        }

        self.headers_scroll = 0;
        self.preview_scroll = 0;

        removed
    }

    pub fn set_persistent_error(&mut self, msg: String) {
        self.persistent_error = Some(PersistentError { message: msg });
    }

    pub fn invalidate_cache_idx(&mut self, idx: usize) {
        if let Some(slot) = self.email_cache.get_mut(idx) {
            *slot = None;
        }
    }

    pub fn invalidate_all_caches(&mut self) {
        for slot in &mut self.email_cache {
            *slot = None;
        }
    }

    pub fn reload_current_mailbox(&mut self) {
        self.invalidate_cache_idx(self.active_mailbox);
        self.switch_mailbox(self.active_mailbox);
        if !self.emails.is_empty() {
            self.list_index = self.list_index.min(self.emails.len() - 1);
        } else {
            self.list_index = 0;
        }
        self.mailbox_counts = count_all_emails(&self.mailboxes);
    }

    fn switch_mailbox(&mut self, idx: usize) {
        let changing = self.active_mailbox != idx;
        self.active_mailbox = idx;
        if changing {
            self.selection.clear();
            self.search_query.clear();
            self.search_includes_body = false;
        }

        if let Some(cached) = self.email_cache.get(idx).and_then(|c| c.as_ref()) {
            self.emails = cached.clone();
        } else if let Some(mb) = self.mailboxes.get(idx) {
            let loaded = load_emails(&mb.dir);
            if let Some(slot) = self.email_cache.get_mut(idx) {
                *slot = Some(loaded.clone());
            }
            self.emails = loaded;
        } else {
            self.emails = Vec::new();
        }

        if let Some(count) = self.mailbox_counts.get_mut(idx) {
            *count = self.emails.len();
        }
        if changing {
            self.list_index = 0;
        }
    }

    fn handle_key(&mut self, key: KeyEvent) -> Option<Message> {
        if self.confirm_dialog.is_some() {
            return self.handle_confirm_key(key);
        }

        if self.persistent_error.is_some() {
            return self.handle_persistent_error_key(key);
        }

        if self.show_help {
            return self.handle_help_key(key);
        }

        if self.focus == Focus::Search {
            return self.handle_search_key(key);
        }

        match key.code {
            KeyCode::Char('q') => return Some(Message::Quit),
            KeyCode::Char('?') => {
                self.g_pending = false;
                self.show_help = true;
                self.help_scroll = 0;
                self.help_filter.clear();
                self.help_filter_active = false;
                return None;
            }
            KeyCode::Char('/') => {
                self.g_pending = false;
                self.focus = Focus::Search;
                self.search_query.clear();
                self.search_includes_body = false;
                self.reload_from_cache();
                return None;
            }
            KeyCode::Char('\\') => {
                self.g_pending = false;
                self.focus = Focus::Search;
                self.search_query.clear();
                self.search_includes_body = true;
                self.reload_from_cache();
                return None;
            }
            KeyCode::Char(c @ '1'..='9') => {
                let idx = (c as usize) - ('1' as usize);
                if idx < self.mailboxes.len() {
                    self.g_pending = false;
                    self.sidebar_index = idx;
                    self.switch_mailbox(idx);
                    self.focus = Focus::List;
                    return None;
                }
            }
            KeyCode::Char('s') => {
                self.g_pending = false;
                self.focus = Focus::Sidebar;
                return None;
            }
            KeyCode::Tab | KeyCode::Char('l') => {
                self.g_pending = false;
                if self.focus == Focus::Sidebar {
                    self.switch_mailbox(self.sidebar_index);
                }
                self.focus = match self.focus {
                    Focus::Sidebar => Focus::List,
                    Focus::List => Focus::Preview,
                    Focus::Preview => Focus::Headers,
                    Focus::Headers => Focus::Sidebar,
                    Focus::Search => Focus::List,
                };
                return None;
            }
            KeyCode::BackTab | KeyCode::Char('h') => {
                self.g_pending = false;
                self.focus = match self.focus {
                    Focus::Sidebar => Focus::Headers,
                    Focus::Headers => Focus::Preview,
                    Focus::Preview => Focus::List,
                    Focus::List => Focus::Sidebar,
                    Focus::Search => Focus::List,
                };
                return None;
            }
            _ => {}
        }

        match self.focus {
            Focus::Sidebar => self.handle_sidebar_key(key),
            Focus::List => self.handle_list_key(key),
            Focus::Headers => self.handle_headers_key(key),
            Focus::Preview => self.handle_preview_key(key),
            Focus::Search => unreachable!(),
        }
    }

    fn handle_confirm_key(&mut self, key: KeyEvent) -> Option<Message> {
        match key.code {
            KeyCode::Char('y') | KeyCode::Enter => {
                if let Some(dialog) = self.confirm_dialog.take() {
                    match dialog.action {
                        ConfirmAction::Archive if !self.selection.is_empty() => {
                            let paths: Vec<PathBuf> = self.selection.drain().collect();
                            self.pending_action = Some(Action::BatchArchive(paths));
                        }
                        ConfirmAction::Delete if !self.selection.is_empty() => {
                            let paths: Vec<PathBuf> = self.selection.drain().collect();
                            self.pending_action = Some(Action::BatchDelete(paths));
                        }
                        _ => {
                            self.pending_action = Some(match dialog.action {
                                ConfirmAction::Archive => Action::Archive,
                                ConfirmAction::Delete => Action::Delete,
                                ConfirmAction::Send => Action::Send,
                                ConfirmAction::SendApproved => Action::SendApproved,
                            });
                        }
                    }
                }
            }
            KeyCode::Char('n') | KeyCode::Esc => {
                self.confirm_dialog = None;
            }
            _ => {}
        }
        None
    }

    fn handle_sidebar_key(&mut self, key: KeyEvent) -> Option<Message> {
        self.g_pending = false;
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                if self.sidebar_index < self.mailboxes.len().saturating_sub(1) {
                    self.sidebar_index += 1;
                }
                None
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.sidebar_index = self.sidebar_index.saturating_sub(1);
                None
            }
            KeyCode::Enter => {
                self.switch_mailbox(self.sidebar_index);
                self.focus = Focus::List;
                None
            }
            _ => None,
        }
    }

    fn handle_headers_key(&mut self, key: KeyEvent) -> Option<Message> {
        self.g_pending = false;
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.headers_scroll = self.headers_scroll.saturating_add(1);
                None
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.headers_scroll = self.headers_scroll.saturating_sub(1);
                None
            }
            _ => None,
        }
    }

    fn handle_list_key(&mut self, key: KeyEvent) -> Option<Message> {
        if self.emails.is_empty() {
            self.g_pending = false;
            match key.code {
                KeyCode::Char('f') => self.pending_action = Some(Action::Fetch),
                KeyCode::Char('F') => self.pending_action = Some(Action::Sync),
                KeyCode::Char('n') => self.pending_action = Some(Action::NewDraft),
                _ => {}
            }
            return None;
        }

        let old_index = self.list_index;

        match key.code {
            KeyCode::Char('g') => {
                if self.g_pending {
                    self.list_index = 0;
                    self.g_pending = false;
                } else {
                    self.g_pending = true;
                }
            }
            KeyCode::Char('G') => {
                self.g_pending = false;
                self.list_index = self.emails.len().saturating_sub(1);
            }
            KeyCode::Char('j') | KeyCode::Down => {
                self.g_pending = false;
                if self.list_index < self.emails.len() - 1 {
                    self.list_index += 1;
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.g_pending = false;
                self.list_index = self.list_index.saturating_sub(1);
            }
            KeyCode::Enter | KeyCode::Char('e') => {
                self.g_pending = false;
                self.pending_action = Some(Action::EditCurrent);
            }
            KeyCode::Char('r') => {
                self.g_pending = false;
                self.pending_action = Some(Action::Reply(false));
            }
            KeyCode::Char('R') => {
                self.g_pending = false;
                self.pending_action = Some(Action::Reply(true));
            }
            KeyCode::Char('w') => {
                self.g_pending = false;
                self.pending_action = Some(Action::Forward);
            }
            KeyCode::Char('a') => {
                self.g_pending = false;
                if key.modifiers.contains(KeyModifiers::CONTROL) {
                    self.selection = self.emails.iter().map(|e| e.path.clone()).collect();
                } else if !self.selection.is_empty() {
                    let count = self.selection.len();
                    self.confirm_dialog = Some(ConfirmDialog {
                        title: format!("Archive {} emails?", count),
                        detail: format!("{} selected emails", count),
                        action: ConfirmAction::Archive,
                    });
                } else if let Some(email) = self.selected_email() {
                    self.confirm_dialog = Some(ConfirmDialog {
                        title: "Archive this email?".to_string(),
                        detail: format!("{} - {}", email.from, email.subject),
                        action: ConfirmAction::Archive,
                    });
                }
            }
            KeyCode::Char('d') => {
                self.g_pending = false;
                if !self.selection.is_empty() {
                    let count = self.selection.len();
                    self.confirm_dialog = Some(ConfirmDialog {
                        title: format!("Delete {} emails?", count),
                        detail: format!("{} selected emails", count),
                        action: ConfirmAction::Delete,
                    });
                } else if let Some(email) = self.selected_email() {
                    self.confirm_dialog = Some(ConfirmDialog {
                        title: "Delete this email?".to_string(),
                        detail: format!("{} - {}", email.from, email.subject),
                        action: ConfirmAction::Delete,
                    });
                }
            }
            KeyCode::Char('A') => {
                self.g_pending = false;
                self.pending_action = Some(Action::Approve);
            }
            KeyCode::Char('x') => {
                self.g_pending = false;
                if let Some(email) = self.selected_email() {
                    self.confirm_dialog = Some(ConfirmDialog {
                        title: "Send this email?".to_string(),
                        detail: format!("To: {} - {}", email.to, email.subject),
                        action: ConfirmAction::Send,
                    });
                }
            }
            KeyCode::Char('X') => {
                self.g_pending = false;
                self.confirm_dialog = Some(ConfirmDialog {
                    title: "Send all approved emails?".to_string(),
                    detail: format!("In {}", self.active_label()),
                    action: ConfirmAction::SendApproved,
                });
            }
            KeyCode::Char('y') => {
                self.g_pending = false;
                self.pending_action = Some(Action::CopyPath);
            }
            KeyCode::Char('n') => {
                self.g_pending = false;
                self.pending_action = Some(Action::NewDraft);
            }
            KeyCode::Char('f') => {
                self.g_pending = false;
                self.pending_action = Some(Action::Fetch);
            }
            KeyCode::Char('F') => {
                self.g_pending = false;
                self.pending_action = Some(Action::Sync);
            }

            KeyCode::Char(' ') => {
                self.g_pending = false;
                if let Some(path) = self.selected_email_path() {
                    if self.selection.contains(&path) {
                        self.selection.remove(&path);
                    } else {
                        self.selection.insert(path);
                    }
                    if self.list_index < self.emails.len() - 1 {
                        self.list_index += 1;
                    }
                }
            }
            KeyCode::Esc => {
                self.g_pending = false;
                if !self.selection.is_empty() {
                    self.selection.clear();
                }
            }

            _ => {
                self.g_pending = false;
            }
        }

        if self.list_index != old_index {
            self.headers_scroll = 0;
            self.preview_scroll = 0;
        }

        None
    }

    fn handle_preview_key(&mut self, key: KeyEvent) -> Option<Message> {
        self.g_pending = false;
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.preview_scroll = self.preview_scroll.saturating_add(1);
                None
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.preview_scroll = self.preview_scroll.saturating_sub(1);
                None
            }
            KeyCode::Char('d') => {
                self.preview_scroll = self.preview_scroll.saturating_add(10);
                None
            }
            KeyCode::Char('u') => {
                self.preview_scroll = self.preview_scroll.saturating_sub(10);
                None
            }
            KeyCode::Esc => {
                self.focus = Focus::List;
                None
            }
            _ => None,
        }
    }

    fn handle_help_key(&mut self, key: KeyEvent) -> Option<Message> {
        if self.help_filter_active {
            match key.code {
                KeyCode::Char(c) => {
                    self.help_filter.push(c);
                    self.help_scroll = 0;
                }
                KeyCode::Backspace => {
                    self.help_filter.pop();
                    self.help_scroll = 0;
                }
                KeyCode::Enter => {
                    self.help_filter_active = false;
                }
                KeyCode::Esc => {
                    if !self.help_filter.is_empty() {
                        self.help_filter.clear();
                        self.help_filter_active = false;
                        self.help_scroll = 0;
                    } else {
                        self.show_help = false;
                    }
                }
                _ => {}
            }
        } else {
            match key.code {
                KeyCode::Char('j') | KeyCode::Down => {
                    self.g_pending = false;
                    self.help_scroll = self.help_scroll.saturating_add(1);
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    self.g_pending = false;
                    self.help_scroll = self.help_scroll.saturating_sub(1);
                }
                KeyCode::Char('g') => {
                    if self.g_pending {
                        self.help_scroll = 0;
                        self.g_pending = false;
                    } else {
                        self.g_pending = true;
                    }
                }
                KeyCode::Char('G') => {
                    self.g_pending = false;
                    self.help_scroll = u16::MAX;
                }
                KeyCode::Char('d') => {
                    self.g_pending = false;
                    self.help_scroll = self.help_scroll.saturating_add(10);
                }
                KeyCode::Char('u') => {
                    self.g_pending = false;
                    self.help_scroll = self.help_scroll.saturating_sub(10);
                }
                KeyCode::Char('/') => {
                    self.g_pending = false;
                    self.help_filter_active = true;
                    self.help_filter.clear();
                    self.help_scroll = 0;
                }
                KeyCode::Char('?') | KeyCode::Esc => {
                    self.g_pending = false;
                    self.show_help = false;
                    self.help_scroll = 0;
                    self.help_filter.clear();
                    self.help_filter_active = false;
                }
                _ => {
                    self.g_pending = false;
                }
            }
        }
        None
    }

    fn handle_persistent_error_key(&mut self, key: KeyEvent) -> Option<Message> {
        match key.code {
            KeyCode::Char('s') => {
                self.persistent_error = None;
                self.pending_action = Some(Action::Sync);
            }
            KeyCode::Char('d') | KeyCode::Esc => {
                self.persistent_error = None;
            }
            _ => {}
        }
        None
    }

    fn handle_search_key(&mut self, key: KeyEvent) -> Option<Message> {
        match key.code {
            KeyCode::Enter => {
                self.focus = Focus::List;
            }
            KeyCode::Esc => {
                self.search_query.clear();
                self.search_includes_body = false;
                self.reload_from_cache();
                self.focus = Focus::List;
            }
            KeyCode::Char(c) => {
                self.search_query.push(c);
                self.apply_search_filter();
            }
            KeyCode::Backspace => {
                self.search_query.pop();
                self.apply_search_filter();
            }
            _ => {}
        }
        None
    }

    fn apply_search_filter(&mut self) {
        self.selection.clear();
        let idx = self.active_mailbox;
        let all_emails = self.email_cache.get(idx)
            .and_then(|c| c.as_ref())
            .cloned()
            .unwrap_or_default();

        if self.search_query.is_empty() {
            self.emails = all_emails;
        } else {
            let query = self.search_query.to_lowercase();
            let kind = self.active_kind();
            let includes_body = self.search_includes_body;
            self.emails = all_emails
                .into_iter()
                .filter(|e| {
                    e.subject.to_lowercase().contains(&query)
                        || e.display_contact(kind).to_lowercase().contains(&query)
                        || e.date_display.to_lowercase().contains(&query)
                        || e.from.to_lowercase().contains(&query)
                        || e.to.to_lowercase().contains(&query)
                        || (includes_body && e.body.to_lowercase().contains(&query))
                })
                .collect();
        }

        self.list_index = 0;
        self.headers_scroll = 0;
        self.preview_scroll = 0;
    }

    fn reload_from_cache(&mut self) {
        let idx = self.active_mailbox;
        if let Some(Some(cached)) = self.email_cache.get(idx) {
            self.emails = cached.clone();
        }
        self.list_index = 0;
        self.headers_scroll = 0;
        self.preview_scroll = 0;
    }
}

fn build_mailboxes(config: &crate::config::GlobalConfig) -> Vec<MailboxInfo> {
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
    });

    result.push(MailboxInfo {
        label: "Drafts".to_string(),
        icon: "\u{f03eb}",
        dir: drafts_dir,
        kind: MailboxKind::Drafts,
    });

    let sent_dir = config.mailboxes.sent.as_ref()
        .map(|m| crate::config::resolve_mailbox_local_path(config, m))
        .unwrap_or_else(|| root.join("sent"));
    result.push(MailboxInfo {
        label: "Sent".to_string(),
        icon: "\u{f046b}",
        dir: sent_dir,
        kind: MailboxKind::Sent,
    });

    let archive_dir = config.mailboxes.archive.as_ref()
        .map(|m| crate::config::resolve_mailbox_local_path(config, m))
        .unwrap_or_else(|| root.join("archive"));
    result.push(MailboxInfo {
        label: "Archive".to_string(),
        icon: "\u{f013c}",
        dir: archive_dir,
        kind: MailboxKind::Archive,
    });

    if let Some(ref extras) = config.mailboxes.extra {
        for m in extras {
            result.push(MailboxInfo {
                label: m.server.clone(),
                icon: "\u{f0247}",
                dir: crate::config::resolve_mailbox_local_path(config, m),
                kind: MailboxKind::Extra,
            });
        }
    }

    result
}

fn count_all_emails(mailboxes: &[MailboxInfo]) -> Vec<usize> {
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
