use std::collections::HashSet;
use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{
    AccountPicker, Action, App, AttachmentPicker, AttachmentPickerMode, CommandPalette,
    ComposeField, ComposeMode, ComposeSuggestion, ComposeWizard, ConfirmAction, ConfirmDialog,
    DirPicker, DirPickerMode, EmailEntry, EntryKey, Focus, MailboxKind, MailboxPicker, Message,
    MessageRef, Overlay, PickerPurpose, RsvpChoice, RsvpOverlay, RsvpRow, SearchField,
    SearchOverlayFocus, SearchScope, SignaturesMode, SignaturesOverlay, StatusLevel, ThreadEntry,
    ThreadOverlay,
};

impl App {
    pub fn handle_key(&mut self, key: KeyEvent) -> Option<Message> {
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
            Overlay::Signatures(_) => return self.handle_signatures_key(key),
            Overlay::Rsvp(_) => return self.handle_rsvp_overlay_key(key),
            Overlay::Calendars(_) => return self.handle_calendars_overlay_key(key),
            Overlay::EventCard(_) => return self.handle_event_card_key(key),
            Overlay::Thread(_) => return self.handle_thread_overlay_key(key),
            Overlay::Palette(_) => return self.handle_command_palette_key(key),
            Overlay::Attachment(_) => return self.handle_attachment_picker_key(key),
            Overlay::Account(_) => return self.handle_account_picker_key(key),
            Overlay::Help => return self.handle_help_key(key),
            Overlay::Activity => return self.handle_activity_overlay_key(key),
            Overlay::Search => return self.handle_search_overlay_key(key),
            Overlay::Compose(_) => return self.handle_compose_wizard_key(key),
            Overlay::None => {}
        }

        if self.focus == Focus::Search {
            return self.handle_search_key(key);
        }

        // Jump-to-date input (#0017): armed by `g d`, it owns the keyboard
        // until Enter or Esc, the same hand-dispatched shape as the two
        // free-text inputs above.
        if self.jump_date_input.is_some() {
            return self.handle_jump_date_key(key);
        }

        // Attach-file input (#0098): armed by `t a` on a Drafts row, it owns
        // the keyboard until Enter or Esc, the same hand-dispatched shape as
        // the jump-to-date prompt above.
        if self.attach_file_input.is_some() {
            return self.handle_attach_file_key(key);
        }

        // Contacts view fuzzy-search input (#0033): free-text, hand-dispatched
        // like the metadata-search input, once armed by `/`.
        if self.view == super::View::Contacts && self.contacts_view.searching {
            return self.handle_contacts_search_key(key);
        }

