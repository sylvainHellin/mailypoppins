//! Notes on mail for the coding agent (PERSO-315): the `n` prompt, the `N`
//! list, the row glyph and the reading pane's band, all over the daemon's
//! `note.*` family.
//!
//! The daemon keeps every note and renders the batch prompt; this module asks
//! it and draws what it answers. The presets the prompt offers are the ones
//! `note.presets` answers at the moment `n` is pressed, never a copy of its
//! own, so a `[notes]` change in `config.toml` reaches the prompt as soon as
//! the daemon reads it. A row's glyph rides on the listing (`note` on every
//! row), and `note.changed` carries the whole list, which patches the rows,
//! refreshes an open `N` list and drops the reading pane's band memo.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use serde_json::json;

use mp_protocol::note::{
    Note, NoteCleared, NoteListing, NoteLookup, NotePreset, NotePresets, NoteState,
    METHOD_NOTE_CLEAR, METHOD_NOTE_CLEAR_DONE, METHOD_NOTE_GET, METHOD_NOTE_LIST,
    METHOD_NOTE_PRESETS, METHOD_NOTE_SET, METHOD_NOTE_SET_MANY,
};

use super::{App, EmailEntry, Focus, MailboxKind, Overlay, StatusLevel, View};

// ---------------------------------------------------------------------------
// The prompt
// ---------------------------------------------------------------------------

/// What a note prompt sets its note on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoteTarget {
    /// The cursor row or the `v`-selected rows, by the selectors the listing
    /// carried: one is `note.set`, several are `note.set_many`.
    Rows(Vec<String>),
    /// A note already set, by its id: an edit from the notes list, which is
    /// how a note asking for a new email is reached at all.
    Id(String),
    /// A new note asking for a new email from this account (`a` in the
    /// notes list).
    NewEmail(String),
}

/// Where a note prompt stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NoteStage {
    /// Waiting for a preset's letter, Space for free text, or, on mail that
    /// already has a note, Backspace to clear it.
    Choose,
    /// Typing the line of text: of the preset `preset` names, or free text
    /// when it names none.
    Text {
        preset: Option<String>,
        text: String,
    },
}

/// What one key did to a note prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotePromptOutcome {
    /// The prompt is still open.
    Pending,
    /// Esc: nothing is written.
    Cancel,
    /// Write the note.
    Save {
        preset: Option<String>,
        text: String,
    },
    /// Remove the note the mail has.
    Clear,
    /// A key the prompt refuses, with the sentence that says why; the prompt
    /// stays open.
    Refused(String),
}

/// The one-line note prompt over the list (`n`), or over the notes list
/// (`a`, `e`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotePrompt {
    pub target: NoteTarget,
    /// The daemon's presets, in prompt order, as `note.presets` answered.
    pub presets: Vec<NotePreset>,
    /// The note the target already has, read with `note.get` (or taken from
    /// the notes list), `None` for unnoted mail and for a selection.
    pub existing: Option<Note>,
    pub stage: NoteStage,
}

impl NotePrompt {
    /// A prompt on `target`. A note still open or handed off opens on its
    /// text, pre-filled for editing; a done note opens on the presets, where
    /// Backspace clears it and a letter gives it a new instruction.
    pub fn new(target: NoteTarget, presets: Vec<NotePreset>, existing: Option<Note>) -> Self {
        let stage = match &existing {
            Some(note) if note.state != NoteState::Done => NoteStage::Text {
                preset: note.preset.clone(),
                text: note.text.clone(),
            },
            _ => NoteStage::Choose,
        };
        NotePrompt {
            target,
            presets,
            existing,
            stage,
        }
    }

    /// The preset whose key is `key`.
    pub fn preset(&self, key: &str) -> Option<&NotePreset> {
        self.presets.iter().find(|preset| preset.key == key)
    }

    /// The word a preset key reads as, its label, or the key itself when
    /// the configuration no longer names it.
    pub fn label(&self, key: &str) -> String {
        self.preset(key)
            .map(|preset| preset.label.clone())
            .unwrap_or_else(|| key.to_string())
    }

