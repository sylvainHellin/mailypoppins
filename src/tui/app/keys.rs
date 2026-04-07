use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{
    Action, App, AttachmentPicker, ConfirmAction, ConfirmDialog, Focus, Message,
    SearchOverlayFocus,
};

impl App {
    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> Option<Message> {
        if self.confirm_dialog.is_some() {
            return self.handle_confirm_key(key);
        }

        if self.persistent_error.is_some() {
            return self.handle_persistent_error_key(key);
        }

        if self.attachment_picker.is_some() {
            return self.handle_attachment_picker_key(key);
        }

        if self.show_help {
            return self.handle_help_key(key);
        }

        if self.show_search_overlay {
            return self.handle_search_overlay_key(key);
        }

        if self.focus == Focus::Search {
            return self.handle_search_key(key);
        }

        // Account switching (only when >1 account)
        if self.accounts.len() > 1 {
            if key.code == KeyCode::Char('`') {
                self.g_pending = false;
                let next = (self.active_account + 1) % self.accounts.len();
                self.switch_account(next);
                return None;
            }
            // Ctrl+1 through Ctrl+9 for direct account jump
            if key.modifiers.contains(KeyModifiers::CONTROL) {
                if let KeyCode::Char(c @ '1'..='9') = key.code {
                    let idx = (c as usize) - ('1' as usize);
                    if idx < self.accounts.len() {
                        self.g_pending = false;
                        self.switch_account(idx);
                        return None;
                    }
                }
            }
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
            KeyCode::Char('!') => {
                self.g_pending = false;
                self.show_activity_log = !self.show_activity_log;
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
            KeyCode::Char('o') => {
                if let Some(email) = self.selected_email() {
                    match crate::parse::list_attachments(&email.path) {
                        Ok(files) if files.is_empty() => {
                            self.set_status("No attachments".to_string());
                        }
                        Ok(files) if files.len() == 1 => {
                            self.pending_action = Some(Action::OpenAttachment(files.into_iter().next().unwrap()));
                        }
                        Ok(files) => {
                            self.attachment_picker = Some(AttachmentPicker { files, selected: 0 });
                        }
                        Err(e) => {
                            self.set_status(format!("Attachments error: {e}"));
                        }
                    }
                }
                None
            }
            KeyCode::Char('b') => {
                if let Some(email) = self.selected_email() {
                    let html_path = email.path.with_extension("html");
                    if html_path.exists() {
                        self.pending_action = Some(Action::OpenHtmlInBrowser(html_path));
                    } else {
                        self.set_status("No HTML version available".to_string());
                    }
                }
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
            KeyCode::Char('S') => {
                self.g_pending = false;
                self.show_search_overlay = true;
                self.server_search_query.clear();
                self.server_search_results.clear();
                self.server_search_index = 0;
                self.server_search_scroll = 0;
                self.server_search_headers_scroll = 0;
                self.server_search_focus = SearchOverlayFocus::Input;
                self.server_search_loading = false;
                self.server_search_status = None;
            }

            KeyCode::Char('o') => {
                self.g_pending = false;
                if let Some(email) = self.selected_email() {
                    match crate::parse::list_attachments(&email.path) {
                        Ok(files) if files.is_empty() => {
                            self.set_status("No attachments".to_string());
                        }
                        Ok(files) if files.len() == 1 => {
                            self.pending_action = Some(Action::OpenAttachment(files.into_iter().next().unwrap()));
                        }
                        Ok(files) => {
                            self.attachment_picker = Some(AttachmentPicker { files, selected: 0 });
                        }
                        Err(e) => {
                            self.set_status(format!("Attachments error: {e}"));
                        }
                    }
                }
            }
            KeyCode::Char('b') => {
                self.g_pending = false;
                if let Some(email) = self.selected_email() {
                    let html_path = email.path.with_extension("html");
                    if html_path.exists() {
                        self.pending_action = Some(Action::OpenHtmlInBrowser(html_path));
                    } else {
                        self.set_status("No HTML version available".to_string());
                    }
                }
            }
            KeyCode::Char('m') => {
                self.g_pending = false;
                if !self.selection.is_empty() {
                    let paths: Vec<PathBuf> = self.selection.iter().cloned().collect();
                    self.pending_action = Some(Action::BatchToggleRead(paths));
                } else {
                    self.pending_action = Some(Action::ToggleRead);
                }
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

    fn handle_search_overlay_key(&mut self, key: KeyEvent) -> Option<Message> {
        match self.server_search_focus {
            SearchOverlayFocus::Input => self.handle_search_overlay_input_key(key),
            SearchOverlayFocus::List => self.handle_search_overlay_list_key(key),
        }
    }

    fn handle_search_overlay_input_key(&mut self, key: KeyEvent) -> Option<Message> {
        match key.code {
            KeyCode::Char(c) => {
                self.server_search_query.push(c);
            }
            KeyCode::Backspace => {
                self.server_search_query.pop();
            }
            KeyCode::Enter => {
                if !self.server_search_query.is_empty() {
                    let criteria = crate::imap_client::parse_search_query(
                        &self.server_search_query,
                    );
                    let (targets, scope_label) = if let Some(ref name) = criteria.in_mailbox {
                        if let Some(target) = self.search_target_by_name(name) {
                            let label = target.label.clone();
                            (vec![target], label)
                        } else {
                            self.server_search_status =
                                Some(format!("Unknown mailbox: {}", name));
                            return None;
                        }
                    } else {
                        (self.all_search_targets(), "All".to_string())
                    };
                    self.server_search_scope_label = scope_label;
                    self.pending_action = Some(Action::ServerSearch {
                        query: self.server_search_query.clone(),
                        targets,
                    });
                }
            }
            KeyCode::Tab | KeyCode::Down => {
                if !self.server_search_results.is_empty() {
                    self.server_search_focus = SearchOverlayFocus::List;
                }
            }
            KeyCode::Esc => {
                self.show_search_overlay = false;
            }
            _ => {}
        }
        None
    }

    fn handle_search_overlay_list_key(&mut self, key: KeyEvent) -> Option<Message> {
        let len = self.server_search_results.len();
        if len == 0 {
            match key.code {
                KeyCode::Tab | KeyCode::BackTab => {
                    self.server_search_focus = SearchOverlayFocus::Input;
                }
                KeyCode::Esc => {
                    self.show_search_overlay = false;
                }
                _ => {}
            }
            return None;
        }

        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                if self.server_search_index < len - 1 {
                    self.server_search_index += 1;
                    self.server_search_scroll = 0;
                    self.server_search_headers_scroll = 0;
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if self.server_search_index > 0 {
                    self.server_search_index -= 1;
                    self.server_search_scroll = 0;
                    self.server_search_headers_scroll = 0;
                }
            }
            KeyCode::Char('g') => {
                if self.g_pending {
                    self.server_search_index = 0;
                    self.server_search_scroll = 0;
                    self.server_search_headers_scroll = 0;
                    self.g_pending = false;
                } else {
                    self.g_pending = true;
                }
                return None;
            }
            KeyCode::Char('G') => {
                self.g_pending = false;
                self.server_search_index = len.saturating_sub(1);
                self.server_search_scroll = 0;
                self.server_search_headers_scroll = 0;
            }
            KeyCode::Char('d') => {
                self.server_search_scroll = self.server_search_scroll.saturating_add(10);
            }
            KeyCode::Char('u') => {
                self.server_search_scroll = self.server_search_scroll.saturating_sub(10);
            }
            KeyCode::Enter | KeyCode::Char('e') => {
                self.pending_action = Some(Action::SearchResultOpen);
            }
            KeyCode::Char('r') => {
                self.pending_action = Some(Action::SearchResultReply(false));
            }
            KeyCode::Char('R') => {
                self.pending_action = Some(Action::SearchResultReply(true));
            }
            KeyCode::Char('w') => {
                self.pending_action = Some(Action::SearchResultForward);
            }
            KeyCode::Char('a') => {
                self.pending_action = Some(Action::SearchResultArchive);
            }
            KeyCode::Char('b') => {
                self.pending_action = Some(Action::SearchResultOpenInBrowser);
            }
            KeyCode::Tab | KeyCode::BackTab => {
                self.server_search_focus = SearchOverlayFocus::Input;
            }
            KeyCode::Esc => {
                self.show_search_overlay = false;
            }
            _ => {}
        }
        self.g_pending = false;
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
            KeyCode::Char('o') => {
                if let Some(email) = self.selected_email() {
                    match crate::parse::list_attachments(&email.path) {
                        Ok(files) if files.is_empty() => {
                            self.set_status("No attachments".to_string());
                        }
                        Ok(files) if files.len() == 1 => {
                            self.pending_action = Some(Action::OpenAttachment(files.into_iter().next().unwrap()));
                        }
                        Ok(files) => {
                            self.attachment_picker = Some(AttachmentPicker { files, selected: 0 });
                        }
                        Err(e) => {
                            self.set_status(format!("Attachments error: {e}"));
                        }
                    }
                }
                None
            }
            KeyCode::Char('b') => {
                if let Some(email) = self.selected_email() {
                    let html_path = email.path.with_extension("html");
                    if html_path.exists() {
                        self.pending_action = Some(Action::OpenHtmlInBrowser(html_path));
                    } else {
                        self.set_status("No HTML version available".to_string());
                    }
                }
                None
            }
            KeyCode::Esc => {
                self.focus = Focus::List;
                None
            }
            _ => None,
        }
    }

    fn handle_attachment_picker_key(&mut self, key: KeyEvent) -> Option<Message> {
        let picker = self.attachment_picker.as_mut().unwrap();
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                if picker.selected < picker.files.len().saturating_sub(1) {
                    picker.selected += 1;
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                picker.selected = picker.selected.saturating_sub(1);
            }
            KeyCode::Enter => {
                let picker = self.attachment_picker.take().unwrap();
                let path = picker.files[picker.selected].clone();
                self.pending_action = Some(Action::OpenAttachment(path));
            }
            KeyCode::Esc | KeyCode::Char('q') => {
                self.attachment_picker = None;
            }
            _ => {}
        }
        None
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

    pub(crate) fn apply_search_filter(&mut self) {
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

    pub(crate) fn reload_from_cache(&mut self) {
        let idx = self.active_mailbox;
        if let Some(Some(cached)) = self.email_cache.get(idx) {
            self.emails = cached.clone();
        }
        self.list_index = 0;
        self.headers_scroll = 0;
        self.preview_scroll = 0;
    }
}
