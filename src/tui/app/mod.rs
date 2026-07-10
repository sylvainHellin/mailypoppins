mod keys;
mod types;

pub use types::*;

use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};

/// Top-level application state.
pub struct App {
    pub focus: Focus,
    pub running: bool,
    pub terminal_width: u16,
    pub terminal_height: u16,

    // Multi-account
    pub accounts: Vec<AccountState>,
    pub active_account: usize,

    // --- Fields proxied from active account (kept in sync) ---
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
    pub email_cache: Vec<Option<Vec<EmailEntry>>>,
    pub search_query: String,
    pub search_includes_body: bool,
    pub watcher_active: bool,
    pub bg_mutations: usize,
    pub imap_config: Option<crate::config::ImapConfig>,
    pub smtp_config: Option<crate::config::SmtpConfig>,
    pub graph_config: Option<crate::config::GraphConfig>,
    pub signature_content: Option<String>,
    pub sent_dir: Option<PathBuf>,
    pub archive_dir: Option<PathBuf>,
    pub archive_server_name: String,
    pub drafts_dir: Option<PathBuf>,
    pub inbox_dir: Option<PathBuf>,
    pub account_config: crate::config::AccountConfig,

    // --- Global state ---
    pub pending_actions: VecDeque<Action>,
    pub confirm_dialog: Option<ConfirmDialog>,
    pub status_message: Option<String>,
    pub status_ticks: u8,
    pub show_help: bool,
    pub help_scroll: u16,
    pub help_filter: String,
    pub help_filter_active: bool,

    pub bg_count: usize,
    pub bg_spin_tick: usize,
    /// Monotonic counter guarding background mailbox loads (P1 step 2).
    /// Every `request_mailbox_load` bumps it and stamps the spawned walk;
    /// a `BgResult::MailboxLoaded` whose stamp no longer matches (the user
    /// switched account/mailbox, requested a newer reload, or mutated the
    /// list optimistically) is dropped in `tui/bg.rs`.
    pub mailbox_load_generation: u64,
    pub queued_action: Option<Action>,
    pub persistent_error: Option<PersistentError>,
    pub attachment_picker: Option<AttachmentPicker>,
    pub dir_picker: Option<DirPicker>,
    pub last_save_dir: Option<PathBuf>,

    pub status_log: VecDeque<StatusEntry>,
    pub show_activity_log: bool,

    // Activity log overlay
    pub show_activity_overlay: bool,
    pub activity_filter: String,
    pub activity_filter_active: bool,
    pub activity_scroll: u16,

    // Server search overlay
    pub show_search_overlay: bool,
    pub server_search_query: String,
    pub server_search_focus: SearchOverlayFocus,
    pub server_search_results: Vec<SearchResultEntry>,
    pub server_search_index: usize,
    pub server_search_headers_scroll: u16,
    pub server_search_scroll: u16,
    pub server_search_loading: bool,
    pub server_search_status: Option<String>,
    pub server_search_scope_label: String,

    // Compose wizard overlay
    pub compose_wizard: Option<ComposeWizard>,

    // Config (loaded once at startup)
    pub global_config: crate::config::GlobalConfig,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    pub fn new() -> Self {
        let global_config = crate::config::load_global_config().unwrap_or_default();

        let accounts: Vec<AccountState> = global_config
            .accounts
            .iter()
            .map(|ac| AccountState::new(ac.clone(), &global_config.email))
            .collect();

        let mut app = Self {
            focus: Focus::List,
            running: true,
            terminal_width: 0,
            terminal_height: 0,
            accounts,
            active_account: 0,
            mailboxes: Vec::new(),
            sidebar_index: 0,
            active_mailbox: 0,
            mailbox_counts: Vec::new(),
            emails: Vec::new(),
            list_index: 0,
            g_pending: false,
            headers_scroll: 0,
            preview_scroll: 0,
            selection: HashSet::new(),
            email_cache: Vec::new(),
            search_query: String::new(),
            search_includes_body: false,
            watcher_active: false,
            bg_mutations: 0,
            imap_config: None,
            smtp_config: None,
            graph_config: None,
            signature_content: None,
            sent_dir: None,
            archive_dir: None,
            archive_server_name: "Archive".to_string(),
            drafts_dir: None,
            inbox_dir: None,
            account_config: crate::config::AccountConfig::default(),
            pending_actions: VecDeque::new(),
            confirm_dialog: None,
            status_message: None,
            status_ticks: 0,
            show_help: false,
            help_scroll: 0,
            help_filter: String::new(),
            help_filter_active: false,
            bg_count: 0,
            bg_spin_tick: 0,
            mailbox_load_generation: 0,
            queued_action: None,
            persistent_error: None,
            attachment_picker: None,
            dir_picker: None,
            last_save_dir: None,
            status_log: VecDeque::new(),
            show_activity_log: true,
            show_activity_overlay: false,
            activity_filter: String::new(),
            activity_filter_active: false,
            activity_scroll: 0,
            show_search_overlay: false,
            server_search_query: String::new(),
            server_search_focus: SearchOverlayFocus::Input,
            server_search_results: Vec::new(),
            server_search_index: 0,
            server_search_headers_scroll: 0,
            server_search_scroll: 0,
            server_search_loading: false,
            server_search_status: None,
            server_search_scope_label: "All".to_string(),
            compose_wizard: None,
            global_config,
        };

        app.load_from_account(0);
        if !app.mailboxes.is_empty() {
            let loaded = load_emails(&app.mailboxes[0].dir);
            app.email_cache[0] = Some(loaded.clone());
            app.emails = loaded;
        }

        app
    }