    /// One key.
    pub fn key(&mut self, key: KeyEvent) -> NotePromptOutcome {
        if key.code == KeyCode::Esc {
            return NotePromptOutcome::Cancel;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return NotePromptOutcome::Pending;
        }
        match &mut self.stage {
            NoteStage::Choose => match key.code {
                KeyCode::Char(' ') => {
                    self.stage = NoteStage::Text {
                        preset: None,
                        text: String::new(),
                    };
                    NotePromptOutcome::Pending
                }
                KeyCode::Backspace | KeyCode::Delete if self.existing.is_some() => {
                    NotePromptOutcome::Clear
                }
                KeyCode::Char(c) => {
                    let Some(preset) = self.preset(&c.to_string()).cloned() else {
                        return NotePromptOutcome::Refused(format!(
                            "No preset on {c}; Space types free text"
                        ));
                    };
                    if preset.needs_text {
                        self.stage = NoteStage::Text {
                            preset: Some(preset.key),
                            text: String::new(),
                        };
                        NotePromptOutcome::Pending
                    } else {
                        NotePromptOutcome::Save {
                            preset: Some(preset.key),
                            text: String::new(),
                        }
                    }
                }
                _ => NotePromptOutcome::Pending,
            },
            NoteStage::Text { preset, text } => match key.code {
                KeyCode::Enter => {
                    let line = text.trim().to_string();
                    if preset.is_none() && line.is_empty() {
                        return NotePromptOutcome::Refused(
                            "Type the instruction, or Esc to cancel".to_string(),
                        );
                    }
                    NotePromptOutcome::Save {
                        preset: preset.clone(),
                        text: line,
                    }
                }
                // Backspace on an empty line goes back to the presets, which
                // is how an edited note changes its preset.
                KeyCode::Backspace if text.is_empty() => {
                    self.stage = NoteStage::Choose;
                    NotePromptOutcome::Pending
                }
                KeyCode::Backspace => {
                    text.pop();
                    NotePromptOutcome::Pending
                }
                KeyCode::Char(c) => {
                    text.push(c);
                    NotePromptOutcome::Pending
                }
                _ => NotePromptOutcome::Pending,
            },
        }
    }

    /// What the prompt line says before the cursor: the presets to choose
    /// from, or the instruction being typed.
    pub fn choices(&self) -> String {
        let mut parts: Vec<String> = Vec::new();
        if let Some(note) = self
            .existing
            .as_ref()
            .filter(|n| n.state == NoteState::Done)
        {
            parts.push(format!(
                "done: {}",
                note.result.as_deref().unwrap_or("(no result)")
            ));
        }
        if self.existing.is_some() {
            parts.push("\u{232b} clear".to_string());
        }
        parts.extend(
            self.presets
                .iter()
                .map(|preset| format!("{} {}", preset.key, preset.label)),
        );
        parts.push("Space free text".to_string());
        parts.join("  ")
    }

    /// The label of the line being typed: `note (Reply): ` or `note: `.
    pub fn text_prefix(&self) -> String {
        match &self.stage {
            NoteStage::Text {
                preset: Some(key), ..
            } => format!("note ({}): ", self.label(key)),
            _ => "note: ".to_string(),
        }
    }
}

// ---------------------------------------------------------------------------
// The notes list overlay
// ---------------------------------------------------------------------------

/// The notes of every account (`N`), as `note.list` last answered or the
/// last `note.changed` carried, newest first, with the cursor and the prompt
/// `a` and `e` open over it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NotesOverlay {
    pub notes: Vec<Note>,
    pub selected: usize,
    pub prompt: Option<NotePrompt>,
    /// The daemon's presets when the list opened, for the labels its rows
    /// name a preset by.
    pub presets: Vec<NotePreset>,
}

impl NotesOverlay {
    /// Take a new list, keeping the cursor on its note when it is still
    /// there, else in range.
    pub fn apply(&mut self, notes: Vec<Note>) {
        let kept = self
            .selected_note()
            .and_then(|old| notes.iter().position(|note| note.id == old.id));
        self.notes = notes;
        self.selected = kept.unwrap_or(self.selected.min(self.notes.len().saturating_sub(1)));
    }

