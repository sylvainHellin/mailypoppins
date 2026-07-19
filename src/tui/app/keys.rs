use std::collections::HashSet;
use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{
    Action, App, AttachmentPicker, AttachmentPickerMode, ComposeField, ComposeMode,
    ComposeSuggestion, ComposeWizard, ConfirmAction, ConfirmDialog, DirPicker, DirPickerMode,
    EmailEntry, Focus, MailboxKind, MailboxPicker, Message, Overlay, RsvpChoice, RsvpOverlay,
    SearchOverlayFocus,
};

impl App {
    pub(crate) fn handle_key(&mut self, key: KeyEvent) -> Option<Message> {
        // Single overlay dispatcher (#0032): exactly one overlay is active by
        // construction, so this matches on `self.overlay` instead of the
        // former cascade of `is_some()` / bool guards. Arm order preserves the
        // historical guard precedence (it never actually mattered — overlays
        // were always mutually exclusive — but keeping it avoids any behavior
        // question).
        match &self.overlay {
            Overlay::Confirm(_) => return self.handle_confirm_key(key),
            Overlay::Error(_) => return self.handle_persistent_error_key(key),
            Overlay::Dir(_) => return self.handle_dir_picker_key(key),
            Overlay::Mailbox(_) => return self.handle_mailbox_picker_key(key),
            Overlay::Rsvp(_) => return self.handle_rsvp_overlay_key(key),
            Overlay::Attachment(_) => return self.handle_attachment_picker_key(key),
            Overlay::Help => return self.handle_help_key(key),
            Overlay::Activity => return self.handle_activity_overlay_key(key),
            Overlay::Search => return self.handle_search_overlay_key(key),
            Overlay::Compose(_) => return self.handle_compose_wizard_key(key),
            Overlay::None => {}
        }

        if self.focus == Focus::Search {
            return self.handle_search_key(key);
        }

        // Normal-mode surface (#0032, (B)-lite): resolve the pressed key through
        // the KEYMAP table into a `KeyAction`, then run one executor. Global
        // bindings are always tried first, then the focused pane's context
        // (mirroring the old "global keys before pane dispatch" precedence).
        // Guards are evaluated against live state so context-sensitive rules
        // (`c` only in Drafts, most list keys only when non-empty, multi-account
        // account jumps) stay in the data model.
        self.dispatch_normal_mode(key)
    }

    /// Table-driven dispatch for the no-overlay surface. Returns the `Message`
    /// (only `Quit` today) or `None`.
    fn dispatch_normal_mode(&mut self, key: KeyEvent) -> Option<Message> {
        let pending = if self.g_pending { Some('g') } else { None };
        let guard_ok = |g: super::Guard| self.guard_satisfied(g);

        // Global context first.
        if let Some(action) = super::resolve(super::KeyCtx::Global, key, pending, &guard_ok) {
            return self.execute(action, key);
        }
        // Then the focused pane's context.
        if let Some(ctx) = self.pane_ctx() {
            if let Some(action) = super::resolve(ctx, key, pending, &guard_ok) {
                return self.execute(action, key);
            }
        }
        // No live binding matched: clear any pending leader (an unrecognised
        // key aborts the chord), matching the old `_ => { g_pending = false }`.
        self.g_pending = false;
        None
    }

    /// The KEYMAP context for the currently focused pane (no overlay active).
    fn pane_ctx(&self) -> Option<super::KeyCtx> {
        match self.focus {
            Focus::Sidebar => Some(super::KeyCtx::Sidebar),
            Focus::List => Some(super::KeyCtx::List),
            Focus::Headers => Some(super::KeyCtx::Headers),
            Focus::Preview => Some(super::KeyCtx::Preview),
            Focus::Search | Focus::ComposeWizard => None,
        }
    }

    /// Evaluate a keymap `Guard` against live app state.
    fn guard_satisfied(&self, guard: super::Guard) -> bool {
        match guard {
            super::Guard::None => true,
            super::Guard::MultiAccount => self.accounts.len() > 1,
            // `c` (edit recipients) is catalogued Drafts-only, but the old code
            // still *matched* the key outside Drafts to show a status hint. We
            // resolve it in any mailbox and branch inside the executor so that
            // UX is preserved; the guard is advisory here.
            super::Guard::DraftsOnly => true,
            super::Guard::NonEmptyList => !self.visible.is_empty(),
        }
    }