    // ---------------------------------------------------------------
    // Account state sync
    // ---------------------------------------------------------------

    pub(crate) fn save_to_account(&mut self) {
        if let Some(acct) = self.accounts.get_mut(self.active_account) {
            acct.sidebar_index = self.sidebar_index;
            acct.active_mailbox = self.active_mailbox;
            acct.mailbox_counts = self.mailbox_counts.clone();
            acct.list_index = self.list_index;
            acct.headers_scroll = self.headers_scroll;
            acct.preview_scroll = self.preview_scroll;
            acct.selection = self.selection.clone();
            acct.email_cache = self.email_cache.clone();
            acct.search_query = self.search_query.clone();
            acct.search_includes_body = self.search_includes_body;
            acct.bg_mutations = self.bg_mutations;
            acct.watcher_active = self.watcher_active;
        }
    }

    pub(crate) fn load_from_account(&mut self, idx: usize) {
        if let Some(acct) = self.accounts.get(idx) {
            self.mailboxes = acct.mailboxes.clone();
            self.sidebar_index = acct.sidebar_index;
            self.active_mailbox = acct.active_mailbox;
            self.mailbox_counts = acct.mailbox_counts.clone();
            self.list_index = acct.list_index;
            self.headers_scroll = acct.headers_scroll;
            self.preview_scroll = acct.preview_scroll;
            self.selection = acct.selection.clone();
            self.email_cache = acct.email_cache.clone();
            self.search_query = acct.search_query.clone();
            self.search_includes_body = acct.search_includes_body;
            self.watcher_active = acct.watcher_active;
            self.bg_mutations = acct.bg_mutations;
            self.imap_config = acct.imap_config.clone();
            self.smtp_config = acct.smtp_config.clone();
            self.graph_config = acct.graph_config.clone();
            self.signature_content = acct.signature_content.clone();
            self.sent_dir = acct.sent_dir.clone();
            self.archive_dir = acct.archive_dir.clone();
            self.archive_server_name = acct.archive_server_name.clone();
            self.drafts_dir = acct.drafts_dir.clone();
            self.inbox_dir = acct.inbox_dir.clone();
            self.account_config = acct.account_config.clone();
        }
    }

    pub fn active_kind(&self) -> MailboxKind {
        self.mailboxes
            .get(self.active_mailbox)
            .map(|m| m.kind)
            .unwrap_or(MailboxKind::Inbox)
    }

    pub fn active_label(&self) -> &str {
        self.mailboxes
            .get(self.active_mailbox)
            .map(|m| m.label.as_str())
            .unwrap_or("Mail")
    }

    pub fn active_dir(&self) -> Option<&PathBuf> {
        self.mailboxes.get(self.active_mailbox).map(|m| &m.dir)
    }

    pub fn active_server_mailbox(&self) -> String {
        self.mailboxes
            .get(self.active_mailbox)
            .and_then(|m| m.server_name.clone())
            .unwrap_or_else(|| "INBOX".to_string())
    }

    pub fn account_name(&self) -> &str {
        &self.account_config.name
    }

    pub fn is_graph(&self) -> bool {
        self.graph_config.is_some()
            && self.account_config.auth_method == crate::config::AuthMethod::Graph
    }

    pub fn account_index_by_from(&self, from: &str) -> usize {
        let lower = from.to_lowercase();
        self.accounts
            .iter()
            .position(|acct| lower.contains(&acct.account_config.default_from.to_lowercase()))
            .unwrap_or(self.active_account)
    }

    pub fn all_search_targets(&self) -> Vec<SearchTarget> {
        self.mailboxes
            .iter()
            .filter(|m| m.server_name.is_some())
            .map(|m| SearchTarget {
                server_name: m.server_name.clone().expect("filtered for is_some"),
                local_dir: m.dir.clone(),
                status: kind_to_status(m.kind),
                label: m.label.clone(),
            })
            .collect()
    }