    /// The note under the cursor, `None` on an empty list.
    pub fn selected_note(&self) -> Option<&Note> {
        self.notes.get(self.selected)
    }

    /// The words a note's instruction reads as: its preset's label, its
    /// text, or both.
    pub fn instruction(&self, note: &Note) -> String {
        let label = note.preset.as_deref().map(|key| {
            self.presets
                .iter()
                .find(|preset| preset.key == key)
                .map(|preset| preset.label.clone())
                .unwrap_or_else(|| key.to_string())
        });
        match (label, note.text.trim()) {
            (Some(label), "") => label,
            (Some(label), text) => format!("{label}: {text}"),
            (None, text) => text.to_string(),
        }
    }

    /// How many notes are open, the ones `x` hands off.
    pub fn open_count(&self) -> usize {
        self.notes
            .iter()
            .filter(|note| note.state == NoteState::Open)
            .count()
    }
}

// ---------------------------------------------------------------------------
// The reading pane's band
// ---------------------------------------------------------------------------

/// The note the reading pane shows above the body (PERSO-315), memoised by
/// the cursor row's selector and the notes epoch, so a frame on an unchanged
/// cursor costs nothing and a `note.changed` reads it again.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PreviewNote {
    key: Option<(String, u64)>,
    /// The note on the cursor row, `None` when it has none.
    pub note: Option<Note>,
    /// The label of its preset, from `note.presets`.
    pub label: Option<String>,
}

// ---------------------------------------------------------------------------
// The rows
// ---------------------------------------------------------------------------

/// The state of the note on `entry`, a row of `account`, among `notes`: a
/// draft by its id, a message by the Message-ID its selector's key is, in
/// any mailbox, as the daemon holds a note.
pub fn note_state_of(entry: &EmailEntry, account: &str, notes: &[Note]) -> Option<NoteState> {
    let mine = notes.iter().filter(|note| note.account == account);
    if let Some(id) = entry.draft_id.as_deref() {
        return mine
            .filter(|note| note.draft_id.as_deref() == Some(id))
            .map(|note| note.state)
            .next();
    }
    let key = mp_core::selector::parse(entry.selector.as_deref()?)
        .ok()?
        .key;
    mine.filter(|note| {
        note.message_id
            .as_deref()
            .is_some_and(|id| mp_core::selector::message_key(id) == key)
    })
    .map(|note| note.state)
    .next()
}

impl App {
    // -----------------------------------------------------------------
    // Daemon reads
    // -----------------------------------------------------------------

    /// The daemon's `note.presets`, in prompt order.
    pub(crate) fn note_presets(&self) -> Result<Vec<NotePreset>, String> {
        let answer = self.daemon_call(METHOD_NOTE_PRESETS, json!({}))?;
        serde_json::from_value::<NotePresets>(answer)
            .map(|answer| answer.presets)
            .map_err(|e| format!("reading note.presets: {e}"))
    }

    /// The daemon's `note.get` on one selector.
    pub(crate) fn note_on(&self, selector: &str) -> Result<Option<Note>, String> {
        let answer = self.daemon_call(METHOD_NOTE_GET, json!({ "selector": selector }))?;
        serde_json::from_value::<NoteLookup>(answer)
            .map(|answer| answer.note)
            .map_err(|e| format!("reading note.get: {e}"))
    }

    /// The daemon's `note.list`, every note of every account.
    pub(crate) fn note_listing(&self) -> Result<Vec<Note>, String> {
        let answer = self.daemon_call(METHOD_NOTE_LIST, json!({}))?;
        serde_json::from_value::<NoteListing>(answer)
            .map(|answer| answer.notes)
            .map_err(|e| format!("reading note.list: {e}"))
    }

    // -----------------------------------------------------------------
    // note.changed
    // -----------------------------------------------------------------