    /// Run a resolved Normal-mode [`KeyAction`]. This is the single executor the
    /// table dispatches into; it replaces the former per-pane match arms.
    fn execute(&mut self, action: super::KeyAction, key: KeyEvent) -> Option<Message> {
        use super::KeyAction as A;

        // Every executed action clears the pending leader except the leader key
        // itself (handled explicitly below) and the `gg` continuation.
        match action {
            // -- Global -------------------------------------------------------
            A::Quit => return Some(Message::Quit),
            A::ToggleHelp => {
                self.g_pending = false;
                self.help_scroll = 0;
                self.help_filter.clear();
                self.help_filter_active = false;
                self.overlay = Overlay::Help;
            }
            A::ToggleActivityLog => {
                self.g_pending = false;
                self.show_activity_log = !self.show_activity_log;
            }
            A::OpenActivityOverlay => {
                self.g_pending = false;
                self.activity_filter.clear();
                self.activity_filter_active = false;
                self.activity_scroll = 0;
                self.overlay = Overlay::Activity;
            }
            A::OpenLogFile => {
                self.g_pending = false;
                self.push_action(Action::OpenLogFile);
            }
            A::OpenConfigFile => {
                self.g_pending = false;
                self.push_action(Action::OpenConfigFile);
            }
            A::FilterMetadata => {
                self.g_pending = false;
                self.focus = Focus::Search;
                self.search_query.clear();
                self.search_includes_body = false;
                self.reload_from_cache();
            }
            A::SearchContent => {
                self.g_pending = false;
                self.focus = Focus::Search;
                self.search_query.clear();
                self.search_includes_body = true;
                self.reload_from_cache();
            }
            A::SwitchAccount => {
                self.g_pending = false;
                let next = (self.active_account + 1) % self.accounts.len();
                self.switch_account(next);
            }
            A::JumpAccount => {
                // Ctrl+1..9 -> direct account jump (guarded to multi-account).
                self.g_pending = false;
                if let KeyCode::Char(c @ '1'..='9') = key.code {
                    let idx = (c as usize) - ('1' as usize);
                    if idx < self.accounts.len() {
                        self.switch_account(idx);
                    }
                }
            }
            A::JumpMailbox => {
                self.g_pending = false;
                if let KeyCode::Char(c @ '1'..='9') = key.code {
                    let idx = (c as usize) - ('1' as usize);
                    if idx < self.mailboxes.len() {
                        self.sidebar_index = idx;
                        self.switch_mailbox(idx);
                        self.focus = Focus::List;
                    }
                }
            }
            A::FocusForward => {
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
                    Focus::ComposeWizard => Focus::ComposeWizard,
                };
            }
            A::FocusBackward => {
                self.g_pending = false;
                self.focus = match self.focus {
                    Focus::Sidebar => Focus::Headers,
                    Focus::Headers => Focus::Preview,
                    Focus::Preview => Focus::List,
                    Focus::List => Focus::Sidebar,
                    Focus::Search => Focus::List,
                    Focus::ComposeWizard => Focus::ComposeWizard,
                };
            }
            // -- Sidebar ------------------------------------------------------
            A::SidebarDown => {
                self.g_pending = false;
                if self.sidebar_index < self.mailboxes.len().saturating_sub(1) {
                    self.sidebar_index += 1;
                }
            }
            A::SidebarUp => {
                self.g_pending = false;
                self.sidebar_index = self.sidebar_index.saturating_sub(1);
            }
            A::SidebarSelect => {
                self.g_pending = false;
                self.switch_mailbox(self.sidebar_index);
                self.focus = Focus::List;
            }
            // -- Headers ------------------------------------------------------
            A::HeadersDown => {
                self.g_pending = false;
                self.headers_scroll = self.headers_scroll.saturating_add(1);
            }
            A::HeadersUp => {
                self.g_pending = false;
                self.headers_scroll = self.headers_scroll.saturating_sub(1);
            }
            // -- Preview / body ----------------------------------------------
            A::PreviewDown => {
                self.g_pending = false;
                self.preview_scroll = self.preview_scroll.saturating_add(1);
            }
            A::PreviewUp => {
                self.g_pending = false;
                self.preview_scroll = self.preview_scroll.saturating_sub(1);
            }
            A::PreviewHalfDown => {
                self.g_pending = false;
                self.preview_scroll = self.preview_scroll.saturating_add(10);
            }
            A::PreviewHalfUp => {
                self.g_pending = false;
                self.preview_scroll = self.preview_scroll.saturating_sub(10);
            }
            A::PreviewToList => {
                self.g_pending = false;
                self.focus = Focus::List;
            }
            // -- List / shared -----------------------------------------------
            _ => return self.execute_list(action, key),
        }
        None
    }

    /// List-context actions (and the shared attachment/browser/RSVP actions the
    /// list, headers, and preview panes all use). Split out to keep `execute`
    /// readable; it also owns the list-cursor scroll-reset bookkeeping.
    fn execute_list(&mut self, action: super::KeyAction, _key: KeyEvent) -> Option<Message> {
        use super::KeyAction as A;
        let old_index = self.list_index;

        match action {
            A::ListDown => {
                self.g_pending = false;
                if self.list_index < self.visible.len() - 1 {
                    self.list_index += 1;
                }
            }
            A::ListUp => {
                self.g_pending = false;
                self.list_index = self.list_index.saturating_sub(1);
            }
            A::ListTop => {
                // Reached only with `g` pending (the leader continuation).
                self.list_index = 0;
                self.g_pending = false;
            }
            A::ListBottom => {
                self.g_pending = false;
                self.list_index = self.visible.len().saturating_sub(1);
            }
            A::OpenEditor => {
                self.g_pending = false;
                self.push_action(Action::EditCurrent);
            }
            A::Reply => {
                self.g_pending = false;
                self.push_action(Action::Reply(false));
            }
            A::ReplyAll => {
                self.g_pending = false;
                self.push_action(Action::Reply(true));
            }
            A::Forward => {
                self.g_pending = false;
                if let Some(path) = self.selected_email_path() {
                    self.push_action(Action::OpenComposeWizard(ComposeMode::Forward {
                        source_path: path,
                    }));
                }
            }
            A::EditRecipients => {
                // Only meaningful in Drafts; outside Drafts keep the old status
                // hint (the guard is advisory, resolved everywhere).
                self.g_pending = false;
                if self.active_kind() == MailboxKind::Drafts {
                    if let Some(path) = self.selected_email_path() {
                        self.push_action(Action::OpenComposeWizard(ComposeMode::EditDraft {
                            source_path: path,
                        }));
                    }
                } else {
                    self.set_status(
                        "Edit recipients (c) is only available in Drafts".to_string(),
                    );
                }
            }
            A::SelectAllVisible => {
                self.g_pending = false;
                self.selection = self.visible_emails().map(|e| e.path.clone()).collect();
            }
            A::Archive => {
                self.g_pending = false;
                if !self.selection.is_empty() {
                    let count = self.selection.len();
                    self.overlay = Overlay::Confirm(ConfirmDialog {
                        title: format!("Archive {} emails?", count),
                        detail: format!("{} selected emails", count),
                        action: ConfirmAction::Archive,
                    });
                } else if let Some(email) = self.selected_email() {
                    self.overlay = Overlay::Confirm(ConfirmDialog {
                        title: "Archive this email?".to_string(),
                        detail: format!("{} - {}", email.from, email.subject),
                        action: ConfirmAction::Archive,
                    });
                }
            }
            A::Delete => {
                self.g_pending = false;
                if !self.selection.is_empty() {
                    let count = self.selection.len();
                    self.overlay = Overlay::Confirm(ConfirmDialog {
                        title: format!("Delete {} emails?", count),
                        detail: format!("{} selected emails", count),
                        action: ConfirmAction::Delete,
                    });
                } else if let Some(email) = self.selected_email() {
                    self.overlay = Overlay::Confirm(ConfirmDialog {
                        title: "Delete this email?".to_string(),
                        detail: format!("{} - {}", email.from, email.subject),
                        action: ConfirmAction::Delete,
                    });
                }
            }
            A::Approve => {
                self.g_pending = false;
                if !self.selection.is_empty() {
                    let count = self.selection.len();
                    self.overlay = Overlay::Confirm(ConfirmDialog {
                        title: format!("Approve {} drafts?", count),
                        detail: format!("{} selected drafts", count),
                        action: ConfirmAction::Approve,
                    });
                } else {
                    self.push_action(Action::Approve);
                }
            }
            A::MarkDraft => {
                self.g_pending = false;
                if !self.selection.is_empty() {
                    let count = self.selection.len();
                    self.overlay = Overlay::Confirm(ConfirmDialog {
                        title: format!("Mark {} drafts as draft?", count),
                        detail: format!("{} selected drafts", count),
                        action: ConfirmAction::MarkDraft,
                    });
                } else {
                    self.push_action(Action::MarkDraft);
                }
            }
            A::Send => {
                self.g_pending = false;
                if let Some(email) = self.selected_email() {
                    self.overlay = Overlay::Confirm(ConfirmDialog {
                        title: "Send this email?".to_string(),
                        detail: format!("To: {} - {}", email.to, email.subject),
                        action: ConfirmAction::Send,
                    });
                }
            }
            A::SendAll => {
                self.g_pending = false;
                self.overlay = Overlay::Confirm(ConfirmDialog {
                    title: "Send all approved emails?".to_string(),
                    detail: format!("In {}", self.active_label()),
                    action: ConfirmAction::SendApproved,
                });
            }
            A::CopyPath => {
                self.g_pending = false;
                self.push_action(Action::CopyPath);
            }
            A::ToggleRead => {
                self.g_pending = false;
                if !self.selection.is_empty() {
                    let paths: Vec<PathBuf> = self.selection.iter().cloned().collect();
                    self.push_action(Action::BatchToggleRead(paths));
                } else {
                    self.push_action(Action::ToggleRead);
                }
            }
            A::MovePicker => {
                self.g_pending = false;
                self.open_mailbox_picker();
            }
            A::Rsvp => {
                self.g_pending = false;
                self.open_rsvp_overlay();
            }
            A::NewDraft => {
                self.g_pending = false;
                self.push_action(Action::OpenComposeWizard(ComposeMode::New));
            }
            A::QuickSync => {
                self.g_pending = false;
                self.push_action(Action::Fetch);
            }
            A::FullSync => {
                self.g_pending = false;
                self.push_action(Action::Sync);
            }
            A::ServerSearch => {
                self.g_pending = false;
                self.server_search_query.clear();
                self.server_search_results.clear();
                self.server_search_index = 0;
                self.server_search_scroll = 0;
                self.server_search_headers_scroll = 0;
                self.server_search_focus = SearchOverlayFocus::Input;
                self.server_search_loading = false;
                self.server_search_status = None;
                self.overlay = Overlay::Search;
            }
            A::OpenAttachment => {
                self.g_pending = false;
                self.open_attachment_picker(AttachmentPickerMode::Open);
            }
            A::SaveAttachment => {
                self.g_pending = false;
                self.open_attachment_picker(AttachmentPickerMode::Save);
            }
            A::OpenInBrowser => {
                self.g_pending = false;
                if let Some(email) = self.selected_email() {
                    let html_path = email.path.with_extension("html");
                    if html_path.exists() {
                        self.push_action(Action::OpenHtmlInBrowser(html_path));
                    } else {
                        self.set_status("No HTML version available".to_string());
                    }
                }
            }
            A::ToggleSelect => {
                self.g_pending = false;
                if let Some(path) = self.selected_email_path() {
                    if self.selection.contains(&path) {
                        self.selection.remove(&path);
                    } else {
                        self.selection.insert(path);
                    }
                    if self.list_index < self.visible.len() - 1 {
                        self.list_index += 1;
                    }
                }
            }
            A::ClearSelection => {
                self.g_pending = false;
                if !self.selection.is_empty() {
                    self.selection.clear();
                }
            }
            // The leader key itself: begin/toggle the pending `g` chord. This is
            // the only action that intentionally leaves `g_pending` set.
            A::Manual => {
                // A::Manual reaches here only for the `g` PrefixLeader row.
                self.g_pending = !self.g_pending;
            }
            // Any pane-only action mistakenly routed here is a no-op.
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

    fn handle_confirm_key(&mut self, key: KeyEvent) -> Option<Message> {
        match key.code {
            KeyCode::Char('y') | KeyCode::Enter => {
                if let Overlay::Confirm(dialog) =
                    std::mem::replace(&mut self.overlay, Overlay::None)
                {
                    match dialog.action {
                        ConfirmAction::Approve if !self.selection.is_empty() => {
                            let paths: Vec<PathBuf> = self.selection.drain().collect();
                            self.push_action(Action::BatchApprove(paths));
                        }
                        ConfirmAction::MarkDraft if !self.selection.is_empty() => {
                            let paths: Vec<PathBuf> = self.selection.drain().collect();
                            self.push_action(Action::BatchMarkDraft(paths));
                        }
                        ConfirmAction::Archive if !self.selection.is_empty() => {
                            let paths: Vec<PathBuf> = self.selection.drain().collect();
                            self.push_action(Action::BatchArchive(paths));
                        }
                        ConfirmAction::Delete if !self.selection.is_empty() => {
                            let paths: Vec<PathBuf> = self.selection.drain().collect();
                            self.push_action(Action::BatchDelete(paths));
                        }
                        _ => {
                            self.push_action(match dialog.action {
                                ConfirmAction::Approve => Action::Approve,
                                ConfirmAction::MarkDraft => Action::MarkDraft,
                                ConfirmAction::Archive => Action::Archive,
                                ConfirmAction::Delete => Action::Delete,
                                ConfirmAction::Send => Action::Send,
                                ConfirmAction::SendApproved => Action::SendApproved,
                            });
                        }
                    }
                    // Consume-and-close: the confirm dialog was taken above.
                    // Promote any error that had been queued behind it.
                    self.promote_pending_error();
                }
            }
            KeyCode::Char('n') | KeyCode::Esc => {
                self.close_overlay();
            }
            _ => {}
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
                    let criteria =
                        crate::imap_client::parse_search_query(&self.server_search_query);
                    let (targets, scope_label) = if let Some(ref name) = criteria.in_mailbox {
                        if let Some(target) = self.search_target_by_name(name) {
                            let label = target.label.clone();
                            (vec![target], label)
                        } else {
                            self.server_search_status = Some(format!("Unknown mailbox: {}", name));
                            return None;
                        }
                    } else {
                        (self.all_search_targets(), "All".to_string())
                    };
                    self.server_search_scope_label = scope_label;
                    self.push_action(Action::ServerSearch {
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
                self.close_overlay();
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
                    self.close_overlay();
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
                self.push_action(Action::SearchResultOpen);
            }
            KeyCode::Char('r') => {
                self.push_action(Action::SearchResultReply(false));
            }
            KeyCode::Char('R') => {
                self.push_action(Action::SearchResultReply(true));
            }
            KeyCode::Char('w') => {
                self.push_action(Action::SearchResultForward);
            }
            KeyCode::Char('a') => {
                self.push_action(Action::SearchResultArchive);
            }
            KeyCode::Char('b') => {
                self.push_action(Action::SearchResultOpenInBrowser);
            }
            KeyCode::Char('o') => {
                self.open_search_result_attachment_picker(AttachmentPickerMode::Open);
            }
            KeyCode::Char('O') => {
                self.open_search_result_attachment_picker(AttachmentPickerMode::Save);
            }
            KeyCode::Tab | KeyCode::BackTab => {
                self.server_search_focus = SearchOverlayFocus::Input;
            }
            KeyCode::Esc => {
                self.close_overlay();
            }
            _ => {}
        }
        self.g_pending = false;
        None
    }

    // -----------------------------------------------------------------
    // Activity log overlay
    // -----------------------------------------------------------------

    fn handle_activity_overlay_key(&mut self, key: KeyEvent) -> Option<Message> {
        if self.activity_filter_active {
            match key.code {
                KeyCode::Char(c) => {
                    self.activity_filter.push(c);
                    self.activity_scroll = 0;
                }
                KeyCode::Backspace => {
                    self.activity_filter.pop();
                    self.activity_scroll = 0;
                }
                KeyCode::Enter => {
                    self.activity_filter_active = false;
                }
                KeyCode::Esc => {
                    if !self.activity_filter.is_empty() {
                        self.activity_filter.clear();
                        self.activity_filter_active = false;
                        self.activity_scroll = 0;
                    } else {
                        self.close_overlay();
                    }
                }
                _ => {}
            }
        } else {
            match key.code {
                KeyCode::Char('j') | KeyCode::Down => {
                    self.g_pending = false;
                    self.activity_scroll = self.activity_scroll.saturating_add(1);
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    self.g_pending = false;
                    self.activity_scroll = self.activity_scroll.saturating_sub(1);
                }
                KeyCode::Char('g') => {
                    if self.g_pending {
                        self.activity_scroll = 0;
                        self.g_pending = false;
                    } else {
                        self.g_pending = true;
                    }
                    return None;
                }
                KeyCode::Char('G') => {
                    self.g_pending = false;
                    self.activity_scroll = u16::MAX;
                }
                KeyCode::Char('d') => {
                    self.g_pending = false;
                    self.activity_scroll = self.activity_scroll.saturating_add(10);
                }
                KeyCode::Char('u') => {
                    self.g_pending = false;
                    self.activity_scroll = self.activity_scroll.saturating_sub(10);
                }
                KeyCode::Char('/') => {
                    self.g_pending = false;
                    self.activity_filter_active = true;
                    self.activity_filter.clear();
                    self.activity_scroll = 0;
                }
                KeyCode::Esc | KeyCode::Char('L') | KeyCode::Char('q') => {
                    self.g_pending = false;
                    self.close_overlay();
                    self.activity_scroll = 0;
                    self.activity_filter.clear();
                    self.activity_filter_active = false;
                }
                _ => {
                    self.g_pending = false;
                }
            }
        }
        None
    }

    // -----------------------------------------------------------------
    // Compose wizard
    // -----------------------------------------------------------------

    fn handle_compose_wizard_key(&mut self, key: KeyEvent) -> Option<Message> {
        let wizard = self.compose_wizard_mut()?;

        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);

        match key.code {
            KeyCode::Esc => {
                self.push_action(Action::ComposeWizardCancel);
                return None;
            }
            KeyCode::Tab => {
                wizard.focus = wizard.focus.next();
                wizard.suggestion_idx = 0;
                self.recompute_compose_suggestions();
                return None;
            }
            KeyCode::BackTab => {
                wizard.focus = wizard.focus.prev();
                wizard.suggestion_idx = 0;
                self.recompute_compose_suggestions();
                return None;
            }
            KeyCode::Up => {
                if wizard.focus.is_address()
                    && !wizard.suggestions.is_empty()
                    && wizard.suggestion_idx > 0
                {
                    wizard.suggestion_idx -= 1;
                }
                return None;
            }
            KeyCode::Down => {
                if wizard.focus.is_address()
                    && !wizard.suggestions.is_empty()
                    && wizard.suggestion_idx + 1 < wizard.suggestions.len()
                {
                    wizard.suggestion_idx += 1;
                }
                return None;
            }
            KeyCode::Char('g') if ctrl => {
                // Force-submit from any field.
                self.push_action(Action::ComposeWizardSubmit);
                return None;
            }
            KeyCode::Char('n') if ctrl => {
                if wizard.focus.is_address()
                    && !wizard.suggestions.is_empty()
                    && wizard.suggestion_idx + 1 < wizard.suggestions.len()
                {
                    wizard.suggestion_idx += 1;
                }
                return None;
            }
            KeyCode::Char('p') if ctrl => {
                if wizard.focus.is_address()
                    && !wizard.suggestions.is_empty()
                    && wizard.suggestion_idx > 0
                {
                    wizard.suggestion_idx -= 1;
                }
                return None;
            }
            KeyCode::Char('u') if ctrl => {
                // Clear the current field.
                current_field_mut(wizard).clear();
                self.recompute_compose_suggestions();
                return None;
            }
            KeyCode::Enter => {
                // On an address field with a highlighted suggestion, accept it.
                if wizard.focus.is_address() && !wizard.suggestions.is_empty() {
                    let sug = wizard.suggestions[wizard.suggestion_idx].clone();
                    accept_suggestion(current_field_mut(wizard), &sug);
                    // After accepting, clear the suggestion list so another
                    // Enter moves on rather than re-appending the same contact.
                    wizard.suggestions.clear();
                    wizard.suggestion_idx = 0;
                    return None;
                }
                // On subject (or an empty-suggestion address field), submit.
                if wizard.focus == ComposeField::Subject || !wizard.subject.trim().is_empty() {
                    self.push_action(Action::ComposeWizardSubmit);
                    return None;
                }
                // Otherwise cycle to the next field.
                wizard.focus = wizard.focus.next();
                wizard.suggestion_idx = 0;
                self.recompute_compose_suggestions();
                return None;
            }
            KeyCode::Backspace => {
                current_field_mut(wizard).pop();
                if wizard.focus.is_address() {
                    wizard.suggestion_idx = 0;
                    self.recompute_compose_suggestions();
                }
                return None;
            }
            KeyCode::Char(c) => {
                // Ctrl-prefixed chars not handled above are ignored.
                if ctrl {
                    return None;
                }
                let _ = shift; // Shift+letter is just the uppercase char.
                current_field_mut(wizard).push(c);
                if wizard.focus.is_address() {
                    wizard.suggestion_idx = 0;
                    self.recompute_compose_suggestions();
                }
                return None;
            }
            _ => {}
        }

        None
    }

    pub(crate) fn recompute_compose_suggestions(&mut self) {
        let Some(wizard) = self.compose_wizard_mut() else {
            return;
        };
        if !wizard.focus.is_address() {
            wizard.suggestions.clear();
            wizard.suggestion_idx = 0;
            return;
        }
        let field_value = match wizard.focus {
            ComposeField::To => &wizard.to,
            ComposeField::Cc => &wizard.cc,
            ComposeField::Bcc => &wizard.bcc,
            ComposeField::Subject => {
                wizard.suggestions.clear();
                return;
            }
        };
        let query = field_value
            .rsplit(',')
            .next()
            .unwrap_or("")
            .trim()
            .to_string();

        let Some(index) = wizard.contacts.as_ref() else {
            wizard.suggestions.clear();
            return;
        };

        // Don't flood with N untyped entries — only show suggestions
        // once the user has typed at least 1 char of the partial.
        if query.is_empty() {
            wizard.suggestions.clear();
            wizard.suggestion_idx = 0;
            return;
        }

        let results = crate::contacts::search(index, &query, 12);
        wizard.suggestions = results
            .into_iter()
            .map(|r| ComposeSuggestion {
                address: r.contact.address.clone(),
                display_name: r.contact.display_name.clone(),
                tier: if r.contact.sent_to > 0 {
                    2
                } else if r.contact.sent_cc > 0 {
                    1
                } else {
                    0
                },
            })
            .collect();
        wizard.suggestion_idx = 0;
    }

    /// Open the RSVP overlay for the cursor email, guarding against
    /// non-invites and self-authored (organizer-side) invites. RSVP is only
    /// for received REQUEST invites (D3): our own Sent invites make us the
    /// organizer, so we hint instead of opening.
    fn open_rsvp_overlay(&mut self) {
        let Some(email) = self.selected_email() else {
            return;
        };
        if !email.is_invite() {
            self.set_status("Not a calendar invite".to_string());
            return;
        }
        if self.active_kind() == MailboxKind::Sent {
            self.set_status(
                "You are the organizer of this invite — nothing to RSVP".to_string(),
            );
            return;
        }
        if !email.is_request_invite() {
            self.set_status(
                "Only received invitations (REQUEST) can be RSVP'd".to_string(),
            );
            return;
        }
        let summary = email
            .event
            .as_ref()
            .and_then(|e| e.summary.clone())
            .unwrap_or_else(|| email.subject.clone());
        self.overlay = Overlay::Rsvp(RsvpOverlay {
            path: email.path.clone(),
            summary,
            selected: 0,
        });
    }

    fn handle_rsvp_overlay_key(&mut self, key: KeyEvent) -> Option<Message> {
        let overlay = self.rsvp_overlay_mut()?;
        match key.code {
            KeyCode::Char('j') | KeyCode::Down | KeyCode::Tab => {
                if overlay.selected < 2 {
                    overlay.selected += 1;
                }
            }
            KeyCode::Char('k') | KeyCode::Up | KeyCode::BackTab => {
                overlay.selected = overlay.selected.saturating_sub(1);
            }
            KeyCode::Char('a') => overlay.selected = 0,
            KeyCode::Char('t') => overlay.selected = 1,
            KeyCode::Char('d') => overlay.selected = 2,
            KeyCode::Enter => {
                let choice = match overlay.selected {
                    0 => RsvpChoice::Accept,
                    1 => RsvpChoice::Tentative,
                    _ => RsvpChoice::Decline,
                };
                let Overlay::Rsvp(overlay) =
                    std::mem::replace(&mut self.overlay, Overlay::None)
                else {
                    return None;
                };
                self.push_action(Action::Rsvp {
                    path: overlay.path,
                    choice,
                });
                // Consume-and-close: promote any error queued behind it.
                self.promote_pending_error();
            }
            KeyCode::Esc | KeyCode::Char('q') => {
                self.close_overlay();
            }
            _ => {}
        }
        None
    }

    fn handle_attachment_picker_key(&mut self, key: KeyEvent) -> Option<Message> {
        let picker = self.attachment_picker_mut().unwrap();
        match picker.mode {
            AttachmentPickerMode::Open => match key.code {
                KeyCode::Char('j') | KeyCode::Down => {
                    if picker.selected < picker.files.len().saturating_sub(1) {
                        picker.selected += 1;
                    }
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    picker.selected = picker.selected.saturating_sub(1);
                }
                KeyCode::Enter => {
                    let Overlay::Attachment(picker) =
                        std::mem::replace(&mut self.overlay, Overlay::None)
                    else {
                        return None;
                    };
                    let path = picker.files[picker.selected].clone();
                    self.push_action(Action::OpenAttachment(path));
                    // Consume-and-close: promote any error queued behind it.
                    self.promote_pending_error();
                }
                KeyCode::Esc | KeyCode::Char('q') => {
                    self.close_overlay();
                }
                _ => {}
            },
            AttachmentPickerMode::Save => match key.code {
                KeyCode::Char('j') | KeyCode::Down => {
                    if picker.selected < picker.files.len().saturating_sub(1) {
                        picker.selected += 1;
                    }
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    picker.selected = picker.selected.saturating_sub(1);
                }
                KeyCode::Char(' ') => {
                    let idx = picker.selected;
                    if picker.selected_set.contains(&idx) {
                        picker.selected_set.remove(&idx);
                    } else {
                        picker.selected_set.insert(idx);
                    }
                    // Advance cursor
                    if picker.selected < picker.files.len().saturating_sub(1) {
                        picker.selected += 1;
                    }
                }
                KeyCode::Char('a') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    if picker.selected_set.len() == picker.files.len() {
                        picker.selected_set.clear();
                    } else {
                        picker.selected_set = (0..picker.files.len()).collect();
                    }
                }
                KeyCode::Enter => {
                    let Overlay::Attachment(picker) =
                        std::mem::replace(&mut self.overlay, Overlay::None)
                    else {
                        return None;
                    };
                    // Collect selected files, or cursor item if none selected
                    let sources: Vec<PathBuf> = if picker.selected_set.is_empty() {
                        vec![picker.files[picker.selected].clone()]
                    } else {
                        let mut indices: Vec<usize> =
                            picker.selected_set.iter().copied().collect();
                        indices.sort();
                        indices
                            .iter()
                            .filter_map(|&i| picker.files.get(i).cloned())
                            .collect()
                    };
                    // Overlay->overlay handoff: `open_dir_picker` sets
                    // `Overlay::Dir`, so `promote_pending_error` (guarded on
                    // `Overlay::None`) correctly does NOT fire between taking
                    // the attachment picker and opening the dir picker.
                    self.open_dir_picker(sources);
                    self.promote_pending_error();
                }
                KeyCode::Esc | KeyCode::Char('q') => {
                    self.close_overlay();
                }
                _ => {}
            },
        }
        None
    }

    fn handle_dir_picker_key(&mut self, key: KeyEvent) -> Option<Message> {
        let picker = match self.dir_picker_mut() {
            Some(p) => p,
            None => return None,
        };

        match picker.mode {
            DirPickerMode::Zoxide => match key.code {
                KeyCode::Down => {
                    if !picker.zoxide_results.is_empty()
                        && picker.selected < picker.zoxide_results.len().saturating_sub(1)
                    {
                        picker.selected += 1;
                    }
                }
                KeyCode::Up => {
                    picker.selected = picker.selected.saturating_sub(1);
                }
                KeyCode::Backspace => {
                    picker.query.pop();
                    picker.selected = 0;
                    refresh_zoxide_results(picker);
                }
                KeyCode::Tab => {
                    // Switch to browser mode. Use highlighted result or default dir.
                    let start_dir = picker
                        .zoxide_results
                        .get(picker.selected)
                        .cloned()
                        .unwrap_or_else(|| picker.current_dir.clone());
                    picker.mode = DirPickerMode::Browser;
                    picker.current_dir = start_dir;
                    picker.selected = 0;
                    refresh_browser_entries(picker);
                }
                KeyCode::Enter => {
                    if let Some(dir) = picker.zoxide_results.get(picker.selected).cloned() {
                        let sources = picker.sources.clone();
                        self.close_overlay();
                        self.push_action(Action::SaveAttachments {
                            sources,
                            dest_dir: dir,
                        });
                    }
                }
                KeyCode::Esc => {
                    self.close_overlay();
                }
                KeyCode::Char(c) => {
                    picker.query.push(c);
                    picker.selected = 0;
                    refresh_zoxide_results(picker);
                }
                _ => {}
            },
            DirPickerMode::Browser => match key.code {
                KeyCode::Char('j') | KeyCode::Down => {
                    let max = picker.dir_entries.len(); // entry 0 = [ Save here ]
                    if picker.selected < max {
                        picker.selected += 1;
                    }
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    picker.selected = picker.selected.saturating_sub(1);
                }
                KeyCode::Char('g') => {
                    picker.selected = 0;
                }
                KeyCode::Char('G') => {
                    picker.selected = picker.dir_entries.len(); // last entry (save-here is 0)
                }
                KeyCode::Char('~') => {
                    let home =
                        PathBuf::from(shellexpand::tilde("~").into_owned());
                    picker.current_dir = home;
                    picker.selected = 0;
                    refresh_browser_entries(picker);
                }
                KeyCode::Char('h') | KeyCode::Backspace => {
                    if let Some(parent) = picker.current_dir.parent() {
                        picker.current_dir = parent.to_path_buf();
                        picker.selected = 0;
                        refresh_browser_entries(picker);
                    }
                }
                KeyCode::Char('l') => {
                    // Descend into selected directory (no-op on "[ Save here ]")
                    if picker.selected > 0 {
                        let idx = picker.selected - 1;
                        if let Some(dir) = picker.dir_entries.get(idx).cloned() {
                            picker.current_dir = dir;
                            picker.selected = 0;
                            refresh_browser_entries(picker);
                        }
                    }
                }
                KeyCode::Enter => {
                    if picker.selected == 0 {
                        // "[ Save here ]" -- confirm
                        let sources = picker.sources.clone();
                        let dest_dir = picker.current_dir.clone();
                        self.close_overlay();
                        self.push_action(Action::SaveAttachments {
                            sources,
                            dest_dir,
                        });
                    } else {
                        // Descend into selected directory
                        let idx = picker.selected - 1;
                        if let Some(dir) = picker.dir_entries.get(idx).cloned() {
                            picker.current_dir = dir;
                            picker.selected = 0;
                            refresh_browser_entries(picker);
                        }
                    }
                }
                KeyCode::Tab => {
                    picker.mode = DirPickerMode::Zoxide;
                    picker.selected = 0;
                    refresh_zoxide_results(picker);
                }
                KeyCode::Esc => {
                    self.close_overlay();
                }
                _ => {}
            },
        }
        None
    }

    // -----------------------------------------------------------------
    // Quick-move mailbox picker (#0018)
    // -----------------------------------------------------------------

    /// Open the quick-move picker for the current selection (or the
    /// cursor email). Candidates are all server-backed mailboxes other
    /// than the active one, so "move to the same mailbox" cannot happen.
    fn open_mailbox_picker(&mut self) {
        // The source mailbox must have a server-side folder; Drafts
        // (local-only) can't be quick-moved -- drafts leave via send.
        if self
            .mailboxes
            .get(self.active_mailbox)
            .and_then(|m| m.server_name.as_ref())
            .is_none()
        {
            self.set_status("Quick-move is not available in this mailbox".to_string());
            return;
        }

        let paths: Vec<PathBuf> = if !self.selection.is_empty() {
            self.selection.iter().cloned().collect()
        } else if let Some(p) = self.selected_email_path() {
            vec![p]
        } else {
            return;
        };

        let candidates: Vec<(usize, String)> = self
            .mailboxes
            .iter()
            .enumerate()
            .filter(|(i, m)| *i != self.active_mailbox && m.server_name.is_some())
            .map(|(i, m)| (i, m.label.clone()))
            .collect();
        if candidates.is_empty() {
            self.set_status("No other mailboxes to move to".to_string());
            return;
        }

        let filtered = (0..candidates.len()).collect();
        self.overlay = Overlay::Mailbox(MailboxPicker {
            query: String::new(),
            candidates,
            filtered,
            selected: 0,
            paths,
        });
    }

    fn handle_mailbox_picker_key(&mut self, key: KeyEvent) -> Option<Message> {
        let picker = self.mailbox_picker_mut()?;

        match key.code {
            KeyCode::Down | KeyCode::Tab => {
                if !picker.filtered.is_empty()
                    && picker.selected < picker.filtered.len() - 1
                {
                    picker.selected += 1;
                }
            }
            KeyCode::Up | KeyCode::BackTab => {
                picker.selected = picker.selected.saturating_sub(1);
            }
            KeyCode::Enter => {
                if let Some(&cand_idx) = picker.filtered.get(picker.selected) {
                    let dest_idx = picker.candidates[cand_idx].0;
                    let Overlay::Mailbox(picker) =
                        std::mem::replace(&mut self.overlay, Overlay::None)
                    else {
                        return None;
                    };
                    self.selection.clear();
                    self.push_action(Action::MoveToMailbox {
                        paths: picker.paths,
                        dest_idx,
                    });
                    // Consume-and-close: promote any error queued behind it.
                    self.promote_pending_error();
                }
            }
            KeyCode::Esc => {
                self.close_overlay();
            }
            KeyCode::Backspace => {
                picker.query.pop();
                refresh_mailbox_picker_filter(picker);
            }
            KeyCode::Char(c) => {
                picker.query.push(c);
                refresh_mailbox_picker_filter(picker);
            }
            _ => {}
        }
        None
    }

    /// Helper to open the attachment picker in the given mode.
    fn open_attachment_picker(&mut self, mode: AttachmentPickerMode) {
        if let Some(email) = self.selected_email() {
            match crate::parse::list_attachments(&email.path) {
                Ok(files) if files.is_empty() => {
                    self.set_status("No attachments".to_string());
                }
                Ok(files) if files.len() == 1 && mode == AttachmentPickerMode::Open => {
                    self.push_action(Action::OpenAttachment(files.into_iter().next().unwrap()));
                }
                Ok(files) if files.len() == 1 && mode == AttachmentPickerMode::Save => {
                    // Single file in save mode -- skip picker, go straight to dir picker
                    let sources = files;
                    self.open_dir_picker(sources);
                }
                Ok(files) => {
                    self.overlay = Overlay::Attachment(AttachmentPicker {
                        files,
                        selected: 0,
                        mode,
                        selected_set: HashSet::new(),
                    });
                }
                Err(e) => {
                    self.set_status(format!("Attachments error: {e}"));
                }
            }
        }
    }

    /// Helper to open the attachment picker for a search result.
    /// Saves the search result locally first, then feeds into the same
    /// attachment picker / dir picker pipeline as the regular list.
    fn open_search_result_attachment_picker(&mut self, mode: AttachmentPickerMode) {
        let path = match super::super::helpers::ensure_search_result_saved(self) {
            Some(p) => p,
            None => {
                self.set_status("Failed to save email locally".to_string());
                return;
            }
        };

        match crate::parse::list_attachments(&path) {
            Ok(files) if files.is_empty() => {
                self.set_status("No attachments".to_string());
            }
            Ok(files) if files.len() == 1 && mode == AttachmentPickerMode::Open => {
                self.push_action(Action::OpenAttachment(files.into_iter().next().unwrap()));
            }
            Ok(files) if files.len() == 1 && mode == AttachmentPickerMode::Save => {
                let sources = files;
                self.open_dir_picker(sources);
            }
            Ok(files) => {
                self.overlay = Overlay::Attachment(AttachmentPicker {
                    files,
                    selected: 0,
                    mode,
                    selected_set: HashSet::new(),
                });
            }
            Err(e) => {
                self.set_status(format!("Attachments error: {e}"));
            }
        }
    }

    /// Open the directory picker overlay with the given source files.
    fn open_dir_picker(&mut self, sources: Vec<PathBuf>) {
        let default_dir = self
            .last_save_dir
            .clone()
            .unwrap_or_else(|| PathBuf::from(shellexpand::tilde("~/Downloads").into_owned()));

        let zoxide_available = which_zoxide();
        let start_mode = if zoxide_available {
            DirPickerMode::Zoxide
        } else {
            DirPickerMode::Browser
        };

        let mut picker = DirPicker {
            mode: start_mode,
            query: String::new(),
            zoxide_results: Vec::new(),
            zoxide_available,
            current_dir: default_dir,
            dir_entries: Vec::new(),
            selected: 0,
            sources,
        };

        if zoxide_available {
            refresh_zoxide_results(&mut picker);
        } else {
            refresh_browser_entries(&mut picker);
        }

        self.overlay = Overlay::Dir(picker);
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
                        self.close_overlay();
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
                    self.close_overlay();
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
                self.close_overlay();
                self.push_action(Action::Sync);
            }
            KeyCode::Char('d') | KeyCode::Esc => {
                self.close_overlay();
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
                let old_lower = self.search_query.to_lowercase();
                self.search_query.push(c);
                // Appending a character normally only shrinks the match
                // set (substring containment is monotone), so narrow the
                // current visible set instead of rescanning everything.
                // But lowercasing is not always append-monotone: Greek
                // capital sigma is context-sensitive ("ΘΕΟΣ" -> "θεος"
                // with final ς, yet "ΘΕΟΣΦ" -> "θεοσφ" with medial σ),
                // so a haystack can match the extended query without
                // matching the shorter one. Narrow only when the old
                // lowercased query is a prefix of the new one; otherwise
                // recompute from the full list.
                let narrow = self.search_query.to_lowercase().starts_with(&old_lower);
                self.apply_search_filter(narrow);
            }
            KeyCode::Backspace => {
                self.search_query.pop();
                self.apply_search_filter(false);
            }
            _ => {}
        }
        None
    }

    /// Recompute the visible view for the current `search_query` (P3).
    ///
    /// `narrow` may be true only when the *lowercased* query changed by
    /// appending characters to the lowercased query the current view was
    /// built from (i.e. a keystroke in search mode that keeps the old
    /// lowered query as a prefix): substring matching is monotone under
    /// query extension, so the new match set is a subset of the current
    /// one and we can retain-filter `visible` instead of rescanning the
    /// full list. Backspace/edits/resets — and appends where lowercasing
    /// rewrites earlier characters (Greek final sigma) — must pass
    /// `narrow = false`.
    ///
    /// The needle is lowercased once per call, not once per email.
    pub(crate) fn apply_search_filter(&mut self, narrow: bool) {
        self.selection.clear();

        if self.search_query.is_empty() {
            self.visible = (0..self.emails.len()).collect();
        } else {
            let kind = self.active_kind();
            let includes_body = self.search_includes_body;
            if narrow {
                let needle = self.search_query.to_lowercase();
                narrow_visible(&self.emails, &mut self.visible, &needle, kind, includes_body);
            } else {
                // `filter_visible` lowercases the needle once internally.
                self.visible =
                    filter_visible(&self.emails, &self.search_query, kind, includes_body);
            }
        }

        self.list_index = 0;
        self.headers_scroll = 0;
        self.preview_scroll = 0;
    }

    /// Reset the view after clearing/entering search: reapply the (now
    /// usually empty) query to the full list and reset the cursor. The
    /// full list is always at hand in `self.emails` (it is never
    /// filtered in place anymore), so no cache round-trip is needed.
    pub(crate) fn reload_from_cache(&mut self) {
        self.rebuild_visible();
        self.list_index = 0;
        self.headers_scroll = 0;
        self.preview_scroll = 0;
    }
}