    pub fn search_target_by_name(&self, name: &str) -> Option<SearchTarget> {
        let lower = name.to_lowercase();
        self.mailboxes
            .iter()
            .find(|m| {
                m.server_name
                    .as_ref()
                    .is_some_and(|s| s.to_lowercase() == lower)
                    || m.label.to_lowercase() == lower
            })
            .and_then(|m| {
                Some(SearchTarget {
                    server_name: m.server_name.clone()?,
                    local_dir: m.dir.clone(),
                    status: kind_to_status(m.kind),
                    label: m.label.clone(),
                })
            })
    }

    pub fn mailbox_index_for_dir(&self, dir: &Path) -> Option<usize> {
        self.mailboxes.iter().position(|m| m.dir == dir)
    }

    pub fn find_mailbox_by_kind(&self, kind: MailboxKind) -> Option<usize> {
        self.mailboxes.iter().position(|m| m.kind == kind)
    }

    pub fn switch_account(&mut self, idx: usize) {
        if idx >= self.accounts.len() || idx == self.active_account {
            return;
        }
        self.save_to_account();
        self.active_account = idx;
        self.accounts[idx].has_unseen = false;
        self.load_from_account(idx);
        let am = self.active_mailbox;
        if let Some(cached) = self.email_cache.get(am).and_then(|c| c.as_ref()) {
            self.emails = cached.clone();
        } else if let Some(mb) = self.mailboxes.get(am) {
            // Cache miss: same off-thread load as `switch_mailbox` (P1
            // step 2) -- show an empty list + loading status until the
            // new account's entries arrive via `BgResult::MailboxLoaded`.
            let label = mb.label.clone();
            self.emails = Vec::new();
            self.set_status_level(format!("Loading {label}..."), StatusLevel::Progress);
            self.request_mailbox_load(am);
        } else {
            self.emails = Vec::new();
        }
        self.focus = Focus::List;
    }

    pub fn update(&mut self, msg: Message) -> Option<Message> {
        match msg {
            Message::Key(key) => self.handle_key(key),
            Message::Resize(w, h) => {
                self.terminal_width = w;
                self.terminal_height = h;
                None
            }
            Message::MailboxChanged { account_index } => {
                if account_index == self.active_account {
                    self.push_action_dedup(Action::Fetch);
                } else if let Some(acct) = self.accounts.get_mut(account_index) {
                    acct.has_unseen = true;
                }
                None
            }
            Message::Quit => {
                self.running = false;
                None
            }
        }
    }

    pub fn push_status(&mut self, message: String, level: StatusLevel) {
        if self.status_log.len() >= STATUS_LOG_CAPACITY {
            self.status_log.pop_front();
        }
        self.status_log.push_back(StatusEntry {
            timestamp: chrono::Local::now(),
            message,
            level,
        });
    }

    pub fn set_status(&mut self, msg: String) {
        self.push_status(msg.clone(), StatusLevel::Info);
        self.status_message = Some(msg);
        self.status_ticks = 12;
    }