    /// A `note.changed` from any client, or the agent's `mp notes done`:
    /// the whole list. An open notes list takes it, the reading pane reads
    /// its band again, and the rows' glyphs follow.
    ///
    /// A flat list and the Drafts list are patched in place from the list.
    /// A conversation list is re-streamed, since a header carries the most
    /// pressing note among members it does not list. The lists cached for
    /// other mailboxes and parked accounts are dropped, read again on their
    /// next show; the answer says whether the open list moved.
    pub fn apply_notes(&mut self, notes: Vec<Note>) -> bool {
        self.notes_epoch = self.notes_epoch.wrapping_add(1);
        if let Overlay::Notes(overlay) = &mut self.overlay {
            overlay.apply(notes.clone());
        }
        let active = self.active_account;
        for index in 0..self.accounts.len() {
            if index != active {
                self.invalidate_all_caches_on(index);
            }
        }
        for idx in 0..self.email_cache.len() {
            if idx != self.active_mailbox {
                self.invalidate_cache_idx(idx);
            }
        }
        if self.conversation_mode() {
            self.request_restream();
            return true;
        }
        let account = self.account_config.name.clone();
        let moved = self.emails.iter().any(|entry| {
            entry.key().is_some() && entry.note != note_state_of(entry, &account, &notes)
        });
        if moved {
            self.with_emails_mut(|entries| {
                for entry in entries.iter_mut().filter(|entry| entry.key().is_some()) {
                    entry.note = note_state_of(entry, &account, &notes);
                }
            });
            if self.noted_only {
                let anchor = self.cursor_anchor();
                let fallback = self.list_index;
                self.rebuild_visible();
                self.restore_cursor(anchor, fallback);
            }
        }
        moved
    }

    /// Refresh the reading pane's band memo for the cursor row: one
    /// `note.get` (and its preset's label) when the row has a note and the
    /// memo answers for another row or an older list, nothing otherwise.
    pub fn refresh_preview_note(&mut self) {
        let key = self
            .selected_email()
            .filter(|entry| entry.note.is_some())
            .and_then(|entry| entry.selector.clone())
            .map(|selector| (selector, self.notes_epoch));
        if key == self.preview_note.key {
            return;
        }
        let note = match &key {
            Some((selector, _)) => self.note_on(selector).unwrap_or_else(|e| {
                log::warn!("[notes] reading the note on {selector}: {e}");
                None
            }),
            None => None,
        };
        let label = note
            .as_ref()
            .and_then(|note| note.preset.clone())
            .map(|key| {
                self.note_presets()
                    .ok()
                    .and_then(|presets| presets.into_iter().find(|p| p.key == key))
                    .map(|preset| preset.label)
                    .unwrap_or(key)
            });
        self.preview_note = PreviewNote { key, note, label };
    }

    /// Park `note` as the reading pane's band for the cursor row, as
    /// [`Self::refresh_preview_note`] would have left it: the frozen
    /// fixtures have no daemon to ask.
    #[cfg(any(test, feature = "test-support"))]
    pub fn prime_preview_note(&mut self, note: Note, label: Option<String>) {
        let key = self
            .selected_email()
            .and_then(|entry| entry.selector.clone())
            .map(|selector| (selector, self.notes_epoch));
        self.preview_note = PreviewNote {
            key,
            note: Some(note),
            label,
        };
    }

    // -----------------------------------------------------------------
    // n: the prompt over the list
    // -----------------------------------------------------------------

    /// The selectors `n` sets a note on: the selected rows when there is a
    /// selection, a conversation header standing for its members by the
    /// selector of its latest, else the cursor row.
    fn note_selectors(&self) -> Vec<String> {
        if self.selection.is_empty() {
            return self
                .selected_email()
                .and_then(|entry| entry.selector.clone())
                .into_iter()
                .collect();
        }
        let mut selectors: Vec<String> = Vec::new();
        for entry in self.emails.iter() {
            let picked = self.is_selected(entry)
                || entry.header().is_some_and(|header| {
                    header
                        .member_ids
                        .iter()
                        .any(|m| self.selection.contains(&super::EntryKey::Msg(*m)))
                });
            if let Some(selector) = entry.selector.clone().filter(|_| picked) {
                if !selectors.contains(&selector) {
                    selectors.push(selector);
                }
            }
        }
        selectors
    }