// ---------------------------------------------------------------------------
// Search filter helpers (P2/P3: visible-indices model)
// ---------------------------------------------------------------------------

/// Does `email` match the (already lowercased) search needle?
fn email_matches(
    email: &EmailEntry,
    needle_lower: &str,
    kind: MailboxKind,
    includes_body: bool,
) -> bool {
    email.subject.to_lowercase().contains(needle_lower)
        || email
            .display_contact(kind)
            .to_lowercase()
            .contains(needle_lower)
        || email.date_display.to_lowercase().contains(needle_lower)
        || email.from.to_lowercase().contains(needle_lower)
        || email.to.to_lowercase().contains(needle_lower)
        || (includes_body && email.body.to_lowercase().contains(needle_lower))
}

/// Build the visible-index view of `emails` for `query` from scratch.
/// Empty query -> all indices (in order). The needle is lowercased once.
pub(super) fn filter_visible(
    emails: &[EmailEntry],
    query: &str,
    kind: MailboxKind,
    includes_body: bool,
) -> Vec<usize> {
    if query.is_empty() {
        return (0..emails.len()).collect();
    }
    let needle = query.to_lowercase();
    emails
        .iter()
        .enumerate()
        .filter(|(_, e)| email_matches(e, &needle, kind, includes_body))
        .map(|(i, _)| i)
        .collect()
}