        // Normal-mode surface (#0032, (B)-lite): resolve the pressed key through
        // the KEYMAP table into a `KeyAction`, then run one executor. Global
        // bindings are always tried first, then the focused pane's context
        // (mirroring the old "global keys before pane dispatch" precedence).
        // Guards are evaluated against live state so context-sensitive rules
        // (`c` only in Drafts, most list keys only when non-empty, the account
        // picker only with two accounts) stay in the data model.
        self.dispatch_normal_mode(key)
    }

    /// Table-driven dispatch for the no-overlay surface. Returns the `Message`
    /// (only `Quit` today) or `None`.
    fn dispatch_normal_mode(&mut self, key: KeyEvent) -> Option<Message> {
        // Undo-send hold window (#0090, daemon-owned since P6-U2): while a
        // send is counting down, `u` asks the daemon to cancel it before it
        // reaches SMTP -- the draft is untouched and recoverable.
        // Hand-dispatched as a transient prompt (the status line advertises
        // it), not a KEYMAP row, so it neither collides with the
        // Message-context `u` (toggle read) once the window has fired nor
        // needs a website key-table entry.
        //
        // The countdown is deliberately left on screen: cancelling is a round
        // trip, and a client that cleared it here would hide a hold whose
        // cancel was refused. `send.hold_cancelled` takes it down.
        if self.hold.is_some()
            && key.code == KeyCode::Char('u')
            && !key.modifiers.contains(KeyModifiers::CONTROL)
        {
            self.push_action_dedup(Action::CancelHeldSend);
            return None;
        }

        let pending = self.pending_prefix;
        let guard_ok = |g: super::Guard| self.guard_satisfied(g);

        // The focused pane's context. Mail consults its focused pane; the
        // Contacts view (#0033) and the Calendar view (#0034) each have a
        // single list context.
        let pane_ctx = match self.view {
            super::View::Mail => self.pane_ctx(),
            super::View::Contacts => Some(super::KeyCtx::Contacts),
            super::View::Calendar => Some(super::KeyCtx::Calendar),
        };

        // Global context first.
        if let Some(action) = super::resolve(super::KeyCtx::Global, key, pending, &guard_ok) {
            // In non-Mail views only the view-agnostic Global surface (view
            // switch / quit / help / activity) is live; a mail-specific Global
            // key resolves but is swallowed so it cannot fire (#0033) -- UNLESS
            // the active view's pane context rebinds that key (e.g. Contacts
            // rebinds `/` to fuzzy search), in which case the pane binding wins.
            // A family leader stays live iff its family carries a view-agnostic
            // continuation (today `s`: config/log/activity, #0092 review fix),
            // so those utilities remain reachable from Contacts and Calendar.
            let agnostic_leader = matches!(action, super::KeyAction::ArmPrefix)
                && matches!(key.code, KeyCode::Char(l) if super::leader_is_view_agnostic(l));
            if self.view != super::View::Mail && !action.is_view_agnostic() && !agnostic_leader {
                let pane_rebinds = pane_ctx
                    .and_then(|ctx| super::resolve(ctx, key, pending, &guard_ok))
                    .is_some();
                if !pane_rebinds {
                    self.pending_prefix = None;
                    return None;
                }
            } else {
                return self.execute(action, key);
            }
        }
        // Then the pane context.
        if let Some(ctx) = pane_ctx {
            if let Some(action) = super::resolve(ctx, key, pending, &guard_ok) {
                return self.execute(action, key);
            }
        }
        // Then the shared MESSAGE context (#0092): a message action resolves
        // identically in the List, Headers and Body panes, so acting on what
        // you are reading never needs a focus hop. Tried after the focused
        // pane so a pane keeps its own scroll keys (`j`/`k`).
        if let Some(action) = self
            .message_ctx()
            .and_then(|ctx| super::resolve(ctx, key, pending, &guard_ok))
        {
            return self.execute(action, key);
        }
        // No live binding matched: clear any pending leader (an unrecognised
        // key aborts the chord). Under strict prefix mode a key that is not a
        // continuation of the pending family cancels the chord here without
        // firing a flat binding (which-key semantics).
        self.pending_prefix = None;
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

    /// Rows one list page spans: what the list pane showed on the last paint,
    /// or a 20-row stand-in before the first one (the body pane's half-page is
    /// a fixed 10, so the two agree until a real height is known).
    fn list_page_rows(&self) -> usize {
        match self.list_viewport_rows {
            0 => 20,
            rows => usize::from(rows),
        }
    }

    /// Advance the list cursor by `step`, clamped to the last visible row.
    fn move_list_cursor_down(&mut self, step: usize) {
        let last = self.visible.len().saturating_sub(1);
        self.list_index = self.list_index.saturating_add(step).min(last);
    }

    /// The shared MESSAGE context, live whenever a reading pane (List, Headers,
    /// Body) holds focus in the Mail view (#0092). `None` off Mail or in the
    /// sidebar / input panes, so a message action never fires where there is
    /// no cursor message to act on.
    fn message_ctx(&self) -> Option<super::KeyCtx> {
        if self.view != super::View::Mail {
            return None;
        }
        match self.focus {
            Focus::List | Focus::Headers | Focus::Preview => Some(super::KeyCtx::Message),
            Focus::Sidebar | Focus::Search | Focus::ComposeWizard => None,
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
                self.pending_prefix = None;
                self.help_scroll = 0;
                self.help_filter.clear();
                self.help_filter_active = false;
                self.overlay = Overlay::Help;
            }
            A::OpenPalette => {
                // `:` / `Ctrl+p` (#0100): open the command palette, a fuzzy
                // finder over the runnable KeyAction catalogue.
                self.pending_prefix = None;
                self.overlay = Overlay::Palette(CommandPalette::new());
            }
            A::ToggleZoom => {
                self.pending_prefix = None;
                self.toggle_zoom();
            }
            A::ToggleActivityLog => {
                self.pending_prefix = None;
                self.show_activity_log = !self.show_activity_log;
            }
            A::OpenSignatures => {
                self.pending_prefix = None;
                self.open_signatures_overlay();
            }
            A::OpenActivityOverlay => {
                self.pending_prefix = None;
                self.activity_filter.clear();
                self.activity_filter_active = false;
                self.activity_scroll = 0;
                self.overlay = Overlay::Activity;
            }
            A::OpenLogFile => {
                self.pending_prefix = None;
                self.push_action(Action::OpenLogFile);
            }
            A::OpenConfigFile => {
                self.pending_prefix = None;
                self.push_action(Action::OpenConfigFile);
            }
            A::FilterMetadata => {
                self.pending_prefix = None;
                self.focus = Focus::Search;
                self.search_query.clear();
                self.reload_from_cache();
            }
            A::SwitchAccount => {
                // `ga` (#0147): the account picker, on the active account.
                // The key's guard needs two accounts; the palette runs this
                // arm without it, so it checks again.
                self.pending_prefix = None;
                if self.accounts.len() > 1 {
                    self.overlay = Overlay::Account(AccountPicker {
                        selected: self.active_account,
                    });
                }
            }
            A::JumpMailbox => {
                self.pending_prefix = None;
                if let KeyCode::Char(c @ '1'..='9') = key.code {
                    let idx = (c as usize) - ('1' as usize);
                    if idx < self.mailboxes.len() {
                        self.sidebar_index = idx;
                        self.switch_mailbox(idx);
                        self.focus = Focus::List;
                    }
                }
            }
            A::ArmPrefix => {
                // A mnemonic family leader (`f`/`c`/`g`/`t`/`s`) was pressed
                // with nothing pending: arm it so the next key is resolved as
                // a continuation and the which-key popup shows the family.
                if let KeyCode::Char(leader) = key.code {
                    self.pending_prefix = Some(leader);
                }
            }
            A::GoMailbox => {
                // `gm`: go to the mailbox sidebar (navigate with j/k, Enter).
                self.pending_prefix = None;
                self.focus = Focus::Sidebar;
            }
            A::GoList => {
                // `gl`: go to the message list. The sidebar's highlight is not
                // opened, as the desktop's `gl` does not open its cursor's
                // mailbox: Enter in the sidebar does that.
                self.pending_prefix = None;
                self.focus = Focus::List;
            }
            A::GoReader => {
                // `gr`: go to the body pane, an explicit "I am reading this"
                // that marks the message read, as Tab into it does (#0110).
                // Under 80 columns it zooms, as `Enter` does, because the
                // split draws no body pane there.
                self.pending_prefix = None;
                if self.focus != Focus::Preview {
                    self.focus = Focus::Preview;
                    if self.terminal_width < 80 {
                        self.zoomed = true;
                    }
                    self.queue_mark_open_read();
                }
            }
            A::FocusForward => {
                self.pending_prefix = None;
                if self.focus == Focus::Sidebar {
                    self.switch_mailbox(self.sidebar_index);
                }
                // Reading order (#0092): Sidebar, List, Headers, Body.
                self.focus = match self.focus {
                    Focus::Sidebar => Focus::List,
                    Focus::List => Focus::Headers,
                    Focus::Headers => Focus::Preview,
                    Focus::Preview => Focus::Sidebar,
                    Focus::Search => Focus::List,
                    Focus::ComposeWizard => Focus::ComposeWizard,
                };
                // Landing in the body pane is an explicit "I am reading this"
                // and marks the message read (#0110).
                if self.focus == Focus::Preview {
                    self.queue_mark_open_read();
                }
            }
            A::FocusBackward => {
                self.pending_prefix = None;
                self.focus = match self.focus {
                    Focus::Sidebar => Focus::Preview,
                    Focus::Preview => Focus::Headers,
                    Focus::Headers => Focus::List,
                    Focus::List => Focus::Sidebar,
                    Focus::Search => Focus::List,
                    Focus::ComposeWizard => Focus::ComposeWizard,
                };
                if self.focus == Focus::Preview {
                    self.queue_mark_open_read();
                }
            }
            A::SwitchView => {
                // `Space m/c/a`: the continuation key selects the target view.
                // `switch_view` clears the pending leader itself.
                if let KeyCode::Char(c) = key.code {
                    if let Some(&target) = super::View::ALL.iter().find(|v| v.switch_key() == c) {
                        self.switch_view(target);
                    }
                }
                self.pending_prefix = None;
            }
            // -- Sidebar ------------------------------------------------------
            A::SidebarDown => {
                self.pending_prefix = None;
                if self.sidebar_index < self.mailboxes.len().saturating_sub(1) {
                    self.sidebar_index += 1;
                }
            }
            A::SidebarUp => {
                self.pending_prefix = None;
                self.sidebar_index = self.sidebar_index.saturating_sub(1);
            }
            A::SidebarSelect => {
                self.pending_prefix = None;
                self.switch_mailbox(self.sidebar_index);
                self.focus = Focus::List;
            }
            // -- Headers ------------------------------------------------------
            A::HeadersDown => {
                self.pending_prefix = None;
                self.headers_scroll = self.headers_scroll.saturating_add(1);
            }
            A::HeadersUp => {
                self.pending_prefix = None;
                self.headers_scroll = self.headers_scroll.saturating_sub(1);
            }
            // -- Preview / body ----------------------------------------------
            A::PreviewDown => {
                self.pending_prefix = None;
                self.preview_scroll = self.preview_scroll.saturating_add(1);
            }
            A::PreviewUp => {
                self.pending_prefix = None;
                self.preview_scroll = self.preview_scroll.saturating_sub(1);
            }
            A::PreviewHalfDown => {
                self.pending_prefix = None;
                self.preview_scroll = self.preview_scroll.saturating_add(10);
            }
            A::PreviewHalfUp => {
                self.pending_prefix = None;
                self.preview_scroll = self.preview_scroll.saturating_sub(10);
            }
            A::PreviewToList => {
                self.pending_prefix = None;
                self.focus = Focus::List;
            }
            // -- Contacts view (#0033) ---------------------------------------
            A::ContactsDown => {
                self.pending_prefix = None;
                let len = self.contacts_view.matches.len();
                if len > 0 && self.contacts_view.list_index < len - 1 {
                    self.contacts_view.list_index += 1;
                }
            }
            A::ContactsUp => {
                self.pending_prefix = None;
                self.contacts_view.list_index = self.contacts_view.list_index.saturating_sub(1);
            }
            A::ContactsTop => {
                // Reached only with `g` pending (the leader continuation).
                self.contacts_view.list_index = 0;
                self.pending_prefix = None;
            }
            A::ContactsBottom => {
                self.pending_prefix = None;
                self.contacts_view.list_index = self.contacts_view.matches.len().saturating_sub(1);
            }
            A::ContactsSearch => {
                self.pending_prefix = None;
                self.contacts_view.searching = true;
            }
            A::ContactsCompose => {
                self.pending_prefix = None;
                if let Some(contact) = self.selected_contact() {
                    let to = mp_core::addresses::format_recipient(
                        &contact.display_name,
                        &contact.address,
                    );
                    self.push_action(Action::ComposeToContact { to });
                }
            }
            A::ContactsVcard => {
                self.pending_prefix = None;
                if let Some(contact) = self.selected_contact() {
                    self.push_action(Action::SendContactVcard {
                        contact: contact.clone(),
                    });
                }
            }
            A::ContactsCopyEmail => {
                self.pending_prefix = None;
                match self.selected_contact() {
                    Some(contact) => {
                        let address = contact.address.clone();
                        self.push_action(Action::CopyContactEmail { address });
                    }
                    None => self.set_status("No contact selected".to_string()),
                }
            }
            A::ContactsRefresh => {
                self.pending_prefix = None;
                self.push_action(Action::RefreshContacts);
            }
            // -- Calendar view (#0034, PERSO-200) ---------------------------
            A::CalendarDown => {
                self.pending_prefix = None;
                let len = self.calendar_view.entries.len();
                if len > 0 && self.calendar_view.list_index < len - 1 {
                    self.calendar_view.list_index += 1;
                }
            }
            A::CalendarUp => {
                self.pending_prefix = None;
                self.calendar_view.list_index = self.calendar_view.list_index.saturating_sub(1);
            }
            A::CalendarTop => {
                // Reached only with `g` pending (the leader continuation).
                self.calendar_view.list_index = 0;
                self.pending_prefix = None;
            }
            A::CalendarBottom => {
                self.pending_prefix = None;
                self.calendar_view.list_index = self.calendar_view.entries.len().saturating_sub(1);
            }
            A::CalendarPrev | A::CalendarNext => {
                self.pending_prefix = None;
                self.calendar_shift(action == A::CalendarNext);
            }
            A::CalendarToday => {
                self.pending_prefix = None;
                let today = chrono::Utc::now()
                    .with_timezone(&self.calendar_view.zone)
                    .date_naive();
                self.calendar_go_to(today);
            }
            A::CalendarJumpToDate => {
                // Reached only with `g` pending (the leader continuation).
                self.pending_prefix = None;
                self.jump_date_input = Some(String::new());
            }
            A::CalendarAgenda | A::CalendarDay | A::CalendarWeek => {
                self.pending_prefix = None;
                self.calendar_set_layout(match action {
                    A::CalendarDay => super::CalendarLayout::Day,
                    A::CalendarWeek => super::CalendarLayout::Week,
                    _ => super::CalendarLayout::Agenda,
                });
            }
            A::CalendarOpenCard => {
                self.pending_prefix = None;
                self.open_event_card();
            }
            A::CalendarOpenSource => {
                self.pending_prefix = None;
                if let Some(event) = self.selected_event().cloned() {
                    self.open_event_ics(&event);
                }
            }
            A::CalendarRsvp => {
                self.pending_prefix = None;
                self.open_rsvp_overlay_for_event();
            }
            A::CalendarCalendars => {
                self.pending_prefix = None;
                self.open_calendars_overlay();
            }
            A::CalendarRefresh => {
                self.pending_prefix = None;
                self.refresh_calendar();
            }
            // -- List / shared -----------------------------------------------
            _ => return self.execute_list(action, key),
        }

        None
    }

    /// List-context actions (and the shared attachment/browser/RSVP actions the
    /// list, headers, and preview panes all use). Split out to keep `execute`
    /// readable; it also owns the list-cursor scroll-reset bookkeeping.
    fn execute_list(&mut self, action: super::KeyAction, key: KeyEvent) -> Option<Message> {
        use super::KeyAction as A;
        let old_index = self.list_index;

        match action {
            A::ListDown => {
                self.pending_prefix = None;
                // Saturating like `NextMessage` below: key dispatch never gets
                // here on an empty list (`Guard::NonEmptyList`), but the
                // command palette (#0100) runs executors directly, and
                // `0 - 1` on an empty `visible` underflows.
                if self.list_index < self.visible.len().saturating_sub(1) {
                    self.list_index += 1;
                }
            }
            A::ListUp => {
                self.pending_prefix = None;
                self.list_index = self.list_index.saturating_sub(1);
            }
            // Next / previous message from any reading pane (`J`/`K`, `gj`/`gk`,
            // #0092): move the list cursor without changing focus, so the
            // preview follows while the body pane keeps the keyboard.
            A::NextMessage => {
                self.pending_prefix = None;
                if self.list_index < self.visible.len().saturating_sub(1) {
                    self.list_index += 1;
                }
            }
            A::PrevMessage => {
                self.pending_prefix = None;
                self.list_index = self.list_index.saturating_sub(1);
            }
            // Esc in any reading pane: clear a live selection first, else
            // return focus to the list (#0092, symmetric across the panes).
            A::EscMessage => {
                self.pending_prefix = None;
                if !self.selection.is_empty() {
                    self.selection.clear();
                } else if self.focus != Focus::List {
                    self.focus = Focus::List;
                }
            }
            // `Enter` in any reading pane reads the message: the keyboard moves
            // into the body pane, which is the explicit open that marks it
            // read, the same as a `Tab` landing there (#0110). `$EDITOR` is
            // `e` (PERSO-92). Already in the body pane, there is nothing to do.
            // Under 80 columns the split draws no body pane (`ui::render`), so
            // the move also zooms: the zoom follows focus, and the body then
            // fills the content area instead of taking the keyboard unseen.
            A::FocusBody => {
                self.pending_prefix = None;
                // `Enter` on a collapsed conversation with more than itself
                // to show expands it and keeps the cursor on it; on an
                // expanded header or a member it reads, as on any row
                // (#0142).
                if self.focus == Focus::List && self.expand_conversation() {
                    return None;
                }
                if self.focus != Focus::Preview {
                    self.focus = Focus::Preview;
                    if self.terminal_width < 80 {
                        self.zoomed = true;
                    }
                    self.queue_mark_open_read();
                }
            }
            A::ListTop => {
                // `gg` (the leader continuation) or `Home`.
                self.list_index = 0;
                self.pending_prefix = None;
            }
            A::ListBottom => {
                self.pending_prefix = None;
                self.list_index = self.visible.len().saturating_sub(1);
            }
            A::ListHalfDown => {
                self.pending_prefix = None;
                self.move_list_cursor_down((self.list_page_rows() / 2).max(1));
            }
            A::ListHalfUp => {
                self.pending_prefix = None;
                let step = (self.list_page_rows() / 2).max(1);
                self.list_index = self.list_index.saturating_sub(step);
            }
            A::ListPageDown => {
                self.pending_prefix = None;
                self.move_list_cursor_down(self.list_page_rows());
            }
            A::ListPageUp => {
                self.pending_prefix = None;
                self.list_index = self.list_index.saturating_sub(self.list_page_rows());
            }
            A::JumpToDate => {
                // Reached only with `g` pending (the leader continuation).
                self.pending_prefix = None;
                self.jump_date_input = Some(String::new());
            }
            A::AttachFile => {
                // Catalogued Drafts-only, but resolved everywhere so the miss
                // shows a hint (the same advisory-guard shape as `ce` edit
                // recipients). Only a Drafts row has an `attachments:` list to
                // grow, so arming the prompt outside Drafts would be a lie.
                self.pending_prefix = None;
                if self.cursor_is_draft_row() {
                    if self
                        .selected_email()
                        .and_then(|e| e.draft_id.as_ref())
                        .is_some()
                    {
                        self.attach_file_input = Some(String::new());
                    } else {
                        self.set_status("Attach needs a draft; this row has none".to_string());
                    }
                } else {
                    self.set_status("Attach file (t a) is only available in Drafts".to_string());
                }
            }
            A::OpenEditor => {
                self.pending_prefix = None;
                self.push_action(Action::EditCurrent);
            }
            A::Reply => {
                self.pending_prefix = None;
                self.push_action(Action::Reply(false));
            }
            A::ReplyAll => {
                self.pending_prefix = None;
                self.push_action(Action::Reply(true));
            }
            A::Forward | A::ForwardAsAttachment => {
                self.pending_prefix = None;
                let as_attachment = matches!(action, A::ForwardAsAttachment);
                if let Some(msg) = self.selected_email_ref() {
                    self.push_action(Action::OpenComposeWizard(ComposeMode::Forward {
                        msg,
                        as_attachment,
                    }));
                } else if self.selected_email().is_some() {
                    // A Drafts row has no message behind it to forward.
                    self.set_status(
                        "Forward needs a received message; a draft has none to quote".to_string(),
                    );
                }
            }
            A::EditRecipients => {
                // Only meaningful in Drafts; outside Drafts keep the old status
                // hint (the guard is advisory, resolved everywhere).
                self.pending_prefix = None;
                if self.cursor_is_draft_row() {
                    // The draft is named by its `id:`, not by the path it
                    // happens to sit at: the wizard resolves it through the
                    // drafts index when it opens and again when it submits.
                    if let Some(id) = self.selected_email().and_then(|e| e.draft_id.clone()) {
                        self.push_action(Action::OpenComposeWizard(ComposeMode::EditDraft { id }));
                    }
                } else {
                    self.set_status("Edit recipients (c) is only available in Drafts".to_string());
                }
            }
            A::SelectAllVisible => {
                self.pending_prefix = None;
                // A conversation header selects its members here and an
                // expanded member is already among them or elsewhere, so the
                // selection is the union of the headers' `member_ids`
                // (#0142).
                self.selection = self
                    .visible_emails()
                    .filter(|e| e.member_mailbox().is_none())
                    .flat_map(selection_keys)
                    .collect();
            }
            A::ExpandConversation => {
                self.pending_prefix = None;
                self.expand_conversation();
            }
            A::CollapseConversation => {
                self.pending_prefix = None;
                self.collapse_conversation();
            }
            A::ToggleConversationMode => {
                self.pending_prefix = None;
                self.toggle_conversation_mode();
            }
            A::Archive => {
                self.pending_prefix = None;
                if !self.selection.is_empty() {
                    let count = self.selection.len();
                    self.overlay = Overlay::Confirm(ConfirmDialog {
                        title: format!("Archive {} emails?", count),
                        detail: format!("{} selected emails", count),
                        action: ConfirmAction::Archive,
                    });
                } else if let Some(email) = self.selected_email() {
                    self.overlay = Overlay::Confirm(ConfirmDialog {
                        title: confirm_title("Archive", email),
                        detail: format!("{} - {}", email.from, email.subject),
                        action: ConfirmAction::Archive,
                    });
                }
            }
            A::Delete => {
                self.pending_prefix = None;
                if !self.selection.is_empty() {
                    let count = self.selection.len();
                    self.overlay = Overlay::Confirm(ConfirmDialog {
                        title: format!("Delete {} emails?", count),
                        detail: format!("{} selected emails", count),
                        action: ConfirmAction::Delete,
                    });
                } else if let Some(email) = self.selected_email() {
                    self.overlay = Overlay::Confirm(ConfirmDialog {
                        title: confirm_title("Delete", email),
                        detail: format!("{} - {}", email.from, email.subject),
                        action: ConfirmAction::Delete,
                    });
                }
            }
            A::Approve => {
                self.pending_prefix = None;
                if !self.selection.is_empty() {
                    self.confirm_draft_batch(
                        "Approve",
                        |count| format!("Approve {count} drafts?"),
                        ConfirmAction::Approve,
                    );
                } else {
                    self.push_action(Action::Approve);
                }
            }
            A::MarkDraft => {
                self.pending_prefix = None;
                if !self.selection.is_empty() {
                    self.confirm_draft_batch(
                        "Mark as draft",
                        |count| format!("Mark {count} drafts as draft?"),
                        ConfirmAction::MarkDraft,
                    );
                } else {
                    self.push_action(Action::MarkDraft);
                }
            }
            A::Send => {
                self.pending_prefix = None;
                if let Some(email) = self.selected_email() {
                    // One dialog either way (#0089): an unapproved draft warns
                    // that confirming approves and sends in the same step; an
                    // approved draft keeps the plain send confirm.
                    let title = if email.status == "draft" {
                        "Draft is not approved. Approve and send?"
                    } else {
                        "Send this email?"
                    };
                    self.overlay = Overlay::Confirm(ConfirmDialog {
                        title: title.to_string(),
                        detail: format!("To: {} - {}", email.to, email.subject),
                        action: ConfirmAction::Send,
                    });
                }
            }
            A::SendAll => {
                self.pending_prefix = None;
                self.overlay = Overlay::Confirm(ConfirmDialog {
                    title: "Send all approved emails?".to_string(),
                    detail: format!("In {}", self.active_label()),
                    action: ConfirmAction::SendApproved,
                });
            }
            A::CopyMessageRef => {
                self.pending_prefix = None;
                self.push_action(Action::CopyMessageRef);
            }
            A::ToggleRead => {
                self.pending_prefix = None;
                if !self.selection.is_empty() {
                    let msgs: Vec<MessageRef> =
                        self.selection.iter().filter_map(|k| k.msg()).collect();
                    self.push_action(Action::BatchToggleRead(msgs));
                } else {
                    self.push_action(Action::ToggleRead);
                }
            }
            A::ToggleFlag => {
                self.pending_prefix = None;
                if !self.selection.is_empty() {
                    let msgs: Vec<MessageRef> =
                        self.selection.iter().filter_map(|k| k.msg()).collect();
                    self.push_action(Action::BatchToggleFlag(msgs));
                } else {
                    self.push_action(Action::ToggleFlag);
                }
            }
            A::ToggleFlaggedFilter => {
                self.pending_prefix = None;
                self.flagged_only = !self.flagged_only;
                let anchor = self.cursor_anchor();
                let fallback = self.list_index;
                self.selection.clear();
                self.rebuild_visible();
                self.restore_cursor(anchor, fallback);
                self.headers_scroll = 0;
                self.preview_scroll = 0;
                let shown = self.visible.len();
                if self.flagged_only {
                    self.set_status(format!("Flagged only ({shown})"));
                } else {
                    self.set_status("Showing all messages".to_string());
                }
            }
            A::MovePicker => {
                self.pending_prefix = None;
                self.open_mailbox_picker();
            }
            A::FolderPicker => {
                self.pending_prefix = None;
                self.open_folder_picker();
            }
            A::Rsvp => {
                self.pending_prefix = None;
                self.open_rsvp_overlay();
            }
            A::OpenThread => {
                self.pending_prefix = None;
                self.open_thread_overlay();
            }
            A::NewDraft => {
                self.pending_prefix = None;
                self.push_action(Action::OpenComposeWizard(ComposeMode::New));
            }
            A::QuickSync => {
                self.pending_prefix = None;
                self.push_action(Action::Fetch);
            }
            A::FullSync => {
                self.pending_prefix = None;
                self.push_action(Action::Sync);
            }
            A::ServerSearch => {
                self.pending_prefix = None;
                self.search_form = crate::app::SearchForm::default();
                self.server_search_results.clear();
                self.server_search_index = 0;
                self.server_search_scroll = 0;
                self.server_search_headers_scroll = 0;
                self.server_search_focus = SearchOverlayFocus::Field(SearchField::From);
                self.server_search_loading = false;
                self.server_search_status = None;
                self.overlay = Overlay::Search;
            }
            A::OpenAttachment => {
                self.pending_prefix = None;
                self.open_attachment_picker(AttachmentPickerMode::Open);
            }
            A::SaveAttachment => {
                self.pending_prefix = None;
                self.open_attachment_picker(AttachmentPickerMode::Save);
            }
            A::ExportMessage => {
                self.pending_prefix = None;
                // The message's `.eml` goes where `ts` puts a part: the
                // directory picker, then the same copy (PERSO-174).
                if let Some(path) = crate::actions::cursor_eml_file(self) {
                    self.open_dir_picker(vec![path]);
                }
            }
            A::OpenInBrowser => {
                self.pending_prefix = None;
                // The markup is a blob (or the html part of the raw message),
                // not a `.html` beside a `.md`, so it is written to a temp
                // file on demand and that is what the browser opens (#0052
                // scope item 9). A Drafts row has no markup of its own, so it
                // opens the HTML part the send would build (PERSO-136).
                if let Some(id) = self.selected_email().and_then(|e| e.draft_id.clone()) {
                    if let Some(path) = crate::actions::html_rendition_for_draft(self, &id) {
                        self.push_action(Action::OpenHtmlInBrowser(path));
                    }
                } else if let Some(msg) = self.selected_email_ref() {
                    if let Some(path) = crate::actions::html_rendition_for_row(self, msg.row_id()) {
                        self.push_action(Action::OpenHtmlInBrowser(path));
                    }
                }
            }
            A::ToggleSelect => {
                self.pending_prefix = None;
                // Drafts are selectable too (#0052): the set is keyed on
                // `EntryKey`, so the Drafts mailbox has a batch again. A
                // conversation header toggles its members here as one
                // (#0142).
                let keys = self
                    .selected_email()
                    .map(selection_keys)
                    .unwrap_or_default();
                if !keys.is_empty() {
                    if keys.iter().all(|key| self.selection.contains(key)) {
                        for key in &keys {
                            self.selection.remove(key);
                        }
                    } else {
                        self.selection.extend(keys);
                    }
                    if self.list_index < self.visible.len() - 1 {
                        self.list_index += 1;
                    }
                }
            }
            A::ClearSelection => {
                self.pending_prefix = None;
                if !self.selection.is_empty() {
                    self.selection.clear();
                }
            }
            // The leader key itself: begin/toggle the pending leader chord.
            // This is the only action that intentionally leaves a leader armed.
            // Two leaders exist (#0033 follow-up): Space (view switcher) and
            // `g` (list `gg`/`G`); the pressed key identifies which one.
            A::Manual => {
                if let KeyCode::Char(leader) = key.code {
                    self.pending_prefix = if self.pending_prefix == Some(leader) {
                        None
                    } else {
                        Some(leader)
                    };
                }
            }
            // Any pane-only action mistakenly routed here is a no-op.
            _ => {
                self.pending_prefix = None;
            }
        }

        if self.list_index != old_index {
            self.headers_scroll = 0;
            self.preview_scroll = 0;
        }

        None
    }

    /// Open the confirmation for a batch that writes to draft files, or say
    /// why there is nothing to confirm.
    ///
    /// The batch takes the drafts half of the selection (see
    /// [`Self::handle_confirm_key`]), so a selection holding no draft key --
    /// `A` over a received-mail selection, which the keymap allows -- has
    /// nothing to approve. Without this guard the dialog opened on the full
    /// count and the batch then reported "Approved 0 drafts", asking a
    /// question whose only honest answer was already known.
    fn confirm_draft_batch(
        &mut self,
        what: &str,
        title: impl Fn(usize) -> String,
        action: ConfirmAction,
    ) {
        let count = self
            .selection
            .iter()
            .filter(|key| key.draft().is_some())
            .count();
        if count == 0 {
            self.set_status(format!(
                "{what} needs drafts; the selection has no draft in it"
            ));
            return;
        }
        self.overlay = Overlay::Confirm(ConfirmDialog {
            title: title(count),
            detail: format!("{count} selected drafts"),
            action,
        });
    }

    fn handle_confirm_key(&mut self, key: KeyEvent) -> Option<Message> {
        match key.code {
            KeyCode::Char('y') | KeyCode::Enter => {
                if let Overlay::Confirm(dialog) =
                    std::mem::replace(&mut self.overlay, Overlay::None)
                {
                    match dialog.action {
                        // Approve and mark-draft write to draft files, so they
                        // take the drafts half of the selection; archive and
                        // delete are store mutations and take the messages
                        // half (#0052). A selection never holds both -- one
                        // mailbox lists one kind of row and switching clears
                        // the set -- but each side filters rather than assumes.
                        ConfirmAction::Approve if !self.selection.is_empty() => {
                            let ids: Vec<String> = self
                                .selection
                                .drain()
                                .filter_map(|k| k.draft().map(str::to_string))
                                .collect();
                            self.push_action(Action::BatchApprove(ids));
                        }
                        ConfirmAction::MarkDraft if !self.selection.is_empty() => {
                            let ids: Vec<String> = self
                                .selection
                                .drain()
                                .filter_map(|k| k.draft().map(str::to_string))
                                .collect();
                            self.push_action(Action::BatchMarkDraft(ids));
                        }
                        ConfirmAction::Archive if !self.selection.is_empty() => {
                            let msgs: Vec<MessageRef> =
                                self.selection.drain().filter_map(|k| k.msg()).collect();
                            self.push_action(Action::BatchArchive(msgs));
                        }
                        ConfirmAction::Delete if !self.selection.is_empty() => {
                            // Delete takes the messages half, as archive does:
                            // a received message is a store mutation. A Drafts
                            // selection holds draft ids and no `messages` row,
                            // so once the messages half is empty the delete is
                            // the local file removal instead (#0073), not
                            // `prepare_delete` reporting nothing to delete. A
                            // selection never mixes the two in practice; each
                            // half still filters rather than assumes.
                            let msgs: Vec<MessageRef> =
                                self.selection.iter().filter_map(|k| k.msg()).collect();
                            if msgs.is_empty() {
                                let drafts: Vec<String> = self
                                    .selection
                                    .drain()
                                    .filter_map(|k| k.draft().map(str::to_string))
                                    .collect();
                                self.push_action(Action::BatchDeleteDrafts(drafts));
                            } else {
                                self.selection.clear();
                                self.push_action(Action::BatchDelete(msgs));
                            }
                        }
                        // The one confirmed action that is not a queued
                        // `Action`: deleting a signature is one
                        // `signature.delete`, whose answer is the listing the
                        // overlay it came from is restored with (so a default
                        // the delete cleared stops being marked).
                        ConfirmAction::DeleteSignature(mut overlay) => {
                            let name = overlay.selected_name().map(str::to_string);
                            if let Some(name) = name {
                                let deleted = self
                                    .signature_call(
                                        "signature.delete",
                                        serde_json::json!({
                                            "account": overlay.account,
                                            "name": name,
                                        }),
                                    )
                                    .and_then(|answer| {
                                        serde_json::from_value(answer)
                                            .map_err(|e| format!("reading signature.delete: {e}"))
                                    });
                                match deleted {
                                    Ok(listing) => {
                                        overlay.apply(listing);
                                        self.set_status(format!("Deleted signature '{name}'"))
                                    }
                                    Err(e) => self.set_status_level(
                                        format!("Cannot delete: {e}"),
                                        StatusLevel::Error,
                                    ),
                                }
                            }
                            self.overlay = Overlay::Signatures(overlay);
                        }
                        _ => {
                            self.push_action(match dialog.action {
                                ConfirmAction::Approve => Action::Approve,
                                ConfirmAction::MarkDraft => Action::MarkDraft,
                                ConfirmAction::Archive => Action::Archive,
                                ConfirmAction::Delete => Action::Delete,
                                ConfirmAction::Send => Action::Send,
                                ConfirmAction::SendApproved => Action::SendApproved,
                                // Consumed by the arm above; the resolver never
                                // reaches this one.
                                ConfirmAction::DeleteSignature(_) => return None,
                            });
                        }
                    }
                    // Consume-and-close: the confirm dialog was taken above.
                    // Promote any error that had been queued behind it.
                    self.promote_pending_error();
                }
            }
            KeyCode::Char('n') | KeyCode::Esc => {
                // A cancelled signature delete goes back to the signatures
                // overlay it floated over, not to the mail view (#0107).
                let taken = std::mem::replace(&mut self.overlay, Overlay::None);
                if let Overlay::Confirm(ConfirmDialog {
                    action: ConfirmAction::DeleteSignature(overlay),
                    ..
                }) = taken
                {
                    self.overlay = Overlay::Signatures(overlay);
                } else {
                    self.promote_pending_error();
                }
            }
            _ => {}
        }
        None
    }

    fn handle_search_overlay_key(&mut self, key: KeyEvent) -> Option<Message> {
        match self.server_search_focus {
            SearchOverlayFocus::Field(field) => self.handle_search_form_key(key, field),
            SearchOverlayFocus::List => self.handle_search_overlay_list_key(key),
        }
    }

    /// The Outlook-shape search form (#0086b). Tab/Shift-Tab cycle the fields
    /// (reusing the compose-wizard convention), Space flips the scope /
    /// attachment toggles, typing edits the focused text field, Enter searches
    /// and Esc closes. The form builds a `search::Query` AST and routes it
    /// through the same lowering path the CLI uses.
    fn handle_search_form_key(&mut self, key: KeyEvent, field: SearchField) -> Option<Message> {
        // A non-blank Advanced line disables the structured fields: skip over
        // them when cycling so Tab lands only on Scope / Attachment / Advanced,
        // and refuse edits to a greyed field.
        let advanced_active = self.search_form.advanced_active();
        let field_enabled = |f: SearchField| !advanced_active || matches!(f, SearchField::Advanced);

        match key.code {
            KeyCode::Esc => {
                self.close_overlay();
            }
            KeyCode::Tab => {
                let mut next = field.next();
                while !field_enabled(next) {
                    next = next.next();
                }
                self.server_search_focus = SearchOverlayFocus::Field(next);
            }
            KeyCode::BackTab => {
                let mut prev = field.prev();
                while !field_enabled(prev) {
                    prev = prev.prev();
                }
                self.server_search_focus = SearchOverlayFocus::Field(prev);
            }
            KeyCode::Down => {
                // From the Advanced line (last field) Down drops into the
                // results list if there are hits, matching the old overlay.
                if field == SearchField::Advanced && !self.server_search_results.is_empty() {
                    self.server_search_focus = SearchOverlayFocus::List;
                }
            }
            KeyCode::Char(' ') if field == SearchField::Scope => {
                self.search_form.scope.0 = self.search_form.scope.0.toggled();
            }
            KeyCode::Char(' ') if field == SearchField::Attachment && !advanced_active => {
                self.search_form.attachment = !self.search_form.attachment;
            }
            KeyCode::Enter => {
                self.submit_search_form();
            }
            KeyCode::Backspace => {
                if field_enabled(field) {
                    if let Some(text) = self.search_form.text_field_mut(field) {
                        text.pop();
                        self.server_search_status = None;
                    }
                }
            }
            KeyCode::Char(c) => {
                // Ctrl-prefixed chars are not text.
                if key.modifiers.contains(KeyModifiers::CONTROL) {
                    return None;
                }
                if field_enabled(field) {
                    if let Some(text) = self.search_form.text_field_mut(field) {
                        text.push(c);
                        self.server_search_status = None;
                    }
                }
            }
            _ => {}
        }
        None
    }

    /// Build the AST from the form and dispatch, or surface the parse error
    /// verbatim. A blank form is a no-op.
    fn submit_search_form(&mut self) {
        let query = match self.search_form.build_query() {
            Ok(Some(q)) => q,
            Ok(None) => return, // blank form: nothing to search for
            Err(e) => {
                self.server_search_status = Some(e);
                return;
            }
        };

        // `in:` inside the Advanced line still resolves a mailbox; otherwise the
        // scope toggle decides the targets (Q3: mailbox or account, never all
        // accounts). The account names no mailbox at all, so the daemon runs
        // its account-wide search, the call the desktop and `mp search` make.
        let (targets, scope_label) = if let Some(ref name) = query.in_mailbox {
            match self.search_target_by_name(name) {
                Some(target) => {
                    let label = target.label.clone();
                    (Some(vec![target]), label)
                }
                None => {
                    self.server_search_status = Some(format!("Unknown mailbox: {name}"));
                    return;
                }
            }
        } else {
            match self.search_form.scope.0 {
                SearchScope::CurrentMailbox => match self.current_mailbox_target() {
                    Some(target) => {
                        let label = target.label.clone();
                        (Some(vec![target]), label)
                    }
                    None => {
                        self.server_search_status =
                            Some("Current mailbox has no server search target".to_string());
                        return;
                    }
                },
                SearchScope::CurrentAccount => (None, "All".to_string()),
            }
        };

        // Scope for the immediate local FTS pass (#0105): the same mailbox the
        // server targets cover, spelled as the `messages.mailbox` key.
        let local_mailbox = if let Some(ref name) = query.in_mailbox {
            self.local_mailbox_key_by_name(name)
        } else {
            match self.search_form.scope.0 {
                SearchScope::CurrentMailbox => self.current_local_mailbox_key(),
                SearchScope::CurrentAccount => None,
            }
        };

        self.server_search_scope_label = scope_label;
        self.server_search_generation = self.server_search_generation.wrapping_add(1);
        self.push_action(Action::ServerSearch {
            query,
            targets,
            local_mailbox,
        });
    }

    fn handle_search_overlay_list_key(&mut self, key: KeyEvent) -> Option<Message> {
        let len = self.server_search_results.len();
        if len == 0 {
            match key.code {
                KeyCode::Tab | KeyCode::BackTab => {
                    self.server_search_focus = SearchOverlayFocus::Field(SearchField::Advanced);
                }
                KeyCode::Esc => {
                    self.close_overlay();
                }
                _ => {}
            }
            return None;
        }

        // `te` and `cF`, the list's own keys for the hit under the cursor
        // (PERSO-174); any other key after `t` or `c` drops the prefix.
        match (self.pending_prefix, key.code) {
            (Some('t'), KeyCode::Char('e')) => {
                self.pending_prefix = None;
                self.push_action(Action::SearchResultExport);
                return None;
            }
            (Some('c'), KeyCode::Char('F')) => {
                self.pending_prefix = None;
                self.push_action(Action::SearchResultForwardAsAttachment);
                return None;
            }
            (Some('t' | 'c'), _) => self.pending_prefix = None,
            _ => {}
        }

        match key.code {
            KeyCode::Char(prefix @ ('t' | 'c')) => {
                self.pending_prefix = Some(prefix);
                return None;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if self.server_search_index < len - 1 {
                    self.server_search_index += 1;
                    self.server_search_scroll = 0;
                    self.server_search_headers_scroll = 0;
                }
                // Navigating means the result-count / decline message has been
                // seen; clearing it uncovers the list-key hints (#0104).
                self.server_search_status = None;
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if self.server_search_index > 0 {
                    self.server_search_index -= 1;
                    self.server_search_scroll = 0;
                    self.server_search_headers_scroll = 0;
                }
                self.server_search_status = None;
            }
            KeyCode::Char('g') => {
                if self.pending_prefix == Some('g') {
                    self.server_search_index = 0;
                    self.server_search_scroll = 0;
                    self.server_search_headers_scroll = 0;
                    self.pending_prefix = None;
                } else {
                    self.pending_prefix = Some('g');
                }
                return None;
            }
            KeyCode::Char('G') => {
                self.pending_prefix = None;
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
            KeyCode::Enter => {
                self.push_action(Action::SearchResultJump);
            }
            KeyCode::Char('e') => {
                self.push_action(Action::SearchResultOpen);
            }
            KeyCode::Char('y') => {
                self.push_action(Action::SearchResultYankPath);
            }
            KeyCode::Char('f') => {
                self.push_action(Action::SearchResultFetch);
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
                self.server_search_focus = SearchOverlayFocus::Field(SearchField::Advanced);
            }
            KeyCode::Esc => {
                self.close_overlay();
            }
            _ => {}
        }
        self.pending_prefix = None;
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
                    self.pending_prefix = None;
                    self.activity_scroll = self.activity_scroll.saturating_add(1);
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    self.pending_prefix = None;
                    self.activity_scroll = self.activity_scroll.saturating_sub(1);
                }
                KeyCode::Char('g') => {
                    if self.pending_prefix == Some('g') {
                        self.activity_scroll = 0;
                        self.pending_prefix = None;
                    } else {
                        self.pending_prefix = Some('g');
                    }
                    return None;
                }
                KeyCode::Char('G') => {
                    self.pending_prefix = None;
                    self.activity_scroll = u16::MAX;
                }
                KeyCode::Char('d') => {
                    self.pending_prefix = None;
                    self.activity_scroll = self.activity_scroll.saturating_add(10);
                }
                KeyCode::Char('u') => {
                    self.pending_prefix = None;
                    self.activity_scroll = self.activity_scroll.saturating_sub(10);
                }
                KeyCode::Char('/') => {
                    self.pending_prefix = None;
                    self.activity_filter_active = true;
                    self.activity_filter.clear();
                    self.activity_scroll = 0;
                }
                KeyCode::Esc | KeyCode::Char('L') | KeyCode::Char('q') => {
                    self.pending_prefix = None;
                    self.close_overlay();
                    self.activity_scroll = 0;
                    self.activity_filter.clear();
                    self.activity_filter_active = false;
                }
                _ => {
                    self.pending_prefix = None;
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
                wizard.focus = wizard.next_field();
                wizard.suggestion_idx = 0;
                self.recompute_compose_suggestions();
                return None;
            }
            KeyCode::BackTab => {
                wizard.focus = wizard.prev_field();
                wizard.suggestion_idx = 0;
                self.recompute_compose_suggestions();
                return None;
            }
            KeyCode::Up => {
                if wizard.focus == ComposeField::Signature {
                    cycle_signature(wizard, false);
                } else if wizard.focus.is_address()
                    && !wizard.suggestions.is_empty()
                    && wizard.suggestion_idx > 0
                {
                    wizard.suggestion_idx -= 1;
                }
                return None;
            }
            KeyCode::Down => {
                if wizard.focus == ComposeField::Signature {
                    cycle_signature(wizard, true);
                } else if wizard.focus.is_address()
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
            KeyCode::Char('e') if ctrl => {
                if wizard.focus == ComposeField::Signature {
                    self.push_action(Action::ComposeEditSignature);
                }
                return None;
            }
            KeyCode::Char('n') if ctrl => {
                if wizard.focus == ComposeField::Signature {
                    cycle_signature(wizard, true);
                } else if wizard.focus.is_address()
                    && !wizard.suggestions.is_empty()
                    && wizard.suggestion_idx + 1 < wizard.suggestions.len()
                {
                    wizard.suggestion_idx += 1;
                }
                return None;
            }
            KeyCode::Char('p') if ctrl => {
                if wizard.focus == ComposeField::Signature {
                    cycle_signature(wizard, false);
                } else if wizard.focus.is_address()
                    && !wizard.suggestions.is_empty()
                    && wizard.suggestion_idx > 0
                {
                    wizard.suggestion_idx -= 1;
                }
                return None;
            }
            KeyCode::Char('u') if ctrl => {
                // The Signature field is a selector, not a buffer to clear.
                if wizard.focus == ComposeField::Signature {
                    return None;
                }
                // Clear the current field.
                current_field_mut(wizard).clear();
                self.recompute_compose_suggestions();
                return None;
            }
            KeyCode::Enter => {
                // The Signature field submits from Enter like any non-last
                // field: advance to the next field (Body / To).
                if wizard.focus == ComposeField::Signature {
                    wizard.focus = wizard.next_field();
                    wizard.suggestion_idx = 0;
                    self.recompute_compose_suggestions();
                    return None;
                }
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
                // The body is multi-line (#0097): Enter inserts a newline, and
                // Ctrl+g is the submit key.
                if wizard.focus == ComposeField::Body {
                    wizard.body.push('\n');
                    return None;
                }
                // Subject is the last field only when there is no body field
                // (Forward / EditDraft): Enter there submits. In a New compose
                // Enter advances to the body instead.
                if wizard.focus == ComposeField::Subject && !wizard.has_body_field() {
                    self.push_action(Action::ComposeWizardSubmit);
                    return None;
                }
                // Otherwise cycle to the next field.
                wizard.focus = wizard.next_field();
                wizard.suggestion_idx = 0;
                self.recompute_compose_suggestions();
                return None;
            }
            KeyCode::Backspace => {
                if wizard.focus == ComposeField::Signature {
                    return None;
                }
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
                // The Signature field is cycled, not typed into; `e` opens the
                // selected signature in `$EDITOR`, everything else is a no-op.
                if wizard.focus == ComposeField::Signature {
                    if c == 'e' {
                        self.push_action(Action::ComposeEditSignature);
                    }
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
            ComposeField::Subject | ComposeField::Signature | ComposeField::Body => {
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

        let results = mp_core::contacts::search(index, &query, 12);
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
        if !email.is_invite {
            self.set_status("Not a calendar invite".to_string());
            return;
        }
        if self.active_kind() == MailboxKind::Sent {
            self.set_status("You are the organizer of this invite — nothing to RSVP".to_string());
            return;
        }
        let subject = email.subject.clone();
        let Some(msg) = email.msg else {
            self.set_status("This search hit has no local copy to RSVP from".to_string());
            return;
        };
        // The method and the summary live in the ics blob, which is parsed for
        // the selected message only (#0038 item 6); `message.invite` names the
        // server event beside it on an account with a calendar backend
        // (PERSO-200), which is what the answer goes to.
        let card = self.load_message_invite_card(msg);
        let event = card.as_ref().map(|card| &card.event);
        if let Some(refusal) = rsvp_refusal(event) {
            self.set_status(refusal);
            return;
        }
        let summary = event.and_then(|e| e.summary.clone()).unwrap_or(subject);
        let event_id = card.and_then(|card| card.event_id).unwrap_or_else(|| {
            format!(
                "{}{}",
                mp_protocol::calendar::IMIP_EVENT_PREFIX,
                msg.row_id()
            )
        });
        let account = self.account_config.name.clone();
        self.overlay = Overlay::Rsvp(RsvpOverlay::new(account, event_id, summary));
    }

    /// Build and open the conversation overlay for the cursor message (#0008).
    ///
    /// `message.thread` on the session the `App` holds (`LST-10`, P5-U10d):
    /// the selected row's `thread_id` and every message ingest gave that same
    /// id, oldest first and one row per `Message-ID`. Nothing re-parses headers
    /// anywhere; the grouping was decided at ingest and the daemon reads it out
    /// of the indexed column. The fold is the daemon's because it is a query
    /// over every mailbox of the account and this client holds one mailbox's
    /// listing at a time.
    ///
    /// A message with no related mail in the store (a lone message, or a reply
    /// whose parents are not downloaded) answers with itself alone, and that is
    /// the sentence below rather than a one-line overlay.
    ///
    /// The refusals collapse into one line, which is the trade every routed
    /// read of this migration has made: the account with no store and the row
    /// that is gone are both the daemon's own sentence now, with the code in
    /// it, where the store read branched on two outcomes of its own.
    fn open_thread_overlay(&mut self) {
        let Some(msg) = self.selected_email_ref() else {
            // A draft or an unresolved server-search hit has no store row.
            if self.selected_email().is_some() {
                self.set_status("A draft has no conversation to show".to_string());
            }
            return;
        };
        let account = self.account_config.name.clone();
        let answer = match self.queries() {
            Some(queries) => crate::queries::thread(queries, &account, msg),
            None => Err(anyhow::anyhow!("there is no daemon session to ask")),
        };
        let thread = match answer {
            Ok(thread) => thread,
            Err(e) => {
                self.set_status(format!("Could not load the conversation: {e:#}"));
                return;
            }
        };
        if thread.messages.len() <= 1 {
            self.set_status("No related emails for this message in the store".to_string());
            return;
        }
        let subject = Some(thread.subject.clone())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "(no subject)".to_string());
        let mut selected = 0;
        let messages: Vec<ThreadEntry> = thread
            .messages
            .iter()
            .enumerate()
            .map(|(i, r)| {
                if r.current {
                    selected = i;
                }
                ThreadEntry {
                    msg: MessageRef::new(r.id),
                    mailbox: r.mailbox.clone(),
                    from: super::extract_display_name(&r.from),
                    date_display: super::resolve_date(
                        &Some(r.date_display.clone()),
                        &None,
                        std::path::Path::new(""),
                    )
                    .0,
                    read: r.flags.seen,
                    answered: r.flags.answered,
                    forwarded: r.flags.forwarded,
                    flagged: r.flags.flagged,
                    current: r.current,
                }
            })
            .collect();
        self.overlay = Overlay::Thread(ThreadOverlay {
            subject,
            messages,
            selected,
        });
    }

    /// Input for the conversation overlay (#0008): `j`/`k` move, `Enter`/`e`
    /// opens the highlighted message (switching mailbox when it lives in
    /// another), `Esc`/`q`/`T` closes.
    fn handle_thread_overlay_key(&mut self, key: KeyEvent) -> Option<Message> {
        let Overlay::Thread(overlay) = &mut self.overlay else {
            return None;
        };
        let len = overlay.messages.len();
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                if overlay.selected + 1 < len {
                    overlay.selected += 1;
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                overlay.selected = overlay.selected.saturating_sub(1);
            }
            KeyCode::Char('g') => {
                overlay.selected = 0;
            }
            KeyCode::Char('G') => {
                overlay.selected = len.saturating_sub(1);
            }
            KeyCode::Enter | KeyCode::Char('e') => {
                let target = overlay.messages.get(overlay.selected).cloned();
                self.close_overlay();
                if let Some(entry) = target {
                    self.open_message(entry.msg, &entry.mailbox);
                }
            }
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('T') => {
                self.close_overlay();
            }
            _ => {}
        }
        None
    }

    /// The respond overlay's keys (#0029, PERSO-200). The arrows walk every
    /// row (the answers, the comment, the send-response toggle), `Tab` and
    /// `Shift+Tab` jump between those three sections keeping the answer; off
    /// the comment `j`/`k` walk too and `a`/`t`/`d` pick an answer; on the
    /// comment every character is typed; `Space` flips the toggle; `Enter`
    /// answers from any row and `Esc` cancels.
    fn handle_rsvp_overlay_key(&mut self, key: KeyEvent) -> Option<Message> {
        let overlay = self.rsvp_overlay_mut()?;
        let rows = overlay.rows();
        let at = rows.iter().position(|row| *row == overlay.row).unwrap_or(0);
        let typing = overlay.row == RsvpRow::Comment;
        let step = |overlay: &mut RsvpOverlay, to: usize| {
            overlay.row = rows[to];
            if let RsvpRow::Choice(choice) = overlay.row {
                overlay.selected = choice;
            }
        };
        // `Tab` jumps between the three sections (the answers, the comment,
        // the toggle) and keeps the answer; the arrows walk every row.
        let section = |row: RsvpRow| match row {
            RsvpRow::Choice(_) => 0,
            RsvpRow::Comment => 1,
            RsvpRow::SendResponse => 2,
        };
        let current = section(overlay.row);
        let last = section(rows[rows.len() - 1]);
        let jump = |overlay: &mut RsvpOverlay, to: usize| {
            overlay.row = match to {
                0 => RsvpRow::Choice(overlay.selected),
                1 => RsvpRow::Comment,
                _ => RsvpRow::SendResponse,
            };
        };
        match key.code {
            KeyCode::Tab => jump(overlay, (current + 1).min(last)),
            KeyCode::BackTab => jump(overlay, current.saturating_sub(1)),
            KeyCode::Down => step(overlay, (at + 1).min(rows.len() - 1)),
            KeyCode::Up => step(overlay, at.saturating_sub(1)),
            KeyCode::Char('j') if !typing => step(overlay, (at + 1).min(rows.len() - 1)),
            KeyCode::Char('k') if !typing => step(overlay, at.saturating_sub(1)),
            KeyCode::Char('a') if !typing => step(overlay, 0),
            KeyCode::Char('t') if !typing => step(overlay, 1),
            KeyCode::Char('d') if !typing => step(overlay, 2),
            KeyCode::Char(' ') if overlay.row == RsvpRow::SendResponse => {
                overlay.send_response = !overlay.send_response;
            }
            KeyCode::Char(c) if typing => overlay.comment.push(c),
            KeyCode::Backspace if typing => {
                overlay.comment.pop();
            }
            KeyCode::Enter => {
                let Overlay::Rsvp(overlay) = std::mem::replace(&mut self.overlay, Overlay::None)
                else {
                    return None;
                };
                let comment = overlay.comment.trim();
                self.push_action(Action::Rsvp {
                    account: overlay.account,
                    event: overlay.event,
                    choice: RsvpChoice::at(overlay.selected),
                    comment: (!comment.is_empty()).then(|| comment.to_string()),
                    send_response: !overlay.server || overlay.send_response,
                });
                // Consume-and-close: promote any error queued behind it.
                self.promote_pending_error();
            }
            KeyCode::Esc => self.close_overlay(),
            KeyCode::Char('q') if !typing => self.close_overlay(),
            _ => {}
        }
        None
    }

    /// The calendars overlay's keys (PERSO-200): `j`/`k` move, `Space`
    /// shows or hides the calendar under the cursor, `Esc`, `q` and `c`
    /// close it.
    fn handle_calendars_overlay_key(&mut self, key: KeyEvent) -> Option<Message> {
        let Overlay::Calendars(overlay) = &mut self.overlay else {
            return None;
        };
        let len = overlay.calendars.len();
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                if overlay.selected + 1 < len {
                    overlay.selected += 1;
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                overlay.selected = overlay.selected.saturating_sub(1);
            }
            KeyCode::Char(' ') | KeyCode::Enter => self.toggle_calendar_visibility(),
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('c') => self.close_overlay(),
            _ => {}
        }
        None
    }

    /// The event card's keys (PERSO-200): `j`/`k` scroll, `V` answers,
    /// `m` opens the invitation mail and `e` its `.ics`, `Esc`, `q` and
    /// `Enter` close it.
    fn handle_event_card_key(&mut self, key: KeyEvent) -> Option<Message> {
        let Overlay::EventCard(card) = &mut self.overlay else {
            return None;
        };
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => card.scroll = card.scroll.saturating_add(1),
            KeyCode::Char('k') | KeyCode::Up => card.scroll = card.scroll.saturating_sub(1),
            KeyCode::Char('V') => {
                let event = card.event.clone();
                self.close_overlay();
                self.open_rsvp_overlay_for(&event);
            }
            KeyCode::Char('m') => {
                let event = card.event.clone();
                self.open_event_mail(&event);
            }
            KeyCode::Char('e') => {
                let event = card.event.clone();
                self.close_overlay();
                self.open_event_ics(&event);
            }
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Enter => self.close_overlay(),
            _ => {}
        }
        None
    }

    /// The account picker's keys (#0147): `j`/`k` and the arrows move and
    /// clamp, Enter switches to the cursor's account, `1` to `9` switch to
    /// the nth, Esc and `q` close it unchanged. The picker leaves before the
    /// switch, so the new account is drawn with no overlay over it.
    fn handle_account_picker_key(&mut self, key: KeyEvent) -> Option<Message> {
        let count = self.accounts.len();
        let Overlay::Account(picker) = &mut self.overlay else {
            return None;
        };
        // A config reload may have removed accounts while the picker was up.
        picker.selected = picker.selected.min(count.saturating_sub(1));
        let target = match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                if picker.selected + 1 < count {
                    picker.selected += 1;
                }
                return None;
            }
            KeyCode::Char('k') | KeyCode::Up => {
                picker.selected = picker.selected.saturating_sub(1);
                return None;
            }
            KeyCode::Enter => picker.selected,
            KeyCode::Char(c @ '1'..='9') => {
                let idx = (c as usize) - ('1' as usize);
                if idx >= count {
                    return None;
                }
                idx
            }
            KeyCode::Esc | KeyCode::Char('q') => {
                self.close_overlay();
                return None;
            }
            _ => return None,
        };
        self.overlay = Overlay::None;
        self.switch_account(target);
        self.promote_pending_error();
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
                        let mut indices: Vec<usize> = picker.selected_set.iter().copied().collect();
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
        let picker = self.dir_picker_mut()?;

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
                    let home = PathBuf::from(shellexpand::tilde("~").into_owned());
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
                        self.push_action(Action::SaveAttachments { sources, dest_dir });
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
    /// than the ones the rows live in, so "move to the same mailbox"
    /// cannot happen: the active one, and an expanded conversation
    /// member's own wherever the list is (#0142).
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

        let msgs: Vec<MessageRef> = if !self.selection.is_empty() {
            self.selection.iter().filter_map(|k| k.msg()).collect()
        } else {
            // A conversation header moves its members here (#0142).
            match self
                .selected_email()
                .map(|e| e.action_refs())
                .filter(|msgs| !msgs.is_empty())
            {
                Some(msgs) => msgs,
                None => return,
            }
        };

        let sources = self.move_sources(&msgs);
        let candidates: Vec<(usize, String)> = self
            .mailboxes
            .iter()
            .enumerate()
            .filter(|(_, m)| !sources.contains(&super::mailbox_key(m)) && m.server_name.is_some())
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
            purpose: PickerPurpose::Move(msgs),
        });
    }

    /// Open the fuzzy picker over every mailbox of the sidebar (`gf`, #0141).
    ///
    /// The number keys reach the first nine; an account whose server lists
    /// more folders than that reaches the rest by name here, as aerc's and
    /// neomutt's folder pickers do. The open mailbox is listed too, so the
    /// candidates read as the sidebar does.
    fn open_folder_picker(&mut self) {
        let candidates: Vec<(usize, String)> = self
            .mailboxes
            .iter()
            .enumerate()
            .map(|(i, m)| (i, m.label.clone()))
            .collect();
        let filtered = (0..candidates.len()).collect();
        self.overlay = Overlay::Mailbox(MailboxPicker {
            query: String::new(),
            candidates,
            filtered,
            selected: 0,
            purpose: PickerPurpose::Jump,
        });
    }

    /// The mailboxes `msgs` live in: an expanded member's own, the active
    /// mailbox for every other row, a collapsed header's members included
    /// (#0142).
    fn move_sources(&self, msgs: &[MessageRef]) -> HashSet<String> {
        let active = self
            .mailboxes
            .get(self.active_mailbox)
            .map(super::mailbox_key);
        msgs.iter()
            .filter_map(|msg| {
                self.emails
                    .iter()
                    .filter(|e| e.msg == Some(*msg))
                    .find_map(|e| e.member_mailbox().map(str::to_string))
                    .or_else(|| active.clone())
            })
            .collect()
    }

    fn handle_mailbox_picker_key(&mut self, key: KeyEvent) -> Option<Message> {
        let picker = self.mailbox_picker_mut()?;

        match key.code {
            KeyCode::Down | KeyCode::Tab => {
                if !picker.filtered.is_empty() && picker.selected < picker.filtered.len() - 1 {
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
                    match picker.purpose {
                        PickerPurpose::Move(msgs) => {
                            self.selection.clear();
                            self.push_action(Action::MoveToMailbox { msgs, dest_idx });
                        }
                        PickerPurpose::Jump => {
                            self.sidebar_index = dest_idx;
                            self.switch_mailbox(dest_idx);
                            self.focus = Focus::List;
                        }
                    }
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

    // -----------------------------------------------------------------
    // Signature management overlay (#0107)
    // -----------------------------------------------------------------

    /// Open the signatures overlay on the active account (`cs`).
    ///
    /// An empty signatures directory is not a reason to refuse: the overlay is
    /// where a first signature gets created, so it opens on an empty list that
    /// says so.
    fn open_signatures_overlay(&mut self) {
        let account = self.account_config.name.clone();
        self.overlay = Overlay::Signatures(SignaturesOverlay::for_account(&account));
        self.refresh_signatures_overlay();
    }

    /// Route to the browse surface or to whichever prompt owns the keyboard.
    fn handle_signatures_key(&mut self, key: KeyEvent) -> Option<Message> {
        let mode = match &self.overlay {
            Overlay::Signatures(overlay) => overlay.mode,
            _ => return None,
        };
        match mode {
            SignaturesMode::Browse => self.handle_signatures_browse_key(key),
            SignaturesMode::New | SignaturesMode::Rename => {
                self.handle_signatures_prompt_key(key, mode)
            }
        }
    }

    /// The list surface: navigate, set the default, and the four file
    /// operations. Each mutation is a `signature.*` call on the daemon and
    /// surfaces its refusal on the status line rather than failing quietly.
    fn handle_signatures_browse_key(&mut self, key: KeyEvent) -> Option<Message> {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                if let Some(overlay) = self.signatures_overlay_mut() {
                    if overlay.selected + 1 < overlay.names.len() {
                        overlay.selected += 1;
                    }
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                if let Some(overlay) = self.signatures_overlay_mut() {
                    overlay.selected = overlay.selected.saturating_sub(1);
                }
            }
            KeyCode::Enter => self.toggle_selected_signature_default(),
            KeyCode::Char('e') => match self.selected_signature_name() {
                Some(name) => self.push_action(Action::EditSignatureFile { name }),
                None => self.set_status("No signature to edit; press n to create one".to_string()),
            },
            KeyCode::Char('n') => {
                if let Some(overlay) = self.signatures_overlay_mut() {
                    overlay.mode = SignaturesMode::New;
                    overlay.input.clear();
                }
            }
            KeyCode::Char('r') => {
                // Seed the prompt with the current name: a rename is usually an
                // edit of what is there, not a retype.
                let current = self.selected_signature_name();
                match current {
                    Some(name) => {
                        if let Some(overlay) = self.signatures_overlay_mut() {
                            overlay.mode = SignaturesMode::Rename;
                            overlay.input = name;
                        }
                    }
                    None => self.set_status("No signature to rename".to_string()),
                }
            }
            KeyCode::Char('d') => self.confirm_delete_signature(),
            KeyCode::Esc | KeyCode::Char('q') => self.close_overlay(),
            _ => {}
        }
        None
    }

    /// The two name prompts (create / rename): free text until Enter commits
    /// or Esc goes back to the list.
    fn handle_signatures_prompt_key(
        &mut self,
        key: KeyEvent,
        mode: SignaturesMode,
    ) -> Option<Message> {
        match key.code {
            KeyCode::Esc => {
                if let Some(overlay) = self.signatures_overlay_mut() {
                    overlay.mode = SignaturesMode::Browse;
                    overlay.input.clear();
                }
            }
            KeyCode::Backspace => {
                if let Some(overlay) = self.signatures_overlay_mut() {
                    overlay.input.pop();
                }
            }
            KeyCode::Enter => match mode {
                SignaturesMode::New => self.commit_new_signature(),
                SignaturesMode::Rename => self.commit_signature_rename(),
                SignaturesMode::Browse => {}
            },
            KeyCode::Char(c) => {
                if let Some(overlay) = self.signatures_overlay_mut() {
                    overlay.input.push(c);
                }
            }
            _ => {}
        }
        None
    }

    /// The signature under the overlay's cursor, owned so the caller can then
    /// take `&mut self` again.
    fn selected_signature_name(&self) -> Option<String> {
        match &self.overlay {
            Overlay::Signatures(overlay) => overlay.selected_name().map(str::to_string),
            _ => None,
        }
    }

    /// Enter on the list: make the cursor signature the account default, or
    /// clear the default when it already is one. Without the second half there
    /// would be no way back to "no signature by default" short of deleting the
    /// file.
    fn toggle_selected_signature_default(&mut self) {
        let (account, name, already_default) = match &self.overlay {
            Overlay::Signatures(overlay) => (
                overlay.account.clone(),
                overlay.selected_name().map(str::to_string),
                overlay.selected_is_default(),
            ),
            _ => return,
        };
        let Some(name) = name else {
            self.set_status("No signature selected".to_string());
            return;
        };

        let target = if already_default {
            None
        } else {
            Some(name.as_str())
        };
        let set = self
            .signature_call(
                "signature.set_default",
                serde_json::json!({"account": account, "name": target}),
            )
            .and_then(|answer| {
                serde_json::from_value(answer)
                    .map_err(|e| format!("reading signature.set_default: {e}"))
            });
        match set {
            Ok(listing) => {
                if let Some(overlay) = self.signatures_overlay_mut() {
                    overlay.apply(listing);
                }
                let msg = if already_default {
                    format!("'{name}' is no longer the default signature")
                } else {
                    format!("'{name}' is now the default signature")
                };
                self.set_status(msg);
            }
            Err(e) => self.set_status_level(
                format!("Cannot set the default signature: {e}"),
                StatusLevel::Error,
            ),
        }
    }

    /// Enter at the create prompt. A name the daemon refuses (empty, a path
    /// separator, already taken) leaves the prompt open with the text intact
    /// so it can be fixed; a good one creates the file and drops the user
    /// straight into `$EDITOR`, which is what they wanted `n` for.
    fn commit_new_signature(&mut self) {
        let name = match &self.overlay {
            Overlay::Signatures(overlay) => overlay.input.clone(),
            _ => return,
        };
        match self.signature_call("signature.create", serde_json::json!({"name": name})) {
            Ok(_) => {
                if let Some(overlay) = self.signatures_overlay_mut() {
                    overlay.mode = SignaturesMode::Browse;
                    overlay.input.clear();
                }
                self.refresh_signatures_overlay();
                if let Some(overlay) = self.signatures_overlay_mut() {
                    overlay.select(&name);
                }
                self.push_action(Action::EditSignatureFile { name: name.clone() });
                self.set_status(format!("Created signature '{name}'"));
            }
            Err(e) => self.set_status_level(format!("Cannot create: {e}"), StatusLevel::Error),
        }
    }

    /// Enter at the rename prompt. Same error contract as the create prompt:
    /// the prompt stays open on a refusal.
    fn commit_signature_rename(&mut self) {
        let (old, new) = match &self.overlay {
            Overlay::Signatures(overlay) => (
                overlay.selected_name().map(str::to_string),
                overlay.input.clone(),
            ),
            _ => return,
        };
        let Some(old) = old else {
            self.set_status("No signature to rename".to_string());
            return;
        };
        let account = match &self.overlay {
            Overlay::Signatures(overlay) => overlay.account.clone(),
            _ => return,
        };
        let renamed = self
            .signature_call(
                "signature.rename",
                serde_json::json!({"account": account, "old": old, "new": new}),
            )
            .and_then(|answer| {
                serde_json::from_value(answer).map_err(|e| format!("reading signature.rename: {e}"))
            });
        match renamed {
            Ok(listing) => {
                if let Some(overlay) = self.signatures_overlay_mut() {
                    overlay.mode = SignaturesMode::Browse;
                    overlay.input.clear();
                    // The rename carries the account default across, so the
                    // listing is what shows the marker on the new name.
                    overlay.apply(listing);
                    overlay.select(&new);
                }
                self.set_status(format!("Renamed '{old}' to '{new}'"));
            }
            Err(e) => self.set_status_level(format!("Cannot rename: {e}"), StatusLevel::Error),
        }
    }

    /// `d`: raise the shared confirm dialog over the overlay, handing it the
    /// overlay so either answer lands back on the list.
    fn confirm_delete_signature(&mut self) {
        let name = self.selected_signature_name();
        let Some(name) = name else {
            self.set_status("No signature to delete".to_string());
            return;
        };
        let Overlay::Signatures(overlay) = std::mem::replace(&mut self.overlay, Overlay::None)
        else {
            return;
        };
        // The file the daemon would delete, as `signature.read` names it.
        let detail = self
            .signature_call("signature.read", serde_json::json!({"name": name}))
            .ok()
            .and_then(|answer| answer["path"].as_str().map(str::to_string))
            .unwrap_or_else(|| format!("{name}.md"));
        self.overlay = Overlay::Confirm(ConfirmDialog {
            title: format!("Delete signature '{name}'?"),
            detail,
            action: ConfirmAction::DeleteSignature(overlay),
        });
    }

    /// Command-palette input (`:` / `Ctrl+p`, #0100): a text-input fuzzy finder
    /// over the runnable `KeyAction` catalogue, modelled on the mailbox picker.
    /// Typed characters filter (family leaders are literal here); Up/Down (and
    /// Tab/BackTab) move the cursor; Enter closes the palette and runs the
    /// selected action against the current context through the same executor a
    /// keypress uses; Esc closes.
    fn handle_command_palette_key(&mut self, key: KeyEvent) -> Option<Message> {
        let palette = match &mut self.overlay {
            Overlay::Palette(p) => p,
            _ => return None,
        };
        match key.code {
            KeyCode::Down | KeyCode::Tab => {
                if !palette.filtered.is_empty() && palette.selected < palette.filtered.len() - 1 {
                    palette.selected += 1;
                }
            }
            KeyCode::Up | KeyCode::BackTab => {
                palette.selected = palette.selected.saturating_sub(1);
            }
            KeyCode::Enter => {
                if let Some(action) = palette.selected_action() {
                    // Consume-and-close, then run the action in the context the
                    // palette floated over. `execute` reads no digit/leader key
                    // for any palette-runnable action, so the Enter event is a
                    // fine stand-in for the key that would have armed it.
                    self.close_overlay();
                    return self.execute(action, key);
                }
            }
            KeyCode::Esc => {
                self.close_overlay();
            }
            KeyCode::Backspace => {
                palette.query.pop();
                refresh_palette_filter(palette);
            }
            KeyCode::Char(c) => {
                palette.query.push(c);
                refresh_palette_filter(palette);
            }
            _ => {}
        }
        None
    }

    /// Helper to open the attachment picker in the given mode.
    ///
    /// The files come out of `message_blobs` (#0052 scope item 8): the row
    /// under the cursor is materialised into a temp directory the way
    /// `mp open` and `mp save` do it, and the picker and the save pipeline
    /// below it address those files, as they always did.
    fn open_attachment_picker(&mut self, mode: AttachmentPickerMode) {
        let Some(files) = crate::actions::cursor_attachment_files(self) else {
            return;
        };
        self.present_attachments(files, mode);
    }

    /// Helper to open the attachment picker for a search result.
    ///
    /// A hit that resolved to a local row is [`Self::open_attachment_picker`]
    /// exactly; one that did not is listed and cut by the daemon from the
    /// server's copy through the same calls (PERSO-155), so neither half
    /// declines.
    fn open_search_result_attachment_picker(&mut self, mode: AttachmentPickerMode) {
        let Some(files) = crate::actions::search_hit_attachment_files(self) else {
            return;
        };
        self.present_attachments(files, mode);
    }

    /// Put a materialised attachment list in front of the user: nothing to
    /// show says so, one file skips the picker (`o` opens it, `O` goes
    /// straight to the directory picker), several open the picker.
    ///
    /// The pre-store build's own branching, kept: only the origin of the files
    /// changed. `mp open`'s CLI shortcut of opening every attachment at once
    /// stays CLI-only, because a TUI that opened six windows on one keypress
    /// would be the surprising half of the two.
    ///
    /// The body's inline images come after the real attachments and the
    /// picker tags them (PERSO-165), so a signed PDF opens the picker with its
    /// logos below it, and an iPhone photo stays one keypress away.
    fn present_attachments(
        &mut self,
        files: crate::app::AttachmentFiles,
        mode: AttachmentPickerMode,
    ) {
        let crate::app::AttachmentFiles { files, inline_from } = files;
        match files.len() {
            0 => self.set_status("No attachments".to_string()),
            1 if mode == AttachmentPickerMode::Open => {
                self.push_action(Action::OpenAttachment(files.into_iter().next().unwrap()));
            }
            1 => self.open_dir_picker(files),
            _ => {
                self.overlay = Overlay::Attachment(AttachmentPicker {
                    files,
                    inline_from,
                    selected: 0,
                    mode,
                    selected_set: HashSet::new(),
                });
            }
        }
    }

    /// Open the directory picker overlay with the given source files.
    pub(crate) fn open_dir_picker(&mut self, sources: Vec<PathBuf>) {
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
                    self.pending_prefix = None;
                    self.help_scroll = self.help_scroll.saturating_add(1);
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    self.pending_prefix = None;
                    self.help_scroll = self.help_scroll.saturating_sub(1);
                }
                KeyCode::Char('g') => {
                    if self.pending_prefix == Some('g') {
                        self.help_scroll = 0;
                        self.pending_prefix = None;
                    } else {
                        self.pending_prefix = Some('g');
                    }
                }
                KeyCode::Char('G') => {
                    self.pending_prefix = None;
                    self.help_scroll = u16::MAX;
                }
                KeyCode::Char('d') => {
                    self.pending_prefix = None;
                    self.help_scroll = self.help_scroll.saturating_add(10);
                }
                KeyCode::Char('u') => {
                    self.pending_prefix = None;
                    self.help_scroll = self.help_scroll.saturating_sub(10);
                }
                KeyCode::Char('/') => {
                    self.pending_prefix = None;
                    self.help_filter_active = true;
                    self.help_filter.clear();
                    self.help_scroll = 0;
                }
                KeyCode::Char('?') | KeyCode::Esc => {
                    self.pending_prefix = None;
                    self.close_overlay();
                    self.help_scroll = 0;
                    self.help_filter.clear();
                    self.help_filter_active = false;
                }
                _ => {
                    self.pending_prefix = None;
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

    /// Jump-to-date input (#0017).
    ///
    /// `Enter` resolves the typed date and moves the cursor, `Esc` abandons
    /// it, and an unreadable date leaves the prompt armed with the reason on
    /// the status line -- a typo costs a correction, not a re-arm.
    ///
    /// `now()` is read here, at the moment the user commits, and passed into
    /// the pure grammar: `last week` means a week before the keypress, and
    /// nothing below this line knows what day it is.
    fn handle_jump_date_key(&mut self, key: KeyEvent) -> Option<Message> {
        match key.code {
            KeyCode::Enter => {
                let input = self.jump_date_input.clone().unwrap_or_default();
                let today = chrono::Local::now().date_naive();
                match super::jump_date::parse_jump_date(&input, today) {
                    Ok(target) => {
                        self.jump_date_input = None;
                        if self.view == super::View::Calendar {
                            self.calendar_go_to(target);
                        } else {
                            self.jump_to_date(target);
                        }
                    }
                    Err(reason) => {
                        self.set_status_level(reason, crate::app::StatusLevel::Warning);
                    }
                }
            }
            KeyCode::Esc => {
                self.jump_date_input = None;
            }
            KeyCode::Char(c) => {
                if let Some(buf) = self.jump_date_input.as_mut() {
                    buf.push(c);
                }
            }
            KeyCode::Backspace => {
                if let Some(buf) = self.jump_date_input.as_mut() {
                    buf.pop();
                }
            }
            _ => {}
        }
        None
    }

    /// Attach-file input (#0098).
    ///
    /// `Enter` resolves the typed path and, when it is on disk, hands it to
    /// [`Action::AttachFileToDraft`] which appends it to the cursor draft's
    /// `attachments:` frontmatter. A path that does not resolve is named on the
    /// status line and the prompt stays armed, so a typo costs a correction
    /// rather than a re-arm -- the same shape the jump-to-date prompt uses.
    /// `Esc` abandons it, and an empty commit is a silent cancel.
    ///
    /// The path is expanded for the existence check the way
    /// [`crate::send::resolve_attachment_paths`] expands it (`~` to `$HOME`),
    /// but the raw text the user typed is what is stored, so a portable
    /// `~`-relative entry survives to be re-expanded at send time.
    fn handle_attach_file_key(&mut self, key: KeyEvent) -> Option<Message> {
        match key.code {
            KeyCode::Enter => {
                let input = self.attach_file_input.clone().unwrap_or_default();
                let trimmed = input.trim();
                if trimmed.is_empty() {
                    // An empty commit cancels rather than storing a blank entry.
                    self.attach_file_input = None;
                    return None;
                }
                let expanded = shellexpand::tilde(trimmed).into_owned();
                if !std::path::Path::new(&expanded).exists() {
                    self.set_status_level(
                        format!("No such file: {trimmed}"),
                        crate::app::StatusLevel::Warning,
                    );
                    return None;
                }
                self.attach_file_input = None;
                self.push_action(Action::AttachFileToDraft {
                    path: trimmed.to_string(),
                });
            }
            KeyCode::Esc => {
                self.attach_file_input = None;
            }
            KeyCode::Char(c) => {
                if let Some(buf) = self.attach_file_input.as_mut() {
                    buf.push(c);
                }
            }
            KeyCode::Backspace => {
                if let Some(buf) = self.attach_file_input.as_mut() {
                    buf.pop();
                }
            }
            _ => {}
        }
        None
    }

    /// Contacts view fuzzy-search input (#0033). Free-text like the metadata
    /// search: each edit recomputes the matched contact list. `Enter` leaves
    /// the input but keeps the filter; `Esc` clears it.
    fn handle_contacts_search_key(&mut self, key: KeyEvent) -> Option<Message> {
        match key.code {
            KeyCode::Enter => {
                self.contacts_view.searching = false;
            }
            KeyCode::Esc => {
                self.contacts_view.searching = false;
                self.contacts_view.query.clear();
                self.recompute_contact_matches();
            }
            KeyCode::Char(c) => {
                self.contacts_view.query.push(c);
                self.recompute_contact_matches();
            }
            KeyCode::Backspace => {
                self.contacts_view.query.pop();
                self.recompute_contact_matches();
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
    pub fn apply_search_filter(&mut self, narrow: bool) {
        self.selection.clear();

        if self.search_query.is_empty() {
            self.visible = (0..self.emails.len()).collect();
        } else {
            let kind = self.active_kind();
            if narrow {
                let needle = self.search_query.to_lowercase();
                narrow_visible(&self.emails, &mut self.visible, &needle, kind);
            } else {
                // `filter_visible` lowercases the needle once internally.
                self.visible = filter_visible(&self.emails, &self.search_query, kind);
            }
        }
        self.apply_flagged_filter();

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
///
/// `bodies` is `Some` only in body-search mode (`\`), and holds the mailbox's
/// bodies already lowercased, so the body test is the same substring test the
/// entry used to answer from its own `body` field before it became lazy
/// (#0038 scope item 5). An entry the index has no body for (no store row, or
/// a blob the retention sweep evicted) simply does not match on body.
/// Why the invite under the mail cursor cannot be RSVP'd, or `None` when it
/// can (#0029 for the REQUEST guard, #0031 for the version guards).
///
/// Pure so the version rules are pinned without a store: the payload the card
/// shows is one *version* of the event, and a reply to a cancelled or
/// superseded version would carry a `SEQUENCE` the organizer has already moved
/// past. Refuse and say why rather than mailing an answer about a dead
/// version; the invite itself stays readable either way.
fn rsvp_refusal(event: Option<&mp_core::types::EventFrontmatter>) -> Option<String> {
    let is_request = event
        .and_then(|e| e.method.as_deref())
        .is_some_and(|m| m.eq_ignore_ascii_case("REQUEST"));
    if !is_request {
        return Some("Only received invitations (REQUEST) can be RSVP'd".to_string());
    }
    if event.is_some_and(|e| e.cancelled) {
        return Some(
            "This event was cancelled by the organizer \u{2014} nothing to RSVP".to_string(),
        );
    }
    if event.is_some_and(|e| e.superseded) {
        return Some(
            "A newer version of this invitation has arrived \u{2014} RSVP from that one"
                .to_string(),
        );
    }
    None
}

fn email_matches(email: &EmailEntry, needle_lower: &str, kind: MailboxKind) -> bool {
    // A conversation header matches on its participants as well as on its
    // `latest`, which the fields below are (#0142).
    if email.header().is_some_and(|h| {
        h.participants
            .iter()
            .any(|p| p.to_lowercase().contains(needle_lower))
    }) {
        return true;
    }
    email.subject.to_lowercase().contains(needle_lower)
        || email
            .display_contact(kind)
            .to_lowercase()
            .contains(needle_lower)
        || email
            .list_date_display()
            .to_lowercase()
            .contains(needle_lower)
        || email.from.to_lowercase().contains(needle_lower)
        || email.to.to_lowercase().contains(needle_lower)
}

/// What selecting `email` puts in the selection: its own key, or for a
/// conversation header the keys of its members in the listed mailbox, which is
/// what a batch over it reaches (#0142).
fn selection_keys(email: &EmailEntry) -> Vec<EntryKey> {
    if email.header().is_some() {
        return email.action_refs().into_iter().map(EntryKey::Msg).collect();
    }
    email.key().into_iter().collect()
}

/// The confirm title of archiving or deleting the cursor row: a conversation
/// of several messages here names how many the action reaches (#0142).
fn confirm_title(verb: &str, email: &EmailEntry) -> String {
    match email.header().map(|h| h.member_ids.len()) {
        Some(n) if n > 1 => format!("{verb} the {n} emails of this conversation?"),
        _ => format!("{verb} this email?"),
    }
}

/// Build the visible-index view of `emails` for `query` from scratch.
/// Empty query -> all indices (in order). The needle is lowercased once.
pub(super) fn filter_visible(emails: &[EmailEntry], query: &str, kind: MailboxKind) -> Vec<usize> {
    if query.is_empty() {
        return (0..emails.len()).collect();
    }
    let needle = query.to_lowercase();
    emails
        .iter()
        .enumerate()
        .filter(|(_, e)| email_matches(e, &needle, kind))
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
) {
    visible.retain(|&i| {
        emails
            .get(i)
            .is_some_and(|e| email_matches(e, needle_lower, kind))
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
        // The Signature field has no editable buffer; the key handler never
        // routes text-editing keys here when it is focused (#0106).
        ComposeField::Signature => unreachable!("signature field has no text buffer"),
        ComposeField::Body => &mut wizard.body,
    }
}

/// Move the compose wizard's signature selection one step through the
/// available signatures (#0106). `forward` cycles To -> next; wraps around.
/// No-op when there are no signatures.
fn cycle_signature(wizard: &mut ComposeWizard, forward: bool) {
    let n = wizard.available_signatures.len();
    if n == 0 {
        return;
    }
    let cur = wizard
        .signature_name
        .as_ref()
        .and_then(|name| wizard.available_signatures.iter().position(|x| x == name));
    let next = match cur {
        Some(i) if forward => (i + 1) % n,
        Some(i) => (i + n - 1) % n,
        None => 0,
    };
    wizard.signature_name = Some(wizard.available_signatures[next].clone());
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

/// Recompute `palette.filtered` from `palette.query` and reset the cursor
/// (#0100). Each entry's section, label and keys are scored with
/// [`command_score`], the desktop palette's scorer, and the matches are listed
/// best first (ties keep catalogue order), so a query and Enter run the same
/// action in both clients.
pub(super) fn refresh_palette_filter(palette: &mut CommandPalette) {
    if palette.query.is_empty() {
        palette.filtered = (0..palette.entries.len()).collect();
        palette.selected = 0;
        return;
    }
    let mut scored: Vec<(usize, f64)> = palette
        .entries
        .iter()
        .enumerate()
        .map(|(i, entry)| (i, command_score(&entry.search, &palette.query)))
        .filter(|&(_, score)| score > 0.0)
        .collect();
    // A stable sort, so equal scores keep catalogue order, as cmdk's does.
    scored.sort_by(|a, b| b.1.total_cmp(&a.1));
    palette.filtered = scored.into_iter().map(|(i, _)| i).collect();
    palette.selected = 0;
}

/// How well `search` matches `value`, from 0 (no match) to 1: a port of
/// `command-score` as cmdk 1.1 ships it, the scorer the desktop palette ranks
/// with. Every char of the search must appear in order; a match at the start,
/// after a word break or after punctuation scores above one in mid-word, a
/// gap costs a little per skipped char, and a case mismatch a little more.
pub(super) fn command_score(value: &str, search: &str) -> f64 {
    fn fold(s: &[char]) -> Vec<char> {
        s.iter()
            .map(|&c| {
                if c.is_whitespace() || c == '-' {
                    ' '
                } else {
                    c.to_lowercase().next().unwrap_or(c)
                }
            })
            .collect()
    }
    let value: Vec<char> = value.chars().collect();
    let search: Vec<char> = search.chars().collect();
    let s = Scorer {
        lower_value: fold(&value),
        lower_search: fold(&search),
        value,
        search,
    };
    s.score(0, 0, &mut std::collections::HashMap::new())
}

struct Scorer {
    value: Vec<char>,
    search: Vec<char>,
    lower_value: Vec<char>,
    lower_search: Vec<char>,
}

impl Scorer {
    const CONTINUE_MATCH: f64 = 1.0;
    const SPACE_WORD_JUMP: f64 = 0.9;
    const NON_SPACE_WORD_JUMP: f64 = 0.8;
    const CHARACTER_JUMP: f64 = 0.17;
    const TRANSPOSITION: f64 = 0.1;
    const PENALTY_SKIPPED: f64 = 0.999;
    const PENALTY_CASE_MISMATCH: f64 = 0.9999;
    const PENALTY_NOT_COMPLETE: f64 = 0.99;

    fn is_gap(c: char) -> bool {
        matches!(
            c,
            '\\' | '/' | '_' | '+' | '.' | '#' | '"' | '@' | '[' | '(' | '{' | '&'
        )
    }

    fn is_space(c: char) -> bool {
        c.is_whitespace() || c == '-'
    }

    fn at(chars: &[char], i: usize) -> Option<char> {
        chars.get(i).copied()
    }

    fn before(chars: &[char], i: usize) -> Option<char> {
        i.checked_sub(1).and_then(|j| chars.get(j).copied())
    }

    fn find(&self, c: char, from: usize) -> Option<usize> {
        self.lower_value
            .get(from..)
            .and_then(|rest| rest.iter().position(|&h| h == c))
            .map(|p| p + from)
    }

    fn score(
        &self,
        vi: usize,
        si: usize,
        memo: &mut std::collections::HashMap<(usize, usize), f64>,
    ) -> f64 {
        if si == self.search.len() {
            return if vi == self.value.len() {
                Self::CONTINUE_MATCH
            } else {
                Self::PENALTY_NOT_COMPLETE
            };
        }
        if si > self.search.len() {
            return 0.0;
        }
        if let Some(&s) = memo.get(&(vi, si)) {
            return s;
        }
        let wanted = self.lower_search[si];
        let mut best = 0.0;
        let mut idx = self.find(wanted, vi);
        while let Some(i) = idx {
            let mut score = self.score(i + 1, si + 1, memo);
            if score > best {
                let prev = Self::before(&self.value, i);
                if i == vi {
                    score *= Self::CONTINUE_MATCH;
                } else if prev.is_some_and(Self::is_gap) {
                    score *= Self::NON_SPACE_WORD_JUMP;
                    let skipped = self.value[vi..i - 1]
                        .iter()
                        .filter(|&&c| Self::is_gap(c))
                        .count();
                    if skipped > 0 && vi > 0 {
                        score *= Self::PENALTY_SKIPPED.powi(skipped as i32);
                    }
                } else if prev.is_some_and(Self::is_space) {
                    score *= Self::SPACE_WORD_JUMP;
                    let skipped = self.value[vi..i - 1]
                        .iter()
                        .filter(|&&c| Self::is_space(c))
                        .count();
                    if skipped > 0 && vi > 0 {
                        score *= Self::PENALTY_SKIPPED.powi(skipped as i32);
                    }
                } else {
                    score *= Self::CHARACTER_JUMP;
                    if vi > 0 {
                        score *= Self::PENALTY_SKIPPED.powi((i - vi) as i32);
                    }
                }
                if Self::at(&self.value, i) != Self::at(&self.search, si) {
                    score *= Self::PENALTY_CASE_MISMATCH;
                }
            }
            let next = Self::at(&self.lower_search, si + 1);
            if (score < Self::TRANSPOSITION
                && Self::before(&self.lower_value, i) == next
                && next.is_some())
                || (next == Some(wanted) && Self::before(&self.lower_value, i) != Some(wanted))
            {
                let transposed = self.score(i + 1, si + 2, memo);
                if transposed * Self::TRANSPOSITION > score {
                    score = transposed * Self::TRANSPOSITION;
                }
            }
            if score > best {
                best = score;
            }
            idx = self.find(wanted, i + 1);
        }
        memo.insert((vi, si), best);
        best
    }
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
    use super::super::PersistentError;
    use super::*;
    use crate::app::{AttachmentPicker, EntryKey};
    use std::path::PathBuf;

    /// A `MessageRef` derived from the subject, so the same fixture email
    /// built twice (a reload delivering a re-sorted `sample()`) carries the
    /// same identity, exactly as two loads of one store row would. The
    /// subject is the fixtures' de-facto primary key.
    fn ref_for(subject: &str) -> MessageRef {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        subject.hash(&mut h);
        MessageRef::new((h.finish() >> 1) as i64)
    }

    fn entry(subject: &str, from: &str) -> EmailEntry {
        EmailEntry {
            draft_sync: None,
            msg: Some(ref_for(subject)),
            draft_id: None,
            skip: None,
            selector: None,
            from: from.to_string(),
            to: "me@example.com".to_string(),
            cc: None,
            reply_to: None,
            bcc: None,
            subject: subject.to_string(),
            status: "inbox".to_string(),
            date_display: "2026-07-01".to_string(),
            date_sort: "2026-07-01T00:00:00".to_string(),
            has_attachments: false,
            read: false,
            answered: false,
            forwarded: false,
            flagged: false,
            is_invite: false,
            conv: None,
        }
    }

    fn sample() -> Vec<EmailEntry> {
        vec![
            entry("Invoice March", "Alice"),
            entry("Invoice April", "Bob"),
            entry("Weekly report", "Alice"),
            entry("Holiday plans", "Carol"),
        ]
    }

    // -----------------------------------------------------------------------
    // filter_visible (P2: visible-index mapping)
    // -----------------------------------------------------------------------

    #[test]
    fn empty_query_yields_all_indices_in_order() {
        let emails = sample();
        assert_eq!(
            filter_visible(&emails, "", MailboxKind::Inbox),
            vec![0, 1, 2, 3]
        );
    }

    #[test]
    fn filter_matches_subject_case_insensitively() {
        let emails = sample();
        assert_eq!(
            filter_visible(&emails, "INVOICE", MailboxKind::Inbox),
            vec![0, 1]
        );
    }

    #[test]
    fn filter_matches_contact_field() {
        let emails = sample();
        // Inbox displays `from`; Alice appears in entries 0 and 2.
        assert_eq!(
            filter_visible(&emails, "alice", MailboxKind::Inbox),
            vec![0, 2]
        );
    }

    /// #0088: the in-list metadata filter matches the sender even where the
    /// sender is not the *displayed* contact, which the old `/` metadata
    /// filter could not do (UX audit §b.5). In a Sent-style mailbox the list
    /// shows the recipient (`to`), yet a search on the sender still lands.
    #[test]
    fn filter_matches_the_sender_even_when_the_recipient_is_shown() {
        let mut outbound = entry("Quarterly numbers", "cfo@acme.example");
        outbound.to = "board@acme.example".to_string();
        let emails = vec![outbound];
        // Sent shows `to` (board@...), so the displayed contact never contains
        // "cfo"; the match can only come from the sender path.
        assert_eq!(filter_visible(&emails, "cfo", MailboxKind::Sent), vec![0]);
    }

    #[test]
    fn filter_indices_map_back_to_underlying_entries() {
        let emails = sample();
        let visible = filter_visible(&emails, "invoice", MailboxKind::Inbox);
        // "invoice" hits subjects 0 and 1.
        assert_eq!(visible, vec![0, 1]);
        // Selecting position 1 of the view must resolve to "Invoice April":
        // the actions layer operates on the underlying entry via this map.
        assert_eq!(emails[visible[1]].subject, "Invoice April");
    }

    // -----------------------------------------------------------------------
    // narrow_visible (P3: incremental search narrowing)
    // -----------------------------------------------------------------------

    #[test]
    fn narrowing_equals_full_recompute_on_append() {
        let emails = sample();
        // Simulate typing "inv" then "invo": narrow from the "inv" view.
        let mut visible = filter_visible(&emails, "inv", MailboxKind::Inbox);
        narrow_visible(&emails, &mut visible, "invo", MailboxKind::Inbox);
        assert_eq!(visible, filter_visible(&emails, "invo", MailboxKind::Inbox));
    }

    #[test]
    fn narrowing_removes_entries_that_stop_matching() {
        let emails = sample();
        let mut visible = filter_visible(&emails, "invoice", MailboxKind::Inbox);
        assert_eq!(visible, vec![0, 1]);
        narrow_visible(&emails, &mut visible, "invoice m", MailboxKind::Inbox);
        assert_eq!(visible, vec![0]);
        narrow_visible(&emails, &mut visible, "invoice mx", MailboxKind::Inbox);
        assert!(visible.is_empty());
    }

    #[test]
    fn narrowing_ignores_out_of_range_indices() {
        let emails = sample();
        // A stale index beyond the list must be dropped, not panic.
        let mut visible = vec![0, 99];
        narrow_visible(&emails, &mut visible, "invoice", MailboxKind::Inbox);
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
        emails.push(entry("θεοσφανια", "Dora"));
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

    /// A parked conversation-jump target lands on its row once the list holds
    /// it, and clears itself so a later switch is not hijacked (#0008).
    #[test]
    fn a_pending_select_lands_on_its_row_and_clears() {
        let mut app = app_with_emails(sample());
        let target = app.emails[2].msg.unwrap();
        app.list_index = 0;
        app.pending_select = Some(target);
        app.consume_pending_select();
        assert_eq!(app.list_index, 2, "the cursor moved onto the parked target");
        assert!(
            app.pending_select.is_none(),
            "the target cleared once it landed"
        );
    }

    /// A target the current list does not hold is left parked (the async load
    /// for a cross-mailbox jump has not arrived yet), not silently dropped.
    #[test]
    fn a_pending_select_absent_from_the_list_stays_parked() {
        let mut app = app_with_emails(sample());
        let absent = MessageRef::new(-999);
        app.list_index = 1;
        app.pending_select = Some(absent);
        app.consume_pending_select();
        assert_eq!(app.list_index, 1, "the cursor did not move");
        assert_eq!(
            app.pending_select,
            Some(absent),
            "the target is still parked"
        );
    }

    /// The list paging keys move by the rendered list height (half of it for
    /// Ctrl+d / Ctrl+u) and clamp at both ends of the list.
    #[test]
    fn list_paging_moves_by_the_viewport_height_and_clamps() {
        let emails = (0..25)
            .map(|i| entry(&format!("Message {i}"), "Alice"))
            .collect();
        let mut app = app_with_emails(emails);
        app.list_viewport_rows = 10;
        let ctrl = |c| KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL);

        app.handle_key(ctrl('d'));
        assert_eq!(app.list_index, 5, "half a 10-row page");
        app.handle_key(ctrl('d'));
        app.handle_key(ctrl('d'));
        app.handle_key(ctrl('d'));
        assert_eq!(app.list_index, 20);
        app.handle_key(ctrl('d'));
        assert_eq!(app.list_index, 24, "clamped to the last row");
        app.handle_key(ctrl('u'));
        assert_eq!(app.list_index, 19);
        for _ in 0..4 {
            app.handle_key(ctrl('u'));
        }
        assert_eq!(app.list_index, 0, "clamped to the first row");

        app.handle_key(KeyEvent::from(KeyCode::PageDown));
        assert_eq!(app.list_index, 10, "a whole page");
        app.handle_key(KeyEvent::from(KeyCode::PageDown));
        app.handle_key(KeyEvent::from(KeyCode::PageDown));
        assert_eq!(app.list_index, 24);
        app.handle_key(KeyEvent::from(KeyCode::PageUp));
        assert_eq!(app.list_index, 14);
        app.handle_key(KeyEvent::from(KeyCode::Home));
        assert_eq!(app.list_index, 0);
        app.handle_key(KeyEvent::from(KeyCode::End));
        assert_eq!(app.list_index, 24);
    }

    /// A Drafts row: no `messages` row behind it, its indexed `id:` instead
    /// (what `entry_from_draft` builds).
    fn draft_entry(id: &str, subject: &str) -> EmailEntry {
        EmailEntry {
            draft_sync: None,
            msg: None,
            draft_id: Some(id.to_string()),
            skip: None,
            selector: None,
            subject: subject.to_string(),
            status: "draft".to_string(),
            read: true,
            answered: false,
            forwarded: false,
            ..entry(subject, "me")
        }
    }

    /// A draft can enter the selection (#0052).
    ///
    /// It could not while the set was keyed on `MessageRef`: a draft has no
    /// store row, so `Ctrl+a` in Drafts selected nothing and `A` fell through
    /// to the single-draft path. The batch it guarded was reachable by
    /// keystroke and dead in fact.
    #[test]
    fn select_all_in_drafts_takes_the_draft_keys() {
        let mut app = app_with_emails(vec![draft_entry("aaa", "One"), draft_entry("bbb", "Two")]);

        app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL));

        assert_eq!(app.selection.len(), 2);
        let mut ids: Vec<&str> = app.selection.iter().filter_map(|k| k.draft()).collect();
        ids.sort_unstable();
        assert_eq!(ids, vec!["aaa", "bbb"]);
    }

    /// `v` toggles a Drafts row in and out of the selection like any other.
    #[test]
    fn toggling_selects_the_draft_under_the_cursor() {
        let mut app = app_with_emails(vec![draft_entry("aaa", "One"), draft_entry("bbb", "Two")]);

        app.handle_key(KeyEvent::from(KeyCode::Char('v')));
        assert_eq!(
            app.selection.iter().next().and_then(|k| k.draft()),
            Some("aaa")
        );
        // The cursor moved on; toggling the second row leaves both selected.
        app.handle_key(KeyEvent::from(KeyCode::Char('v')));
        assert_eq!(app.selection.len(), 2);
    }

    // The batch approve / mark-draft keys (`A` / `D`) were removed by the
    // #0092 keymap redesign: sending a single draft with `x` approves it, and
    // batch approve / send-all now live on the CLI (`mp mark-approved`,
    // `mp send-approved`). The batch-approve confirm path is therefore no
    // longer reachable from the TUI; the batch delete over `d` is, and still
    // takes only the messages half of a mixed selection.
    #[test]
    fn batch_delete_takes_only_the_messages_half_of_a_mixed_selection() {
        let mut app = app_with_emails(sample());
        let msg = app.emails[0].msg.unwrap();
        app.selection = std::collections::HashSet::from([
            EntryKey::Msg(msg),
            EntryKey::Draft("aaa".to_string()),
        ]);

        app.handle_key(KeyEvent::from(KeyCode::Char('d')));
        app.handle_key(KeyEvent::from(KeyCode::Char('y')));
        match app.pending_actions.pop_front() {
            Some(Action::BatchDelete(msgs)) => assert_eq!(msgs, vec![msg]),
            other => panic!("expected BatchDelete, got {other:?}"),
        }
    }

    /// `d` over a Drafts selection deletes the draft files, not store rows
    /// (#0073): a draft has no `messages` row, so the old `BatchDelete` half
    /// found nothing and reported "nothing to delete".
    #[test]
    fn delete_over_a_drafts_selection_takes_the_draft_ids() {
        let mut app = app_with_emails(vec![draft_entry("aaa", "One"), draft_entry("bbb", "Two")]);
        app.selection = std::collections::HashSet::from([
            EntryKey::Draft("aaa".to_string()),
            EntryKey::Draft("bbb".to_string()),
        ]);
        app.handle_key(KeyEvent::from(KeyCode::Char('d')));
        app.handle_key(KeyEvent::from(KeyCode::Char('y')));
        match app.pending_actions.pop_front() {
            Some(Action::BatchDeleteDrafts(mut ids)) => {
                ids.sort();
                assert_eq!(ids, vec!["aaa".to_string(), "bbb".to_string()]);
            }
            other => panic!("expected BatchDeleteDrafts, got {other:?}"),
        }
        assert!(app.selection.is_empty(), "the selection is consumed");
    }

    /// `y` queues the selector copy, not the dead path copy (#0050 scope item
    /// 7). Dispatch-level only, like the contacts copy test: the `arboard`
    /// call lives in `actions.rs` and would touch the real system clipboard.
    #[test]
    fn copy_key_queues_the_selector_copy() {
        let mut app = app_with_emails(sample());
        app.handle_key(KeyEvent::from(KeyCode::Char('y')));
        assert!(
            matches!(
                app.pending_actions.pop_front(),
                Some(Action::CopyMessageRef)
            ),
            "y must queue CopyMessageRef"
        );
    }

    // -----------------------------------------------------------------------
    // Mark-read on an explicit open (#0110, reversing #0087)
    // -----------------------------------------------------------------------

    fn queued_mark_reads(app: &App) -> usize {
        app.pending_actions
            .iter()
            .filter(|a| matches!(a, Action::MarkAsRead(_)))
            .count()
    }

    fn queued_mark_read_refs(app: &App) -> Vec<MessageRef> {
        app.pending_actions
            .iter()
            .filter_map(|a| match a {
                Action::MarkAsRead(msg) => Some(*msg),
                _ => None,
            })
            .collect()
    }

    /// `Tab` into the body pane is an explicit open and marks the message read
    /// (#0110). The intermediate stop on the headers pane is not: only the
    /// keypress that lands on `Focus::Preview` queues the mutation, and it
    /// queues exactly one.
    #[test]
    fn tab_into_the_body_pane_queues_one_mark_read() {
        let mut app = app_with_emails(sample());
        app.list_index = 0;
        assert_eq!(app.focus, Focus::List);

        app.handle_key(KeyEvent::from(KeyCode::Tab));
        assert_eq!(app.focus, Focus::Headers);
        assert_eq!(queued_mark_reads(&app), 0, "headers is not an open");

        app.handle_key(KeyEvent::from(KeyCode::Tab));
        assert_eq!(app.focus, Focus::Preview);
        assert_eq!(queued_mark_reads(&app), 1);
    }

    /// `Shift+Tab` reaches the body pane from the sidebar, and marks read there
    /// for the same reason (#0110).
    #[test]
    fn shift_tab_into_the_body_pane_queues_one_mark_read() {
        let mut app = app_with_emails(sample());
        app.list_index = 0;
        app.focus = Focus::Sidebar;

        app.handle_key(KeyEvent::from(KeyCode::BackTab));
        assert_eq!(app.focus, Focus::Preview);
        assert_eq!(queued_mark_reads(&app), 1);
    }

    /// An already-read row has nothing to write, so focusing the body pane on
    /// it queues nothing at all rather than a no-op action that would still
    /// cost a store open in `actions.rs` (#0110).
    #[test]
    fn focusing_the_body_pane_on_a_read_message_queues_nothing() {
        let mut read_row = entry("Read already", "Alice");
        read_row.read = true;
        let mut app = app_with_emails(vec![read_row]);
        app.list_index = 0;
        app.focus = Focus::Headers;

        app.handle_key(KeyEvent::from(KeyCode::Tab));
        assert_eq!(app.focus, Focus::Preview);
        assert_eq!(queued_mark_reads(&app), 0);
    }

    /// A Drafts row has no `messages` row behind it, so it can never be marked
    /// read however the body pane is reached (#0110).
    #[test]
    fn focusing_the_body_pane_on_a_draft_queues_nothing() {
        let mut app = app_with_emails(vec![draft_entry("aaa", "One")]);
        app.list_index = 0;
        app.focus = Focus::Headers;

        app.handle_key(KeyEvent::from(KeyCode::Tab));
        assert_eq!(app.focus, Focus::Preview);
        assert_eq!(queued_mark_reads(&app), 0);
    }

    /// The queued mark names the row that was opened, so a cursor move landing
    /// in the same coalesced batch (#0108) cannot redirect it: `Tab` then `J`
    /// still marks the row `Tab` opened, not the one the cursor ended on.
    #[test]
    fn a_queued_mark_read_survives_a_later_cursor_move() {
        let mut app = app_with_emails(sample());
        app.list_index = 0;
        app.focus = Focus::Headers;
        let opened = app.emails[0].msg.unwrap();

        app.handle_key(KeyEvent::from(KeyCode::Tab));
        assert_eq!(app.focus, Focus::Preview);
        app.handle_key(KeyEvent::from(KeyCode::Char('J')));
        assert_eq!(app.list_index, 1, "the cursor moved off the opened row");

        assert_eq!(
            queued_mark_read_refs(&app),
            vec![opened],
            "the mark must name the opened row, not the one under the cursor"
        );
    }

    /// `J` / `K` move the cursor without changing focus, so they are not an
    /// open and mark nothing (#0110, Decision D1). This is the whole point of
    /// the reversal: walking an unread inbox leaves it unread.
    #[test]
    fn next_message_queues_no_mark_read() {
        let mut app = app_with_emails(sample());
        app.list_index = 0;

        app.handle_key(KeyEvent::from(KeyCode::Char('J')));
        assert_eq!(app.list_index, 1);
        assert_eq!(app.focus, Focus::List, "J must not move focus");
        assert_eq!(queued_mark_reads(&app), 0);
    }

    /// Coming back to Mail from Contacts restores the saved focus, which may be
    /// the body pane. That restore is not an open (#0110): triggering there
    /// would re-mark a message the user had manually `u`-toggled to unread
    /// every time they left and returned.
    #[test]
    fn returning_to_the_mail_view_with_the_body_focused_queues_nothing() {
        let mut app = app_with_emails(sample());
        app.list_index = 0;
        app.focus = Focus::Preview;

        app.switch_view(View::Contacts);
        app.pending_actions.clear();
        app.switch_view(View::Mail);

        assert_eq!(app.focus, Focus::Preview);
        assert_eq!(queued_mark_reads(&app), 0);
    }

    // -----------------------------------------------------------------------
    // `Enter` reads, `e` opens `$EDITOR` (PERSO-92)
    // -----------------------------------------------------------------------

    fn queued_edits(app: &App) -> usize {
        app.pending_actions
            .iter()
            .filter(|a| matches!(a, Action::EditCurrent))
            .count()
    }

    /// `Enter` on a received row in the list moves the keyboard into the body
    /// pane, the same as the desktop client, and is the explicit open that
    /// marks it read. It no longer hands the terminal to `$EDITOR`.
    #[test]
    fn enter_on_a_received_row_focuses_the_body_pane() {
        let mut app = app_with_emails(sample());
        app.list_index = 0;
        assert_eq!(app.focus, Focus::List);

        app.handle_key(KeyEvent::from(KeyCode::Enter));

        assert_eq!(app.focus, Focus::Preview);
        assert_eq!(queued_edits(&app), 0, "Enter must not open $EDITOR");
        assert_eq!(queued_mark_reads(&app), 1, "Enter is an open (#0110)");
    }

    /// `gr` goes to the body pane and marks the message read once, `gl` comes
    /// back to the list, and `gm` reaches the sidebar: the desktop's three
    /// `g` pane jumps.
    #[test]
    fn g_prefix_jumps_to_the_reader_the_list_and_the_sidebar() {
        let mut app = app_with_emails(sample());
        app.terminal_width = 120;
        app.list_index = 0;
        let press = |app: &mut App, c: char| {
            app.handle_key(KeyEvent::from(KeyCode::Char(c)));
        };

        press(&mut app, 'g');
        press(&mut app, 'r');
        assert_eq!(app.focus, Focus::Preview);
        assert_eq!(app.pending_prefix, None);
        assert_eq!(
            queued_mark_reads(&app),
            1,
            "landing in the body is an open (#0110)"
        );
        assert_eq!(app.zoomed_pane(), None, "a wide terminal keeps the split");

        press(&mut app, 'g');
        press(&mut app, 'r');
        assert_eq!(
            queued_mark_reads(&app),
            1,
            "already reading: no second open"
        );

        press(&mut app, 'g');
        press(&mut app, 'l');
        assert_eq!(app.focus, Focus::List);

        press(&mut app, 'g');
        press(&mut app, 'm');
        assert_eq!(app.focus, Focus::Sidebar);
        press(&mut app, 'g');
        press(&mut app, 'l');
        assert_eq!(app.focus, Focus::List);
    }

    /// Under 80 columns `gr` zooms the body pane, as `Enter` does.
    #[test]
    fn gr_in_a_narrow_terminal_zooms_the_body_pane() {
        let mut app = app_with_emails(sample());
        app.terminal_width = 60;
        app.handle_key(KeyEvent::from(KeyCode::Char('g')));
        app.handle_key(KeyEvent::from(KeyCode::Char('r')));
        assert_eq!(app.focus, Focus::Preview);
        assert_eq!(app.zoomed_pane(), Some(Focus::Preview));
    }

    /// Under 80 columns the split draws no body pane, so `Enter` zooms it:
    /// focus must never land in a pane the user cannot see.
    #[test]
    fn enter_in_a_narrow_terminal_zooms_the_body_pane() {
        let mut app = app_with_emails(sample());
        app.terminal_width = 60;
        app.list_index = 0;

        app.handle_key(KeyEvent::from(KeyCode::Enter));

        assert_eq!(app.focus, Focus::Preview);
        assert_eq!(app.zoomed_pane(), Some(Focus::Preview));
        assert_eq!(queued_mark_reads(&app), 1);
    }

    /// At a width that draws the body pane, `Enter` only moves focus and
    /// leaves the split alone.
    #[test]
    fn enter_in_a_wide_terminal_keeps_the_split() {
        let mut app = app_with_emails(sample());
        app.terminal_width = 120;
        app.list_index = 0;

        app.handle_key(KeyEvent::from(KeyCode::Enter));

        assert_eq!(app.focus, Focus::Preview);
        assert_eq!(app.zoomed_pane(), None);
    }

    /// `Enter` from the headers pane lands in the body pane too, and a second
    /// `Enter` there is inert: nothing more is queued.
    #[test]
    fn enter_in_the_headers_and_body_panes_stays_on_the_body() {
        let mut app = app_with_emails(sample());
        app.list_index = 0;
        app.focus = Focus::Headers;

        app.handle_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.focus, Focus::Preview);
        let queued = app.pending_actions.len();

        app.handle_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.focus, Focus::Preview);
        assert_eq!(app.pending_actions.len(), queued, "a second Enter is inert");
        assert_eq!(queued_edits(&app), 0);
    }

    /// `e` on a received row opens its Markdown rendition in `$EDITOR`
    /// (#0075) through `Action::EditCurrent`, and leaves focus on the list.
    #[test]
    fn e_on_a_received_row_opens_the_editor() {
        let mut app = app_with_emails(sample());
        app.list_index = 0;

        app.handle_key(KeyEvent::from(KeyCode::Char('e')));

        assert_eq!(queued_edits(&app), 1, "e must queue EditCurrent");
        assert_eq!(app.focus, Focus::List, "e must not move focus");
    }

    /// `e` works from the body pane as well, so a message being read can be
    /// taken into the editor without a focus hop back to the list.
    #[test]
    fn e_in_the_body_pane_opens_the_editor() {
        let mut app = app_with_emails(sample());
        app.list_index = 0;
        app.focus = Focus::Preview;

        app.handle_key(KeyEvent::from(KeyCode::Char('e')));

        assert_eq!(queued_edits(&app), 1);
        assert_eq!(app.focus, Focus::Preview);
    }

    /// A draft keeps `e` = edit the draft, and `Enter` reads it in the body
    /// pane like any other row (the desktop client does the same).
    #[test]
    fn a_draft_keeps_e_for_editing_and_enter_reads_it() {
        let mut app = app_with_emails(vec![draft_entry("aaa", "One")]);
        app.list_index = 0;

        app.handle_key(KeyEvent::from(KeyCode::Char('e')));
        assert_eq!(queued_edits(&app), 1, "e must still edit a draft");
        app.pending_actions.clear();

        app.handle_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.focus, Focus::Preview);
        assert_eq!(queued_edits(&app), 0);
        assert_eq!(queued_mark_reads(&app), 0, "a draft is never marked read");
    }

    #[test]
    fn selected_email_resolves_through_visible_indices() {
        let mut app = app_with_emails(sample());
        app.search_query = "invoice".to_string();
        app.apply_search_filter(false);
        // View: [Invoice March, Invoice April]
        app.list_index = 1;
        assert_eq!(app.selected_email().unwrap().subject, "Invoice April");
    }

    #[test]
    fn remove_selected_maps_back_to_underlying_entry_under_filter() {
        let mut app = app_with_emails(sample());
        app.search_query = "invoice".to_string();
        app.apply_search_filter(false);
        // View: [Invoice March, Invoice April].
        app.list_index = 1; // "Invoice April"

        let removed = app.remove_selected_from_list().unwrap();
        assert_eq!(removed, ref_for("Invoice April"));
        // The underlying list lost exactly that entry...
        assert_eq!(app.emails.len(), 3);
        assert!(app.emails.iter().all(|e| e.subject != "Invoice April"));
        // ...the cache slot shares the same (updated) allocation...
        let cached = app.email_cache[0].as_ref().unwrap();
        assert!(std::sync::Arc::ptr_eq(cached, &app.emails));
        // ...and the filtered view is rebuilt with the cursor clamped onto the
        // one remaining "invoice" hit ("Invoice March").
        assert_eq!(app.visible, vec![0]);
        assert_eq!(app.list_index, 0);
    }

    #[test]
    fn set_email_read_updates_shared_cache_without_deep_clone() {
        let mut app = app_with_emails(sample());
        let msg = app.emails[1].msg.unwrap();
        app.set_email_read(msg, true);
        assert!(app.emails[1].read);
        let cached = app.email_cache[0].as_ref().unwrap();
        assert!(std::sync::Arc::ptr_eq(cached, &app.emails));
        assert!(cached[1].read);
    }

    #[test]
    fn with_emails_mut_leaves_invalidated_cache_slot_none() {
        let mut app = app_with_emails(sample());
        app.email_cache[0] = None; // e.g. load in flight
        let msg = app.emails[0].msg.unwrap();
        app.set_email_read(msg, true);
        assert!(app.emails[0].read);
        assert!(app.email_cache[0].is_none());
    }

    // -----------------------------------------------------------------------
    // Cursor stability across list rebuilds
    //
    // `list_index` is a bare position into `visible`, so any reload that
    // re-sorts or grows the entry list used to move the cursor to a
    // different email (approving a draft re-sorted the list; new inbox mail
    // shifted every row down under queued keystrokes). The cursor is
    // anchored on the entry's `MessageRef` and restored after the rebuild.
    // -----------------------------------------------------------------------

    /// Deliver a fresh entry list through the real async funnel
    /// (`BgResult::MailboxLoaded`), the single path every reload takes.
    fn deliver_mailbox_load(app: &mut App, entries: Vec<EmailEntry>) {
        crate::bg::handle_bg_result(
            app,
            super::super::BgResult::MailboxLoaded {
                account_index: app.active_account,
                mailbox_idx: app.active_mailbox,
                generation: app.mailbox_load_generation,
                entries,
            },
        );
    }

    #[test]
    fn cursor_follows_its_email_when_the_reload_resorts_the_list() {
        let mut app = app_with_emails(sample());
        app.list_index = 1;
        let anchored = app.selected_email().unwrap().msg;

        // The approved draft's status/date changed, so the reload sorts it
        // last instead of second.
        let mut resorted = sample();
        let moved = resorted.remove(1);
        resorted.push(moved);
        deliver_mailbox_load(&mut app, resorted);

        assert_eq!(app.list_index, 3);
        assert_eq!(app.selected_email().unwrap().msg, anchored);
    }

    #[test]
    fn cursor_stays_put_when_new_mail_is_prepended() {
        let mut app = app_with_emails(sample());
        app.list_index = 2;
        let anchored = app.selected_email().unwrap().msg;

        // New inbox mail sorts above everything and shifts every row down.
        let mut grown = vec![entry("Fresh arrival", "Dave")];
        grown.extend(sample());
        deliver_mailbox_load(&mut app, grown);

        assert_eq!(app.list_index, 3);
        assert_eq!(app.selected_email().unwrap().msg, anchored);
    }

    #[test]
    fn cursor_falls_back_to_the_clamped_index_when_its_email_is_gone() {
        let mut app = app_with_emails(sample());
        app.list_index = 3; // "Holiday plans"

        // The reload lost the anchored email (archived from another client)
        // and is shorter than the old cursor position.
        let shorter = vec![sample()[0].clone(), sample()[1].clone()];
        deliver_mailbox_load(&mut app, shorter);

        assert_eq!(app.list_index, 1);
        assert_eq!(app.selected_email().unwrap().subject, "Invoice April");
    }

    #[test]
    fn empty_reload_leaves_the_cursor_at_zero() {
        let mut app = app_with_emails(sample());
        app.list_index = 3;
        deliver_mailbox_load(&mut app, Vec::new());
        assert_eq!(app.list_index, 0);
        assert!(app.selected_email().is_none());
    }

    #[test]
    fn batch_removal_above_the_cursor_does_not_drag_it() {
        // Six rows, cursor in the middle: with only four survivors the old
        // clamp (`min(list_index, len - 1)`) would have left the cursor at
        // row 3, two emails below the one the user was looking at.
        let mut emails = sample();
        emails.push(entry("Team sync", "Dave"));
        emails.push(entry("Renewal notice", "Eve"));
        let mut app = app_with_emails(emails);
        app.list_index = 3; // "Holiday plans"
        let anchored = app.selected_email().unwrap().msg;

        // Archive the two rows above the cursor: the cursor's own email
        // survives, so it must stay under the cursor (at its new position).
        let mut batch = std::collections::HashSet::new();
        batch.insert(app.emails[0].msg.unwrap());
        batch.insert(app.emails[1].msg.unwrap());
        let removed = app.remove_selected_from_list_batch(&batch);

        assert_eq!(removed.len(), 2);
        assert_eq!(app.list_index, 1);
        assert_eq!(app.selected_email().unwrap().msg, anchored);
    }

    #[test]
    fn batch_removal_including_the_cursor_lands_on_the_next_survivor() {
        let mut emails = sample();
        emails.push(entry("Team sync", "Dave"));
        emails.push(entry("Renewal notice", "Eve"));
        let mut app = app_with_emails(emails);
        app.list_index = 3; // "Holiday plans"

        // The cursor's own row is part of the batch, together with two rows
        // above it: the cursor falls back to the count of survivors above
        // it, i.e. the row that took its place.
        let mut batch = std::collections::HashSet::new();
        batch.insert(app.emails[0].msg.unwrap());
        batch.insert(app.emails[1].msg.unwrap());
        batch.insert(app.emails[3].msg.unwrap());
        app.remove_selected_from_list_batch(&batch);

        assert_eq!(app.list_index, 1);
        assert_eq!(app.selected_email().unwrap().subject, "Team sync");
    }

    /// A deleted row's `MessageRef` must not survive in the selection.
    ///
    /// The id of a deleted row is not reserved: the next ingest is handed the
    /// same number (pinned by `store::write`'s
    /// `a_deleted_row_id_can_be_handed_to_the_next_message`), so a reference
    /// held across the boundary can name a *different* message. The list drops
    /// it, and so must the selection set (#0038 scope item 7).
    #[test]
    fn removing_a_row_drops_its_reference_from_the_selection() {
        let mut app = app_with_emails(sample());
        app.list_index = 2;
        let doomed = app.selected_email().unwrap().msg.unwrap();
        let survivor = app.emails[0].msg.unwrap();
        app.selection.insert(EntryKey::Msg(doomed));
        app.selection.insert(EntryKey::Msg(survivor));

        app.remove_selected_from_list();

        assert!(
            !app.selection.contains(&EntryKey::Msg(doomed)),
            "the selection kept a reference to a row that no longer exists"
        );
        assert!(app.selection.contains(&EntryKey::Msg(survivor)));
        assert!(app
            .cursor_anchor()
            .is_none_or(|a| a != crate::app::CursorAnchor::Row(doomed)));
    }

    /// Same guarantee for a batch: every removed reference leaves the
    /// selection, so a follow-up mutation cannot act on a freed row id.
    #[test]
    fn batch_removal_drops_every_removed_reference_from_the_selection() {
        let mut app = app_with_emails(sample());
        let batch: std::collections::HashSet<MessageRef> =
            app.emails[..2].iter().filter_map(|e| e.msg).collect();
        app.selection = app.emails.iter().filter_map(|e| e.key()).collect();

        app.remove_selected_from_list_batch(&batch);

        assert_eq!(app.selection.len(), 2);
        assert!(app
            .selection
            .iter()
            .all(|k| !k.msg().is_some_and(|m| batch.contains(&m))));
        assert!(app
            .emails
            .iter()
            .all(|e| !e.msg.is_some_and(|m| batch.contains(&m))));
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
            purpose: PickerPurpose::Move(vec![MessageRef::new(1)]),
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

    fn mb_info(
        label: &str,
        id: &str,
        kind: MailboxKind,
        server: Option<&str>,
    ) -> super::super::MailboxInfo {
        super::super::MailboxInfo {
            label: label.to_string(),
            icon: "",
            id: id.to_string(),
            kind,
            server_name: server.map(|s| s.to_string()),
        }
    }

    fn app_with_mailboxes() -> App {
        let mut app = app_with_emails(sample());
        app.mailboxes = vec![
            mb_info("Inbox", "inbox", MailboxKind::Inbox, Some("INBOX")),
            mb_info("Drafts", "drafts", MailboxKind::Drafts, None),
            mb_info("Sent", "sent", MailboxKind::Sent, Some("Sent")),
            mb_info("Archive", "archive", MailboxKind::Archive, Some("Archive")),
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
        let labels: Vec<&str> = picker.candidates.iter().map(|(_, l)| l.as_str()).collect();
        assert_eq!(labels, vec!["Sent", "Archive"]);
        // Cursor email is carried as the move target.
        assert_eq!(picker.msgs().len(), 1);
    }

    /// `gf` lists every mailbox of the sidebar, a discovered folder past the
    /// nine number keys included, and Enter opens the one picked rather than
    /// moving anything (#0141).
    #[test]
    fn the_folder_picker_opens_any_mailbox_by_name() {
        let mut app = app_with_mailboxes();
        for name in [
            "Spesen",
            "Archive1",
            "Read",
            "Scheduled",
            "Deleted Items",
            "Labels/AI",
        ] {
            app.mailboxes
                .push(mb_info(name, name, MailboxKind::Extra, Some(name)));
        }
        app.email_cache = vec![None; app.mailboxes.len()];
        app.mailbox_counts = vec![0; app.mailboxes.len()];
        assert!(app.mailboxes.len() > 9, "past the number keys");

        app.handle_key(KeyEvent::from(KeyCode::Char('g')));
        app.handle_key(KeyEvent::from(KeyCode::Char('f')));
        let Overlay::Mailbox(picker) = &app.overlay else {
            panic!("gf opens the folder picker");
        };
        assert_eq!(
            picker.candidates.len(),
            app.mailboxes.len(),
            "every mailbox"
        );
        assert!(picker.msgs().is_empty(), "nothing to move");

        for c in "labai".chars() {
            app.handle_key(KeyEvent::from(KeyCode::Char(c)));
        }
        app.handle_key(KeyEvent::from(KeyCode::Enter));
        assert!(matches!(app.overlay, Overlay::None));
        assert_eq!(app.mailboxes[app.active_mailbox].id, "Labels/AI");
        assert_eq!(app.focus, Focus::List);
        assert!(
            !app.pending_actions
                .iter()
                .any(|a| matches!(a, Action::MoveToMailbox { .. })),
            "a jump moves nothing"
        );
    }

    #[test]
    fn open_picker_uses_selection_when_present() {
        let mut app = app_with_mailboxes();
        app.selection
            .insert(EntryKey::Msg(app.emails[0].msg.unwrap()));
        app.selection
            .insert(EntryKey::Msg(app.emails[2].msg.unwrap()));
        app.handle_key(KeyEvent::from(KeyCode::Char('M')));
        let Overlay::Mailbox(picker) = &app.overlay else {
            panic!("picker should open");
        };
        assert_eq!(picker.msgs().len(), 2);
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
            Some(Action::MoveToMailbox { msgs, dest_idx }) => {
                assert_eq!(dest_idx, 3); // Archive
                assert_eq!(msgs.len(), 1);
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

    fn press(app: &mut App, c: char) {
        app.handle_key(KeyEvent::from(KeyCode::Char(c)));
    }

    fn type_text(app: &mut App, text: &str) {
        for c in text.chars() {
            press(app, c);
        }
    }

    // -----------------------------------------------------------------------
    // Command palette (#0100)
    // -----------------------------------------------------------------------

    /// `:` and `Ctrl+p` both open the command palette over the runnable
    /// KeyAction catalogue, and Esc closes it without side effects.
    #[test]
    fn command_palette_opens_on_colon_and_ctrl_p() {
        let mut app = app_with_emails(sample());
        app.handle_key(KeyEvent::from(KeyCode::Char(':')));
        let Overlay::Palette(palette) = &app.overlay else {
            panic!("`:` should open the palette");
        };
        assert!(!palette.entries.is_empty(), "the catalogue is non-empty");
        assert_eq!(
            palette.filtered.len(),
            palette.entries.len(),
            "an empty query matches every entry"
        );

        app.handle_key(KeyEvent::from(KeyCode::Esc));
        assert!(matches!(app.overlay, Overlay::None));
        assert!(app.pending_actions.is_empty());

        app.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));
        assert!(
            matches!(app.overlay, Overlay::Palette(_)),
            "Ctrl+p opens it too"
        );
    }

    /// A family leader typed into the palette is literal text (it filters, it
    /// does not arm the leader), consistent with the search prompt.
    #[test]
    fn command_palette_treats_family_leaders_as_literal_text() {
        let mut app = app_with_emails(sample());
        app.handle_key(KeyEvent::from(KeyCode::Char(':')));
        app.handle_key(KeyEvent::from(KeyCode::Char('f')));
        assert!(
            app.pending_prefix.is_none(),
            "`f` must not arm a leader here"
        );
        let Overlay::Palette(palette) = &app.overlay else {
            panic!("palette should still be open");
        };
        assert_eq!(palette.query, "f");
    }

    /// The palette runs executors without the key dispatch's guard checks
    /// (#0100 review finding): running a `Guard::NonEmptyList` action by name
    /// on an empty mailbox must not underflow the cursor arithmetic.
    #[test]
    fn command_palette_runs_a_list_action_on_an_empty_list_without_panicking() {
        let mut app = app_with_emails(Vec::new());
        assert!(app.visible.is_empty(), "the mailbox must start empty");
        app.handle_key(KeyEvent::from(KeyCode::Char(':')));
        for c in "navigate emails".chars() {
            app.handle_key(KeyEvent::from(KeyCode::Char(c)));
        }
        app.handle_key(KeyEvent::from(KeyCode::Enter));

        assert!(matches!(app.overlay, Overlay::None), "the palette closed");
        assert_eq!(app.list_index, 0, "the cursor stays put on an empty list");
    }

    /// Typing filters the catalogue, and Enter closes the palette and runs the
    /// selected action against the current context.
    #[test]
    fn command_palette_filters_and_runs_the_selected_action() {
        let mut app = app_with_emails(sample());
        app.handle_key(KeyEvent::from(KeyCode::Char(':')));
        for c in "toggle this help".chars() {
            app.handle_key(KeyEvent::from(KeyCode::Char(c)));
        }
        let Overlay::Palette(palette) = &app.overlay else {
            panic!("palette should be open");
        };
        assert_eq!(
            palette.selected_action(),
            Some(crate::app::KeyAction::ToggleHelp),
            "the query narrowed to the one help action"
        );

        app.handle_key(KeyEvent::from(KeyCode::Enter));
        // The palette closed and the action ran in the current context: the
        // help overlay is now up.
        assert!(matches!(app.overlay, Overlay::Help));
    }

    /// The scorer gives what cmdk's `command-score` gives the desktop palette,
    /// to the last digit (values from cmdk 1.1's dist/command-score.mjs).
    #[test]
    fn command_score_matches_cmdk() {
        for (value, search, want) in [
            ("GLOBAL Toggle this help ?", "help", 0.891),
            (
                "MESSAGE (list, headers, body) Archive a",
                "arc",
                0.8909109000000001,
            ),
            ("EMAIL LIST Jump to top gg", "gg", 0.9),
            ("APP Add account", "Add account", 0.9),
            ("SIDEBAR Next mailbox j", "nxt", 0.151303398147),
            ("a-b_c d", "bd", 0.81),
            ("Archive", "ahcrive", 0.0),
            ("foo.bar/baz", "fbb", 0.6336),
            ("GLOBAL Quit q", "QUIT", 0.8907327267291091),
            ("abc", "abcd", 0.0),
        ] {
            let got = command_score(value, search);
            assert!(
                (got - want).abs() < 1e-12,
                "{search:?} in {value:?}: {got}, cmdk gives {want}"
            );
        }
    }

    /// The palette lists the best match first, as the desktop palette does:
    /// "Forward" puts the Forward action above "Cycle focus forward", which
    /// comes first in the keymap, so Enter forwards.
    #[test]
    fn command_palette_ranks_the_best_match_first() {
        let mut app = app_with_emails(sample());
        app.handle_key(KeyEvent::from(KeyCode::Char(':')));
        for c in "Forward".chars() {
            app.handle_key(KeyEvent::from(KeyCode::Char(c)));
        }
        let Overlay::Palette(palette) = &app.overlay else {
            panic!("palette should be open");
        };
        let labels: Vec<&str> = palette
            .filtered
            .iter()
            .map(|&i| palette.entries[i].label)
            .collect();
        assert_eq!(labels.first(), Some(&"Forward"), "ranked: {labels:?}");
        assert!(
            labels.contains(&"Cycle focus forward"),
            "the weaker match stays listed: {labels:?}"
        );

        // The section and the keys match too, as on the desktop.
        app.handle_key(KeyEvent::from(KeyCode::Esc));
        app.handle_key(KeyEvent::from(KeyCode::Char(':')));
        for c in "cf".chars() {
            app.handle_key(KeyEvent::from(KeyCode::Char(c)));
        }
        let Overlay::Palette(palette) = &app.overlay else {
            panic!("palette should be open");
        };
        assert!(
            palette
                .filtered
                .iter()
                .any(|&i| palette.entries[i].label == "Forward"),
            "its key cf finds Forward"
        );
    }

    /// Enter with no match (an over-narrow query) is a no-op: nothing runs and
    /// the palette stays open.
    #[test]
    fn command_palette_enter_on_no_match_is_noop() {
        let mut app = app_with_emails(sample());
        app.handle_key(KeyEvent::from(KeyCode::Char(':')));
        for c in "zzzznope".chars() {
            app.handle_key(KeyEvent::from(KeyCode::Char(c)));
        }
        let Overlay::Palette(palette) = &app.overlay else {
            panic!("palette should be open");
        };
        assert!(palette.filtered.is_empty());
        app.handle_key(KeyEvent::from(KeyCode::Enter));
        assert!(matches!(app.overlay, Overlay::Palette(_)), "stays open");
        assert!(app.pending_actions.is_empty());
    }

    // -----------------------------------------------------------------------
    // Account picker (`ga`, #0147)
    // -----------------------------------------------------------------------

    /// An app on `work`, the first of `names`, with every name an account.
    fn app_with_accounts(names: &[&str]) -> App {
        let mut app = app_with_emails(sample());
        app.account_config.name = names[0].to_string();
        app.accounts = names
            .iter()
            .map(|n| crate::app::AccountState::for_tests(n))
            .collect();
        app.active_account = 0;
        app
    }

    fn picker_cursor(app: &App) -> usize {
        let Overlay::Account(picker) = &app.overlay else {
            panic!("the account picker should be open");
        };
        picker.selected
    }

    fn key(app: &mut App, code: KeyCode) {
        app.handle_key(KeyEvent::from(code));
    }

    #[test]
    fn ga_opens_the_account_picker_on_the_active_account() {
        let mut app = app_with_accounts(&["work", "home", "club"]);
        app.active_account = 1;
        type_text(&mut app, "ga");
        assert_eq!(picker_cursor(&app), 1);
        assert_eq!(app.active_account, 1, "opening the picker switches nothing");
        assert!(app.pending_prefix.is_none());
    }

    #[test]
    fn account_picker_j_and_enter_switch_and_k_clamps_at_the_top() {
        let mut app = app_with_accounts(&["work", "home", "club"]);
        type_text(&mut app, "ga");
        press(&mut app, 'k');
        assert_eq!(picker_cursor(&app), 0, "k at the top stays at the top");
        press(&mut app, 'j');
        key(&mut app, KeyCode::Down);
        press(&mut app, 'j');
        assert_eq!(
            picker_cursor(&app),
            2,
            "j at the bottom stays at the bottom"
        );
        key(&mut app, KeyCode::Up);
        key(&mut app, KeyCode::Enter);
        assert!(matches!(app.overlay, Overlay::None));
        assert_eq!(app.active_account, 1);
        assert_eq!(app.account_config.name, "home");
    }

    #[test]
    fn account_picker_esc_and_q_close_it_unchanged() {
        let mut app = app_with_accounts(&["work", "home", "club"]);
        for close in [KeyCode::Esc, KeyCode::Char('q')] {
            type_text(&mut app, "ga");
            press(&mut app, 'j');
            key(&mut app, close);
            assert!(matches!(app.overlay, Overlay::None), "{close:?} closes it");
            assert_eq!(app.active_account, 0, "{close:?} switches nothing");
        }
    }

    #[test]
    fn account_picker_digits_jump_and_a_digit_past_the_end_does_nothing() {
        let mut app = app_with_accounts(&["work", "home", "club"]);
        type_text(&mut app, "ga");
        press(&mut app, '9');
        assert_eq!(picker_cursor(&app), 0, "the picker stays open");
        assert_eq!(app.active_account, 0);
        press(&mut app, '3');
        assert!(matches!(app.overlay, Overlay::None));
        assert_eq!(app.active_account, 2);
        assert_eq!(app.account_config.name, "club");
    }

    #[test]
    fn account_picker_enter_on_the_active_account_only_closes_it() {
        let mut app = app_with_accounts(&["work", "home"]);
        app.search_query = "invoice".to_string();
        type_text(&mut app, "ga");
        key(&mut app, KeyCode::Enter);
        assert!(matches!(app.overlay, Overlay::None));
        assert_eq!(app.active_account, 0);
        assert_eq!(app.search_query, "invoice", "no switch ran");
    }

    #[test]
    fn account_picker_clears_the_new_accounts_unseen_dot() {
        let mut app = app_with_accounts(&["work", "home"]);
        app.accounts[1].has_unseen = true;
        type_text(&mut app, "ga");
        press(&mut app, '2');
        assert!(!app.accounts[1].has_unseen);
    }

    #[test]
    fn with_one_account_ga_and_the_palette_open_nothing_and_the_popup_omits_ga() {
        let mut app = app_with_accounts(&["work"]);
        press(&mut app, 'g');
        let rows = app.prefix_popup_rows();
        assert!(!rows.is_empty(), "the g family still shows");
        assert!(rows.iter().all(|(k, _)| *k != "ga"), "{rows:?}");
        press(&mut app, 'a');
        assert!(matches!(app.overlay, Overlay::None));
        assert!(app.pending_prefix.is_none());

        app.handle_key(KeyEvent::from(KeyCode::Char(':')));
        type_text(&mut app, "switch account");
        let Overlay::Palette(palette) = &app.overlay else {
            panic!("palette should be open");
        };
        assert_eq!(
            palette.selected_action(),
            Some(crate::app::KeyAction::SwitchAccount)
        );
        key(&mut app, KeyCode::Enter);
        assert!(matches!(app.overlay, Overlay::None), "nothing opened");
    }

    #[test]
    fn with_two_accounts_the_popup_lists_ga_and_the_palette_opens_the_picker() {
        let mut app = app_with_accounts(&["work", "home"]);
        press(&mut app, 'g');
        assert!(app.prefix_popup_rows().iter().any(|(k, _)| *k == "ga"));
        key(&mut app, KeyCode::Esc);
        app.handle_key(KeyEvent::from(KeyCode::Char(':')));
        type_text(&mut app, "switch account");
        key(&mut app, KeyCode::Enter);
        assert_eq!(picker_cursor(&app), 0);
    }

    // -----------------------------------------------------------------------
    // Flagged-only view (#0079)
    // -----------------------------------------------------------------------

    /// `F` narrows the list to flagged rows and widens it back, keeping the
    /// cursor on the row it was on when that row survives the narrowing.
    #[test]
    fn f_toggles_the_flagged_only_view() {
        let mut app = app_with_emails(sample());
        let mut emails = (*app.emails).clone();
        emails[1].flagged = true;
        app.emails = std::sync::Arc::new(emails);
        app.email_cache = vec![Some(std::sync::Arc::clone(&app.emails))];
        app.rebuild_visible();
        app.list_index = 1;

        // `fF` (the find family's flagged-only filter, #0092).
        app.handle_key(KeyEvent::from(KeyCode::Char('f')));
        app.handle_key(KeyEvent::from(KeyCode::Char('F')));

        assert!(app.flagged_only);
        assert_eq!(app.visible, vec![1]);
        assert_eq!(app.list_index, 0, "cursor followed its row");

        app.handle_key(KeyEvent::from(KeyCode::Char('f')));
        app.handle_key(KeyEvent::from(KeyCode::Char('F')));

        assert!(!app.flagged_only);
        assert_eq!(app.visible, vec![0, 1, 2, 3]);
        assert_eq!(app.list_index, 1, "cursor still on the same row");
    }

    /// The flagged view and the `/` search are independent narrowings: what is
    /// visible is the intersection, whichever order they were armed in.
    #[test]
    fn the_flagged_view_intersects_with_the_search_filter() {
        let mut app = app_with_emails(sample());
        let mut emails = (*app.emails).clone();
        for e in emails.iter_mut() {
            e.flagged = true;
        }
        emails[3].flagged = false;
        app.emails = std::sync::Arc::new(emails);
        app.email_cache = vec![Some(std::sync::Arc::clone(&app.emails))];
        app.rebuild_visible();

        app.handle_key(KeyEvent::from(KeyCode::Char('f')));
        app.handle_key(KeyEvent::from(KeyCode::Char('F')));
        assert_eq!(app.visible, vec![0, 1, 2]);

        // The query that alone would match only the one unflagged row.
        app.search_query = "holiday".to_string();
        app.apply_search_filter(false);
        assert!(app.visible.is_empty(), "flagged view still applies");

        app.handle_key(KeyEvent::from(KeyCode::Char('f')));
        app.handle_key(KeyEvent::from(KeyCode::Char('F')));
        assert_eq!(app.visible, vec![3], "search alone again");
    }

    /// The toggle is not guarded by a non-empty list: a filter that emptied
    /// the list must still be undoable with the key that armed it.
    #[test]
    fn the_flagged_view_can_be_left_when_it_shows_nothing() {
        let mut app = app_with_emails(sample());

        app.handle_key(KeyEvent::from(KeyCode::Char('f')));
        app.handle_key(KeyEvent::from(KeyCode::Char('F')));
        assert!(app.visible.is_empty(), "no flagged rows in the fixture");

        app.handle_key(KeyEvent::from(KeyCode::Char('f')));
        app.handle_key(KeyEvent::from(KeyCode::Char('F')));
        assert!(!app.flagged_only);
        assert_eq!(app.visible, vec![0, 1, 2, 3]);
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
            body: String::new(),
            focus: ComposeField::To,
            signature_name: None,
            signature_initial: None,
            signature_edited: false,
            available_signatures: Vec::new(),
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

    /// Tab order in a New compose reaches the inline body field (#0097) right
    /// after Subject, and BackTab from To lands back on it.
    #[test]
    fn compose_wizard_tab_reaches_the_body_field() {
        let mut app = app_with_emails(sample());
        open_compose_wizard_for_test(&mut app);
        app.compose_wizard_mut().unwrap().focus = ComposeField::Subject;

        // Subject -> Signature -> Body (#0106 inserts the Signature field).
        app.handle_key(KeyEvent::from(KeyCode::Tab));
        assert_eq!(app.compose_wizard().unwrap().focus, ComposeField::Signature);
        app.handle_key(KeyEvent::from(KeyCode::Tab));
        assert_eq!(app.compose_wizard().unwrap().focus, ComposeField::Body);

        // One more Tab wraps back to the first field.
        app.handle_key(KeyEvent::from(KeyCode::Tab));
        assert_eq!(app.compose_wizard().unwrap().focus, ComposeField::To);

        // BackTab from To lands on Body (the last field).
        app.handle_key(KeyEvent::from(KeyCode::BackTab));
        assert_eq!(app.compose_wizard().unwrap().focus, ComposeField::Body);
    }

    /// On the multi-line body field Enter inserts a newline rather than
    /// submitting (#0097); typed characters accumulate into the body.
    #[test]
    fn compose_wizard_enter_on_body_inserts_a_newline() {
        let mut app = app_with_emails(sample());
        open_compose_wizard_for_test(&mut app);
        app.compose_wizard_mut().unwrap().focus = ComposeField::Body;

        for c in "hi".chars() {
            app.handle_key(KeyEvent::from(KeyCode::Char(c)));
        }
        app.handle_key(KeyEvent::from(KeyCode::Enter));
        app.handle_key(KeyEvent::from(KeyCode::Char('x')));

        assert_eq!(app.compose_wizard().unwrap().body, "hi\nx");
        // Enter did not queue a submit and the wizard is still open.
        assert!(matches!(app.overlay, Overlay::Compose(_)));
        assert!(
            !app.pending_actions
                .iter()
                .any(|a| matches!(a, Action::ComposeWizardSubmit)),
            "Enter on the body must not submit"
        );
    }

    /// Ctrl+g is the submit key from the body field (#0097): it queues the
    /// submit action from anywhere in the wizard.
    #[test]
    fn compose_wizard_ctrl_g_submits_from_the_body() {
        let mut app = app_with_emails(sample());
        open_compose_wizard_for_test(&mut app);
        app.compose_wizard_mut().unwrap().focus = ComposeField::Body;

        app.handle_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL));
        assert!(
            app.pending_actions
                .iter()
                .any(|a| matches!(a, Action::ComposeWizardSubmit)),
            "Ctrl+g must submit from the body field"
        );
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

        // Picker (and its carried messages) survive; the error is queued.
        let Overlay::Mailbox(picker) = &app.overlay else {
            panic!("picker must not be clobbered by a background error");
        };
        assert_eq!(picker.msgs().len(), 1);
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
            inline_from: 1,
            selected: 0,
            mode: AttachmentPickerMode::Save,
            selected_set: std::collections::HashSet::new(),
        });
        app.pending_error = Some(PersistentError {
            message: "bg boom".to_string(),
        });

        // Enter triggers the attachment -> dir picker handoff.
        app.handle_key(KeyEvent::from(KeyCode::Enter));

        // The dir picker is now active; the error did NOT fire in between.
        assert!(matches!(app.overlay, Overlay::Dir(_)));
        assert!(app.pending_error.is_some());
    }

    // -----------------------------------------------------------------------
    // Multi-view: leader switching, per-view key gating (#0033)
    // -----------------------------------------------------------------------

    use super::super::View;

    /// `Space c` / `Space a` / `Space m` switch the active top-level view; the
    /// leader is consumed and `App::view` updates. Space alone only arms the
    /// leader (#0033 follow-up: Space is the view leader).
    #[test]
    fn leader_switches_top_level_view() {
        let mut app = app_with_mailboxes();
        assert_eq!(app.view, View::Mail);

        // Bare Space arms the leader without switching.
        app.handle_key(KeyEvent::from(KeyCode::Char(' ')));
        assert_eq!(app.pending_prefix, Some(' '), "Space must arm the leader");
        assert_eq!(app.view, View::Mail);

        // `Space c` -> Contacts.
        app.handle_key(KeyEvent::from(KeyCode::Char('c')));
        assert_eq!(app.view, View::Contacts);
        assert_eq!(app.pending_prefix, None, "leader consumed by the switch");

        // `Space a` -> Calendar (proving Space arms from a non-Mail view).
        app.handle_key(KeyEvent::from(KeyCode::Char(' ')));
        app.handle_key(KeyEvent::from(KeyCode::Char('a')));
        assert_eq!(app.view, View::Calendar);

        // `Space m` -> back to Mail.
        app.handle_key(KeyEvent::from(KeyCode::Char(' ')));
        app.handle_key(KeyEvent::from(KeyCode::Char('m')));
        assert_eq!(app.view, View::Mail);
    }

    /// `v` toggles list selection (the binding that moved off Space, #0033
    /// follow-up). Space no longer touches selection.
    #[test]
    fn v_toggles_list_selection_and_space_does_not() {
        let mut app = app_with_mailboxes();
        app.focus = Focus::List;
        assert!(
            !app.visible.is_empty(),
            "fixture must have selectable emails"
        );
        let before = app.selection.len();
        app.handle_key(KeyEvent::from(KeyCode::Char('v')));
        assert_eq!(
            app.selection.len(),
            before + 1,
            "v must add to the selection"
        );

        // Space with a non-empty list arms the view leader instead of toggling.
        let sel_after_v = app.selection.len();
        app.handle_key(KeyEvent::from(KeyCode::Char(' ')));
        assert_eq!(app.pending_prefix, Some(' '), "Space arms the view leader");
        assert_eq!(
            app.selection.len(),
            sel_after_v,
            "Space must not change the selection"
        );
    }

    /// `g` arms only where continuations exist. Both content views own
    /// `gg`/`G` jumps, so `g` arms in each (Calendar gained them in #0034 --
    /// this test previously asserted the opposite, when Calendar was a
    /// placeholder with no pane context).
    #[test]
    fn g_leader_arms_in_both_content_views() {
        let mut app = app_with_mailboxes();
        app.switch_view(View::Calendar);
        app.handle_key(KeyEvent::from(KeyCode::Char('g')));
        assert_eq!(app.pending_prefix, Some('g'), "g arms in Calendar (#0034)");

        app.switch_view(View::Contacts);
        app.handle_key(KeyEvent::from(KeyCode::Char('g')));
        assert_eq!(app.pending_prefix, Some('g'), "g arms in Contacts");
    }

    /// Space switches views from the Contacts view too (Global leader).
    #[test]
    fn space_leader_switches_view_from_contacts() {
        let mut app = app_with_mailboxes();
        app.switch_view(View::Contacts);
        app.handle_key(KeyEvent::from(KeyCode::Char(' ')));
        assert_eq!(app.pending_prefix, Some(' '));
        app.handle_key(KeyEvent::from(KeyCode::Char('m')));
        assert_eq!(app.view, View::Mail);
    }

    /// ...and from the Calendar view, now that it owns a pane context (#0034):
    /// the Global Space leader still resolves before the pane.
    #[test]
    fn space_leader_switches_view_from_calendar() {
        let mut app = app_in_calendar();
        app.handle_key(KeyEvent::from(KeyCode::Char(' ')));
        assert_eq!(app.pending_prefix, Some(' '));
        app.handle_key(KeyEvent::from(KeyCode::Char('c')));
        assert_eq!(app.view, View::Contacts);
    }

    /// `Tab` follows reading order — Sidebar, List, Headers, Body — and
    /// `Shift+Tab` mirrors it (#0092 review fix).
    #[test]
    fn tab_cycles_focus_in_reading_order() {
        let mut app = app_with_mailboxes();
        app.focus = Focus::List;
        app.handle_key(KeyEvent::from(KeyCode::Tab));
        assert_eq!(app.focus, Focus::Headers);
        app.handle_key(KeyEvent::from(KeyCode::Tab));
        assert_eq!(app.focus, Focus::Preview);
        app.handle_key(KeyEvent::from(KeyCode::Tab));
        assert_eq!(app.focus, Focus::Sidebar);
        app.handle_key(KeyEvent::from(KeyCode::BackTab));
        assert_eq!(app.focus, Focus::Preview);
        app.handle_key(KeyEvent::from(KeyCode::BackTab));
        assert_eq!(app.focus, Focus::Headers);
    }

    /// The `s` system family carries view-agnostic utilities, so its leader
    /// arms in the content views and `sl` opens the activity overlay there;
    /// the other four families stay Mail-only (#0092 review fix).
    #[test]
    fn the_system_leader_arms_in_non_mail_views() {
        for view in [View::Contacts, View::Calendar] {
            let mut app = app_with_mailboxes();
            app.switch_view(view);
            app.handle_key(KeyEvent::from(KeyCode::Char('s')));
            assert_eq!(app.pending_prefix, Some('s'), "`s` must arm in {view:?}");
            app.handle_key(KeyEvent::from(KeyCode::Char('l')));
            assert!(
                matches!(app.overlay, Overlay::Activity),
                "`sl` must open the activity overlay in {view:?}"
            );
        }
    }

    /// Switching to Mail restores the parked focus (the `MailView` proxy),
    /// mirroring the `AccountState` save/load pattern.
    #[test]
    fn switching_back_to_mail_restores_parked_focus() {
        let mut app = app_with_mailboxes();
        app.focus = Focus::Preview;
        app.switch_view(View::Contacts);
        // Focus is irrelevant while in a non-Mail view; on return it is
        // restored from the parked mail-view snapshot.
        app.switch_view(View::Mail);
        assert_eq!(app.focus, Focus::Preview);
    }

    /// Mail-specific keys must not fire while a non-Mail view is active: the
    /// Global mail surface (mailbox jump) is gated off, and the Mail pane
    /// contexts are not consulted at all (`a` = archive is not rebound by
    /// either content view).
    #[test]
    fn mail_keys_do_not_fire_in_contacts_or_calendar() {
        for view in [View::Contacts, View::Calendar] {
            let mut app = app_with_mailboxes();
            app.switch_view(view);
            // The Contacts view's first show asks the daemon for its index;
            // what is asserted below is what the keys queue after it.
            app.pending_actions
                .retain(|a| !matches!(a, Action::LoadContacts { .. }));
            let before = app.active_mailbox;

            // Digit jump (a Global mail action) is swallowed.
            app.handle_key(KeyEvent::from(KeyCode::Char('2')));
            assert_eq!(
                app.active_mailbox, before,
                "digit jump must not fire in {view:?}"
            );

            // A List-context key (archive) does nothing: the Mail pane contexts
            // are not consulted outside Mail, and it is not rebound there.
            app.handle_key(KeyEvent::from(KeyCode::Char('a')));
            assert!(matches!(app.overlay, Overlay::None));
            assert!(app.pending_actions.is_empty());
            assert_eq!(app.view, view, "a bare mail key must not change the view");
        }
    }

    /// View-agnostic Global keys (help, quit) still work in the non-Mail views.
    #[test]
    fn view_agnostic_keys_work_in_non_mail_views() {
        for view in [View::Contacts, View::Calendar] {
            let mut app = app_with_mailboxes();
            app.switch_view(view);

            // Help overlay toggles.
            app.handle_key(KeyEvent::from(KeyCode::Char('?')));
            assert!(matches!(app.overlay, Overlay::Help), "? in {view:?}");
            app.handle_key(KeyEvent::from(KeyCode::Char('?')));
            assert!(matches!(app.overlay, Overlay::None));

            // Quit is honoured.
            let msg = app.handle_key(KeyEvent::from(KeyCode::Char('q')));
            assert!(matches!(msg, Some(Message::Quit)), "q in {view:?}");
        }
    }

    /// Digits 1-9 still jump mailboxes in the Mail view (no leader collision).
    #[test]
    fn digits_still_jump_mailboxes_in_mail_view() {
        let mut app = app_with_mailboxes();
        assert_eq!(app.view, View::Mail);
        assert_eq!(app.active_mailbox, 0);

        // `2` -> mailbox index 1 (Drafts).
        app.handle_key(KeyEvent::from(KeyCode::Char('2')));
        assert_eq!(app.active_mailbox, 1);
        assert_eq!(app.focus, Focus::List);
    }

    // -- Contacts view (#0033) -------------------------------------------

    fn contact(addr: &str, name: &str, sent_to: u32) -> mp_core::contacts::Contact {
        mp_core::contacts::Contact {
            address: addr.into(),
            display_name: name.into(),
            sent_to,
            sent_cc: 0,
            received: 0,
            first_seen: "2026-01-01T00:00:00Z".into(),
            last_seen: "2026-04-08T00:00:00Z".into(),
            source: mp_core::contacts::ContactSource::Local,
        }
    }

    /// Build an app already in the Contacts view with a seeded index.
    fn app_in_contacts() -> App {
        let mut app = app_with_mailboxes();
        let mut contacts = std::collections::HashMap::new();
        for c in [
            contact("alice@foo.com", "Alice Smith", 3),
            contact("bob@bar.com", "Bob Jones", 2),
            contact("carol@baz.com", "Carol", 1),
        ] {
            contacts.insert(c.address.clone(), c);
        }
        app.contacts_view.index = Some(mp_core::contacts::ContactIndex {
            account: "test".into(),
            contacts,
            built_at: "2026-04-08T00:00:00Z".into(),
            extractor_version: mp_core::contacts::EXTRACTOR_VERSION,
        });
        app.contacts_view.loaded = true;
        app.view = View::Contacts;
        app.recompute_contact_matches();
        app
    }

    /// The generation of the one `LoadContacts` queued, failing if there is
    /// none or more than one.
    fn queued_contact_load(app: &App) -> u64 {
        let loads: Vec<u64> = app
            .pending_actions
            .iter()
            .filter_map(|a| match a {
                Action::LoadContacts { generation } => Some(*generation),
                _ => None,
            })
            .collect();
        assert_eq!(loads.len(), 1, "one contact.search queued: {loads:?}");
        loads[0]
    }

    /// The Contacts view asks the daemon for the account's index on its first
    /// show rather than reading the cache file: a file on disk is not what the
    /// pane shows, the daemon's answer is, a stale answer is dropped, and an
    /// answer for the account the user left does not land on the new one.
    #[test]
    fn the_contacts_view_shows_the_daemons_index_and_not_the_cache_file() {
        let _data = mp_core::config::test_env::TestDataDir::new();
        let mut app = app_with_mailboxes();
        let root = mp_core::config::account_dir(&app.account_config.name);
        std::fs::create_dir_all(&root).unwrap();
        let mut on_disk = std::collections::HashMap::new();
        let cached = contact("cached@disk.example", "On Disk", 9);
        on_disk.insert(cached.address.clone(), cached);
        mp_core::contacts::save_cache(
            &root,
            &mp_core::contacts::ContactIndex {
                account: app.account_config.name.clone(),
                contacts: on_disk,
                built_at: String::new(),
                extractor_version: mp_core::contacts::EXTRACTOR_VERSION,
            },
        )
        .unwrap();

        app.switch_view(View::Contacts);
        assert!(app.contacts_view.index.is_none(), "no file was read");
        assert!(app.contacts_view.loading);
        let first = queued_contact_load(&app);

        let answer = |address: &str| {
            crate::queries::decode_contacts(
                "test",
                &serde_json::json!({"contacts": [{
                    "address": address, "display_name": "", "sent_to": 1,
                    "sent_cc": 0, "received": 0, "first_seen": "", "last_seen": "",
                    "score": 1,
                }]}),
            )
        };
        // A `contact.changed` supersedes the first request.
        app.pending_actions.clear();
        app.reload_contacts();
        let second = queued_contact_load(&app);
        assert!(second > first);
        app.land_contacts(app.active_account, first, Ok(answer("old@daemon.example")));
        assert!(
            app.contacts_view.index.is_none(),
            "a stale answer is dropped"
        );
        app.land_contacts(app.active_account, second, Ok(answer("ada@daemon.example")));
        assert_eq!(
            app.contacts_view.matches,
            vec!["ada@daemon.example".to_string()]
        );
        assert!(app.contacts_view.loaded && !app.contacts_view.loading);

        // An account switch empties the view; an answer still in flight for
        // the account that was left is dropped.
        app.pending_actions.clear();
        app.reset_contacts_view();
        app.ensure_contacts_loaded();
        let third = queued_contact_load(&app);
        assert!(third > second, "the request counter survives the reset");
        app.land_contacts(
            app.active_account,
            second,
            Ok(answer("late@daemon.example")),
        );
        assert!(app.contacts_view.index.is_none());
    }

    /// `mp -A <account>` opens the TUI on that account, matched as the CLI
    /// matches `-A`; a name no account carries leaves the first one active.
    #[test]
    fn the_tui_opens_on_the_account_dash_a_names() {
        let mut app = App::default_for_tests();
        app.accounts = vec![
            crate::app::AccountState::for_tests("home"),
            crate::app::AccountState::for_tests("work"),
        ];
        app.load_from_account(0);
        assert!(!app.open_on_account("nobody"));
        assert_eq!(app.active_account, 0);
        assert!(app.open_on_account("Work"));
        assert_eq!(app.active_account, 1);
        assert_eq!(app.account_config.name, "work");
    }

    /// The cache guard's refusal of the build a search ran is said on the
    /// status line, as the CLI says it on stderr, and the kept index shows.
    #[test]
    fn a_refused_implicit_build_is_said_on_the_status_line() {
        let mut app = app_with_mailboxes();
        app.switch_view(View::Contacts);
        let generation = queued_contact_load(&app);
        let answer = crate::queries::decode_contacts(
            "test",
            &serde_json::json!({
                "contacts": [{"address": "kept@example.com"}],
                "rebuild": {"saved": "refused_shrunk", "contacts": 1, "kept": 10},
            }),
        );
        app.land_contacts(app.active_account, generation, Ok(answer));
        assert_eq!(
            app.status_message.as_deref(),
            Some("Contacts rebuild found only 1, kept 10 cached")
        );
        assert_eq!(
            app.contacts_view.matches,
            vec!["kept@example.com".to_string()]
        );
    }

    /// The fuzzy-search input filters the contact list at the App level, and
    /// `Esc` clears the query and restores the full ranked list.
    #[test]
    fn contacts_fuzzy_search_filters_list() {
        let mut app = app_in_contacts();
        // Empty query: all three, rank-ordered (sent_to desc): alice, bob, carol.
        assert_eq!(app.contacts_view.matches.len(), 3);
        assert_eq!(app.contacts_view.matches[0], "alice@foo.com");

        // `/` arms the search input; typing narrows the list.
        app.handle_key(KeyEvent::from(KeyCode::Char('/')));
        assert!(app.contacts_view.searching);
        for c in "bob".chars() {
            app.handle_key(KeyEvent::from(KeyCode::Char(c)));
        }
        assert_eq!(app.contacts_view.matches, vec!["bob@bar.com".to_string()]);

        // `Esc` clears the query and restores the full list.
        app.handle_key(KeyEvent::from(KeyCode::Esc));
        assert!(!app.contacts_view.searching);
        assert!(app.contacts_view.query.is_empty());
        assert_eq!(app.contacts_view.matches.len(), 3);
    }

    /// Contacts navigation keys move the list cursor and drive the selection.
    #[test]
    fn contacts_navigation_moves_cursor() {
        let mut app = app_in_contacts();
        assert_eq!(app.contacts_view.list_index, 0);
        app.handle_key(KeyEvent::from(KeyCode::Char('j')));
        assert_eq!(app.contacts_view.list_index, 1);
        assert_eq!(app.selected_contact().unwrap().address, "bob@bar.com");
        app.handle_key(KeyEvent::from(KeyCode::Char('k')));
        assert_eq!(app.contacts_view.list_index, 0);
    }

    /// `Enter`/`n` in Contacts opens the compose wizard seeded with the
    /// selected contact's address.
    #[test]
    fn compose_to_contact_seeds_wizard() {
        let mut app = app_in_contacts();
        app.handle_key(KeyEvent::from(KeyCode::Char('j'))); // select bob
        app.handle_key(KeyEvent::from(KeyCode::Enter));
        match app.pending_actions.pop_front() {
            Some(Action::ComposeToContact { to }) => {
                assert_eq!(to, "Bob Jones <bob@bar.com>");
            }
            other => panic!("expected ComposeToContact, got {other:?}"),
        }
    }

    /// `v` in Contacts queues a vCard export for the selected contact.
    #[test]
    fn vcard_key_queues_export_for_selection() {
        let mut app = app_in_contacts();
        app.handle_key(KeyEvent::from(KeyCode::Char('v')));
        match app.pending_actions.pop_front() {
            Some(Action::SendContactVcard { contact }) => {
                assert_eq!(contact.address, "alice@foo.com");
            }
            other => panic!("expected SendContactVcard, got {other:?}"),
        }
    }

    /// `c` in Contacts queues the clipboard copy for the selected contact.
    /// Dispatch-level only: the actual `arboard` call lives in `actions.rs` and
    /// would touch the real system clipboard (unavailable headless), so tests
    /// stop at the queued `Action`, like the vCard test above.
    #[test]
    fn copy_email_key_queues_clipboard_copy_for_selection() {
        let mut app = app_in_contacts();
        app.handle_key(KeyEvent::from(KeyCode::Char('j'))); // select bob
        app.handle_key(KeyEvent::from(KeyCode::Char('c')));
        match app.pending_actions.pop_front() {
            Some(Action::CopyContactEmail { address }) => {
                assert_eq!(address, "bob@bar.com");
            }
            other => panic!("expected CopyContactEmail, got {other:?}"),
        }
    }

    /// With no contact selected (empty index) `c` is a no-op with a status
    /// hint, not a queued copy.
    #[test]
    fn copy_email_key_is_noop_without_selection() {
        let mut app = app_with_mailboxes();
        app.view = View::Contacts;
        app.contacts_view.loaded = true;
        app.recompute_contact_matches();
        assert!(app.selected_contact().is_none());
        app.handle_key(KeyEvent::from(KeyCode::Char('c')));
        assert!(
            app.pending_actions
                .iter()
                .all(|a| !matches!(a, Action::CopyContactEmail { .. })),
            "c must not queue a copy without a selection"
        );
        assert_eq!(app.status_message.as_deref(), Some("No contact selected"));
    }

    /// Contacts keys must not fire in the Mail view: pressing `v` (vCard) in
    /// Mail does not queue a contact action (it is not a Mail binding).
    #[test]
    fn contacts_keys_do_not_fire_in_mail() {
        let mut app = app_with_mailboxes();
        assert_eq!(app.view, View::Mail);
        app.handle_key(KeyEvent::from(KeyCode::Char('v')));
        assert!(
            !app.pending_actions
                .iter()
                .any(|a| matches!(a, Action::SendContactVcard { .. })),
            "v must not queue a vCard export in Mail"
        );
        // And a bare `n` in Mail is the New-draft path, not compose-to-contact.
        app.pending_actions.clear();
        app.handle_key(KeyEvent::from(KeyCode::Char('n')));
        assert!(
            app.pending_actions
                .iter()
                .all(|a| !matches!(a, Action::ComposeToContact { .. })),
            "n in Mail must be NewDraft, not ComposeToContact"
        );
        // `c` in Mail is edit-recipients (Drafts-only), never a contact copy.
        app.pending_actions.clear();
        app.handle_key(KeyEvent::from(KeyCode::Char('c')));
        assert!(
            app.pending_actions
                .iter()
                .all(|a| !matches!(a, Action::CopyContactEmail { .. })),
            "c in Mail must not queue a contact copy"
        );
    }

    /// `c` must not queue a contact copy from the Calendar view either (it has
    /// no `c` binding, and the Global `c` is only a `Space` continuation).
    #[test]
    fn copy_email_key_does_not_fire_in_calendar() {
        let mut app = app_in_calendar();
        app.handle_key(KeyEvent::from(KeyCode::Char('c')));
        assert!(
            app.pending_actions
                .iter()
                .all(|a| !matches!(a, Action::CopyContactEmail { .. })),
            "c must not queue a contact copy in Calendar"
        );
    }

    // -- Calendar view (#0034, PERSO-200) ---------------------------------

    /// The frozen calendar fixture: a week of events from Mon 2026-08-03,
    /// today Tue 2026-08-04, in the agenda layout with the cursor on the
    /// first event. It has no session, so a key that reads the daemon again
    /// leaves the view empty; the daemon-backed half is in the root crate.
    fn app_in_calendar() -> App {
        crate::ui::golden_frames::calendar_fixture()
    }

    fn summary_under_cursor(app: &App) -> String {
        app.selected_event()
            .map(|event| event.summary.clone())
            .unwrap_or_default()
    }

    /// `j`/`k` and the arrows walk the events day by day, the same event
    /// once per day it covers.
    #[test]
    fn calendar_navigation_moves_cursor() {
        let mut app = app_in_calendar();
        assert_eq!(app.calendar_view.entries.len(), 7);
        assert_eq!(summary_under_cursor(&app), "Baustellen\u{fc}bergabe");
        app.handle_key(KeyEvent::from(KeyCode::Char('j')));
        assert_eq!(summary_under_cursor(&app), "Jour fixe");
        app.handle_key(KeyEvent::from(KeyCode::Down));
        assert_eq!(summary_under_cursor(&app), "PhD Kolloquium");
        app.handle_key(KeyEvent::from(KeyCode::Up));
        app.handle_key(KeyEvent::from(KeyCode::Char('k')));
        assert_eq!(app.calendar_view.list_index, 0);
        app.handle_key(KeyEvent::from(KeyCode::Char('k')));
        assert_eq!(
            app.calendar_view.list_index, 0,
            "the cursor stops at the top"
        );
    }

    /// `gg` / `G` jump to the ends of the view.
    #[test]
    fn calendar_top_bottom_jumps() {
        let mut app = app_in_calendar();
        app.handle_key(KeyEvent::from(KeyCode::Char('G')));
        assert_eq!(app.calendar_view.list_index, 6);
        assert_eq!(summary_under_cursor(&app), "Abgesagter Termin");
        app.handle_key(KeyEvent::from(KeyCode::Char('g')));
        assert_eq!(app.pending_prefix, Some('g'));
        app.handle_key(KeyEvent::from(KeyCode::Char('g')));
        assert_eq!(app.calendar_view.list_index, 0);
        assert_eq!(app.pending_prefix, None);
    }

    /// `h`/`l` and `[`/`]` move the anchor by the layout's step: a week on
    /// the agenda and the week grid, a day on the day grid.
    #[test]
    fn calendar_h_and_l_move_by_the_layouts_step() {
        let day = |raw: &str| chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d").unwrap();
        let mut app = app_in_calendar();
        app.handle_key(KeyEvent::from(KeyCode::Char('l')));
        assert_eq!(app.calendar_view.anchor, day("2026-08-10"));
        app.handle_key(KeyEvent::from(KeyCode::Char('[')));
        assert_eq!(app.calendar_view.anchor, day("2026-08-03"));
        app.calendar_view.layout = super::super::CalendarLayout::Day;
        app.handle_key(KeyEvent::from(KeyCode::Char(']')));
        assert_eq!(app.calendar_view.anchor, day("2026-08-04"));
        app.handle_key(KeyEvent::from(KeyCode::Char('h')));
        assert_eq!(app.calendar_view.anchor, day("2026-08-03"));
    }

    /// `a`, `d` and `w` switch the layout, anchored on the day under the
    /// cursor; `t` goes back to today.
    #[test]
    fn calendar_layout_keys_and_today() {
        use super::super::CalendarLayout;
        let day = |raw: &str| chrono::NaiveDate::parse_from_str(raw, "%Y-%m-%d").unwrap();
        let mut app = app_in_calendar();
        app.handle_key(KeyEvent::from(KeyCode::Char('j'))); // Tue 2026-08-04
        app.handle_key(KeyEvent::from(KeyCode::Char('d')));
        assert_eq!(app.calendar_view.layout, CalendarLayout::Day);
        assert_eq!(app.calendar_view.anchor, day("2026-08-04"));
        app.handle_key(KeyEvent::from(KeyCode::Char('w')));
        assert_eq!(app.calendar_view.layout, CalendarLayout::Week);
        app.handle_key(KeyEvent::from(KeyCode::Char('a')));
        assert_eq!(app.calendar_view.layout, CalendarLayout::Agenda);
        app.handle_key(KeyEvent::from(KeyCode::Char('t')));
        let today = chrono::Utc::now()
            .with_timezone(&app.calendar_view.zone)
            .date_naive();
        assert_eq!(app.calendar_view.anchor, today);
    }

    /// `gt` arms the jump-to-date prompt, and Enter moves the view rather
    /// than the mail list.
    #[test]
    fn calendar_gt_jumps_the_view_to_a_date() {
        let mut app = app_in_calendar();
        app.handle_key(KeyEvent::from(KeyCode::Char('g')));
        app.handle_key(KeyEvent::from(KeyCode::Char('t')));
        assert_eq!(app.jump_date_input.as_deref(), Some(""));
        for c in "2026-09-14".chars() {
            app.handle_key(KeyEvent::from(KeyCode::Char(c)));
        }
        app.handle_key(KeyEvent::from(KeyCode::Enter));
        assert!(app.jump_date_input.is_none());
        assert_eq!(
            app.calendar_view.anchor,
            chrono::NaiveDate::from_ymd_opt(2026, 9, 14).unwrap()
        );
        assert_eq!(app.view, View::Calendar);
    }

    /// `Enter` opens the event card; `Esc` closes it.
    #[test]
    fn calendar_enter_opens_the_event_card() {
        let mut app = app_in_calendar();
        app.handle_key(KeyEvent::from(KeyCode::Enter));
        let Overlay::EventCard(card) = &app.overlay else {
            panic!("Enter must open the event card");
        };
        assert_eq!(card.event.summary, "Baustellen\u{fc}bergabe");
        app.handle_key(KeyEvent::from(KeyCode::Esc));
        assert!(matches!(app.overlay, Overlay::None));
    }

    /// `e` on an invitation mail's event queues its `.ics` for `$EDITOR`;
    /// on a server event there is no mail to open.
    #[test]
    fn calendar_e_opens_the_invitations_ics_only_for_a_mail() {
        let mut app = app_in_calendar();
        app.handle_key(KeyEvent::from(KeyCode::Char('e')));
        assert!(app.pending_actions.is_empty(), "a server event has no mail");
        app.handle_key(KeyEvent::from(KeyCode::Char('j')));
        app.handle_key(KeyEvent::from(KeyCode::Char('j'))); // PhD Kolloquium, imip
        app.account_config.name = "perso".to_string();
        app.handle_key(KeyEvent::from(KeyCode::Char('e')));
        match app.pending_actions.pop_front() {
            Some(Action::OpenEventSource { msg }) => assert_eq!(msg, MessageRef::new(3)),
            other => panic!("expected OpenEventSource, got {other:?}"),
        }
    }

    /// `V` on a server invitation opens the respond overlay against the
    /// event's own account and id, with the send-response toggle.
    #[test]
    fn calendar_rsvp_opens_against_the_events_id() {
        let mut app = app_in_calendar();
        app.handle_key(KeyEvent::from(KeyCode::Char('j'))); // Jour fixe
        app.handle_key(KeyEvent::from(KeyCode::Char('V')));
        let Overlay::Rsvp(overlay) = &app.overlay else {
            panic!("V on an invitation must open the respond overlay");
        };
        assert_eq!(overlay.account, "evoqs");
        assert_eq!(overlay.event, "ev-jour-1");
        assert_eq!(overlay.summary, "Jour fixe");
        assert!(overlay.server, "a server event offers the toggle");
        assert!(overlay.send_response);
    }

    /// `V` on an invitation mail's event offers no send-response toggle.
    #[test]
    fn calendar_rsvp_on_an_imip_event_has_no_toggle() {
        let mut app = app_in_calendar();
        app.calendar_view.list_index = 2; // PhD Kolloquium
        app.handle_key(KeyEvent::from(KeyCode::Char('V')));
        let Overlay::Rsvp(overlay) = &app.overlay else {
            panic!("V on an invitation mail's event must open the respond overlay");
        };
        assert_eq!(overlay.event, "imip:3");
        assert!(!overlay.server);
        assert!(!overlay.rows().contains(&RsvpRow::SendResponse));
    }

    /// `V` on an event we organise refuses with a hint.
    #[test]
    fn calendar_rsvp_refused_for_organizer_event() {
        let mut app = app_in_calendar();
        app.calendar_view.events[0].own_response = "organizer".to_string();
        app.handle_key(KeyEvent::from(KeyCode::Char('V')));
        assert!(matches!(app.overlay, Overlay::None));
        assert!(app
            .status_message
            .as_deref()
            .is_some_and(|m| m.contains("organizer")));
    }

    /// #0031: the mail-view RSVP refuses a cancelled or superseded version of
    /// an event, so no reply goes out carrying a `SEQUENCE` the organizer has
    /// already moved past. A current REQUEST is still accepted.
    #[test]
    fn rsvp_refusal_covers_cancelled_and_superseded_versions() {
        let request = |f: fn(&mut mp_core::types::EventFrontmatter)| {
            let mut ev = mp_core::types::EventFrontmatter {
                method: Some("REQUEST".into()),
                ..Default::default()
            };
            f(&mut ev);
            ev
        };
        assert!(super::rsvp_refusal(Some(&request(|_| {}))).is_none());
        assert!(super::rsvp_refusal(None).is_some_and(|m| m.contains("Only received invitations")));
        assert!(super::rsvp_refusal(Some(&mp_core::types::EventFrontmatter {
            method: Some("CANCEL".into()),
            ..Default::default()
        }))
        .is_some_and(|m| m.contains("Only received invitations")));
        assert!(super::rsvp_refusal(Some(&request(|e| e.cancelled = true)))
            .is_some_and(|m| m.contains("cancelled")),);
        assert!(super::rsvp_refusal(Some(&request(|e| e.superseded = true)))
            .is_some_and(|m| m.contains("newer version")),);
    }

    /// `V` on a cancelled event refuses: the organizer already called the
    /// meeting off.
    #[test]
    fn calendar_rsvp_refused_for_cancelled_event() {
        let mut app = app_in_calendar();
        app.calendar_view.list_index = 6;
        app.handle_key(KeyEvent::from(KeyCode::Char('V')));
        assert!(matches!(app.overlay, Overlay::None));
        assert!(app
            .status_message
            .as_deref()
            .is_some_and(|m| m.contains("cancelled")));
    }

    /// `V` on a server event that is no invitation refuses.
    #[test]
    fn calendar_rsvp_refused_for_an_event_without_attendance() {
        let mut app = app_in_calendar();
        app.calendar_view.list_index = 3; // Holiday
        app.handle_key(KeyEvent::from(KeyCode::Char('V')));
        assert!(matches!(app.overlay, Overlay::None));
        assert!(app
            .status_message
            .as_deref()
            .is_some_and(|m| m.contains("not an invitation")));
    }

    /// The respond overlay: `t` picks Tentative on an answer row rather than
    /// moving the Calendar to today, the comment row takes typed text, Space
    /// flips the toggle, and Enter queues `event.respond`'s answer.
    #[test]
    fn the_respond_overlay_takes_an_answer_a_comment_and_the_toggle() {
        let mut app = app_in_calendar();
        app.overlay = Overlay::Rsvp(RsvpOverlay::new(
            "evoqs".to_string(),
            "ev-jour-1".to_string(),
            "Jour fixe".to_string(),
        ));
        let anchor = app.calendar_view.anchor;
        app.handle_key(KeyEvent::from(KeyCode::Char('t')));
        assert_eq!(app.calendar_view.anchor, anchor, "t is the overlay's");
        // The arrows walk the answers and on past Decline to the comment.
        app.handle_key(KeyEvent::from(KeyCode::Down));
        app.handle_key(KeyEvent::from(KeyCode::Down));
        {
            let Overlay::Rsvp(overlay) = &app.overlay else {
                panic!("the overlay stays open");
            };
            assert_eq!(overlay.row, RsvpRow::Comment);
            assert_eq!(overlay.selected, 2, "walking over Decline chose it");
        }
        // Tab and Shift+Tab jump between sections and keep the answer.
        app.handle_key(KeyEvent::from(KeyCode::BackTab));
        app.handle_key(KeyEvent::from(KeyCode::Char('k'))); // Tentative
        app.handle_key(KeyEvent::from(KeyCode::Tab)); // the comment
        for c in "late, sorry".chars() {
            app.handle_key(KeyEvent::from(KeyCode::Char(c)));
        }
        app.handle_key(KeyEvent::from(KeyCode::Backspace));
        app.handle_key(KeyEvent::from(KeyCode::Char('y')));
        app.handle_key(KeyEvent::from(KeyCode::Tab)); // the toggle
        app.handle_key(KeyEvent::from(KeyCode::Char(' ')));
        app.handle_key(KeyEvent::from(KeyCode::Enter));
        assert!(matches!(app.overlay, Overlay::None));
        match app.pending_actions.pop_front() {
            Some(Action::Rsvp {
                account,
                event,
                choice,
                comment,
                send_response,
            }) => {
                assert_eq!(account, "evoqs");
                assert_eq!(event, "ev-jour-1");
                assert_eq!(choice, RsvpChoice::Tentative);
                assert_eq!(comment.as_deref(), Some("late, sorry"));
                assert!(!send_response);
            }
            other => panic!("expected Rsvp, got {other:?}"),
        }
    }

    /// Calendar keys must not fire in Mail: `V` there is the mail-list RSVP
    /// path (guarded on the cursor email being an invite), never the calendar
    /// one, and `t` is not a Mail binding at all.
    #[test]
    fn calendar_keys_do_not_fire_in_mail() {
        let mut app = app_in_calendar();
        app.view = View::Mail;
        app.handle_key(KeyEvent::from(KeyCode::Char('V')));
        assert!(!matches!(app.overlay, Overlay::Rsvp(_)));
        let anchor = app.calendar_view.anchor;
        app.handle_key(KeyEvent::from(KeyCode::Char('l')));
        app.handle_key(KeyEvent::from(KeyCode::Char('w')));
        assert_eq!(app.calendar_view.anchor, anchor);
        assert_eq!(
            app.calendar_view.layout,
            super::super::CalendarLayout::Agenda
        );
    }

    /// Mail-specific Global keys stay swallowed in Calendar even though it now
    /// owns a pane context: digits do not jump mailboxes, `/` does not open the
    /// metadata filter (Calendar rebinds neither).
    #[test]
    fn mail_global_keys_stay_swallowed_in_calendar() {
        let mut app = app_in_calendar();
        let before = app.active_mailbox;
        app.handle_key(KeyEvent::from(KeyCode::Char('2')));
        assert_eq!(app.active_mailbox, before, "digit jump must not fire");
        app.handle_key(KeyEvent::from(KeyCode::Char('/')));
        assert_ne!(app.focus, Focus::Search, "/ must not arm the mail filter");
        assert_eq!(app.view, View::Calendar);
    }

    // -----------------------------------------------------------------------
    // Jump to date (#0017)
    // -----------------------------------------------------------------------

    /// A dated list, newest first, the way `store::read` orders one.
    fn dated_emails(days: &[&str]) -> Vec<EmailEntry> {
        days.iter()
            .map(|day| {
                let mut e = entry(day, "Alice");
                e.date_display = (*day).to_string();
                e.date_sort = format!("{day}T09:00:00");
                e
            })
            .collect()
    }

    fn app_on_dates() -> App {
        let mut app = App::default_for_tests();
        app.emails = std::sync::Arc::new(dated_emails(&[
            "2026-08-10",
            "2026-08-01",
            "2026-07-15",
            "2026-06-30",
            "2025-12-24",
        ]));
        app.email_cache = vec![Some(std::sync::Arc::clone(&app.emails))];
        app.mailbox_counts = vec![app.emails.len()];
        app.rebuild_visible();
        app
    }

    fn day(s: &str) -> chrono::NaiveDate {
        chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    /// The cursor lands on the newest row that is not after the target, and
    /// the list itself does not move: this is navigation, not a filter.
    #[test]
    fn a_jump_lands_on_the_newest_row_on_or_before_the_date() {
        let mut app = app_on_dates();
        let before = app.visible.clone();

        app.jump_to_date(day("2026-07-20"));
        assert_eq!(
            app.list_index, 2,
            "2026-07-15 is the newest row on or before it"
        );
        assert_eq!(app.visible, before, "nothing is filtered out");

        // An exact hit lands on that row, not past it.
        app.jump_to_date(day("2026-08-01"));
        assert_eq!(app.list_index, 1);

        // A date newer than everything lands on the newest row.
        app.jump_to_date(day("2026-12-31"));
        assert_eq!(app.list_index, 0);
    }

    /// A date older than the whole mailbox parks on the oldest row and says
    /// so, rather than silently pretending it found it.
    #[test]
    fn a_jump_past_the_oldest_row_says_where_it_stopped() {
        let mut app = app_on_dates();
        app.jump_to_date(day("2020-01-01"));
        assert_eq!(app.list_index, 4);
        let status = app.status_message.clone().unwrap();
        assert!(
            status.contains("Nothing on or before 2020-01-01"),
            "{status}"
        );
    }

    /// `g t` arms the prompt, typing edits it, Enter jumps and disarms, and a
    /// date the grammar cannot read keeps the prompt up with the reason shown.
    #[test]
    fn the_prompt_is_armed_typed_and_committed_by_the_keyboard() {
        let mut app = app_on_dates();
        app.handle_key(KeyEvent::from(KeyCode::Char('g')));
        app.handle_key(KeyEvent::from(KeyCode::Char('t')));
        assert_eq!(
            app.jump_date_input.as_deref(),
            Some(""),
            "the prompt is armed"
        );

        for c in "2026-07-20".chars() {
            app.handle_key(KeyEvent::from(KeyCode::Char(c)));
        }
        assert_eq!(app.jump_date_input.as_deref(), Some("2026-07-20"));
        app.handle_key(KeyEvent::from(KeyCode::Backspace));
        assert_eq!(app.jump_date_input.as_deref(), Some("2026-07-2"));
        app.handle_key(KeyEvent::from(KeyCode::Char('0')));

        app.handle_key(KeyEvent::from(KeyCode::Enter));
        assert!(
            app.jump_date_input.is_none(),
            "committing disarms the prompt"
        );
        assert_eq!(app.list_index, 2);
    }

    /// While the prompt is armed it owns the keyboard: `d` types a character,
    /// it does not delete the message under the cursor.
    #[test]
    fn the_armed_prompt_swallows_the_keys_that_would_otherwise_act() {
        let mut app = app_on_dates();
        app.jump_date_input = Some(String::new());
        app.handle_key(KeyEvent::from(KeyCode::Char('d')));
        assert_eq!(app.jump_date_input.as_deref(), Some("d"));
        assert!(app.pending_actions.is_empty(), "{:?}", app.pending_actions);

        app.handle_key(KeyEvent::from(KeyCode::Esc));
        assert!(app.jump_date_input.is_none(), "Esc abandons the prompt");
        assert_eq!(app.list_index, 0, "an abandoned prompt moves nothing");
    }

    /// An unreadable date is a correction, not a re-arm: the prompt stays up
    /// and the status line names the forms that would have worked.
    #[test]
    fn an_unreadable_date_keeps_the_prompt_and_explains() {
        let mut app = app_on_dates();
        app.jump_date_input = Some("tomorrow".to_string());
        app.handle_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(app.jump_date_input.as_deref(), Some("tomorrow"));
        assert_eq!(app.list_index, 0, "nothing moved");
        let status = app.status_message.clone().unwrap();
        assert!(status.contains("YYYY-MM-DD"), "{status}");
    }

    // -----------------------------------------------------------------------
    // Attach file to a draft (#0098)
    // -----------------------------------------------------------------------

    /// The Drafts mailbox active with one draft under the cursor and the list
    /// focused: the surface `t a` (attach) is reachable from.
    fn app_in_drafts() -> App {
        let mut app = app_with_mailboxes();
        app.active_mailbox = 1; // Drafts
        app.emails = std::sync::Arc::new(vec![draft_entry("draft-1", "Re: report")]);
        app.email_cache = vec![None, Some(std::sync::Arc::clone(&app.emails)), None, None];
        app.rebuild_visible();
        app.list_index = 0;
        app.focus = Focus::List;
        app
    }

    /// `t a` on a Drafts row arms the prompt, typing edits it, an on-disk path
    /// commits (disarms and queues the append), and a path that is not on disk
    /// keeps the prompt up with the reason on the status line -- the same
    /// correction-not-re-arm shape the jump-to-date prompt uses (#0098).
    #[test]
    fn the_attach_prompt_is_armed_typed_and_committed_by_the_keyboard() {
        let mut app = app_in_drafts();
        app.handle_key(KeyEvent::from(KeyCode::Char('t')));
        app.handle_key(KeyEvent::from(KeyCode::Char('a')));
        assert_eq!(
            app.attach_file_input.as_deref(),
            Some(""),
            "the prompt is armed"
        );

        // A path that is not on disk is surfaced, not stored: the prompt stays.
        for c in "/no/such/file".chars() {
            app.handle_key(KeyEvent::from(KeyCode::Char(c)));
        }
        app.handle_key(KeyEvent::from(KeyCode::Enter));
        assert_eq!(
            app.attach_file_input.as_deref(),
            Some("/no/such/file"),
            "a missing path keeps the prompt armed"
        );
        assert!(app.pending_actions.is_empty(), "nothing was queued");
        assert!(app
            .status_message
            .as_deref()
            .unwrap()
            .contains("No such file"));

        // A path that exists: Enter disarms and queues the append verbatim.
        let tmp = std::env::temp_dir().join(format!("mp-attach-test-{}.txt", std::process::id()));
        std::fs::write(&tmp, b"x").unwrap();
        let tmp_str = tmp.to_string_lossy().into_owned();
        app.attach_file_input = Some(tmp_str.clone());
        app.handle_key(KeyEvent::from(KeyCode::Enter));
        assert!(
            app.attach_file_input.is_none(),
            "an on-disk path disarms the prompt"
        );
        assert!(
            matches!(
                app.pending_actions.front(),
                Some(Action::AttachFileToDraft { path }) if *path == tmp_str
            ),
            "the append is queued with the typed path: {:?}",
            app.pending_actions
        );
        std::fs::remove_file(&tmp).ok();
    }

    /// While the attach prompt is armed it owns the keyboard: `d` types a
    /// character, it does not delete the draft under the cursor; Esc abandons.
    #[test]
    fn the_armed_attach_prompt_swallows_the_keys_that_would_otherwise_act() {
        let mut app = app_in_drafts();
        app.attach_file_input = Some(String::new());
        app.handle_key(KeyEvent::from(KeyCode::Char('d')));
        assert_eq!(app.attach_file_input.as_deref(), Some("d"));
        assert!(app.pending_actions.is_empty(), "{:?}", app.pending_actions);

        app.handle_key(KeyEvent::from(KeyCode::Esc));
        assert!(app.attach_file_input.is_none(), "Esc abandons the prompt");
    }

    /// Attach is Drafts-only: pressed in a received-mail mailbox it arms
    /// nothing and says why (the advisory-guard shape `ce` edit recipients
    /// uses), so `t a` never grows a non-existent `attachments:` list (#0098).
    #[test]
    fn attach_is_refused_outside_drafts() {
        let mut app = app_with_mailboxes(); // Inbox active, received mail
        app.focus = Focus::List;
        app.handle_key(KeyEvent::from(KeyCode::Char('t')));
        app.handle_key(KeyEvent::from(KeyCode::Char('a')));
        assert!(app.attach_file_input.is_none(), "no prompt outside Drafts");
        assert!(app
            .status_message
            .as_deref()
            .unwrap()
            .contains("only available in Drafts"));
    }

    // -----------------------------------------------------------------------
    // Pane zoom (#TKT-0044)
    // -----------------------------------------------------------------------

    #[test]
    fn z_zooms_the_focused_pane_and_z_again_restores_the_split() {
        let mut app = app_with_emails(sample());
        app.focus = Focus::Preview;
        assert_eq!(app.zoomed_pane(), None);
        app.handle_key(KeyEvent::from(KeyCode::Char('z')));
        assert_eq!(app.zoomed_pane(), Some(Focus::Preview));
        app.handle_key(KeyEvent::from(KeyCode::Char('z')));
        assert_eq!(app.zoomed_pane(), None);
    }

    #[test]
    fn the_zoom_follows_the_focus() {
        // herdr zooms *the active pane*, so cycling focus under a zoom moves
        // the zoom rather than leaving a zoomed pane the keyboard no longer
        // drives.
        let mut app = app_with_emails(sample());
        app.focus = Focus::List;
        app.handle_key(KeyEvent::from(KeyCode::Char('z')));
        assert_eq!(app.zoomed_pane(), Some(Focus::List));
        app.handle_key(KeyEvent::from(KeyCode::Tab));
        assert_eq!(app.focus, Focus::Headers);
        assert_eq!(app.zoomed_pane(), Some(Focus::Headers));
    }

    #[test]
    fn the_search_prompt_zooms_the_list_it_filters() {
        let mut app = app_with_emails(sample());
        app.focus = Focus::Search;
        app.zoomed = true;
        assert_eq!(app.zoomed_pane(), Some(Focus::List));
    }

    #[test]
    fn the_compose_wizard_has_nothing_to_zoom_and_says_so() {
        let mut app = app_with_emails(sample());
        app.focus = Focus::ComposeWizard;
        app.toggle_zoom();
        assert!(!app.zoomed, "the flag must not arm for a later pane");
        assert_eq!(app.status_message.as_deref(), Some("Nothing to zoom here."));
    }

    #[test]
    fn zoom_is_a_mail_view_layout_and_is_ignored_elsewhere() {
        // `z` never fires off Mail (`is_view_agnostic` leaves `ToggleZoom`
        // out), and even a flag set before the switch changes nothing there.
        let mut app = app_with_emails(sample());
        app.zoomed = true;
        app.view = View::Contacts;
        assert_eq!(app.zoomed_pane(), None);
        app.handle_key(KeyEvent::from(KeyCode::Char('z')));
        assert!(app.zoomed, "the flag is untouched off Mail");
        // Back on Mail the zoom the user armed is still there.
        app.view = View::Mail;
        assert_eq!(app.zoomed_pane(), Some(Focus::List));
    }

    #[test]
    fn a_zoom_survives_an_account_switch() {
        // Session state, not per-account state: switching accounts must not
        // silently un-zoom the pane the user is reading.
        let mut app = app_with_emails(sample());
        app.focus = Focus::Preview;
        app.zoomed = true;
        app.save_to_account();
        app.load_from_account(0);
        assert_eq!(app.zoomed_pane(), Some(Focus::Preview));
    }
}