    /// `n`: the note prompt on the cursor row or the selection, with the
    /// daemon's presets. A noted row is read with `note.get` so the prompt
    /// can edit or clear what it has.
    pub(crate) fn open_note_prompt(&mut self) {
        if self.view != View::Mail {
            return;
        }
        let selectors = self.note_selectors();
        if selectors.is_empty() {
            self.set_status_level(
                "This row has no selector to set a note on".to_string(),
                StatusLevel::Warning,
            );
            return;
        }
        let presets = match self.note_presets() {
            Ok(presets) => presets,
            Err(e) => {
                self.set_status_level(format!("Cannot read the presets: {e}"), StatusLevel::Error);
                return;
            }
        };
        let existing = match (self.selection.is_empty(), self.selected_email()) {
            (true, Some(entry)) if entry.note.is_some() => match self.note_on(&selectors[0]) {
                Ok(note) => note,
                Err(e) => {
                    self.set_status_level(format!("Cannot read the note: {e}"), StatusLevel::Error);
                    return;
                }
            },
            _ => None,
        };
        self.note_prompt = Some(NotePrompt::new(
            NoteTarget::Rows(selectors),
            presets,
            existing,
        ));
    }

    /// A key while the list's note prompt is open.
    pub(crate) fn handle_note_prompt_key(&mut self, key: KeyEvent) {
        let Some(prompt) = self.note_prompt.as_mut() else {
            return;
        };
        let outcome = prompt.key(key);
        let prompt = prompt.clone();
        if !matches!(
            outcome,
            NotePromptOutcome::Pending | NotePromptOutcome::Refused(_)
        ) {
            self.note_prompt = None;
        }
        self.land_prompt(outcome, &prompt);
    }

    /// What a prompt's key asked for, once the prompt that is over is
    /// closed: the write, the clear, or the sentence of a refusal.
    fn land_prompt(&mut self, outcome: NotePromptOutcome, prompt: &NotePrompt) {
        match outcome {
            NotePromptOutcome::Pending | NotePromptOutcome::Cancel => {}
            NotePromptOutcome::Refused(why) => self.set_status_level(why, StatusLevel::Warning),
            NotePromptOutcome::Save { preset, text } => {
                self.save_note(&prompt.target, preset.as_deref(), &text);
            }
            NotePromptOutcome::Clear => {
                if let Some(note) = &prompt.existing {
                    self.clear_note(note);
                }
            }
        }
    }

    /// Write a note: `note.set` on one selector, an id or a new email,
    /// `note.set_many` on several selectors. The rows follow with the
    /// `note.changed` the write publishes; the status line says what was
    /// written, or why not.
    pub(crate) fn save_note(&mut self, target: &NoteTarget, preset: Option<&str>, text: &str) {
        let mut params = json!({ "text": text });
        if let Some(preset) = preset {
            params["preset"] = json!(preset);
        }
        let (method, what) = match target {
            NoteTarget::Rows(selectors) if selectors.len() == 1 => {
                params["selector"] = json!(selectors[0]);
                (METHOD_NOTE_SET, "Note set".to_string())
            }
            NoteTarget::Rows(selectors) => {
                params["selectors"] = json!(selectors);
                (
                    METHOD_NOTE_SET_MANY,
                    format!("Note set on {} messages", selectors.len()),
                )
            }
            NoteTarget::Id(id) => {
                params["id"] = json!(id);
                (METHOD_NOTE_SET, "Note saved".to_string())
            }
            NoteTarget::NewEmail(account) => {
                params["account"] = json!(account);
                (
                    METHOD_NOTE_SET,
                    format!("Note for a new email from {account}"),
                )
            }
        };
        match self.daemon_call(method, params) {
            Ok(_) => self.set_status_level(what, StatusLevel::Success),
            Err(e) => {
                self.set_status_level(format!("Cannot set the note: {e}"), StatusLevel::Error)
            }
        }
    }