/// Narrow an existing visible set in place: keep only the indices whose
/// email still matches the (extended) needle. Valid only when the new
/// query is an extension of the one `visible` was built from.
fn narrow_visible(
    emails: &[EmailEntry],
    visible: &mut Vec<usize>,
    needle_lower: &str,
    kind: MailboxKind,
    includes_body: bool,
) {
    visible.retain(|&i| {
        emails
            .get(i)
            .is_some_and(|e| email_matches(e, needle_lower, kind, includes_body))
    });
}

// ---------------------------------------------------------------------------
// Compose wizard free helpers
// ---------------------------------------------------------------------------

fn current_field_mut(wizard: &mut ComposeWizard) -> &mut String {
    match wizard.focus {
        ComposeField::To => &mut wizard.to,
        ComposeField::Cc => &mut wizard.cc,
        ComposeField::Bcc => &mut wizard.bcc,
        ComposeField::Subject => &mut wizard.subject,
    }
}

/// Aerc-style suggestion acceptance: replace the trailing partial
/// (everything after the last comma) with the suggestion's address,
/// then append `, ` so the user can keep typing more recipients.
fn accept_suggestion(field: &mut String, suggestion: &ComposeSuggestion) {
    let prefix_end = field.rfind(',').map(|i| i + 1).unwrap_or(0);
    field.truncate(prefix_end);
    if prefix_end > 0 && !field.ends_with(' ') {
        field.push(' ');
    }
    // If we have a display name, render "Name <addr>, ". Otherwise just "addr, ".
    if suggestion.display_name.is_empty() {
        field.push_str(&suggestion.address);
    } else {
        // Quote the display name if it contains commas.
        if suggestion.display_name.contains(',') {
            field.push('"');
            field.push_str(&suggestion.display_name);
            field.push('"');
        } else {
            field.push_str(&suggestion.display_name);
        }
        field.push_str(" <");
        field.push_str(&suggestion.address);
        field.push('>');
    }
    field.push_str(", ");
}

