use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

use anyhow::{Context, Result};
use serde_json::json;
use ratatui::{backend::CrosstermBackend, Terminal};

use super::app::{
    mailbox_key, Action, App, BgResult, ComposeField, ComposeMode, ComposeWizard, Focus,
    MailboxKind, MessageRef, Overlay, SearchOverlayFocus, StatusLevel,
};
use super::helpers::{
    edit_file, resume_terminal, suspend_terminal,
};
use super::commands;
use super::session::QueryHandle;

use crate::draft::{DraftFromSource, DraftRecipientEdit};
use crate::selector::Selector;

// ---------------------------------------------------------------------------
// Parking a sync behind the background work it cannot run alongside
// ---------------------------------------------------------------------------

/// How many local hits the immediate FTS pass of the search overlay shows
/// before the server leg answers (#0105).
const LOCAL_SEARCH_LIMIT: usize = 50;

/// Whether a fetch or a sync must wait. One gate, named once, because the
/// release condition in the event loop has to be the *same* condition: they
/// drifted apart (`bg_count` here, a mutation counter there), and the 250 ms tick
/// then released the parked action into this refusal about four times a
/// second.
pub(super) fn sync_is_blocked(app: &App) -> bool {
    app.bg_count > 0
}

/// Whether the event loop may hand a parked action back to the dispatcher.
///
/// Literally the negation of [`sync_is_blocked`], plus "nothing else is
/// already queued". Taking the same `&App` rather than the scalars it reads is
/// the point: restating the gate as `bg_count == 0` is how the two drifted
/// apart in the first place, and a call through [`sync_is_blocked`] cannot.
pub(super) fn queued_action_is_releasable(app: &App) -> bool {
    !sync_is_blocked(app) && app.pending_actions.is_empty()
}

/// Park `action` until the running sync or fetch clears, announcing it once.
///
/// Re-parking the same action is silent: the user asked for one sync and one
/// activity-log line is the honest record of that. A different action taking
/// the slot announces itself, because it is a different answer to the user.
///
/// The message no longer counts "ops pending" (#0039): a mutation used to be a
/// background job that a requested sync stacked behind, re-announcing itself on
/// every keypress. Mutations now enqueue silently into the durable queue and
/// block nothing, so the only thing a sync can wait behind is another sync or
/// fetch, and the line says just that.
pub(super) fn park_until_idle(app: &mut App, action: Action, label: &str) {
    let already_parked = app
        .queued_action
        .as_ref()
        .is_some_and(|parked| std::mem::discriminant(parked) == std::mem::discriminant(&action));
    if !already_parked {
        app.set_status(format!("{label} queued (waiting for the current sync)"));
    }
    app.queued_action = Some(action);
}

// ---------------------------------------------------------------------------
// Files materialised out of the store (#0052 scope items 8, 9 and 10)
// ---------------------------------------------------------------------------

/// A file the TUI renders out of a message rather than one the message
/// carries: the browser rendition and the event source.
///
/// It lands in a `render/` subdirectory of the row's materialisation dir
/// rather than beside its attachments: an attachment filename is sanitised of
/// path separators, so it can never name a subdirectory, and a message
/// carrying its own `message.html` cannot overwrite the rendition.
fn render_temp_file(stem: &str, name: &str) -> Result<PathBuf> {
    let dir = crate::parse::materialisation_dir(stem)?.join("render");
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("creating {}", dir.display()))?;
    Ok(dir.join(name))
}

/// The message under the cursor for a flow that reads its blobs, or `None`
/// with the status line saying why there is not one.
///
/// Same shape as [`cursor_message`], different explanation: what reaches here
/// with no store row is no longer a draft (a draft answers from its own
/// `attachments:` list since #0016) but a row with no identity at all -- a
/// parse-skipped draft file (#0080) or a server-search hit that resolved to
/// nothing -- and neither has bytes to materialise.
fn cursor_message_for_files(app: &mut App, what: &str) -> Option<MessageRef> {
    if let Some(msg) = app.selected_email_ref() {
        return Some(msg);
    }
    if app.selected_email().is_some() {
        app.set_status_level(
            format!("{what} needs a message or a readable draft; this row has neither"),
            StatusLevel::Warning,
        );
    }
    None
}

/// The attachments of the row under the cursor, materialised into files
/// (#0052 scope item 8).
///
/// This is `mp open` / `mp save`'s own read: [`materialise_attachments`]
/// writes the row's blobs into the temp directory keyed by the row and hands
/// back the paths. The picker and the save pipeline above it still address
/// files, which they always did; what changed is where the bytes come from.
///
/// An empty vector is a message with no attachments, which is the caller's
/// status line to write. `None` is a failure already on the status line.
pub fn cursor_attachment_files(app: &mut App) -> Option<Vec<PathBuf>> {
    // A draft names its attachments itself, so it answers from its own
    // frontmatter rather than from blobs it has none of (#0016).
    if app.selected_email().is_some_and(|e| e.draft_id.is_some()) {
        return draft_attachment_files(app);
    }
    let msg = cursor_message_for_files(app, "Attachments")?;
    row_attachment_files(app, msg.row_id())
}

/// The attachments of the *draft* under the cursor: the paths in its
/// `attachments:` frontmatter, resolved on disk (#0016).
///
/// Nothing is materialised, because nothing has to be: a draft's attachments
/// are already files, written there by the forward builder (into the stable
/// per-account mirror, #0006) or named by the user in `$EDITOR`. So this is
/// the one attachment source that hands the picker the real paths instead of
/// a private temp copy, and `o` opens the very file that will be sent, which
/// is the point of the key on a draft.
///
/// `~` is expanded the way the send path expands it ([`crate::send`]'s
/// `draft_attachments`), so a draft that sends is a draft that opens.
/// A path that is not there is named rather than skipped silently: a stale
/// entry is precisely what `o` is being pressed to find out about, and it is
/// the failure `mp send` would hit later (see the forwarded-attachment note in
/// `docs/lessons-learned.md`).
fn draft_attachment_files(app: &mut App) -> Option<Vec<PathBuf>> {
    let (_id, path) = cursor_draft(app, "Attachments needs a draft or a received message")?;
    let draft = match crate::draft::parse_email_draft(&path) {
        Ok(draft) => draft,
        Err(e) => {
            app.set_status_level(format!("Attachments failed: {e:#}"), StatusLevel::Error);
            return None;
        }
    };

    let listed = draft.frontmatter.attachments.unwrap_or_default();
    let mut files = Vec::new();
    let mut missing = Vec::new();
    for entry in &listed {
        let expanded = shellexpand::tilde(entry).into_owned();
        let candidate = PathBuf::from(expanded);
        if candidate.is_file() {
            files.push(candidate);
        } else {
            missing.push(entry.clone());
        }
    }

    if !missing.is_empty() {
        let level = if files.is_empty() { StatusLevel::Error } else { StatusLevel::Warning };
        let n = missing.len();
        let noun = if n == 1 { "attachment" } else { "attachments" };
        app.set_status_level(
            format!("{n} {noun} missing: {}", missing.join(", ")),
            level,
        );
        if files.is_empty() {
            return None;
        }
    }
    Some(files)
}

/// Append a file path to the cursor draft's `attachments:` frontmatter (#0098).
///
/// The write counterpart of [`draft_attachment_files`] (#0016): where that
/// resolves and opens the paths a draft already lists, this adds one. It
/// reuses the same [`cursor_draft`] resolution, so the file it writes is the
/// draft under the cursor, and it stores the path verbatim -- `~` and all --
/// so the entry is the very one [`crate::send`]'s `draft_attachments` expands
/// and sends. The path was checked for existence at the prompt (see
/// `App::handle_attach_file_key`), which is where a stale path is surfaced;
/// a write failure here is named on the status line.
fn attach_file_to_draft(app: &mut App, path: &str) {
    let Some((id, draft_path)) = cursor_draft(
        app,
        "Attach needs a draft; received mail has no attachments to grow",
    ) else {
        return;
    };
    match crate::draft::append_draft_attachment(&draft_path, path) {
        Ok(()) => {
            let selector = Selector::for_draft(&app.account_config.name, &id);
            app.set_status(format!("Attached {path} to {selector}"));
            commands::refresh_drafts_after_flip(app);
        }
        Err(e) => {
            app.set_status_level(format!("Attach failed: {e:#}"), StatusLevel::Error);
        }
    }
}

/// [`cursor_attachment_files`] for a row named directly, which is the
/// server-search hit that resolved to one.
///
/// Daemon-routed since P5-U6: `message.get` for the part list and
/// `message.materialise_attachment` per part, so the bytes are written by the
/// process that owns the blobs. The files land in the daemon's handle
/// directory rather than in `parse::materialisation_dir`, which is where `mp
/// open` still puts its own; the picker and the save pipeline address files
/// either way.
pub(super) fn row_attachment_files(app: &mut App, row_id: i64) -> Option<Vec<PathBuf>> {
    let account = app.account_config.name.clone();
    match commands::attachment_files(&daemon_door(app), &account, row_id) {
        Ok(files) => Some(files),
        Err(e) => {
            app.set_status_level(format!("Attachments failed: {e}"), StatusLevel::Error);
            None
        }
    }
}

/// The attachments of a server-search hit that resolved to no local row: the
/// bytes the overlay is already holding, written where the store-backed ones
/// go so the picker sees files either way (#0052 scope item 11).
///
/// Keyed by the hit's position in the result list rather than by a row id it
/// does not have. The filename is sanitised the way ingest sanitises it, so a
/// hostile `../` in a Content-Disposition cannot escape the temp directory.
pub fn fetched_attachment_files(
    app: &mut App,
    fetched: &crate::parse::FetchedEmail,
    index: usize,
) -> Option<Vec<PathBuf>> {
    let dest = match crate::parse::materialisation_dir(&format!("search-{index}")) {
        Ok(dir) => dir,
        Err(e) => {
            app.set_status_level(format!("Attachments failed: {e:#}"), StatusLevel::Error);
            return None;
        }
    };
    match write_fetched_attachments(fetched, &dest) {
        Ok(files) => Some(files),
        Err(e) => {
            app.set_status_level(format!("Attachments failed: {e:#}"), StatusLevel::Error);
            None
        }
    }
}

fn write_fetched_attachments(
    fetched: &crate::parse::FetchedEmail,
    dest: &Path,
) -> Result<Vec<PathBuf>> {
    if fetched.attachments.is_empty() {
        return Ok(Vec::new());
    }
    std::fs::create_dir_all(dest)?;
    let mut written = Vec::new();
    for att in &fetched.attachments {
        let out = dest.join(crate::parse::sanitize_attachment_filename(&att.filename));
        std::fs::write(&out, &att.content)?;
        written.push(out);
    }
    Ok(written)
}