    /// `note.clear` on one note, by its id.
    pub(crate) fn clear_note(&mut self, note: &Note) {
        let cleared = self
            .daemon_call(METHOD_NOTE_CLEAR, json!({ "id": note.id }))
            .and_then(|answer| {
                serde_json::from_value::<NoteCleared>(answer)
                    .map_err(|e| format!("reading note.clear: {e}"))
            });
        match cleared {
            Ok(_) => self.set_status("Note cleared".to_string()),
            Err(e) => {
                self.set_status_level(format!("Cannot clear the note: {e}"), StatusLevel::Error)
            }
        }
    }

    // -----------------------------------------------------------------
    // N: the notes list
    // -----------------------------------------------------------------

    /// `N`: every note of every account, read from the daemon.
    pub(crate) fn open_notes(&mut self) {
        match self.note_listing() {
            Ok(notes) => {
                let mut overlay = NotesOverlay {
                    presets: self.note_presets().unwrap_or_default(),
                    ..NotesOverlay::default()
                };
                overlay.apply(notes);
                self.overlay = Overlay::Notes(Box::new(overlay));
            }
            Err(e) => {
                self.set_status_level(format!("Cannot list the notes: {e}"), StatusLevel::Error)
            }
        }
    }

    /// A key in the notes list: `j`/`k` move, Enter opens the mail, `a`
    /// adds a note for a new email, `e` edits, `d` drops, `D` clears every
    /// done note, `y` copies the batch prompt, `x` hands the open notes to
    /// the agent, Esc or `q` closes. While `a` or `e` has its prompt open,
    /// every key is the prompt's.
    pub(crate) fn handle_notes_key(&mut self, key: KeyEvent) {
        let Overlay::Notes(overlay) = &mut self.overlay else {
            return;
        };
        if let Some(prompt) = overlay.prompt.as_mut() {
            let outcome = prompt.key(key);
            let prompt = prompt.clone();
            if !matches!(
                outcome,
                NotePromptOutcome::Pending | NotePromptOutcome::Refused(_)
            ) {
                overlay.prompt = None;
            }
            self.land_prompt(outcome, &prompt);
            return;
        }
        let selected = overlay.selected_note().cloned();
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                if overlay.selected + 1 < overlay.notes.len() {
                    overlay.selected += 1;
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                overlay.selected = overlay.selected.saturating_sub(1);
            }
            KeyCode::Enter => match selected {
                Some(note) if note.selector.is_none() => self.edit_listed_note(note),
                Some(note) => self.open_noted_mail(&note),
                None => {}
            },
            KeyCode::Char('a') => {
                let account = self.account_config.name.clone();
                self.open_listed_prompt(NoteTarget::NewEmail(account), None);
            }
            KeyCode::Char('e') => {
                if let Some(note) = selected {
                    self.edit_listed_note(note);
                }
            }
            KeyCode::Char('d') => match selected {
                Some(note) => self.clear_note(&note),
                None => self.set_status("No note to drop".to_string()),
            },
            KeyCode::Char('D') => self.clear_done_notes(),
            KeyCode::Char('y') => self.push_action(super::Action::CopyNotesExport),
            KeyCode::Char('x') => self.push_action(super::Action::HandOffNotes),
            KeyCode::Esc | KeyCode::Char('q') => self.close_overlay(),
            _ => {}
        }
    }

    /// `e` (and Enter on a note for a new email): the prompt on the note,
    /// pre-filled. A note on mail is addressed by its selector, so a change
    /// of preset replaces the old one; a note for a new email has only its
    /// id.
    fn edit_listed_note(&mut self, note: Note) {
        let target = match &note.selector {
            Some(selector) => NoteTarget::Rows(vec![selector.clone()]),
            None => NoteTarget::Id(note.id.clone()),
        };
        self.open_listed_prompt(target, Some(note));
    }

    /// Open the prompt over the notes list.
    fn open_listed_prompt(&mut self, target: NoteTarget, existing: Option<Note>) {
        let presets = match self.note_presets() {
            Ok(presets) => presets,
            Err(e) => {
                self.set_status_level(format!("Cannot read the presets: {e}"), StatusLevel::Error);
                return;
            }
        };
        if let Overlay::Notes(overlay) = &mut self.overlay {
            overlay.prompt = Some(NotePrompt::new(target, presets, existing));
        }
    }

    /// `D`: `note.clear_done`.
    fn clear_done_notes(&mut self) {
        let cleared = self
            .daemon_call(METHOD_NOTE_CLEAR_DONE, json!({}))
            .and_then(|answer| {
                serde_json::from_value::<NoteCleared>(answer)
                    .map_err(|e| format!("reading note.clear_done: {e}"))
            });
        match cleared {
            Ok(cleared) => self.set_status(format!("Cleared {} done notes", cleared.removed)),
            Err(e) => self.set_status_level(
                format!("Cannot clear the done notes: {e}"),
                StatusLevel::Error,
            ),
        }
    }

    /// Enter on a note: its mail in the list, in its account and the mailbox
    /// its selector names, the cursor placed once the list holds it.
    fn open_noted_mail(&mut self, note: &Note) {
        let Some(selector) = note.selector.clone() else {
            return;
        };
        if note.orphaned {
            self.set_status_level(
                "That note's mail is gone; d drops the note".to_string(),
                StatusLevel::Warning,
            );
            return;
        }
        let Some(mailbox) = mp_core::selector::parse(&selector)
            .ok()
            .and_then(|parts| parts.mailbox)
        else {
            self.set_status(format!("{selector} names no mailbox"));
            return;
        };
        let Some(account) = self
            .accounts
            .iter()
            .position(|acct| acct.account_config.name == note.account)
        else {
            self.set_status(format!("{} is not a configured account", note.account));
            return;
        };
        self.close_overlay();
        self.switch_view(View::Mail);
        self.switch_account(account);
        let idx = if mailbox == mp_core::selector::DRAFTS_MAILBOX {
            self.find_mailbox_by_kind(MailboxKind::Drafts)
        } else {
            self.mailboxes
                .iter()
                .position(|mb| super::mailbox_key(mb) == mailbox)
        };
        let Some(idx) = idx else {
            self.set_status(format!("{mailbox} is not in the sidebar"));
            return;
        };
        self.pending_select_selector = Some(selector);
        self.focus = Focus::List;
        if idx == self.active_mailbox {
            self.consume_pending_select();
        } else {
            self.sidebar_index = idx;
            self.switch_mailbox(idx);
        }
    }

    /// Whether `entry` is the row `selector` names: the same string, or the
    /// same account and key, which is the message in another copy.
    pub(crate) fn row_is_selector(entry: &EmailEntry, selector: &str) -> bool {
        let Some(own) = entry.selector.as_deref() else {
            return false;
        };
        if own == selector {
            return true;
        }
        match (
            mp_core::selector::parse(own),
            mp_core::selector::parse(selector),
        ) {
            (Ok(own), Ok(wanted)) => own.account == wanted.account && own.key == wanted.key,
            _ => false,
        }
    }
}