// ---------------------------------------------------------------------------
// Quick-move mailbox picker helpers (#0018)
// ---------------------------------------------------------------------------

/// Case-insensitive subsequence ("fuzzy") match: every char of `needle`
/// must appear in `haystack` in order. Empty needle matches everything.
pub(super) fn fuzzy_match(needle: &str, haystack: &str) -> bool {
    let hay = haystack.to_lowercase();
    let mut hay_chars = hay.chars();
    needle
        .to_lowercase()
        .chars()
        .all(|nc| hay_chars.any(|hc| hc == nc))
}

/// Recompute `picker.filtered` from `picker.query` and reset the cursor.
pub(super) fn refresh_mailbox_picker_filter(picker: &mut MailboxPicker) {
    picker.filtered = picker
        .candidates
        .iter()
        .enumerate()
        .filter(|(_, (_, label))| fuzzy_match(&picker.query, label))
        .map(|(i, _)| i)
        .collect();
    picker.selected = 0;
}

// ---------------------------------------------------------------------------
// Directory picker helpers
// ---------------------------------------------------------------------------

/// Check whether `zoxide` is available on PATH.
fn which_zoxide() -> bool {
    std::process::Command::new("zoxide")
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Run `zoxide query --list <terms>` and populate `picker.zoxide_results`.
fn refresh_zoxide_results(picker: &mut DirPicker) {
    if !picker.zoxide_available {
        picker.zoxide_results.clear();
        return;
    }

    let mut cmd = std::process::Command::new("zoxide");
    cmd.arg("query").arg("--list");
    if !picker.query.is_empty() {
        // Split query on whitespace so "no do" becomes two positional args
        for term in picker.query.split_whitespace() {
            cmd.arg(term);
        }
    }
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::null());

    picker.zoxide_results = match cmd.output() {
        Ok(output) if output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            stdout
                .lines()
                .filter(|l| !l.is_empty())
                .take(20)
                .map(|l| PathBuf::from(l.trim()))
                .collect()
        }
        _ => Vec::new(),
    };
}