/// The HTML rendition of a message, written to a file a browser can open
/// (#0052 scope item 9).
///
/// The file build wrote a `.html` beside every received `.md` and the browser
/// opened that one; after #0037 the markup is a blob, or the html part of the
/// raw message, so it is materialised on demand into the same temp area the
/// attachments use. `stem` keys the directory: the row id for a stored
/// message, the hit's position for one that is not stored.
fn html_temp_file(html: &str, stem: &str) -> Result<PathBuf> {
    let path = render_temp_file(stem, "message.html")?;
    // The browser reads the file with no HTTP headers to lean on: without a
    // charset it guesses latin-1 and breaks umlauts, and without a CSP a
    // hostile email runs scripts and loads tracking pixels (both pre-#0037
    // fixes, lost with the legacy save path in the store rebuild).
    let html = crate::parse::inject_csp_meta(&crate::parse::ensure_utf8_charset(html));
    std::fs::write(&path, html)
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

/// The browser rendition of a stored row, or `None` with the status line
/// saying why: a message whose sender wrote no markup has none, which is not
/// an error.
///
/// Daemon-routed since P5-U6: `message.materialise_html` writes the very same
/// rendition, charset meta, CSP tag and inlined `cid:` images included, which
/// is why it exists rather than serving raw markup through a new door. A row
/// with no markup is refused, and the refusal is the line this always printed.
pub fn html_rendition_for_row(app: &mut App, row_id: i64) -> Option<PathBuf> {
    let account = app.account_config.name.clone();
    match commands::html_rendition(&daemon_door(app), &account, row_id) {
        Ok(path) => Some(path),
        Err(e) => {
            log::warn!("[actions] no browser rendition for row {row_id}: {e}");
            app.set_status("No HTML version available".to_string());
            None
        }
    }
}

/// [`html_rendition_for_row`] over markup already in hand, which is the
/// server-search hit that resolved to no row.
pub fn html_rendition(app: &mut App, html: &str, stem: &str) -> Option<PathBuf> {
    match html_temp_file(html, stem) {
        Ok(path) => Some(path),
        Err(e) => {
            app.set_status_level(format!("Open failed: {e:#}"), StatusLevel::Error);
            None
        }
    }
}

/// The read-only Markdown view of a stored row, or `None` with the status line
/// saying why there is none (#0075).
///
/// Daemon-routed since P5-U10c (`RD-06`, #0126): `message.materialise_markdown`
/// writes the very same rendition, `store::read::render_markdown` over the
/// same row at mode 0444 under a name slugged from the subject. The file moved
/// with it, from the per-row `parse::materialisation_dir` to the handle
/// directory of the daemon's own runtime, which is where the browser rendition
/// and every materialised attachment already land; both are private, and what
/// the editor is handed is a file either way.
///
/// The caller owes the handle a [`release`](commands::release_rendition) when
/// the editor exits, which is what makes the rendition scratch: it unlinks the
/// directory, where the pre-daemon flow unlinked the file.
///
/// A refusal is the one sentence this has always printed. `-32602` for a row
/// nothing resolves to is the only case a user can meet - a message the store
/// no longer holds - and a transport failure is indistinguishable from it at
/// this seam, which is the reason `draft_path`'s two lines became one.
pub fn readonly_view_for_row(app: &mut App, row_id: i64) -> Option<commands::Rendition> {
    let account = app.account_config.name.clone();
    match commands::markdown_rendition(&daemon_door(app), &account, row_id) {
        Ok(rendition) => Some(rendition),
        Err(e) => {
            log::warn!("[actions] no read-only view for row {row_id}: {e}");
            app.set_status_level(
                "Open failed: that message is no longer in the store".to_string(),
                StatusLevel::Error,
            );
            None
        }
    }
}

/// Parse and validate a draft, and only then persist its approved status.
///
/// `x` merges approve + send (#0092): the redesign dropped the separate
/// approve key, so an unapproved draft is approved as part of the send.
/// Approval runs strictly after the draft parses and validates (#0089): a
/// draft that fails validation keeps its `draft` status instead of carrying an
/// approved flag from a send that never happened. `mark_as_approved` is
/// idempotent, so a draft approved out of band passes through unchanged.
///
/// The draft is re-parsed after the approval write, and that is load-bearing:
/// `mark_as_approved` rewrites `status:` in the file, not in the struct this
/// function already parsed. Returning the pre-approval value handed
/// [`Action::Send`] a copy still reading `status: draft`, which
/// [`crate::send::build_draft_message`] then refused with "Email not approved
/// for sending" even though the file on disk was approved -- the `x`
/// approve-and-send key could never send an unapproved draft.
pub fn validate_then_approve(path: &Path) -> Result<crate::types::EmailDraft> {
    let draft = crate::draft::parse_email_draft(path)?;
    crate::draft::validate_draft(&draft)?;
    crate::draft::mark_as_approved(path)?;
    crate::draft::parse_email_draft(path)
}

/// Hand the read-only view of a stored row to `$EDITOR` and discard it on the
/// way back (#0075).
///
/// Same suspend / launch / resume dance as the draft flow and the event
/// source; what differs is the end, where the file is removed. Nothing read it
/// back, so an edit forced past the read-only buffer reaches nothing, and the
/// status line says so.
fn open_readonly_view(
    app: &mut App,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    row_id: i64,
) -> Result<()> {
    let Some(rendition) = readonly_view_for_row(app, row_id) else {
        return Ok(());
    };
    // Neither half of the terminal dance may return before the rendition is
    // discarded: a terminal that cannot be suspended or restored must not
    // also leave the 0444 file behind for the next open to trip over.
    if let Err(e) = suspend_terminal(terminal) {
        discard_readonly_view(app, &rendition);
        return Err(e);
    }
    let result = edit_file(&rendition.path);
    let resumed = resume_terminal(terminal);
    finish_readonly_view(app, &rendition, result);
    resumed
}

/// Release the read-only view's handle, which is what unlinks it (#0075).
///
/// The release is unconditional: a clean exit, an editor that never launched
/// and one that exited non-zero all land here, because the file is scratch in
/// every case. A refusal is a log line and no more, which is what an expired
/// handle answers and what a user can do nothing about.
fn discard_readonly_view(app: &mut App, rendition: &commands::Rendition) {
    commands::release_rendition(&daemon_door(app), &rendition.handle);
}

/// Discard the read-only view and say how the editor session ended (#0075).
pub fn finish_readonly_view(app: &mut App, rendition: &commands::Rendition, result: Result<()>) {
    discard_readonly_view(app, rendition);
    match result {
        Ok(()) => app.set_status(
            "Returned from the read-only copy (edits do not reach the message)".to_string(),
        ),
        Err(e) => app.set_status_level(format!("Open failed: {e}"), StatusLevel::Error),
    }
}

// ---------------------------------------------------------------------------
// Drafts written from a source message (#0052 scope items 1, 2 and 11)
// ---------------------------------------------------------------------------

/// The message under the cursor, or `None` with the status line saying why
/// there is not one.
///
/// A Drafts row is the case worth naming: it has no `messages` row behind it,
/// so there is nothing to quote, and an empty list is not an error at all.
fn cursor_message(app: &mut App, what: &str) -> Option<MessageRef> {
    if let Some(msg) = app.selected_email_ref() {
        return Some(msg);
    }
    if app.selected_email().is_some() {
        app.set_status_level(
            format!("{what} needs a received message; a draft has none to quote"),
            StatusLevel::Warning,
        );
    }
    None
}

/// Build the draft through the daemon and hand it straight to `$EDITOR`, which
/// is what reply and forward did before the read path moved (the draft is a
/// starting point, not a finished message).
///
/// `draft.reply` / `draft.forward`, addressed by `row_id` (P5-U6): the row
/// read, the `SourceMessage` build and `create_draft_from_source` are one call
/// now, run by the process that owns the blobs, through the very same library
/// functions `mp reply` and `mp forward` run. `headers` is the compose
/// wizard's override of the recipients and the subject it collected before the
/// draft existed.
fn write_draft_and_edit(
    app: &mut App,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    msg: MessageRef,
    kind: DraftFromSource,
    headers: Option<&DraftRecipientEdit>,
    what: &str,
) -> Result<()> {
    let account = app.account_config.name.clone();
    let built = commands::draft_from_source(
        &daemon_door(app),
        &account,
        msg.row_id(),
        kind,
        headers,
    );
    let (path, selector) = match built {
        Ok(pair) => pair,
        Err(e) => {
            app.set_status_level(format!("{what} failed: {e}"), StatusLevel::Error);
            return Ok(());
        }
    };
    edit_new_draft(app, terminal, &path, format!("{what} draft ready: {selector}"))
}

/// [`write_draft_and_edit`] over a message the client already holds, which is
/// the server-search hit that resolved to no local row.
///
/// `draft.create_from_message` since P5-U10d (`DFT-08`, `DFT-09`, #0126): the
/// message is not in the store, so there is no row to address and no `source`
/// this family takes, and the content is the fetch the overlay is rendering.
/// So the message travels instead of an address and the daemon runs the same
/// `mp_core::draft` builder over it that the resolved hit's reply runs.
///
/// Composing this out of `message.fetch` plus `draft.reply` is what P5-U10c-I2
/// was briefed to do and refused: it would ingest the message, move the unread
/// count and turn the hit into a resolved one, which is what the overlay's
/// separate `f` key is for.
fn write_fetched_draft_and_edit(
    app: &mut App,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    fetched: &crate::parse::FetchedEmail,
    kind: DraftFromSource,
    what: &str,
) -> Result<()> {
    let account = app.account_config.name.clone();
    let built = commands::draft_from_message(&daemon_door(app), &account, kind, fetched);
    let (path, selector) = match built {
        Ok(pair) => pair,
        Err(e) => {
            app.set_status_level(format!("{what} failed: {e}"), StatusLevel::Error);
            return Ok(());
        }
    };
    edit_new_draft(app, terminal, &path, format!("{what} draft ready: {selector}"))
}

/// Hand a freshly written draft to `$EDITOR` and put `ready` on the status
/// line, with the list and the sidebar caught up afterwards.
///
/// The recount and the reload on the way out are what shows the subject and
/// the recipients the user just typed: the editor session is a write this
/// application did not make, and both calls answer from the daemon's own fresh
/// scan of the drafts directory (P5-U10d, #0126). There is no index refresh to
/// pay first; the daemon owns that table and refreshes it for the count it
/// serves.
fn edit_new_draft(
    app: &mut App,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    path: &Path,
    ready: String,
) -> Result<()> {
    if let Some(idx) = app.find_mailbox_by_kind(MailboxKind::Drafts) {
        app.invalidate_cache_idx(idx);
    }
    suspend_terminal(terminal)?;
    let result = edit_file(path);
    resume_terminal(terminal)?;
    match result {
        Ok(()) => app.set_status(ready),
        Err(e) => app.set_status_level(format!("Edit failed: {e}"), StatusLevel::Error),
    }
    app.recount_all_mailboxes();
    app.reload_current_mailbox();
    Ok(())
}

/// The draft under the cursor, as its indexed id and the file that id names,
/// or `None` with the status line saying why there is not one.
///
/// `why` is what to say when the cursor is on a received message instead,
/// which every draft-only operation has to answer for itself: after #0037 a
/// received message is a store row, so "the same thing but for mail" does not
/// exist for editing, sending or approving.
pub fn cursor_draft(app: &mut App, why: &str) -> Option<(String, PathBuf)> {
    let id = match app.selected_email() {
        Some(email) => email.draft_id.clone(),
        None => return None,
    };
    let Some(id) = id else {
        app.set_status_level(why.to_string(), StatusLevel::Warning);
        return None;
    };
    let path = indexed_draft_path(app, &id)?;
    Some((id, path))
}

/// The file behind an indexed draft id, or `None` with the status line saying
/// the index no longer holds it.
///
/// `draft.path` since P5-U6, which answers from a fresh scan of the drafts
/// directory the way the store-backed lookup did. It has one refusal where the
/// lookup had two outcomes ("not in the index" and "the index could not be
/// read"), so the second line is gone and its reason is in the log.
pub fn indexed_draft_path(app: &mut App, id: &str) -> Option<PathBuf> {
    let account = app.account_config.name.clone();
    match commands::draft_path(&daemon_door(app), &account, id) {
        Some(path) => Some(path),
        None => {
            app.set_status_level(
                format!("That draft is no longer in the index ({id})"),
                StatusLevel::Error,
            );
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Approve and mark-draft (#0052 scope items 4 and 5)
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Send (#0052 scope item 3)
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Mutation plumbing (#0038 scope item 7)
// ---------------------------------------------------------------------------

/// A blocking door onto the daemon that a `&mut App` can be held across.
///
/// The `App` owns the `Session` and every action arm needs the `App` mutably,
/// so a borrow of the session cannot outlive the call it is made for. A
/// [`QueryHandle`] is a clone of the session's call channel: it starts no
/// second connection, keeps no session alive, and answers like any other closed
/// door when there is no session at all, which is a wedged session thread and
/// nothing else in a real run.
fn daemon_door(app: &App) -> QueryHandle {
    app.session
        .as_ref()
        .map(|session| session.handle())
        .unwrap_or_else(QueryHandle::closed)
}

/// The canonical `mp://` selector of the entry under the cursor (#0050 scope
/// item 7), or `None` when the entry has no name to copy.
///
/// The row carries it (`RD-07`, #0126): `message.list` and `draft.list` both
/// render the selector daemon-side and `entry_from_row` puts it on the entry,
/// so `y` costs neither a round trip nor a store read. It was one indexed
/// lookup per keypress until this unit, because [`MessageRef`] is deliberately
/// just the synthetic key and the listing carried neither the Message-ID nor
/// the mailbox the selector needs.
///
/// `None` is the row with no name to copy: the server-search hit that does not
/// resolve locally, which has no `messages` row for a selector to point at,
/// and the parse-skipped draft file, which has no `id:` to be named by.
fn selected_selector(app: &App) -> Option<String> {
    app.selected_email()?.selector.clone()
}

// The server half of a mutation is no longer fired from here (#0039): a
// mutation enqueues its op through `mutations::queue_*` and the durable
// `pending_ops` drain retires it at the next sync/fetch resume point, rolling
// back a refusal itself. So this module keeps no backend resolver, no per-op
// dispatch thread and no rollback of its own: the queue owns all three.

pub(super) fn handle_action(
    app: &mut App,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    action: Action,
    bg_tx: &mpsc::Sender<BgResult>,
) -> Result<()> {
    // The command layer first (P5-U6): the fifteen actions that are one or
    // more daemon calls and no terminal are handled there, and `true` means
    // this function owes them nothing. The door is taken by value because
    // `dispatch` needs `&mut App` and a borrow of the session inside it would
    // collide; a handle is a clone of the call channel and nothing more.
    let door = daemon_door(app);
    if commands::dispatch(app, &door, &action) {
        return Ok(());
    }
    match action {
        Action::EditCurrent => {
            // One key, two things to open, because the row under the cursor is
            // one of two things.
            //
            // A received row is materialised out of the store as Markdown and
            // opened read-only (#0075): the pre-nuke build handed `$EDITOR`
            // the message's own `.md`, and #0037 deleted the files without
            // replacing what they were good for -- searching, folding and
            // yanking a long message in a real editor rather than in a pane.
            // The store stays truth, so the copy is 0444 and discarded on the
            // way back.
            //
            // A drafts row is `mp edit <selector>` done in-process (#0052
            // scope item 7): resolved through the index and handed to
            // `$EDITOR` writable, with the index refreshed afterwards so the
            // list shows what the user just typed.
            if let Some(msg) = app.selected_email_ref() {
                // Opening a message is the explicit read (#0110). Marked
                // before the terminal is handed to `$EDITOR` so the list the
                // user comes back to already shows the new state, and before
                // any reload can read the row back unread (see
                // lessons-learned, the #0004 ordering trap).
                commands::mark_open_read(app, &door, msg);
                return open_readonly_view(app, terminal, msg.row_id());
            }
            // A parse-skipped draft (#0080) has no index row to resolve, so it
            // opens its raw file writable: the whole point of the error row is
            // to let the user reach the broken YAML and fix it. The index
            // refresh on the way out then lists it as a normal draft.
            if let Some(skip) = app.selected_email().and_then(|e| e.skip.clone()) {
                edit_new_draft(
                    app,
                    terminal,
                    std::path::Path::new(&skip.path),
                    "Returned from editor".to_string(),
                )?;
                return Ok(());
            }
            let Some((_id, path)) = cursor_draft(app, "Open in $EDITOR needs a message or a draft")
            else {
                return Ok(());
            };
            edit_new_draft(app, terminal, &path, "Returned from editor".to_string())?;
        }
        Action::Reply(reply_all) => {
            let what = if reply_all { "Reply-all" } else { "Reply" };
            let Some(msg) = cursor_message(app, what) else {
                return Ok(());
            };
            write_draft_and_edit(
                app,
                terminal,
                msg,
                DraftFromSource::Reply { all: reply_all },
                None,
                what,
            )?;
        }
        Action::Send => {
            // `send.draft` with the hold the daemon owns (P6-U2). The draft is
            // resolved through the index and approved the way `x` has always
            // approved it -- a write to a file this client is looking at --
            // and everything after that is one method call: the build, the
            // outbox row, the transport and the draft file's fate are the
            // daemon's, and so is the undo window.
            let Some((id, path)) = cursor_draft(
                app,
                "Send needs a draft; received mail has nothing to send",
            ) else {
                return Ok(());
            };
            if let Err(e) = validate_then_approve(&path) {
                app.set_status_level(format!("Send failed: {e:#}"), StatusLevel::Error);
                return Ok(());
            }
            // Which account sends it is the draft's own `from:`, not the open
            // mailbox: a draft written for another configured account leaves
            // through that account's credentials, and `send.draft` resolves
            // the draft under the account it is told.
            let (account_index, smtp, _imap, graph, account_config, _signature) =
                super::helpers::resolve_send_account(app, &path);
            // The account's `auth_method` decides the transport, not which
            // config happened to load. The check stays here because its
            // refusal is this key's sentence, named before an operation is
            // started for work that cannot happen.
            if let Err(missing) =
                super::helpers::resolve_send_transport(&account_config, graph, smtp)
            {
                app.set_status_level(missing.to_string(), StatusLevel::Error);
                return Ok(());
            }
            commands::start_draft_send(app, &door, &account_config.name, account_index, &id);
        }
        // `draft.create` since P5-U10d (#0126), which `ACTION_ROUTING` has
        // claimed since the table was written: minting the id, writing the
        // skeleton with the account's `default_from` and signature already in
        // it, and the `.md` suffixing are the daemon's, because the daemon
        // owns the drafts directory the file lands in.
        //
        // One refusal where there were two: a name the directory already holds
        // and a write that failed both come back as the method's own sentence,
        // which names the path in the first case.
        Action::NewDraft => {
            let account = app.account_config.name.clone();
            let name = chrono::Local::now()
                .format("draft-%Y%m%d-%H%M%S")
                .to_string();
            match commands::create_draft(&daemon_door(app), &account, &name) {
                Ok(path) => {
                    suspend_terminal(terminal)?;
                    let _ = edit_file(&path);
                    resume_terminal(terminal)?;
                    app.set_status(format!("Created: {name}.md"));
                    if let Some(idx) = app.find_mailbox_by_kind(MailboxKind::Drafts) {
                        app.invalidate_cache_idx(idx);
                    }
                    app.reload_current_mailbox();
                }
                Err(e) => app.set_status_level(format!("New draft failed: {e}"), StatusLevel::Error),
            }
        }

        Action::CopyMessageRef => match selected_selector(app) {
            Some(text) => {
                match super::helpers::copy_to_clipboard(&text) {
                    Ok(()) => app.set_status(format!("{text} copied to clipboard")),
                    Err(e) => app.set_status_level(
                        format!("Copy failed: {e}"),
                        StatusLevel::Error,
                    ),
                }
            }
            None => app.set_status_level(
                "That message is not in the local store, so it has no selector yet".to_string(),
                StatusLevel::Warning,
            ),
        },
        Action::OpenLogFile => match crate::config::latest_log_file() {
            Some(path) => {
                suspend_terminal(terminal)?;
                let result = edit_file(&path);
                resume_terminal(terminal)?;
                match result {
                    Ok(()) => app.set_status("Returned from log file".to_string()),
                    Err(e) => app.set_status_level(
                        format!("Open log failed: {e}"),
                        StatusLevel::Error,
                    ),
                }
            }
            None => app.set_status_level(
                format!(
                    "No log file found in {}",
                    crate::config::logs_dir().display()
                ),
                StatusLevel::Warning,
            ),
        },

        Action::OpenConfigFile => {
            let path = crate::config::config_path();
            if path.exists() {
                suspend_terminal(terminal)?;
                let result = edit_file(&path);
                resume_terminal(terminal)?;
                match result {
                    // Theme and other settings are read once at startup
                    // (theme is an `OnceLock`), so there is no hot-reload.
                    Ok(()) => app.set_status(
                        "Config saved \u{2014} restart mailypoppins to apply changes".to_string(),
                    ),
                    Err(e) => app.set_status_level(
                        format!("Open config failed: {e}"),
                        StatusLevel::Error,
                    ),
                }
            } else {
                app.set_status_level(
                    format!(
                        "Config file not found at {}. Run `mp config init` to create it.",
                        path.display()
                    ),
                    StatusLevel::Warning,
                );
            }
        }

        Action::OpenAttachment(path) => match crate::parse::open_file_with_system(&path) {
            Ok(()) => {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| path.display().to_string());
                app.set_status(format!("Opened: {name}"));
            }
            Err(e) => {
                app.set_status_level(format!("Open failed: {e}"), StatusLevel::Error);
            }
        },

        Action::SaveAttachments { sources, dest_dir } => {
            let mut saved = 0usize;
            let mut failed = 0usize;
            for source in &sources {
                match crate::parse::save_attachment(source, &dest_dir) {
                    Ok(_) => saved += 1,
                    Err(e) => {
                        log::warn!("Save attachment failed for {}: {e}", source.display());
                        failed += 1;
                    }
                }
            }
            app.last_save_dir = Some(dest_dir.clone());
            if failed == 0 {
                let dir_display = dest_dir.display();
                app.set_status_level(
                    format!("Saved {} file(s) to {dir_display}", saved),
                    StatusLevel::Success,
                );
            } else {
                app.set_status_level(
                    format!("Saved {}/{} file(s) ({} failed)", saved, saved + failed, failed),
                    StatusLevel::Warning,
                );
            }
        }

        Action::OpenHtmlInBrowser(path) => match crate::parse::open_file_with_system(&path) {
            Ok(()) => {
                app.set_status("Opened in browser".to_string());
            }
            Err(e) => {
                app.set_status_level(format!("Open failed: {e}"), StatusLevel::Error);
            }
        },

        Action::LoadMailbox { mailbox_idx, generation } => {
            // Background mailbox load (P1 step 2, daemon-backed since P5-U4).
            // Queued by `App::request_mailbox_load` on cache-miss
            // switches/reloads so the load (seconds on large mailboxes) never
            // blocks the UI thread. Follows the `BgResult::IndexReady` pattern:
            // bump `bg_count` (spinner), spawn, deliver via `bg_tx`. The
            // handler in `tui/bg.rs` drops the result if the generation
            // or account/mailbox indices went stale meanwhile.
            //
            // The thread stays: what changed is that it blocks on one
            // `message.list` (or `draft.list`) through a `QueryHandle` instead
            // of on a store open, so the wait is still off the draw thread and
            // the whole-list transfer is paid once per mailbox open, as
            // `docs/baselines/decisions/list-transfer.md` chose.
            let mailbox = match app.mailboxes.get(mailbox_idx) {
                Some(mb) => super::app::mailbox_key(mb),
                None => return Ok(()),
            };
            let account = app.account_config.name.clone();
            let account_index = app.active_account;
            let queries = app.session.as_ref().map(|session| session.handle());
            app.bg_count += 1;
            let tx = bg_tx.clone();
            std::thread::spawn(move || {
                let entries = match queries {
                    Some(queries) => {
                        super::queries::list_emails(&queries, &account, &mailbox).unwrap_or_else(
                            |e| {
                                log::warn!("[queries] listing {account}/{mailbox}: {e:#}");
                                Vec::new()
                            },
                        )
                    }
                    None => {
                        log::warn!(
                            "[queries] no daemon session: {account}/{mailbox} lists empty"
                        );
                        Vec::new()
                    }
                };
                let _ = tx.send(BgResult::MailboxLoaded {
                    account_index,
                    mailbox_idx,
                    generation,
                    entries,
                });
            });
        }

        Action::ServerSearch { query, targets, local_mailbox } => {
            // The account name travels with the search so each hit can be
            // resolved against that account's store (#0038).
            let account = app.account_config.name.clone();
            let generation = app.server_search_generation;

            // Local-first pass (#0105): the FTS index answers in milliseconds,
            // so its hits render before the server round trip starts. A query
            // the local grammar cannot lower (to:/cc:/filename:, mixed OR
            // groups) skips the pass silently; the server covers those terms.
            app.server_search_results.clear();
            app.server_search_index = 0;
            app.server_search_scroll = 0;
            app.server_search_headers_scroll = 0;
            app.server_search_results = commands::local_search(
                &daemon_door(app),
                &account,
                &query,
                local_mailbox.as_deref(),
                LOCAL_SEARCH_LIMIT,
            );
            let local_count = app.server_search_results.len();
            app.server_search_status = Some(if local_count > 0 {
                format!(
                    "{local_count} local hit{}; searching server...",
                    if local_count == 1 { "" } else { "s" }
                )
            } else {
                "Searching server...".to_string()
            });
            if local_count > 0 {
                app.server_search_focus = SearchOverlayFocus::List;
            }

            // The server leg is `message.search_server` since P5-U10c
            // (`LST-08`, #0126): one durable operation on the daemon, which
            // opens the session, splits the budget across the mailboxes and
            // streams one `message.server_hit` per hit, where this spawned a
            // thread and painted the whole batch when the last mailbox
            // answered. The Message-IDs the local pass is already showing
            // travel as `exclude_message_ids`, so the dedup is the daemon's
            // and the count it settles with is a fact any client reproduces.
            app.server_search_loading = true;
            let excluded: Vec<String> = app
                .server_search_results
                .iter()
                .filter_map(|hit| hit.fetched.message_id.clone())
                .collect();
            let mailboxes: Vec<String> = targets
                .iter()
                .map(|target| target.server_name.clone())
                .collect();
            let params = json!({
                "account": account,
                "query": crate::search::to_query_string(&query),
                "mailboxes": mailboxes,
                "exclude_message_ids": excluded,
            });
            let started = commands::start_server_search(app, &daemon_door(app), params, generation);
            if !started {
                app.server_search_loading = false;
            }
        }

        Action::SearchResultOpen
        | Action::SearchResultJump
        | Action::SearchResultYankPath
        | Action::SearchResultReply(_)
        | Action::SearchResultForward
        | Action::SearchResultArchive
        | Action::SearchResultOpenInBrowser => {
            handle_search_result_action(app, terminal, action)?;
        }

        Action::SearchResultFetch => {
            fetch_search_hit(app);
        }

        Action::OpenComposeWizard(mode) => {
            open_compose_wizard(app, mode);
        }

        Action::ComposeToContact { to } => {
            open_compose_wizard_seeded(app, to);
        }

        Action::SendContactVcard { contact } => {
            send_contact_as_vcard(app, terminal, &contact)?;
        }

        Action::CopyContactEmail { address } => {
            match super::helpers::copy_to_clipboard(&address) {
                Ok(()) => app.set_status(format!("{address} copied to clipboard")),
                Err(e) => app.set_status_level(format!("Copy failed: {e}"), StatusLevel::Error),
            }
        }

        Action::OpenEventSource { msg } => {
            // The agenda row carries its own [`MessageRef`] (the invite may
            // live in any mailbox of the account), so this does not go through
            // the mail cursor like `Action::EditCurrent`.
            //
            // What `$EDITOR` gets is the invite's `.ics` blob written to a temp
            // file (#0052 scope item 10), where the file build handed it the
            // message's `.md`. Edits to that copy reach nothing, which is why
            // `Action::EditCurrent` declines on a received row rather than
            // doing the same thing: this flow is inspecting an artifact the
            // message carries, not composing, and the `.ics` is worth reading.
            let row_id = msg.row_id();
            // `message.ics` on the session the `App` holds (#0126): the bytes
            // are the row's `invite.ics` blob either way, and the read that
            // opened a store here was the action layer's last one.
            let Some(ics) = app.load_message_ics(msg) else {
                app.set_status_level(
                    "That event has no ics source in the store".to_string(),
                    StatusLevel::Warning,
                );
                return Ok(());
            };
            let path = match render_temp_file(&row_id.to_string(), "invite.ics") {
                Ok(path) => path,
                Err(e) => {
                    app.set_status_level(format!("Open failed: {e:#}"), StatusLevel::Error);
                    return Ok(());
                }
            };
            if let Err(e) = std::fs::write(&path, &ics) {
                app.set_status_level(format!("Open failed: {e}"), StatusLevel::Error);
                return Ok(());
            }
            suspend_terminal(terminal)?;
            let result = edit_file(&path);
            resume_terminal(terminal)?;
            match result {
                Ok(()) => app.set_status(
                    "Returned from the event source (a copy of the ics; edits do not reach the message)"
                        .to_string(),
                ),
                Err(e) => {
                    app.set_status_level(format!("Open failed: {e}"), StatusLevel::Error)
                }
            }
        }
        Action::AttachFileToDraft { path } => {
            attach_file_to_draft(app, &path);
        }
        Action::ComposeWizardCancel => {
            app.close_overlay();
            app.focus = Focus::List;
            app.set_status("Compose cancelled".to_string());
        }

        Action::ComposeEditSignature => {
            edit_compose_signature(app, terminal)?;
        }

        Action::EditSignatureFile { name } => {
            edit_signature_file(app, terminal, &name)?;
        }

        // The twenty-one the command layer owns (P5-U6's fifteen, P5-U8's five
        // operations and P6-U2's cancel). Listed rather than wildcarded so a
        // new action still has to be classified here, and unreachable because
        // `dispatch` answered `true` for every one of them before this match
        // was entered.
        Action::Fetch
        | Action::Sync
        | Action::FetchAccount(_)
        | Action::SendApproved
        | Action::CancelHeldSend
        | Action::Rsvp { .. }
        | Action::Approve
        | Action::BatchApprove(_)
        | Action::MarkDraft
        | Action::BatchMarkDraft(_)
        | Action::Archive
        | Action::Delete
        | Action::BatchArchive(_)
        | Action::BatchDelete(_)
        | Action::BatchDeleteDrafts(_)
        | Action::MoveToMailbox { .. }
        | Action::ToggleRead
        | Action::MarkAsRead(_)
        | Action::BatchToggleRead(_)
        | Action::ToggleFlag
        | Action::RefreshContacts
        | Action::BatchToggleFlag(_) => {
            log::error!("[actions] {action:?} reached handle_action; tui::commands owns it");
        }

        Action::ComposeWizardSubmit => {
            submit_compose_wizard(app, terminal)?;
            // Consume-and-close: `submit_compose_wizard` takes the wizard via
            // `mem::replace` and (unless validation re-opens it) leaves the
            // overlay at `None`. Promote any error queued behind it. Guarded
            // on `Overlay::None`, so the validation re-open path is a no-op.
            app.promote_pending_error();
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Compose wizard handlers
// ---------------------------------------------------------------------------

fn open_compose_wizard(app: &mut App, mode: ComposeMode) {
    // Load contact cache (if any) for the active account.
    let contacts = {
        let root = crate::config::account_dir(&app.account_config.name);
        crate::contacts::load_cache(&root).ok().flatten()
    };

    // The signature names for the Signature field selector (#0106), read from
    // the app-managed signatures directory (#0107).
    let available_signatures = crate::signatures::list();
    // Only auto-select the default when signatures are globally on, so a New
    // draft with `include_signature = false` keeps carrying none (#0099 parity).
    let default_signature = if app.global_config.email.include_signature {
        crate::signatures::default_signature_name(&app.account_config.name)
    } else {
        None
    };

    let (to, cc, bcc, subject, signature_name) = match &mode {
        ComposeMode::New => (
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            default_signature.clone(),
        ),
        // The forward's subject is shown before its draft exists, so it is
        // built by the same rule the draft will use.
        ComposeMode::Forward { msg } => {
            let account = app.account_config.name.clone();
            let subject =
                commands::forward_subject(&daemon_door(app), &account, msg.row_id());
            (
                String::new(),
                String::new(),
                String::new(),
                subject,
                default_signature.clone(),
            )
        }
        ComposeMode::EditDraft { id } => {
            let Some(path) = indexed_draft_path(app, id) else {
                app.focus = Focus::List;
                return;
            };
            match crate::draft::parse_email_draft(&path) {
                Ok(draft) => {
                    // Absent `signature:` frontmatter means the account default.
                    let sig = draft
                        .frontmatter
                        .signature
                        .clone()
                        .or_else(|| default_signature.clone());
                    (
                        draft.frontmatter.to.unwrap_or_default(),
                        draft.frontmatter.cc.unwrap_or_default(),
                        draft.frontmatter.bcc.unwrap_or_default(),
                        draft.frontmatter.subject,
                        sig,
                    )
                }
                Err(e) => {
                    app.set_status_level(format!("Cannot edit draft: {e}"), StatusLevel::Error);
                    app.focus = Focus::List;
                    return;
                }
            }
        }
    };

    // Keep the selection inside the available set; if a draft names a signature
    // no longer configured, fall back to the default (or "(none)").
    let signature_name = signature_name.filter(|n| available_signatures.contains(n));

    app.overlay = Overlay::Compose(ComposeWizard {
        mode,
        to,
        cc,
        bcc,
        subject,
        body: String::new(),
        focus: ComposeField::To,
        signature_initial: signature_name.clone(),
        signature_edited: false,
        signature_name,
        available_signatures,
        suggestions: Vec::new(),
        suggestion_idx: 0,
        contacts,
    });
    app.focus = Focus::ComposeWizard;
    // No suggestions shown until the user types (see recompute_compose_suggestions).
}

/// Open the compose wizard as a new draft pre-seeded with a recipient (#0033,
/// compose-to-contact). Reuses the same overlay + submit path as `n` in the
/// Mail list; the wizard floats above the Contacts view, so on submit/cancel
/// the user returns to Contacts (the overlay is view-agnostic). Starts focus on
/// the Subject field since the recipient is already filled.
fn open_compose_wizard_seeded(app: &mut App, to: String) {
    let contacts = {
        let root = crate::config::account_dir(&app.account_config.name);
        crate::contacts::load_cache(&root).ok().flatten()
    };
    let available_signatures = crate::signatures::list();
    let signature_name = if app.global_config.email.include_signature {
        crate::signatures::default_signature_name(&app.account_config.name)
            .filter(|n| available_signatures.contains(n))
    } else {
        None
    };
    app.overlay = Overlay::Compose(ComposeWizard {
        mode: ComposeMode::New,
        to,
        cc: String::new(),
        bcc: String::new(),
        subject: String::new(),
        body: String::new(),
        focus: ComposeField::Subject,
        signature_initial: signature_name.clone(),
        signature_edited: false,
        signature_name,
        available_signatures,
        suggestions: Vec::new(),
        suggestion_idx: 0,
        contacts,
    });
    app.focus = Focus::ComposeWizard;
}

/// Export a contact to a `.vcf` and attach it to a brand-new draft (#0033,
/// send-contact-as-vCard). The vCard is written into the drafts mailbox's
/// `_vcards/` sidecar dir and referenced by absolute path in the draft's
/// `attachments:` frontmatter (the same plumbing `src/send.rs` already reads).
/// Then hands off to `$EDITOR` exactly like the new-draft flow.
///
/// v1 intentionally targets a *new* draft only: attaching to an existing draft
/// would need a draft picker + an in-place `attachments:` frontmatter rewrite
/// (new machinery beyond the reused new-draft path); deferred per the ticket.
fn send_contact_as_vcard(
    app: &mut App,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    contact: &crate::contacts::Contact,
) -> Result<()> {
    let dir = app.drafts_dir();

    // Write the .vcf into a sidecar dir beside the drafts so it is stable and
    // out of the mailbox listing (which only reads `*.md`).
    let vcard_dir = dir.join("_vcards");
    if let Err(e) = std::fs::create_dir_all(&vcard_dir) {
        app.set_status_level(format!("vCard dir failed: {e}"), StatusLevel::Error);
        return Ok(());
    }
    let stem = crate::contacts::vcard_file_stem(contact);
    let mut vcf_path = vcard_dir.join(format!("{stem}.vcf"));
    let mut counter = 1usize;
    while vcf_path.exists() {
        vcf_path = vcard_dir.join(format!("{stem}-{counter}.vcf"));
        counter += 1;
    }
    let vcard = crate::contacts::contact_to_vcard(contact);
    if let Err(e) = std::fs::write(&vcf_path, vcard) {
        app.set_status_level(format!("vCard write failed: {e}"), StatusLevel::Error);
        return Ok(());
    }

    // Create a new draft addressed to the contact with the .vcf attached.
    let recipient = mp_core::addresses::format_recipient(&contact.display_name, &contact.address);
    let subject = format!("Contact: {}", vcard_display_name(contact));
    let path = match write_vcard_draft(app, &dir, &recipient, &subject, &vcf_path) {
        Ok(p) => p,
        Err(e) => {
            app.set_status_level(format!("Draft creation failed: {e}"), StatusLevel::Error);
            return Ok(());
        }
    };

    if let Some(idx) = app.find_mailbox_by_kind(MailboxKind::Drafts) {
        app.invalidate_cache_idx(idx);
    }

    // Hand off to $EDITOR (matches the new-draft flow).
    suspend_terminal(terminal)?;
    let edit_result = edit_file(&path);
    resume_terminal(terminal)?;
    match edit_result {
        Ok(()) => {
            app.set_status(format!("vCard draft: {}", recipient));
        }
        Err(e) => app.set_status_level(format!("Edit failed: {e}"), StatusLevel::Error),
    }
    app.reload_current_mailbox();
    Ok(())
}

/// The name shown in the vCard-draft subject: display name if any, else the
/// address local-part.
fn vcard_display_name(contact: &crate::contacts::Contact) -> String {
    if contact.display_name.trim().is_empty() {
        contact
            .address
            .split('@')
            .next()
            .unwrap_or(&contact.address)
            .to_string()
    } else {
        contact.display_name.trim().to_string()
    }
}

/// Write a new draft addressed to `recipient` with `vcf_path` in the
/// `attachments:` frontmatter list. Mirrors `write_new_draft_from_wizard`'s
/// frontmatter shape plus a single attachment entry.
fn write_vcard_draft(
    app: &App,
    dir: &std::path::Path,
    recipient: &str,
    subject: &str,
    vcf_path: &std::path::Path,
) -> Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let default_from = app
        .smtp_config
        .as_ref()
        .map(|s| s.default_from.clone())
        .unwrap_or_else(|| app.account_config.default_from.clone());
    let from = default_from.as_str();
    let now = chrono::Utc::now().to_rfc2822();

    let slug = slugify_subject_for_filename(subject);
    let timestamp = chrono::Local::now().format("%Y-%m-%d-%H%M%S");
    let base_name = if slug.is_empty() {
        format!("draft-{timestamp}.md")
    } else {
        format!("draft-{timestamp}-{slug}.md")
    };
    let mut path = dir.join(&base_name);
    let mut counter = 1usize;
    while path.exists() {
        path = dir.join(format!("draft-{timestamp}-{slug}-{counter}.md"));
        counter += 1;
    }

    let mut fm = String::from("---\n");
    fm.push_str(&format!("to: {}\n", yaml_escape(recipient)));
    fm.push_str("cc:\n");
    fm.push_str("bcc:\n");
    fm.push_str(&format!("subject: {}\n", yaml_escape(subject)));
    fm.push_str("status: draft\n");
    fm.push_str(&format!("from: \"{from}\"\n"));
    fm.push_str(&format!("date: {now}\n"));
    fm.push_str("reply_to:\n");
    fm.push_str("attachments:\n");
    fm.push_str(&format!(
        "  - \"{}\"\n",
        vcf_path.display().to_string().replace('"', "\\\"")
    ));
    fm.push_str("---\n\n");

    std::fs::write(&path, fm)?;
    Ok(path)
}

/// Edit the compose wizard's selected signature in `$EDITOR` (#0106).
///
/// Since #0107 a signature is exactly one file, `signatures/<name>.md`, so the
/// edit is always in place and always persistent: no temp copy, no per-draft
/// override.
///
/// A successful edit is recorded on the wizard ([`note_compose_signature_edited`]).
/// The submit path re-resolves the file, but on an `EditDraft` it only
/// re-splices when something changed, and the selection did not: without the
/// flag the draft would keep the pre-edit block while the status line claimed
/// the signature was updated.
fn edit_compose_signature(
    app: &mut App,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
) -> Result<()> {
    let Some(wizard) = app.compose_wizard() else {
        return Ok(());
    };
    let Some(name) = wizard.signature_name.clone() else {
        app.set_status("No signature selected to edit".to_string());
        return Ok(());
    };
    if wizard.available_signatures.is_empty() {
        app.set_status("No signatures yet".to_string());
        return Ok(());
    }

    // The file may be missing if the selection went stale (renamed or deleted
    // from elsewhere); create it rather than dropping the user into $EDITOR on
    // a path that will not save.
    if !crate::signatures::exists(&name) {
        if let Err(e) = crate::signatures::write(&name, "") {
            app.set_status_level(format!("Cannot open signature: {e:#}"), StatusLevel::Error);
            return Ok(());
        }
    }
    let path = crate::signatures::signature_file(&name);

    suspend_terminal(terminal)?;
    let edit_result = edit_file(&path);
    resume_terminal(terminal)?;

    match edit_result {
        Ok(()) => {
            note_compose_signature_edited(app);
            app.set_status(format!("Signature '{name}' edited"));
        }
        Err(e) => app.set_status_level(format!("Edit failed: {e}"), StatusLevel::Error),
    }
    Ok(())
}

/// Record on the open compose wizard that its signature file was edited
/// (#0107), so an `EditDraft` submit re-splices the block even though the
/// selected name never moved.
fn note_compose_signature_edited(app: &mut App) {
    if let Some(wizard) = app.compose_wizard_mut() {
        wizard.signature_edited = true;
    }
}

/// Edit one app-managed signature file in `$EDITOR` (#0107).
///
/// The signatures overlay's `e`, and the tail of its `n`. Same suspend /
/// restore dance as [`edit_compose_signature`]; what differs is the caller,
/// which stays open underneath, so the overlay is refreshed on the way out
/// (the file may only now exist, or may have gained the content that decides
/// whether the account signature resolves at all).
fn edit_signature_file(
    app: &mut App,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    name: &str,
) -> Result<()> {
    if let Err(e) = crate::signatures::validate_name(name) {
        app.set_status_level(format!("Cannot open signature: {e:#}"), StatusLevel::Error);
        return Ok(());
    }
    // Missing means the selection went stale (renamed or deleted elsewhere):
    // create it rather than dropping the user into `$EDITOR` on a path whose
    // parent directory may not exist.
    if !crate::signatures::exists(name) {
        if let Err(e) = crate::signatures::write(name, "") {
            app.set_status_level(format!("Cannot open signature: {e:#}"), StatusLevel::Error);
            return Ok(());
        }
    }
    let path = crate::signatures::signature_file(name);

    suspend_terminal(terminal)?;
    let edit_result = edit_file(&path);
    resume_terminal(terminal)?;

    match edit_result {
        Ok(()) => app.set_status(format!("Signature '{name}' edited")),
        Err(e) => app.set_status_level(format!("Edit failed: {e}"), StatusLevel::Error),
    }

    app.refresh_signature_content();
    if let Overlay::Signatures(overlay) = &mut app.overlay {
        overlay.refresh();
        overlay.select(name);
    }
    Ok(())
}

fn submit_compose_wizard(
    app: &mut App,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
) -> Result<()> {
    let Overlay::Compose(mut wizard) =
        std::mem::replace(&mut app.overlay, Overlay::None)
    else {
        return Ok(());
    };
    app.focus = Focus::List;

    // Normalize the address fields: strip the trailing `, ` that
    // `accept_suggestion` leaves behind for further typing, plus any other
    // trailing separators. Otherwise mailparse::addrparse sees an empty
    // entry at the end and the send fails.
    wizard.to = normalize_recipient_field(&wizard.to);
    wizard.cc = normalize_recipient_field(&wizard.cc);
    wizard.bcc = normalize_recipient_field(&wizard.bcc);
    wizard.subject = wizard.subject.trim().to_string();

    // Basic validation: must have at least one recipient across to/cc/bcc.
    if wizard.to.is_empty() && wizard.cc.is_empty() && wizard.bcc.is_empty() {
        app.set_status_level(
            "Cannot submit: no recipients (to/cc/bcc all empty)".to_string(),
            StatusLevel::Error,
        );
        // Re-open the wizard so the user can fix the field.
        app.overlay = Overlay::Compose(wizard);
        app.focus = Focus::ComposeWizard;
        return Ok(());
    }

    let edit = DraftRecipientEdit {
        to: wizard.to.clone(),
        cc: wizard.cc.clone(),
        bcc: wizard.bcc.clone(),
        subject: wizard.subject.clone(),
    };

    // Editing an existing draft's recipients/subject rewrites the file in
    // place and does NOT open $EDITOR -- the whole point is a quick,
    // fuzzy-finder edit of the header fields.
    if let ComposeMode::EditDraft { id } = &wizard.mode {
        let id = id.clone();
        let Some(path) = indexed_draft_path(app, &id) else {
            return Ok(());
        };
        match crate::draft::rewrite_draft_recipients(&path, &edit) {
            Ok(()) => {
                // Re-splice the signature block only when the selection changed
                // from what the wizard opened with (#0106) or the selected file
                // was edited from the wizard (#0107), so a plain recipient edit
                // leaves the body untouched.
                let sig_changed = wizard.signature_needs_respice();
                if sig_changed {
                    let (sig_md, sig_name) = wizard_signature(app, &wizard);
                    if let Err(e) = crate::draft::rewrite_draft_signature(
                        &path,
                        sig_md.as_deref(),
                        sig_name.as_deref(),
                    ) {
                        app.set_status_level(
                            format!("Signature update failed: {e}"),
                            StatusLevel::Error,
                        );
                    }
                }
                let account = app.account_config.name.clone();
                // The file changed, so the cached listing is stale; the
                // reload reads it back through `draft.list`, which scans the
                // directory fresh, and the selector is the draft's own id
                // either way.
                if let Some(idx) = app.find_mailbox_by_kind(MailboxKind::Drafts) {
                    app.invalidate_cache_idx(idx);
                }
                app.reload_current_mailbox();
                let msg = if sig_changed {
                    format!(
                        "Recipients and signature updated: {}",
                        Selector::for_draft(&account, &id)
                    )
                } else {
                    format!("Recipients updated: {}", Selector::for_draft(&account, &id))
                };
                app.set_status(msg);
            }
            Err(e) => {
                app.set_status_level(format!("Recipient update failed: {e}"), StatusLevel::Error);
            }
        }
        return Ok(());
    }

    // A forward keeps the wizard's recipients and subject over the ones the
    // builder derived, which is the whole reason it asks for them first.
    if let ComposeMode::Forward { msg } = wizard.mode {
        return write_draft_and_edit(
            app,
            terminal,
            msg,
            DraftFromSource::Forward,
            Some(&edit),
            "Forward",
        );
    }

    let draft_result = match &wizard.mode {
        ComposeMode::New => write_new_draft_from_wizard(app, &wizard),
        ComposeMode::Forward { .. } | ComposeMode::EditDraft { .. } => {
            unreachable!("handled above")
        }
    };

    let path = match draft_result {
        Ok(p) => p,
        Err(e) => {
            app.set_status_level(format!("Draft creation failed: {e}"), StatusLevel::Error);
            return Ok(());
        }
    };

    // Invalidate drafts-mailbox cache so the list reloads on next render.
    if let Some(idx) = app.find_mailbox_by_kind(MailboxKind::Drafts) {
        app.invalidate_cache_idx(idx);
    }

    // A non-empty inline body (#0097) means the draft is complete: it was
    // written into the file by `write_new_draft_from_wizard`, so skip the
    // `$EDITOR` round-trip and land in Drafts directly. An empty body keeps
    // the original behaviour and hands off to `$EDITOR` below.
    if !wizard.body.trim().is_empty() {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string());
        app.set_status(format!("Created: {}", name));
        app.reload_current_mailbox();
        return Ok(());
    }

    // Hand off to $EDITOR.
    suspend_terminal(terminal)?;
    let edit_result = edit_file(&path);
    resume_terminal(terminal)?;
    match edit_result {
        Ok(()) => {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string());
            app.set_status(format!("Created: {}", name));
        }
        Err(e) => app.set_status_level(format!("Edit failed: {e}"), StatusLevel::Error),
    }
    app.reload_current_mailbox();
    Ok(())
}

fn write_new_draft_from_wizard(app: &App, wizard: &ComposeWizard) -> Result<PathBuf> {
    let dir = app.drafts_dir();
    std::fs::create_dir_all(&dir)?;

    let default_from = app
        .smtp_config
        .as_ref()
        .map(|s| s.default_from.clone())
        .unwrap_or_else(|| app.account_config.default_from.clone());
    let from = default_from.as_str();
    let now = chrono::Utc::now().to_rfc2822();

    // Build a unique filename from the subject slug (fall back to timestamp).
    let slug = slugify_subject_for_filename(&wizard.subject);
    let timestamp = chrono::Local::now().format("%Y-%m-%d-%H%M%S");
    let base_name = if slug.is_empty() {
        format!("draft-{timestamp}.md")
    } else {
        format!("draft-{timestamp}-{slug}.md")
    };
    let mut path = dir.join(&base_name);
    let mut counter = 1usize;
    while path.exists() {
        let name = if slug.is_empty() {
            format!("draft-{timestamp}-{counter}.md")
        } else {
            format!("draft-{timestamp}-{slug}-{counter}.md")
        };
        path = dir.join(name);
        counter += 1;
    }

    let mut fm = String::from("---\n");
    fm.push_str(&format!("to: {}\n", yaml_escape(&wizard.to)));
    if !wizard.cc.trim().is_empty() {
        fm.push_str(&format!("cc: {}\n", yaml_escape(&wizard.cc)));
    } else {
        fm.push_str("cc:\n");
    }
    if !wizard.bcc.trim().is_empty() {
        fm.push_str(&format!("bcc: {}\n", yaml_escape(&wizard.bcc)));
    } else {
        fm.push_str("bcc:\n");
    }
    fm.push_str(&format!("subject: {}\n", yaml_escape(&wizard.subject)));
    fm.push_str("status: draft\n");
    fm.push_str(&format!("from: \"{from}\"\n"));
    fm.push_str(&format!("date: {now}\n"));
    fm.push_str("reply_to:\n");
    // The selected signature name (#0106); absent means the account default.
    let (sig_md, sig_name) = wizard_signature(app, wizard);
    if let Some(name) = &sig_name {
        fm.push_str(&format!("signature: {}\n", yaml_escape(name)));
    }
    fm.push_str("attachments:\n");
    fm.push_str("---\n\n");

    // An inline body (#0097) is written straight into the draft so a short
    // message never needs `$EDITOR`. Empty leaves the body blank, which is the
    // signal `submit_compose_wizard` uses to open `$EDITOR` instead.
    let body = wizard.body.trim();
    if !body.is_empty() {
        fm.push_str(body);
        fm.push('\n');
    }

    // The selected signature (#0099, #0106): spliced into the body at creation
    // wrapped in the sentinel comments so a later re-splice can find it, below
    // the inline body when there is one. An empty body still opens `$EDITOR`
    // (that decision reads `wizard.body`, not the file), so the user edits a
    // draft that already carries the signature.
    if let Some(block) = sig_md.as_deref().and_then(crate::draft::wrap_signature_block) {
        if !body.is_empty() {
            fm.push('\n');
        }
        fm.push_str(&block);
    }

    std::fs::write(&path, fm)?;
    Ok(path)
}

/// The effective signature Markdown and name for a compose wizard (#0106):
/// the selected signature file, resolved fresh so an `$EDITOR` edit made from
/// the wizard is picked up (#0107). Honours the global `include_signature`
/// toggle so a New draft written with signatures turned off carries none, as
/// before.
fn wizard_signature(app: &App, wizard: &ComposeWizard) -> (Option<String>, Option<String>) {
    if !app.global_config.email.include_signature {
        return (None, None);
    }
    let name = wizard.signature_name.clone();
    let md = crate::config::resolve_signature_markdown(&app.account_config, name.as_deref());
    (md, name)
}

/// Normalize a recipient-list field on wizard submit:
/// - trim leading/trailing whitespace,
/// - strip trailing commas and whitespace left behind by the
///   "accept suggestion + continue typing" flow,
/// - collapse whitespace inside the separators between recipients.
///
/// Leaves the interior of each recipient alone (including display-name
/// quoting), so `"Doe, Jane" <addr>, bob@x.com, ` becomes
/// `"Doe, Jane" <addr>, bob@x.com`.
fn normalize_recipient_field(s: &str) -> String {
    let trimmed = s.trim();
    // Repeatedly strip any trailing `,` or whitespace chars.
    let cleaned = trimmed.trim_end_matches(|c: char| c == ',' || c.is_whitespace());
    cleaned.to_string()
}

/// YAML double-quote escape for a scalar string value.
fn yaml_escape(s: &str) -> String {
    let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

fn slugify_subject_for_filename(subject: &str) -> String {
    let slug: String = subject
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let slug = crate::types::collapse_hyphens(&slug);
    // Trim to a reasonable length so filenames don't blow up.
    slug.chars().take(40).collect()
}

/// The overlay's own action set: a server-search hit is not a list row, so it
/// has its own Open / Save / Reply / Forward / Archive.
///
/// All of them run off the store for a hit that resolved to a row, and off the
/// fetch the overlay is already rendering for one that did not, with two
/// exceptions. Archive needs a local row to move and says so when there is
/// none. Open needs a stored row to render, because the read-only view is a
/// materialisation of the store (#0075) and a hit that never synced has no row
/// to materialise.
fn handle_search_result_action(
    app: &mut App,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    action: Action,
) -> Result<()> {
    match action {
        Action::SearchResultOpen => {
            // A hit that resolved is opened exactly as the list opens it: the
            // read-only Markdown rendition of its row. One that did not is a
            // message on the server this account has never ingested, so there
            // is nothing to render, and the overlay is already showing the
            // headers and the body that editor window would hold.
            let Some(hit) = app.server_search_results.get(app.server_search_index) else {
                return Ok(());
            };
            let Some(msg) = hit.entry.msg else {
                // The overlay covers the status bar, so the decline goes to
                // the overlay's own footer line (#0104).
                app.server_search_status = Some(
                    "Not in the local store; press f to fetch it first".to_string(),
                );
                return Ok(());
            };
            open_readonly_view(app, terminal, msg.row_id())?;
        }

        Action::SearchResultJump => {
            // Close the overlay and put the list cursor on the hit, the way
            // Apple Mail / Outlook open a search result (#0104).
            let Some(hit) = app.server_search_results.get(app.server_search_index) else {
                return Ok(());
            };
            let Some(msg) = hit.entry.msg else {
                app.server_search_status = Some(
                    "Not in the local store; press f to fetch it first".to_string(),
                );
                return Ok(());
            };
            // The hit names its own mailbox: `source_label` is the sidebar
            // label it was found under, and the sidebar is what turns that
            // into the `messages.mailbox` key. The store read this arm made
            // to learn the same thing went with `RD-07` (#0126).
            let label = app.server_search_results[app.server_search_index]
                .source_label
                .clone();
            let Some(mailbox) = app
                .mailboxes
                .iter()
                .find(|m| m.label == label)
                .map(mailbox_key)
            else {
                app.server_search_status =
                    Some(format!("Cannot open: mailbox {label} is not in the sidebar"));
                return Ok(());
            };
            app.close_overlay();
            app.open_message(msg, &mailbox);
        }

        Action::SearchResultYankPath => {
            // Materialise the read-only Markdown rendition and copy its path
            // (#0104): the store keeps no per-message .md file, this scratch
            // rendition is the file-shaped answer.
            let Some(hit) = app.server_search_results.get(app.server_search_index) else {
                return Ok(());
            };
            let Some(msg) = hit.entry.msg else {
                app.server_search_status = Some(
                    "Not in the local store; press f to fetch it first".to_string(),
                );
                return Ok(());
            };
            let Some(rendition) = readonly_view_for_row(app, msg.row_id()) else {
                app.server_search_status =
                    Some("Yank failed; see the activity log".to_string());
                return Ok(());
            };
            let shown = rendition.path.display().to_string();
            app.server_search_status = match super::helpers::copy_to_clipboard(&shown) {
                Ok(()) => Some(format!("Copied {shown}")),
                Err(e) => Some(format!("Copy failed: {e:#}")),
            };
        }

        Action::SearchResultOpenInBrowser => {
            let Some(hit) = app.server_search_results.get(app.server_search_index) else {
                return Ok(());
            };
            // A hit that resolved reads its markup out of `message_blobs`, the
            // same as the list flow; one that did not has the html part of the
            // fetch the overlay is rendering, and that is what the browser
            // gets rather than a decline.
            let index = app.server_search_index;
            let path = match hit.entry.msg {
                Some(msg) => html_rendition_for_row(app, msg.row_id()),
                None => match hit.fetched.html_body.clone() {
                    Some(html) => html_rendition(app, &html, &format!("search-{index}")),
                    None => {
                        app.set_status("No HTML version available".to_string());
                        None
                    }
                },
            };
            let Some(path) = path else {
                return Ok(());
            };
            match crate::parse::open_file_with_system(&path) {
                Ok(()) => app.set_status("Opened in browser".to_string()),
                Err(e) => app.set_status_level(format!("Open failed: {e}"), StatusLevel::Error),
            }
        }

        Action::SearchResultReply(all) => {
            let what = if all { "Reply-all" } else { "Reply" };
            search_result_draft(app, terminal, DraftFromSource::Reply { all }, what)?;
        }

        Action::SearchResultForward => {
            search_result_draft(app, terminal, DraftFromSource::Forward, "Forward")?;
        }

        Action::SearchResultArchive => {
            let hit = app
                .server_search_results
                .get(app.server_search_index)
                .and_then(|r| r.entry.msg);
            let Some(msg) = hit else {
                app.server_search_status = Some(
                    "Not in the local store; press f to fetch it first".to_string(),
                );
                return Ok(());
            };

            let index = app.server_search_index;
            app.server_search_results.remove(index);
            if app.server_search_index >= app.server_search_results.len()
                && !app.server_search_results.is_empty()
            {
                app.server_search_index = app.server_search_results.len() - 1;
            }

            commands::archive_msgs(app, &daemon_door(app), vec![msg], false);
        }

        _ => {}
    }
    Ok(())
}


/// Reply to or forward the selected server-search hit.
///
/// A hit that resolved to a row is the list flow exactly: same store read,
/// same builder, same draft. A hit that did not resolve has no row, and its
/// content is the fetch the overlay is already rendering, so the draft is
/// built from that rather than declined: the message is in front of the user,
/// and refusing to quote it would be a limitation of the plumbing, not of what
/// is known.
fn search_result_draft(
    app: &mut App,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    kind: DraftFromSource,
    what: &str,
) -> Result<()> {
    let Some(hit) = app.server_search_results.get(app.server_search_index) else {
        return Ok(());
    };
    let msg = hit.entry.msg;
    // Cloned only for the unresolved hit, which is the one case that needs the
    // fetched payload while `app` is borrowed mutably for the status line.
    let fetched = msg.is_none().then(|| hit.fetched.clone());

    // A hit that resolved is the list flow exactly, addressed by the row id it
    // resolved to; one that did not has no row for the daemon to read.
    if let Some(msg) = msg {
        return write_draft_and_edit(app, terminal, msg, kind, None, what);
    }
    let Some(fetched) = fetched else {
        return Ok(());
    };
    write_fetched_draft_and_edit(app, terminal, &fetched, kind, what)
}

/// The `f` key of the search overlay (#0104): ingest a server-only hit.
///
/// `message.fetch` since P5-U10c (`LST-09`, #0126): one durable operation on
/// the daemon, which asks the server for the uid the mailbox holds the message
/// under and ingests the raw bytes it gets back, where this opened an IMAP
/// session on a thread of its own. The Graph/IMAP split went with it, uid
/// derivation included.
///
/// The client keeps its "Already in the local store" line and keeps it as a
/// guard rather than a refusal to match: the method is idempotent, so a hit
/// the overlay has already resolved never costs a round trip, and a client
/// that raced a sync is handed the row rather than an error.
fn fetch_search_hit(app: &mut App) {
    let Some(hit) = app.server_search_results.get(app.server_search_index) else {
        return;
    };
    if hit.entry.msg.is_some() {
        app.server_search_status = Some("Already in the local store".to_string());
        return;
    }
    let Some(message_id) = hit.fetched.message_id.clone() else {
        app.server_search_status =
            Some("This hit carries no Message-ID; cannot fetch it".to_string());
        return;
    };
    // The mailbox the hit came from, as the sidebar label the daemon resolves
    // to both the store key it records the row under and the server name it
    // selects.
    let source_label = hit.source_label.clone();
    if !app.mailboxes.iter().any(|m| m.label == source_label) {
        app.server_search_status = Some(format!(
            "Cannot fetch: mailbox {source_label} is not in the sidebar"
        ));
        return;
    }
    let account = app.account_config.name.clone();
    let generation = app.server_search_generation;
    app.server_search_status = Some("Fetching...".to_string());
    commands::start_hit_fetch(
        app,
        &daemon_door(app),
        json!({
            "account": account,
            "mailbox": source_label,
            "message_id": message_id,
        }),
        generation,
        message_id,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every `edit_file` call site in this file must be reachable only from an
    /// action that `Action::suspends_terminal` returns true for (#0108): the
    /// event drain in `run_loop` stops on such an action so that the keystrokes
    /// still sitting in the tty buffer reach `$EDITOR` rather than being
    /// swallowed by the app.
    ///
    /// The mapping cannot be checked by the compiler, so this is a tripwire on
    /// the count. When it fails, a call site was added or removed: re-derive
    /// which `Action` reaches it, update `Action::suspends_terminal` in
    /// `src/tui/app/types.rs`, then update the number below.
    ///
    /// The needle is assembled at runtime so this test's own source does not
    /// contain it and does not count itself.
    #[test]
    fn edit_file_call_sites_are_accounted_for() {
        let needle: String = ["edit_", "file("].concat();
        let source = include_str!("actions.rs");
        let sites: Vec<usize> = source
            .lines()
            .enumerate()
            .filter(|(_, line)| line.contains(&needle))
            .map(|(i, _)| i + 1)
            .collect();
        assert_eq!(
            sites.len(),
            10,
            "the number of {needle}) call sites changed (now at lines {sites:?}); \
             re-derive the Action::suspends_terminal mapping before updating this count"
        );
    }


    // -----------------------------------------------------------------------
    // Parking a sync: one announcement, and a release that matches the gate
    // -----------------------------------------------------------------------

    /// A parked action is announced when it is parked, and never again. The
    /// event loop re-offers it on every tick until the background work clears,
    /// and each re-offer used to push another activity line: ~4 per second for
    /// as long as the sync ran.
    #[test]
    fn re_parking_the_same_action_does_not_announce_it_again() {
        let mut app = App::default_for_tests();
        app.bg_count = 2;

        park_until_idle(&mut app, Action::Fetch, "Quick sync");
        for _ in 0..40 {
            park_until_idle(&mut app, Action::Fetch, "Quick sync");
        }

        assert_eq!(app.status_log.len(), 1, "one keypress, one activity line");
        assert_eq!(
            app.status_log[0].message,
            "Quick sync queued (waiting for the current sync)"
        );
        assert!(matches!(app.queued_action, Some(Action::Fetch)));
    }

    /// A different action taking the slot is a different answer to the user,
    /// so it says so.
    #[test]
    fn parking_a_different_action_announces_it() {
        let mut app = App::default_for_tests();
        app.bg_count = 1;

        park_until_idle(&mut app, Action::Fetch, "Quick sync");
        park_until_idle(&mut app, Action::Sync, "Full sync");
        park_until_idle(&mut app, Action::Sync, "Full sync");

        assert_eq!(app.status_log.len(), 2);
        assert_eq!(
            app.status_log[1].message,
            "Full sync queued (waiting for the current sync)"
        );
        assert!(matches!(app.queued_action, Some(Action::Sync)));
    }

    /// The release condition and the gate the released action re-enters are
    /// the same condition. A background sync raises `bg_count` without
    /// raising any mutation counter, which is exactly the case the old
    /// mutations-are-idle release got wrong.
    #[test]
    fn a_parked_action_is_released_only_once_the_gate_it_re_enters_has_cleared() {
        let mut app = App::default_for_tests();
        app.bg_count = 1;
        assert!(sync_is_blocked(&app));
        assert!(
            !queued_action_is_releasable(&app),
            "releasing here hands the action straight back into its own refusal"
        );

        app.bg_count = 0;
        assert!(!sync_is_blocked(&app));
        assert!(queued_action_is_releasable(&app));

        app.pending_actions.push_back(Action::Fetch);
        assert!(
            !queued_action_is_releasable(&app),
            "a queue that already holds work waits its turn"
        );
    }

    #[test]
    fn normalize_strips_trailing_comma_and_space() {
        // The exact case the user reported: wizard leaves `, ` dangling after
        // an accepted suggestion.
        let input = "\"Doe, Jane\" <jane@example.com>, ";
        let out = normalize_recipient_field(input);
        assert_eq!(out, "\"Doe, Jane\" <jane@example.com>");
    }

    #[test]
    fn normalize_strips_multiple_trailing_separators() {
        assert_eq!(normalize_recipient_field("a@x.com,,  "), "a@x.com");
        assert_eq!(normalize_recipient_field("a@x.com  , "), "a@x.com");
        assert_eq!(normalize_recipient_field("a@x.com"), "a@x.com");
    }

    #[test]
    fn normalize_preserves_multi_recipient_list() {
        assert_eq!(
            normalize_recipient_field("alice@x.com, bob@x.com, "),
            "alice@x.com, bob@x.com"
        );
    }

    #[test]
    fn normalize_preserves_interior_commas_in_quoted_names() {
        let input = "\"Doe, Jane\" <jane@x.com>, \"Roe, John\" <john@x.com>, ";
        let out = normalize_recipient_field(input);
        assert_eq!(
            out,
            "\"Doe, Jane\" <jane@x.com>, \"Roe, John\" <john@x.com>"
        );
    }

    #[test]
    fn normalize_empty_and_whitespace_only() {
        assert_eq!(normalize_recipient_field(""), "");
        assert_eq!(normalize_recipient_field("   "), "");
        assert_eq!(normalize_recipient_field(", ,  "), "");
    }

    /// End-to-end guarantee: the normalized output must round-trip
    /// through `mailparse::addrparse` without leaving an empty entry,
    /// since that's what actually triggered the send failure in the
    /// bug report.
    #[test]
    fn normalized_field_parses_cleanly_with_mailparse() {
        let cases = [
            "\"Doe, Jane\" <jane@example.com>, ",
            "alice@x.com, bob@x.com, ",
            "Alice <alice@x.com>,",
        ];
        for raw in cases {
            let cleaned = normalize_recipient_field(raw);
            let parsed = mailparse::addrparse(&cleaned)
                .unwrap_or_else(|e| panic!("addrparse failed for {cleaned:?}: {e}"));
            assert!(
                parsed.iter().next().is_some(),
                "no recipients parsed from {cleaned:?}"
            );
            for info in parsed.iter() {
                match info {
                    mailparse::MailAddr::Single(s) => {
                        assert!(
                            !s.addr.trim().is_empty(),
                            "empty address from {cleaned:?}"
                        );
                    }
                    mailparse::MailAddr::Group(g) => {
                        for s in &g.addrs {
                            assert!(
                                !s.addr.trim().is_empty(),
                                "empty group address from {cleaned:?}"
                            );
                        }
                    }
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // Inline compose body (#0097)
    // -----------------------------------------------------------------------

    /// A `New` wizard with the given inline body, pointed at a tempdir so the
    /// draft lands inside the fixture instead of the real Drafts folder.
    fn wizard_with_body(body: &str) -> ComposeWizard {
        ComposeWizard {
            mode: ComposeMode::New,
            to: "alice@example.com".to_string(),
            cc: String::new(),
            bcc: String::new(),
            subject: "Quick note".to_string(),
            body: body.to_string(),
            focus: ComposeField::Body,
            signature_name: None,
            signature_initial: None,
            signature_edited: false,
            available_signatures: Vec::new(),
            suggestions: Vec::new(),
            suggestion_idx: 0,
            contacts: None,
        }
    }

    /// A non-empty inline body is written straight into the draft `.md`, after
    /// the frontmatter fence, so a short message never needs `$EDITOR`.
    #[test]
    fn wizard_body_is_written_into_the_draft_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::default_for_tests();
        app.drafts_dir = Some(dir.path().to_path_buf());

        let wizard = wizard_with_body("Thanks, see you Tuesday.");
        let path = write_new_draft_from_wizard(&app, &wizard).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("subject: \"Quick note\""), "{content}");
        // Body follows the closing frontmatter fence, not the opening one.
        let after_fence = content
            .rsplit_once("---\n\n")
            .map(|(_, tail)| tail)
            .unwrap_or_default();
        assert_eq!(after_fence, "Thanks, see you Tuesday.\n", "{content}");
    }

    /// An empty inline body leaves the draft body blank: the file ends at the
    /// closing fence, which is the signal `submit_compose_wizard` uses to open
    /// `$EDITOR` instead.
    #[test]
    fn wizard_empty_body_leaves_the_draft_body_blank() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::default_for_tests();
        app.drafts_dir = Some(dir.path().to_path_buf());

        let wizard = wizard_with_body("   ");
        let path = write_new_draft_from_wizard(&app, &wizard).unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.ends_with("---\n\n"), "{content:?}");
    }

    // -----------------------------------------------------------------------
    // Editing the signature *file* from the wizard (#0107)
    // -----------------------------------------------------------------------

    /// `e` on the Signature field edits the file in place, so the selected
    /// name never changes; before the `signature_edited` flag an `EditDraft`
    /// submit therefore skipped the re-splice and left the draft carrying the
    /// pre-edit block while the status line claimed the signature was updated.
    ///
    /// The `$EDITOR` round-trip itself cannot run here (it suspends the
    /// terminal), so this drives the two halves the submit path uses: the flag
    /// `edit_compose_signature` sets on success, and the re-splice that flag
    /// gates.
    #[test]
    fn a_signature_file_edit_respices_an_edit_draft_on_submit() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = crate::config::test_env::ConfigDirOverride::new(dir.path());
        cfg.set_config_dir(&dir.path().join("config"));
        let _data = crate::config::test_env::TestDataDir::new();
        crate::signatures::write("work", "-- \nAlice").unwrap();

        let mut app = App::default_for_tests();
        app.account_config.name = "work".to_string();
        app.overlay = Overlay::Compose(ComposeWizard {
            mode: ComposeMode::EditDraft {
                id: "d1".to_string(),
            },
            to: "bob@example.com".to_string(),
            cc: String::new(),
            bcc: String::new(),
            subject: "Re: hello".to_string(),
            body: String::new(),
            focus: ComposeField::Signature,
            signature_name: Some("work".to_string()),
            signature_initial: Some("work".to_string()),
            signature_edited: false,
            available_signatures: vec!["work".to_string()],
            suggestions: Vec::new(),
            suggestion_idx: 0,
            contacts: None,
        });

        // A draft carrying the block as it stood when the wizard opened.
        let path = dir.path().join("draft.md");
        std::fs::write(
            &path,
            "---\nto: bob@example.com\nsubject: \"Re: hello\"\nstatus: draft\n---\n\nThanks.\n",
        )
        .unwrap();
        let opened_with = wizard_signature(&app, app.compose_wizard().unwrap()).0;
        crate::draft::rewrite_draft_signature(&path, opened_with.as_deref(), Some("work")).unwrap();
        assert!(
            std::fs::read_to_string(&path).unwrap().contains("Alice"),
            "the draft starts with the pre-edit block"
        );

        // Nothing has happened yet: a plain recipient edit leaves the body be.
        assert!(!app.compose_wizard().unwrap().signature_needs_respice());

        // `$EDITOR` rewrites the file and `edit_compose_signature` records it.
        crate::signatures::write("work", "-- \nAlice B., Team Lead").unwrap();
        note_compose_signature_edited(&mut app);
        assert!(
            app.compose_wizard().unwrap().signature_needs_respice(),
            "an in-place file edit still has to reach the draft"
        );

        // What the submit path then does with that decision.
        let (sig_md, sig_name) = wizard_signature(&app, app.compose_wizard().unwrap());
        crate::draft::rewrite_draft_signature(&path, sig_md.as_deref(), sig_name.as_deref())
            .unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("Alice B., Team Lead"), "{content}");
        assert!(!content.contains("\nAlice\n"), "{content}");
    }
}