/// The JSON a test daemon answers `note.presets` with: the built-in presets.
#[cfg(test)]
pub(crate) fn presets_answer() -> serde_json::Value {
    json!({ "presets": mp_core::config::default_note_presets() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::from(code)
    }

    fn prompt(existing: Option<Note>) -> NotePrompt {
        NotePrompt::new(
            NoteTarget::Rows(vec!["mp://work/INBOX/a@x".to_string()]),
            mp_core::config::default_note_presets(),
            existing,
        )
    }

    fn note(state: NoteState) -> Note {
        Note {
            id: "n1abcdef".to_string(),
            selector: Some("mp://work/INBOX/a@x".to_string()),
            account: "work".to_string(),
            message_id: Some("a@x".to_string()),
            draft_id: None,
            preset: Some("r".to_string()),
            text: "say yes".to_string(),
            state,
            result: (state == NoteState::Done).then(|| "Drafted".to_string()),
            created: String::new(),
            updated: String::new(),
            from: String::new(),
            to: String::new(),
            subject: String::new(),
            orphaned: false,
        }
    }

    /// A preset that needs no text saves on its letter.
    #[test]
    fn a_preset_with_no_text_saves_on_its_letter() {
        let mut p = prompt(None);
        assert_eq!(
            p.key(key(KeyCode::Char('s'))),
            NotePromptOutcome::Save {
                preset: Some("s".to_string()),
                text: String::new()
            }
        );
    }

    /// A preset that needs text waits for a line, then Enter.
    #[test]
    fn a_preset_that_needs_text_waits_for_the_line() {
        let mut p = prompt(None);
        assert_eq!(p.key(key(KeyCode::Char('r'))), NotePromptOutcome::Pending);
        assert_eq!(p.text_prefix(), "note (Reply): ");
        for c in "ok ".chars() {
            p.key(key(KeyCode::Char(c)));
        }
        assert_eq!(
            p.key(key(KeyCode::Enter)),
            NotePromptOutcome::Save {
                preset: Some("r".to_string()),
                text: "ok".to_string()
            }
        );
    }

    /// Space goes to free text, which may not be empty; an unknown letter
    /// is refused; Esc cancels.
    #[test]
    fn space_types_free_text_which_needs_words() {
        let mut p = prompt(None);
        assert!(matches!(
            p.key(key(KeyCode::Char('z'))),
            NotePromptOutcome::Refused(_)
        ));
        assert_eq!(p.key(key(KeyCode::Char(' '))), NotePromptOutcome::Pending);
        assert!(matches!(
            p.key(key(KeyCode::Enter)),
            NotePromptOutcome::Refused(_)
        ));
        p.key(key(KeyCode::Char('x')));
        assert_eq!(
            p.key(key(KeyCode::Enter)),
            NotePromptOutcome::Save {
                preset: None,
                text: "x".to_string()
            }
        );
        assert_eq!(p.key(key(KeyCode::Esc)), NotePromptOutcome::Cancel);
    }

    /// An open note opens on its text for editing; Backspace past its start
    /// goes back to the presets, where Backspace clears it.
    #[test]
    fn an_open_note_is_pre_filled_and_can_be_cleared() {
        let mut p = prompt(Some(note(NoteState::Open)));
        assert_eq!(
            p.stage,
            NoteStage::Text {
                preset: Some("r".to_string()),
                text: "say yes".to_string()
            }
        );
        for _ in 0.."say yes".len() {
            p.key(key(KeyCode::Backspace));
        }
        assert_eq!(p.key(key(KeyCode::Backspace)), NotePromptOutcome::Pending);
        assert_eq!(p.stage, NoteStage::Choose);
        assert_eq!(p.key(key(KeyCode::Backspace)), NotePromptOutcome::Clear);
    }

    /// A done note offers its result, clear, or a new instruction.
    #[test]
    fn a_done_note_offers_clear_or_a_new_instruction() {
        let mut p = prompt(Some(note(NoteState::Done)));
        assert_eq!(p.stage, NoteStage::Choose);
        assert!(p
            .choices()
            .starts_with("done: Drafted  \u{232b} clear  r Reply"));
        assert_eq!(
            p.key(key(KeyCode::Char('e'))),
            NotePromptOutcome::Save {
                preset: Some("e".to_string()),
                text: String::new()
            }
        );
        assert_eq!(p.key(key(KeyCode::Delete)), NotePromptOutcome::Clear);
    }

    /// A row's state is its note's: a draft by id, a message by the key of
    /// its selector in any mailbox, only in its own account.
    #[test]
    fn a_rows_note_is_found_by_its_draft_id_or_its_message_key() {
        let mut open = note(NoteState::Open);
        open.message_id = Some("<a@x>".to_string());
        let mut draft = note(NoteState::HandedOff);
        draft.message_id = None;
        draft.draft_id = Some("d1".to_string());
        let notes = vec![open, draft];
        let mut row = EmailEntry {
            selector: Some("mp://work/Archive/a@x/7".to_string()),
            ..crate::app::entry_from_skip(mp_protocol::draft::DraftSkip {
                path: "x.md".to_string(),
                error: String::new(),
            })
        };
        assert_eq!(note_state_of(&row, "work", &notes), Some(NoteState::Open));
        assert_eq!(note_state_of(&row, "home", &notes), None);
        row.draft_id = Some("d1".to_string());
        assert_eq!(
            note_state_of(&row, "work", &notes),
            Some(NoteState::HandedOff)
        );
    }
}