/// Scan `picker.current_dir` for sub-directories and populate `picker.dir_entries`.
fn refresh_browser_entries(picker: &mut DirPicker) {
    picker.dir_entries = match std::fs::read_dir(&picker.current_dir) {
        Ok(rd) => {
            let mut dirs: Vec<PathBuf> = rd
                .filter_map(|e| e.ok())
                .filter(|e| e.file_type().is_ok_and(|ft| ft.is_dir()))
                .filter(|e| {
                    // Hide dotfiles
                    e.file_name()
                        .to_str()
                        .map(|s| !s.starts_with('.'))
                        .unwrap_or(false)
                })
                .map(|e| e.path())
                .collect();
            dirs.sort_by(|a, b| {
                a.file_name()
                    .unwrap_or_default()
                    .to_ascii_lowercase()
                    .cmp(&b.file_name().unwrap_or_default().to_ascii_lowercase())
            });
            dirs
        }
        Err(_) => Vec::new(),
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::PersistentError;
    use std::path::PathBuf;

    fn entry(subject: &str, from: &str, body: &str) -> EmailEntry {
        EmailEntry {
            path: PathBuf::from(format!("/mail/inbox/{subject}.md")),
            from: from.to_string(),
            to: "me@example.com".to_string(),
            cc: None,
            subject: subject.to_string(),
            status: "inbox".to_string(),
            date_display: "2026-07-01".to_string(),
            date_sort: "2026-07-01T00:00:00".to_string(),
            body: body.to_string(),
            has_attachments: false,
            read: false,
            event: None,
        }
    }

    fn sample() -> Vec<EmailEntry> {
        vec![
            entry("Invoice March", "Alice", "please pay"),
            entry("Invoice April", "Bob", "reminder"),
            entry("Weekly report", "Alice", "invoice attached"),
            entry("Holiday plans", "Carol", "beach"),
        ]
    }

    // -----------------------------------------------------------------------
    // filter_visible (P2: visible-index mapping)
    // -----------------------------------------------------------------------

    #[test]
    fn empty_query_yields_all_indices_in_order() {
        let emails = sample();
        assert_eq!(
            filter_visible(&emails, "", MailboxKind::Inbox, false),
            vec![0, 1, 2, 3]
        );
    }

    #[test]
    fn filter_matches_subject_case_insensitively() {
        let emails = sample();
        assert_eq!(
            filter_visible(&emails, "INVOICE", MailboxKind::Inbox, false),
            vec![0, 1]
        );
    }

    #[test]
    fn filter_matches_contact_field() {
        let emails = sample();
        // Inbox displays `from`; Alice appears in entries 0 and 2.
        assert_eq!(
            filter_visible(&emails, "alice", MailboxKind::Inbox, false),
            vec![0, 2]
        );
    }

    #[test]
    fn body_matches_only_when_included() {
        let emails = sample();
        assert_eq!(
            filter_visible(&emails, "beach", MailboxKind::Inbox, false),
            Vec::<usize>::new()
        );
        assert_eq!(
            filter_visible(&emails, "beach", MailboxKind::Inbox, true),
            vec![3]
        );
    }

    #[test]
    fn filter_indices_map_back_to_underlying_entries() {
        let emails = sample();
        let visible = filter_visible(&emails, "invoice", MailboxKind::Inbox, true);
        // "invoice" hits subjects 0/1 and the body of 2 (content search).
        assert_eq!(visible, vec![0, 1, 2]);
        // Selecting position 2 of the view must resolve to "Weekly report":
        // the actions layer operates on the underlying entry via this map.
        assert_eq!(emails[visible[2]].subject, "Weekly report");
    }

    // -----------------------------------------------------------------------
    // narrow_visible (P3: incremental search narrowing)
    // -----------------------------------------------------------------------

    #[test]
    fn narrowing_equals_full_recompute_on_append() {
        let emails = sample();
        // Simulate typing "inv" then "invo": narrow from the "inv" view.
        let mut visible = filter_visible(&emails, "inv", MailboxKind::Inbox, true);
        narrow_visible(&emails, &mut visible, "invo", MailboxKind::Inbox, true);
        assert_eq!(
            visible,
            filter_visible(&emails, "invo", MailboxKind::Inbox, true)
        );
    }

    #[test]
    fn narrowing_removes_entries_that_stop_matching() {
        let emails = sample();
        let mut visible = filter_visible(&emails, "invoice", MailboxKind::Inbox, false);
        assert_eq!(visible, vec![0, 1]);
        narrow_visible(&emails, &mut visible, "invoice m", MailboxKind::Inbox, false);
        assert_eq!(visible, vec![0]);
        narrow_visible(&emails, &mut visible, "invoice mx", MailboxKind::Inbox, false);
        assert!(visible.is_empty());
    }

    #[test]
    fn narrowing_ignores_out_of_range_indices() {
        let emails = sample();
        // A stale index beyond the list must be dropped, not panic.
        let mut visible = vec![0, 99];
        narrow_visible(&emails, &mut visible, "invoice", MailboxKind::Inbox, false);
        assert_eq!(visible, vec![0]);
    }

    #[test]
    fn greek_final_sigma_append_falls_back_to_full_recompute() {
        // Lowercasing is context-sensitive for Greek capital sigma:
        // "ΘΕΟΣ".to_lowercase() == "θεος" (final ς) does NOT match the
        // haystack "θεοσφανια", but "ΘΕΟΣΦ".to_lowercase() == "θεοσφ"
        // (medial σ) does. Naive narrowing over the previous visible set
        // would drop the entry forever; the fifth keystroke must fall
        // back to a full recompute and bring it back.
        let mut emails = sample();
        emails.push(entry("θεοσφανια", "Dora", ""));
        let theos_idx = emails.len() - 1;
        let mut app = app_with_emails(emails);
        app.focus = Focus::Search;
        for c in "ΘΕΟΣ".chars() {
            app.handle_search_key(KeyEvent::from(KeyCode::Char(c)));
        }
        // "θεος" matches nothing.
        assert!(app.visible.is_empty());
        app.handle_search_key(KeyEvent::from(KeyCode::Char('Φ')));
        // "θεοσφ" matches the haystack again.
        assert_eq!(app.visible, vec![theos_idx]);
    }

    // -----------------------------------------------------------------------
    // App-level: selection mapping through the visible view
    // -----------------------------------------------------------------------

    fn app_with_emails(emails: Vec<EmailEntry>) -> App {
        let mut app = App::default_for_tests();
        app.emails = std::sync::Arc::new(emails);
        app.email_cache = vec![Some(std::sync::Arc::clone(&app.emails))];
        app.mailbox_counts = vec![app.emails.len()];
        app.rebuild_visible();
        app
    }

    #[test]
    fn selected_email_resolves_through_visible_indices() {
        let mut app = app_with_emails(sample());
        app.search_query = "invoice".to_string();
        app.search_includes_body = true;
        app.apply_search_filter(false);
        // View: [Invoice March, Invoice April, Weekly report]
        app.list_index = 2;
        assert_eq!(app.selected_email().unwrap().subject, "Weekly report");
    }

    #[test]
    fn remove_selected_maps_back_to_underlying_entry_under_filter() {
        let mut app = app_with_emails(sample());
        app.search_query = "invoice".to_string();
        app.search_includes_body = true;
        app.apply_search_filter(false);
        app.list_index = 2; // "Weekly report" via body match

        let removed = app.remove_selected_from_list().unwrap();
        assert!(removed.ends_with("Weekly report.md"));
        // The underlying list lost exactly that entry...
        assert_eq!(app.emails.len(), 3);
        assert!(app.emails.iter().all(|e| e.subject != "Weekly report"));
        // ...the cache slot shares the same (updated) allocation...
        let cached = app.email_cache[0].as_ref().unwrap();
        assert!(std::sync::Arc::ptr_eq(cached, &app.emails));
        // ...and the filtered view is rebuilt with the cursor clamped.
        assert_eq!(app.visible, vec![0, 1]);
        assert_eq!(app.list_index, 1);
    }

    #[test]
    fn set_email_read_updates_shared_cache_without_deep_clone() {
        let mut app = app_with_emails(sample());
        let path = app.emails[1].path.clone();
        app.set_email_read(&path, true);
        assert!(app.emails[1].read);
        let cached = app.email_cache[0].as_ref().unwrap();
        assert!(std::sync::Arc::ptr_eq(cached, &app.emails));
        assert!(cached[1].read);
    }

    #[test]
    fn with_emails_mut_leaves_invalidated_cache_slot_none() {
        let mut app = app_with_emails(sample());
        app.email_cache[0] = None; // e.g. load in flight
        let path = app.emails[0].path.clone();
        app.set_email_read(&path, true);
        assert!(app.emails[0].read);
        assert!(app.email_cache[0].is_none());
    }

    // -----------------------------------------------------------------------
    // Quick-move mailbox picker (#0018)
    // -----------------------------------------------------------------------

    #[test]
    fn fuzzy_match_empty_needle_matches_everything() {
        assert!(fuzzy_match("", "Inbox"));
        assert!(fuzzy_match("", ""));
    }

    #[test]
    fn fuzzy_match_is_case_insensitive_subsequence() {
        assert!(fuzzy_match("arc", "Archive"));
        assert!(fuzzy_match("ACV", "archive")); // a..c..v in order
        assert!(fuzzy_match("inbx", "Inbox"));
        assert!(!fuzzy_match("xz", "Inbox"));
        assert!(!fuzzy_match("xobni", "Inbox")); // order matters
    }

    fn picker_with_labels(labels: &[&str]) -> super::super::MailboxPicker {
        super::super::MailboxPicker {
            query: String::new(),
            candidates: labels
                .iter()
                .enumerate()
                .map(|(i, l)| (i, l.to_string()))
                .collect(),
            filtered: (0..labels.len()).collect(),
            selected: 0,
            paths: vec![PathBuf::from("/mail/inbox/a.md")],
        }
    }

    #[test]
    fn picker_filter_narrows_and_resets_cursor() {
        let mut picker = picker_with_labels(&["Inbox", "Sent", "Archive", "Newsletters"]);
        picker.selected = 3;
        picker.query = "ne".to_string();
        refresh_mailbox_picker_filter(&mut picker);
        // "ne" (subsequence): Se~n~t? no 'e' after 'n'... Sent = s,e,n,t:
        // n then e fails (e precedes n). Archive has no 'n'. Newsletters
        // and Inbox? i-n-b-o-x: n then no e. Only Newsletters matches.
        assert_eq!(picker.filtered, vec![3]);
        assert_eq!(picker.selected, 0);
    }

    #[test]
    fn picker_filter_empty_query_restores_all() {
        let mut picker = picker_with_labels(&["Inbox", "Archive"]);
        picker.query = "arch".to_string();
        refresh_mailbox_picker_filter(&mut picker);
        assert_eq!(picker.filtered, vec![1]);
        picker.query.clear();
        refresh_mailbox_picker_filter(&mut picker);
        assert_eq!(picker.filtered, vec![0, 1]);
    }

    #[test]
    fn picker_filter_no_match_yields_empty() {
        let mut picker = picker_with_labels(&["Inbox", "Archive"]);
        picker.query = "zzz".to_string();
        refresh_mailbox_picker_filter(&mut picker);
        assert!(picker.filtered.is_empty());
    }

    fn mb_info(label: &str, dir: &str, kind: MailboxKind, server: Option<&str>) -> super::super::MailboxInfo {
        super::super::MailboxInfo {
            label: label.to_string(),
            icon: "",
            dir: PathBuf::from(dir),
            kind,
            server_name: server.map(|s| s.to_string()),
        }
    }

    fn app_with_mailboxes() -> App {
        let mut app = app_with_emails(sample());
        app.mailboxes = vec![
            mb_info("Inbox", "/mail/inbox", MailboxKind::Inbox, Some("INBOX")),
            mb_info("Drafts", "/mail/drafts", MailboxKind::Drafts, None),
            mb_info("Sent", "/mail/sent", MailboxKind::Sent, Some("Sent")),
            mb_info("Archive", "/mail/archive", MailboxKind::Archive, Some("Archive")),
        ];
        app.active_mailbox = 0;
        app
    }

    #[test]
    fn open_picker_excludes_active_mailbox_and_local_only() {
        let mut app = app_with_mailboxes();
        app.handle_key(KeyEvent::from(KeyCode::Char('M')));
        let Overlay::Mailbox(picker) = &app.overlay else {
            panic!("picker should open");
        };
        // Active mailbox (Inbox) and local-only Drafts are excluded.
        let labels: Vec<&str> = picker
            .candidates
            .iter()
            .map(|(_, l)| l.as_str())
            .collect();
        assert_eq!(labels, vec!["Sent", "Archive"]);
        // Cursor email is carried as the move target.
        assert_eq!(picker.paths.len(), 1);
    }

    #[test]
    fn open_picker_uses_selection_when_present() {
        let mut app = app_with_mailboxes();
        app.selection.insert(app.emails[0].path.clone());
        app.selection.insert(app.emails[2].path.clone());
        app.handle_key(KeyEvent::from(KeyCode::Char('M')));
        let Overlay::Mailbox(picker) = &app.overlay else {
            panic!("picker should open");
        };
        assert_eq!(picker.paths.len(), 2);
    }

    #[test]
    fn open_picker_refused_in_local_only_mailbox() {
        let mut app = app_with_mailboxes();
        app.active_mailbox = 1; // Drafts: no server folder
        app.handle_key(KeyEvent::from(KeyCode::Char('M')));
        assert!(!matches!(app.overlay, Overlay::Mailbox(_)));
    }

    #[test]
    fn picker_enter_pushes_move_action_with_dest() {
        let mut app = app_with_mailboxes();
        app.handle_key(KeyEvent::from(KeyCode::Char('M')));
        // Filter to "Archive", then confirm.
        for c in "arch".chars() {
            app.handle_key(KeyEvent::from(KeyCode::Char(c)));
        }
        app.handle_key(KeyEvent::from(KeyCode::Enter));
        assert!(!matches!(app.overlay, Overlay::Mailbox(_)));
        match app.pending_actions.pop_front() {
            Some(Action::MoveToMailbox { paths, dest_idx }) => {
                assert_eq!(dest_idx, 3); // Archive
                assert_eq!(paths.len(), 1);
            }
            other => panic!("expected MoveToMailbox, got {:?}", other),
        }
    }

    #[test]
    fn picker_esc_closes_without_action() {
        let mut app = app_with_mailboxes();
        app.handle_key(KeyEvent::from(KeyCode::Char('M')));
        assert!(matches!(app.overlay, Overlay::Mailbox(_)));
        app.handle_key(KeyEvent::from(KeyCode::Esc));
        assert!(!matches!(app.overlay, Overlay::Mailbox(_)));
        assert!(app.pending_actions.is_empty());
    }

    #[test]
    fn picker_enter_on_no_match_is_noop() {
        let mut app = app_with_mailboxes();
        app.handle_key(KeyEvent::from(KeyCode::Char('M')));
        for c in "zzz".chars() {
            app.handle_key(KeyEvent::from(KeyCode::Char(c)));
        }
        app.handle_key(KeyEvent::from(KeyCode::Enter));
        // No match highlighted: picker stays open, nothing queued.
        assert!(matches!(app.overlay, Overlay::Mailbox(_)));
        assert!(app.pending_actions.is_empty());
    }

    #[test]
    fn clearing_search_restores_full_view() {
        let mut app = app_with_emails(sample());
        app.search_query = "holiday".to_string();
        app.apply_search_filter(false);
        assert_eq!(app.visible, vec![3]);
        app.search_query.clear();
        app.reload_from_cache();
        assert_eq!(app.visible, vec![0, 1, 2, 3]);
        assert_eq!(app.list_index, 0);
    }

    // -----------------------------------------------------------------------
    // Background error over an open overlay (#0032 regression)
    //
    // `set_persistent_error` is called from bg.rs regardless of overlay
    // state. It must NOT clobber an active overlay (that both destroys the
    // overlay's unsaved state and, for the compose wizard, leaves
    // `focus == Focus::ComposeWizard` while `overlay == None` -- a state
    // that hits `keys.rs`'s `unreachable!()` on the next keystroke). The
    // error is queued and promoted to `Overlay::Error` when the overlay
    // actually closes.
    // -----------------------------------------------------------------------

    fn open_compose_wizard_for_test(app: &mut App) {
        app.overlay = Overlay::Compose(ComposeWizard {
            mode: ComposeMode::New,
            to: "alice@example.com".to_string(),
            cc: String::new(),
            bcc: String::new(),
            subject: "draft in progress".to_string(),
            focus: ComposeField::To,
            suggestions: Vec::new(),
            suggestion_idx: 0,
            contacts: None,
        });
        app.focus = Focus::ComposeWizard;
    }

    /// A background failure while the compose wizard is open must not
    /// clobber the wizard (which would strand `focus == ComposeWizard` and
    /// panic on the next key). The error is queued, surfaced on the status
    /// line, and promoted only after the wizard closes -- and the wizard
    /// close resets focus, so the panicking `None`+`ComposeWizard` combo
    /// never occurs.
    #[test]
    fn bg_error_over_compose_wizard_does_not_clobber_or_panic() {
        let mut app = app_with_emails(sample());
        open_compose_wizard_for_test(&mut app);

        app.set_persistent_error(
            "Archive failed: boom\nEmail restored to inbox. Sync (F) to fix?".to_string(),
        );

        // The wizard is preserved (draft intact), focus stays consistent
        // with the overlay, and the error is queued + surfaced immediately.
        assert!(
            matches!(app.overlay, Overlay::Compose(_)),
            "wizard must not be clobbered by a background error"
        );
        assert_eq!(app.focus, Focus::ComposeWizard);
        assert!(app.pending_error.is_some());
        assert_eq!(app.status_message.as_deref(), Some("Archive failed: boom"));

        // Close the wizard the way `Action::ComposeWizardCancel` does.
        app.close_overlay();
        app.focus = Focus::List;

        // The queued error is now the active overlay, and focus is a valid
        // pane -- the `overlay == None && focus == ComposeWizard` state that
        // would hit `unreachable!()` never exists.
        assert!(matches!(app.overlay, Overlay::Error(_)));
        assert_eq!(app.focus, Focus::List);
        assert!(app.pending_error.is_none());

        // Dismissing the promoted error returns to the normal view and does
        // not re-open anything.
        app.handle_key(KeyEvent::from(KeyCode::Char('d')));
        assert!(matches!(app.overlay, Overlay::None));
    }

    /// A background failure while the mailbox picker is open preserves the
    /// picker (and its selection/paths) and shows the error only after the
    /// picker closes.
    #[test]
    fn bg_error_over_mailbox_picker_preserves_picker_until_close() {
        let mut app = app_with_mailboxes();
        app.handle_key(KeyEvent::from(KeyCode::Char('M')));
        assert!(matches!(app.overlay, Overlay::Mailbox(_)));

        app.set_persistent_error("Move failed: boom\nEmail restored.".to_string());

        // Picker (and its carried paths) survive; the error is queued.
        let Overlay::Mailbox(picker) = &app.overlay else {
            panic!("picker must not be clobbered by a background error");
        };
        assert_eq!(picker.paths.len(), 1);
        assert!(app.pending_error.is_some());
        assert_eq!(app.status_message.as_deref(), Some("Move failed: boom"));

        // Closing the picker promotes the queued error.
        app.handle_key(KeyEvent::from(KeyCode::Esc));
        assert!(matches!(app.overlay, Overlay::Error(_)));
        assert!(app.pending_error.is_none());
    }

    /// With no overlay open, `set_persistent_error` behaves exactly as
    /// before: it opens the error overlay directly and queues nothing.
    #[test]
    fn persistent_error_with_no_overlay_opens_directly() {
        let mut app = app_with_emails(sample());
        app.set_persistent_error("Delete failed: boom".to_string());
        assert!(matches!(app.overlay, Overlay::Error(_)));
        assert!(app.pending_error.is_none());
    }

    /// The attachment picker -> dir picker handoff (`O` save) must not let a
    /// queued error fire mid-transition: promotion is guarded on
    /// `Overlay::None`, so after the handoff the dir picker is active and
    /// the error stays queued until the dir picker itself closes.
    #[test]
    fn pending_error_does_not_fire_during_attachment_to_dir_handoff() {
        let mut app = app_with_emails(sample());
        // Simulate the attachment save picker with a queued error behind it.
        app.overlay = Overlay::Attachment(AttachmentPicker {
            files: vec![PathBuf::from("/tmp/a.pdf")],
            selected: 0,
            mode: AttachmentPickerMode::Save,
            selected_set: std::collections::HashSet::new(),
        });
        app.pending_error = Some(PersistentError { message: "bg boom".to_string() });

        // Enter triggers the attachment -> dir picker handoff.
        app.handle_key(KeyEvent::from(KeyCode::Enter));

        // The dir picker is now active; the error did NOT fire in between.
        assert!(matches!(app.overlay, Overlay::Dir(_)));
        assert!(app.pending_error.is_some());
    }
}