    pub fn set_status_level(&mut self, msg: String, level: StatusLevel) {
        self.push_status(msg.clone(), level);
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
        self.invalidate_pending_mailbox_loads();

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
        let removed: Vec<PathBuf> = self
            .emails
            .iter()
            .filter(|e| paths.contains(&e.path))
            .map(|e| e.path.clone())
            .collect();

        self.emails.retain(|e| !paths.contains(&e.path));
        self.invalidate_pending_mailbox_loads();

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

    /// Remove a message_id from the in-memory index for the active account.
    /// Called optimistically when archiving/deleting an email.
    pub fn remove_from_message_index(&mut self, file_path: &std::path::Path, message_id: &str) {
        if let Some(acct) = self.accounts.get_mut(self.active_account) {
            for dir_map in acct.message_id_index.values_mut() {
                if dir_map.get(message_id).is_some_and(|p| p == file_path) {
                    dir_map.remove(message_id);
                    return;
                }
            }
        }
    }

    /// Insert a message_id into the in-memory index for the active account.
    /// Used when archiving (file moves to archive dir).
    pub fn insert_into_message_index(
        &mut self,
        dir: &std::path::Path,
        message_id: String,
        file_path: std::path::PathBuf,
    ) {
        if let Some(acct) = self.accounts.get_mut(self.active_account) {
            acct.message_id_index
                .entry(dir.to_path_buf())
                .or_default()
                .insert(message_id, file_path);
        }
    }

    pub fn set_persistent_error(&mut self, msg: String) {
        self.persistent_error = Some(PersistentError { message: msg });
    }

    pub fn invalidate_cache_idx(&mut self, idx: usize) {
        if let Some(slot) = self.email_cache.get_mut(idx) {
            *slot = None;
        }
    }

    pub fn invalidate_cache_idx_on(&mut self, account_index: usize, mailbox_idx: usize) {
        if let Some(acct) = self.accounts.get_mut(account_index) {
            if let Some(slot) = acct.email_cache.get_mut(mailbox_idx) {
                *slot = None;
            }
        }
    }

    pub fn invalidate_all_caches(&mut self) {
        for slot in &mut self.email_cache {
            *slot = None;
        }
    }

    pub fn invalidate_all_caches_on(&mut self, account_index: usize) {
        if let Some(acct) = self.accounts.get_mut(account_index) {
            for slot in &mut acct.email_cache {
                *slot = None;
            }
        }
    }

    /// Push an action to the queue.
    pub fn push_action(&mut self, action: Action) {
        self.pending_actions.push_back(action);
    }

    /// Push an action only if no equivalent variant is already queued.
    /// Used for watcher-triggered fetches to avoid duplicates.
    pub fn push_action_dedup(&mut self, action: Action) {
        let dominated = self.pending_actions.iter().any(|a| {
            std::mem::discriminant(a) == std::mem::discriminant(&action)
        });
        if !dominated {
            self.pending_actions.push_back(action);
        }
    }

    pub fn reload_current_mailbox(&mut self) {
        let am = self.active_mailbox;
        self.invalidate_cache_idx(am);
        self.switch_mailbox(am);

        // The fresh entries arrive asynchronously via
        // `BgResult::MailboxLoaded`; the stale list stays visible
        // meanwhile. Clamp the cursor against it so the UI stays valid --
        // the arrival handler clamps again against the fresh list and
        // updates the mailbox count.
        if !self.emails.is_empty() {
            self.list_index = self.list_index.min(self.emails.len() - 1);
        } else {
            self.list_index = 0;
        }
    }

    /// Recount all mailbox sizes by walking every directory.
    /// Only needed after full sync/reconciliation that moves emails between mailboxes.
    pub fn recount_all_mailboxes(&mut self) {
        self.mailbox_counts = count_all_emails(&self.mailboxes);
    }

    pub(crate) fn switch_mailbox(&mut self, idx: usize) {
        let changing = self.active_mailbox != idx;
        self.active_mailbox = idx;
        if changing {
            self.selection.clear();
            self.search_query.clear();
            self.search_includes_body = false;
        }

        if let Some(cached) = self.email_cache.get(idx).and_then(|c| c.as_ref()) {
            self.emails = cached.clone();
            if let Some(count) = self.mailbox_counts.get_mut(idx) {
                *count = self.emails.len();
            }
        } else if let Some(mb) = self.mailboxes.get(idx) {
            // Cache miss: walk the directory off the UI thread (P1 step 2).
            // The entries arrive via `BgResult::MailboxLoaded`; the mailbox
            // count is updated then.
            if changing {
                let label = mb.label.clone();
                // The user expects the NEW mailbox's content -- show an
                // empty list with the existing bg spinner (same loading
                // indication as `BgResult::IndexReady`) rather than the
                // previous mailbox's entries.
                self.emails = Vec::new();
                self.set_status_level(
                    format!("Loading {label}..."),
                    StatusLevel::Progress,
                );
            }
            // else: same-mailbox reload -- keep the stale list visible
            // until the fresh entries arrive (no flicker, no empty state).
            self.request_mailbox_load(idx);
        } else {
            self.emails = Vec::new();
        }

        if changing {
            self.list_index = 0;
        }
    }

    /// Queue a background `load_emails` walk for mailbox `idx` of the
    /// active account (P1 step 2). Bumping the generation first makes any
    /// still-in-flight older walk stale, so out-of-order arrivals cannot
    /// clobber a newer result.
    fn request_mailbox_load(&mut self, idx: usize) {
        self.mailbox_load_generation = self.mailbox_load_generation.wrapping_add(1);
        self.push_action(Action::LoadMailbox {
            mailbox_idx: idx,
            generation: self.mailbox_load_generation,
        });
    }

    /// Drop any in-flight background mailbox load. Called after optimistic
    /// list mutations (archive/delete remove entries before the server
    /// confirms): a walk that started before the mutation could otherwise
    /// resurrect the removed email when it lands.
    fn invalidate_pending_mailbox_loads(&mut self) {
        self.mailbox_load_generation = self.mailbox_load_generation.wrapping_add(1);
    }
}
