use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

use anyhow::{Context, Result};
use ratatui::{backend::CrosstermBackend, Terminal};
use serde_json::{json, Value};

use super::app::{
    mailbox_key, Action, App, BgResult, ComposeField, ComposeMode, ComposeWizard, Focus,
    MailboxKind, MessageAddress, MessageRef, Overlay, StatusLevel,
};
use super::commands;
use super::helpers::{edit_file, resume_terminal, suspend_terminal};
use super::session::QueryHandle;

use mp_core::draft::{DraftFromSource, DraftRecipientEdit};
use mp_core::selector::Selector;

// ---------------------------------------------------------------------------
// Parking a sync behind the background work it cannot run alongside
// ---------------------------------------------------------------------------

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
    let dir = mp_core::parse::materialisation_dir(stem)?.join("render");
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
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
/// An empty list is a message with no attachments, which is the caller's
/// status line to write. `None` is a failure already on the status line.
pub fn cursor_attachment_files(app: &mut App) -> Option<crate::app::AttachmentFiles> {
    // A draft names its attachments itself, so it answers from its own
    // frontmatter rather than from blobs it has none of (#0016).
    if app.selected_email().is_some_and(|e| e.draft_id.is_some()) {
        return draft_attachment_files(app).map(crate::app::AttachmentFiles::real);
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
    let draft = match mp_core::draft::parse_email_draft(&path) {
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
        let level = if files.is_empty() {
            StatusLevel::Error
        } else {
            StatusLevel::Warning
        };
        let n = missing.len();
        let noun = if n == 1 { "attachment" } else { "attachments" };
        app.set_status_level(format!("{n} {noun} missing: {}", missing.join(", ")), level);
        if files.is_empty() {
            return None;
        }
    }
    Some(files)
}

/// Append a file path to the cursor draft's `attachments:` frontmatter (#0098),
/// through the daemon's `draft.attach`, the call the desktop's attach makes.
///
/// The write counterpart of [`draft_attachment_files`] (#0016): where that
/// resolves and opens the paths a draft already lists, this adds one. It
/// reuses the same [`cursor_draft`] resolution, so the draft it names is the
/// one under the cursor; the daemon checks the file and writes the entry,
/// `~` kept as the user typed it. The path was checked for existence at the
/// prompt too (see `App::handle_attach_file_key`), which is where a stale path
/// is surfaced first; a refusal here is named on the status line.
fn attach_file_to_draft(app: &mut App, path: &str) {
    let Some((id, _path)) = cursor_draft(
        app,
        "Attach needs a draft; received mail has no attachments to grow",
    ) else {
        return;
    };
    let account = app.account_config.name.clone();
    let attached = daemon_door(app).call(
        "draft.attach",
        json!({"account": account, "id": id, "path": path}),
    );
    match attached {
        Ok(_) => {
            let selector = Selector::for_draft(&account, &id);
            app.set_status(format!("Attached {path} to {selector}"));
            commands::refresh_drafts_after_flip(app);
        }
        Err(e) => {
            let why = match mp_client::session::refusal(&e) {
                Some(refused) => refused.message.clone(),
                None => format!("{e:#}"),
            };
            app.set_status_level(format!("Attach failed: {why}"), StatusLevel::Error);
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
pub(super) fn row_attachment_files(
    app: &mut App,
    row_id: i64,
) -> Option<crate::app::AttachmentFiles> {
    let account = app.account_config.name.clone();
    match commands::attachment_files(&daemon_door(app), &account, &MessageAddress::Row(row_id)) {
        Ok(files) => Some(files),
        Err(e) => {
            app.set_status_level(format!("Attachments failed: {e}"), StatusLevel::Error);
            None
        }
    }
}

/// The search overlay's selected hit as the address every daemon call takes,
/// or `None` with the overlay's own line saying why (PERSO-155).
///
/// A hit that resolved is its row; a server-only one is addressed as `f`
/// addresses it, by its `Message-ID` and the sidebar label it was found under,
/// and the daemon reads it from the server's copy without ingesting it. A hit
/// without a `Message-ID` cannot be asked for.
pub fn selected_hit_address(app: &mut App) -> Option<MessageAddress> {
    let hit = app.server_search_results.get(app.server_search_index)?;
    match (hit.entry.msg, hit.fetched.message_id.clone()) {
        (Some(msg), _) => Some(MessageAddress::Row(msg.row_id())),
        (None, Some(message_id)) => Some(MessageAddress::Hit {
            message_id,
            mailbox: hit.source_label.clone(),
        }),
        (None, None) => {
            app.server_search_status = Some(
                "This hit carries no Message-ID; the server cannot be asked for it".to_string(),
            );
            None
        }
    }
}

/// The attachments of the search overlay's selected hit, a row's or a
/// server-only hit's through the same two calls (PERSO-155), or `None` with
/// the overlay's line saying why: the overlay covers the status bar.
pub fn search_hit_attachment_files(app: &mut App) -> Option<crate::app::AttachmentFiles> {
    let address = selected_hit_address(app)?;
    let account = app.account_config.name.clone();
    match commands::attachment_files(&daemon_door(app), &account, &address) {
        Ok(files) => Some(files),
        Err(e) => {
            app.server_search_status = Some(format!("Attachments failed: {e}"));
            None
        }
    }
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
    let html = mp_core::parse::inject_csp_meta(&mp_core::parse::ensure_utf8_charset(html));
    std::fs::write(&path, html).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

/// The cursor's received message as its `.eml` file, for `te` to hand to the
/// directory picker, or `None` with the status line saying why (PERSO-174).
///
/// A Drafts row has no message to export: the draft is a file already.
pub fn cursor_eml_file(app: &mut App) -> Option<PathBuf> {
    let Some(msg) = app.selected_email_ref() else {
        if app.selected_email().is_some() {
            app.set_status(
                "Export needs a received message; a draft is a file already".to_string(),
            );
        }
        return None;
    };
    let account = app.account_config.name.clone();
    match commands::eml_file(
        &daemon_door(app),
        &account,
        &MessageAddress::Row(msg.row_id()),
    ) {
        Ok(path) => Some(path),
        Err(e) => {
            app.set_status_level(format!("Export failed: {e}"), StatusLevel::Error);
            None
        }
    }
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

/// The browser rendition of a draft, or `None` with the status line saying
/// why (PERSO-136).
///
/// The Markdown file has no markup of its own, but the send renders one, and
/// `draft.html` answers exactly that: the signature the body carries, the
/// configured font, and the quoted original of a reply or a forward. It is
/// written where a server-search hit's markup goes, keyed by the draft id, so
/// a second `tb` on the same draft overwrites the first preview.
pub fn html_rendition_for_draft(app: &mut App, id: &str) -> Option<PathBuf> {
    let account = app.account_config.name.clone();
    match commands::draft_html(&daemon_door(app), &account, id) {
        Ok(html) => {
            // A hand-written `id:` is any string, and this one names a
            // directory.
            let stem: String = id
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                        c
                    } else {
                        '_'
                    }
                })
                .collect();
            html_rendition(app, &html, &format!("draft-{stem}"))
        }
        Err(e) => {
            log::warn!("[actions] no browser rendition for draft {id}: {e}");
            app.set_status_level(format!("Draft preview failed: {e}"), StatusLevel::Error);
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
    let address = MessageAddress::Row(row_id);
    match commands::markdown_rendition(&daemon_door(app), &account, &address) {
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

/// [`readonly_view_for_row`] of any address: a server-only hit's view is the
/// daemon's rendering of the server's copy (PERSO-155), and its failure goes
/// to the search overlay's own line, the only place a hit is shown.
pub fn readonly_view(app: &mut App, address: &MessageAddress) -> Option<commands::Rendition> {
    match address {
        MessageAddress::Row(row_id) => readonly_view_for_row(app, *row_id),
        MessageAddress::Hit { .. } => {
            let account = app.account_config.name.clone();
            match commands::markdown_rendition(&daemon_door(app), &account, address) {
                Ok(rendition) => Some(rendition),
                Err(e) => {
                    app.server_search_status = Some(format!("Open failed: {e}"));
                    None
                }
            }
        }
    }
}

/// Validate a draft, and only then approve it, both through the daemon.
///
/// `x` merges approve + send (#0092): the redesign dropped the separate
/// approve key, so an unapproved draft is approved as part of the send.
/// Approval runs strictly after the draft validates (#0089): a draft that
/// fails validation keeps its `draft` status instead of carrying an approved
/// flag from a send that never happened. `draft.approve` is idempotent, so a
/// draft approved out of band passes through unchanged.
///
/// `draft.validate` then `draft.approve`, the sequence the desktop's send runs
/// before `send.draft`: the draft file is the daemon's to rewrite. `Err` is
/// the sentence the status line shows after `Send failed: `.
pub fn validate_then_approve(
    door: &dyn super::queries::Queries,
    account: &str,
    id: &str,
) -> std::result::Result<(), String> {
    let refused = |e: anyhow::Error| match mp_client::session::refusal(&e) {
        Some(refused) => refused.message.clone(),
        None => format!("{e:#}"),
    };
    let answer = door
        .call("draft.validate", json!({"account": account, "id": id}))
        .map_err(refused)?;
    let validation: mp_protocol::draft::DraftValidation =
        serde_json::from_value(answer).map_err(|e| format!("reading draft.validate: {e}"))?;
    if let Some(report) = validation
        .reports
        .iter()
        .find(|report| report.id == id && !report.valid)
    {
        return Err(report
            .error
            .clone()
            .unwrap_or_else(|| "the draft does not validate".to_string()));
    }
    door.call("draft.approve", json!({"account": account, "id": id}))
        .map_err(refused)?;
    Ok(())
}

/// Hand the read-only view of a stored row to `$EDITOR` and discard it on the
/// way back (#0075).
///
/// Same suspend / launch / resume dance as the draft flow and the event
/// source; what differs is the end, where the file is removed. Nothing read it
/// back, so an edit forced past the read-only buffer reaches nothing, and the
/// status line says so. Answers whether the editor returned cleanly.
fn open_readonly_view(
    app: &mut App,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    address: &MessageAddress,
) -> Result<bool> {
    let Some(rendition) = readonly_view(app, address) else {
        return Ok(false);
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
    let clean = finish_readonly_view(app, &rendition, result);
    resumed.map(|()| clean)
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

/// Discard the read-only view and say how the editor session ended (#0075);
/// answers whether the editor returned cleanly.
pub fn finish_readonly_view(
    app: &mut App,
    rendition: &commands::Rendition,
    result: Result<()>,
) -> bool {
    discard_readonly_view(app, rendition);
    match result {
        Ok(()) => {
            app.set_status(
                "Returned from the read-only copy (edits do not reach the message)".to_string(),
            );
            true
        }
        Err(e) => {
            app.set_status_level(format!("Open failed: {e}"), StatusLevel::Error);
            false
        }
    }
}

/// Where the keyboard lands once `e`'s editor returned cleanly: the list,
/// from the headers or the body pane `e` was pressed in, as the desktop
/// client's reader gives its focus back to the list when the embedded editor
/// exits with code 0 (PERSO-124). The sidebar and the list keep theirs.
pub fn return_to_list_after_edit(app: &mut App) {
    if matches!(app.focus, Focus::Headers | Focus::Preview) {
        app.focus = Focus::List;
    }
}

/// Where the keyboard lands once the editor `r`, `cf` or `cn` opened on the
/// draft it just wrote has ended: the list it was pressed over, as after `e`
/// and as the desktop client after any embedded draft session (PERSO-129).
/// None of the three switches to Drafts, so the list is the current one. An
/// editor that failed keeps the focus where it was.
pub fn land_after_draft_editor(app: &mut App, clean: bool) {
    if clean {
        return_to_list_after_edit(app);
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
///
/// Answers whether the editor returned cleanly; a draft that could not be
/// built opened no editor and answers `false`.
fn write_draft_and_edit(
    app: &mut App,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    address: &MessageAddress,
    kind: DraftFromSource,
    headers: Option<&DraftRecipientEdit>,
    what: &str,
) -> Result<bool> {
    let account = app.account_config.name.clone();
    let built = commands::draft_from_source(&daemon_door(app), &account, address, kind, headers);
    let (path, selector) = match built {
        Ok(pair) => pair,
        Err(e) => {
            app.set_status_level(format!("{what} failed: {e}"), StatusLevel::Error);
            return Ok(false);
        }
    };
    edit_new_draft(
        app,
        terminal,
        &path,
        format!("{what} draft ready: {selector}"),
    )
}

/// A reply to a message the client already holds, which is the server-search
/// hit that resolved to no local row, written and handed to `$EDITOR` as
/// [`write_draft_and_edit`] does.
///
/// `draft.create_from_message` since P5-U10d (`DFT-08`, #0126): the message is
/// not in the store, so there is no row to address and `draft.reply` takes no
/// hit, and the content is the fetch the overlay is rendering. So the message
/// travels instead of an address and the daemon runs the same `mp_core::draft`
/// builder over it that the resolved hit's reply runs. A forward of the same
/// hit is `draft.forward`'s, whose `hit` source carries the server copy's parts
/// (PERSO-155).
///
/// Composing this out of `message.fetch` plus `draft.reply` is what P5-U10c-I2
/// was briefed to do and refused: it would ingest the message, move the unread
/// count and turn the hit into a resolved one, which is what the overlay's
/// separate `f` key is for.
fn write_fetched_reply_and_edit(
    app: &mut App,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    fetched: &mp_core::parse::FetchedEmail,
    all: bool,
    what: &str,
) -> Result<()> {
    let account = app.account_config.name.clone();
    let built = commands::draft_from_message(&daemon_door(app), &account, all, fetched);
    let (path, selector) = match built {
        Ok(pair) => pair,
        Err(e) => {
            app.set_status_level(format!("{what} failed: {e}"), StatusLevel::Error);
            return Ok(());
        }
    };
    edit_new_draft(
        app,
        terminal,
        &path,
        format!("{what} draft ready: {selector}"),
    )?;
    Ok(())
}

/// Hand a freshly written draft to `$EDITOR` and put `ready` on the status
/// line, with the list and the sidebar caught up afterwards; answers whether
/// the editor returned cleanly.
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
) -> Result<bool> {
    if let Some(idx) = app.find_mailbox_by_kind(MailboxKind::Drafts) {
        app.invalidate_cache_idx(idx);
    }
    suspend_terminal(terminal)?;
    let result = edit_file(path);
    resume_terminal(terminal)?;
    let clean = result.is_ok();
    match result {
        Ok(()) => app.set_status(ready),
        Err(e) => app.set_status_level(format!("Edit failed: {e}"), StatusLevel::Error),
    }
    app.recount_all_mailboxes();
    app.reload_current_mailbox();
    Ok(clean)
}

/// The draft under the cursor, as its indexed id and the file that id names,
/// or `None` with the status line saying why there is not one.
///
/// `why` is what to say when the cursor is on a received message instead,
/// which every draft-only operation has to answer for itself: after #0037 a
/// received message is a store row, so "the same thing but for mail" does not
/// exist for editing, sending or approving.
pub fn cursor_draft(app: &mut App, why: &str) -> Option<(String, PathBuf)> {
    let id = app.selected_email()?.draft_id.clone();
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
// `pending_ops` drain retires it, which the daemon runs about 1.5 s after the
// account's last mutation (#0133) or at a sync tick, rolling back a refusal
// itself. So this module keeps no backend resolver, no per-op
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
                if open_readonly_view(app, terminal, &MessageAddress::Row(msg.row_id()))? {
                    return_to_list_after_edit(app);
                }
                return Ok(());
            }
            // A parse-skipped draft (#0080) has no index row to resolve, so it
            // opens its raw file writable: the whole point of the error row is
            // to let the user reach the broken YAML and fix it. The index
            // refresh on the way out then lists it as a normal draft.
            if let Some(skip) = app.selected_email().and_then(|e| e.skip.clone()) {
                if edit_new_draft(
                    app,
                    terminal,
                    std::path::Path::new(&skip.path),
                    "Returned from editor".to_string(),
                )? {
                    return_to_list_after_edit(app);
                }
                return Ok(());
            }
            let Some((_id, path)) = cursor_draft(app, "Open in $EDITOR needs a message or a draft")
            else {
                return Ok(());
            };
            if edit_new_draft(app, terminal, &path, "Returned from editor".to_string())? {
                return_to_list_after_edit(app);
            }
        }
        Action::Reply(reply_all) => {
            let what = if reply_all { "Reply-all" } else { "Reply" };
            let Some(msg) = cursor_message(app, what) else {
                return Ok(());
            };
            let clean = write_draft_and_edit(
                app,
                terminal,
                &MessageAddress::Row(msg.row_id()),
                DraftFromSource::Reply { all: reply_all },
                None,
                what,
            )?;
            land_after_draft_editor(app, clean);
        }
        Action::Send => {
            // `send.draft` with the hold the daemon owns (P6-U2). The draft is
            // resolved through the index, validated and approved through the
            // daemon as the desktop's send does it, and everything after that
            // is one method call: the build, the outbox row, the transport and
            // the draft file's fate are the daemon's, and so is the undo
            // window.
            let Some((id, path)) =
                cursor_draft(app, "Send needs a draft; received mail has nothing to send")
            else {
                return Ok(());
            };
            let drafts_account = app.account_config.name.clone();
            if let Err(e) = validate_then_approve(&door, &drafts_account, &id) {
                app.set_status_level(format!("Send failed: {e}"), StatusLevel::Error);
                return Ok(());
            }
            // Which account sends it is the draft's own `from:`, not the open
            // mailbox: a draft written for another configured account leaves
            // through that account's credentials, and `send.draft` resolves
            // the draft under the account it is told.
            let (account_index, graph, smtp, account_config) =
                super::helpers::resolve_send_account(app, &path);
            // The account's `auth_method` decides the transport, not which
            // credentials happen to be stored. The check stays here because
            // its refusal is this key's sentence, named before an operation
            // is started for work that cannot happen.
            if let Err(missing) =
                super::helpers::resolve_send_transport(&account_config, graph, smtp)
            {
                app.set_status_level(missing.to_string(), StatusLevel::Error);
                return Ok(());
            }
            commands::start_draft_send(app, &door, &account_config.name, account_index, &id);
        }
        Action::CopyMessageRef => match selected_selector(app) {
            Some(text) => {
                let (line, level) = super::clipboard::copy_text(terminal, &text).status(&text);
                app.set_status_level(line, level);
            }
            None => app.set_status_level(
                "That message is not in the local store, so it has no selector yet".to_string(),
                StatusLevel::Warning,
            ),
        },
        Action::OpenLogFile => match mp_core::config::latest_log_file() {
            Some(path) => {
                suspend_terminal(terminal)?;
                let result = edit_file(&path);
                resume_terminal(terminal)?;
                match result {
                    Ok(()) => app.set_status("Returned from log file".to_string()),
                    Err(e) => {
                        app.set_status_level(format!("Open log failed: {e}"), StatusLevel::Error)
                    }
                }
            }
            None => app.set_status_level(
                format!(
                    "No log file found in {}",
                    mp_core::config::logs_dir().display()
                ),
                StatusLevel::Warning,
            ),
        },

        Action::OpenConfigFile => {
            let path = mp_core::config::config_path();
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
                    Err(e) => {
                        app.set_status_level(format!("Open config failed: {e}"), StatusLevel::Error)
                    }
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

        Action::OpenAttachment(path) => match mp_core::parse::open_file_with_system(&path) {
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
                match mp_core::parse::save_attachment(source, &dest_dir) {
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
                    format!(
                        "Saved {}/{} file(s) ({} failed)",
                        saved,
                        saved + failed,
                        failed
                    ),
                    StatusLevel::Warning,
                );
            }
        }

        Action::OpenHtmlInBrowser(path) => match mp_core::parse::open_file_with_system(&path) {
            Ok(()) => {
                app.set_status("Opened in browser".to_string());
            }
            Err(e) => {
                app.set_status_level(format!("Open failed: {e}"), StatusLevel::Error);
            }
        },

        Action::LoadMailbox {
            mailbox_idx,
            generation,
        } => {
            // Background mailbox load (P1 step 2, daemon-backed since P5-U4).
            // Queued by `App::request_mailbox_load` on cache-miss
            // switches/reloads so the load (seconds on large mailboxes) never
            // blocks the UI thread. Follows the `BgResult::IndexReady` pattern:
            // bump `bg_count` (spinner), spawn, deliver via `bg_tx`. The
            // handler in `tui/bg.rs` drops the result if the generation
            // or account/mailbox indices went stale meanwhile.
            //
            // The thread stays: what changed is that it blocks on one
            // `message.list_stream` (or `draft.list`) through a `QueryHandle`
            // instead of on a store open, so the wait is still off the draw
            // thread and the whole-list transfer is paid once per mailbox
            // open, as `docs/baselines/decisions/list-transfer.md` chose. The
            // stream is collected on the session thread (#0138), so the rows
            // still arrive here as one list.
            let mailbox = match app.mailboxes.get(mailbox_idx) {
                Some(mb) => super::app::mailbox_key(mb),
                // No load runs, so none lands to end the stream in flight.
                None => {
                    app.restream_landed(generation);
                    return Ok(());
                }
            };
            let account = app.account_config.name.clone();
            let account_index = app.active_account;
            let queries = app.session.as_ref().map(|session| session.handle());
            // A conversation list (#0142) re-expands what the list on screen
            // has open, so a re-stream does not fold the user's view.
            let expanded = app
                .mailbox_lists_conversations(mailbox_idx)
                .then(|| app.expanded_threads());
            app.bg_count += 1;
            let tx = bg_tx.clone();
            std::thread::spawn(move || {
                let entries = match queries {
                    Some(queries) => super::queries::list_mailbox(
                        &queries,
                        &account,
                        &mailbox,
                        expanded.as_deref(),
                    )
                    .unwrap_or_else(|e| {
                        log::warn!("[queries] listing {account}/{mailbox}: {e:#}");
                        Vec::new()
                    }),
                    None => {
                        log::warn!("[queries] no daemon session: {account}/{mailbox} lists empty");
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

        Action::LoadContacts { generation } => {
            // The active account's whole index, off the UI thread: a search
            // with no index builds one on the daemon, which is seconds on a
            // large mailbox. Lands as `BgResult::ContactsLoaded`.
            let account = app.account_config.name.clone();
            let account_index = app.active_account;
            let queries = app.session.as_ref().map(|session| session.handle());
            app.bg_count += 1;
            let tx = bg_tx.clone();
            std::thread::spawn(move || {
                let result = match queries {
                    Some(queries) => {
                        super::queries::contact_index(&queries, &account).map_err(|e| {
                            match mp_client::session::refusal(&e) {
                                Some(refused) => refused.message.clone(),
                                None => format!("{e:#}"),
                            }
                        })
                    }
                    None => Err("no daemon session".to_string()),
                };
                let _ = tx.send(BgResult::ContactsLoaded {
                    account_index,
                    generation,
                    result,
                });
            });
        }

        Action::ServerSearch {
            query,
            targets,
            local_mailbox,
        } => {
            commands::start_search(
                app,
                &daemon_door(app),
                &query,
                targets.as_deref(),
                local_mailbox.as_deref(),
            );
        }

        Action::SearchResultOpen
        | Action::SearchResultJump
        | Action::SearchResultYankPath
        | Action::SearchResultReply(_)
        | Action::SearchResultForward
        | Action::SearchResultForwardAsAttachment
        | Action::SearchResultExport
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
            let (line, level) = super::clipboard::copy_text(terminal, &address).status(&address);
            app.set_status_level(line, level);
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
            cancel_compose_wizard(app);
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

/// Close the compose wizard without writing anything.
///
/// A forward from the search overlay goes back to it, with the results as
/// they were, as its reply does after the editor (PERSO-155).
pub fn cancel_compose_wizard(app: &mut App) {
    let from_search = matches!(
        &app.overlay,
        Overlay::Compose(wizard) if matches!(wizard.mode, ComposeMode::SearchForward(_))
    );
    app.close_overlay();
    app.focus = Focus::List;
    if from_search {
        app.overlay = Overlay::Search;
    }
    app.set_status("Compose cancelled".to_string());
}

/// The signature names a compose wizard offers and the one it preselects, as
/// the daemon's `signature.list` answers them for the active account (#0106).
///
/// A refusal leaves the selector empty and says why in the log; the wizard
/// still opens. The default is preselected only when signatures are globally
/// on, so a New draft with `include_signature = false` keeps carrying none
/// (#0099 parity).
fn wizard_signatures(app: &App) -> (Vec<String>, Option<String>) {
    let listing = match app.signature_listing(&app.account_config.name) {
        Ok(listing) => listing,
        Err(e) => {
            log::warn!("[actions] signature.list for the compose wizard: {e}");
            return (Vec::new(), None);
        }
    };
    let default = if app.global_config.email.include_signature {
        listing.default.filter(|name| listing.names.contains(name))
    } else {
        None
    };
    (listing.names, default)
}

fn open_compose_wizard(app: &mut App, mode: ComposeMode) {
    // The active account's contact index, for recipient completion: the
    // daemon's, and on its way when the Contacts view has not asked yet.
    let contacts = app.compose_contacts();
    let (available_signatures, default_signature) = wizard_signatures(app);
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
        ComposeMode::Forward { msg, .. } => {
            let account = app.account_config.name.clone();
            let subject = commands::forward_subject(&daemon_door(app), &account, msg.row_id());
            (
                String::new(),
                String::new(),
                String::new(),
                subject,
                default_signature.clone(),
            )
        }
        ComposeMode::SearchForward(forward) => (
            String::new(),
            String::new(),
            String::new(),
            forward.subject.clone(),
            default_signature.clone(),
        ),
        ComposeMode::EditDraft { id } => {
            let Some(path) = indexed_draft_path(app, id) else {
                app.focus = Focus::List;
                return;
            };
            match mp_core::draft::parse_email_draft(&path) {
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
    let contacts = app.compose_contacts();
    let (available_signatures, signature_name) = wizard_signatures(app);
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

/// Send a contact as a vCard (#0033): `contact.vcard_draft` writes a new draft
/// addressed to the contact, with no signature, and the contact's `.vcf`
/// under the drafts directory's `_vcards/`, attached; the desktop's `v` makes
/// the same call. Then hands off to `$EDITOR` exactly like the new-draft flow.
///
/// v1 intentionally targets a *new* draft only: attaching to an existing draft
/// would need a draft picker; deferred per the ticket.
fn send_contact_as_vcard(
    app: &mut App,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    contact: &mp_core::contacts::Contact,
) -> Result<()> {
    let Some(door) = app.queries() else {
        app.set_status_level(
            "vCard draft failed: no daemon session".to_string(),
            StatusLevel::Error,
        );
        return Ok(());
    };
    // The file name the TUI has always given a draft it writes: a timestamp,
    // then the subject's slug.
    let shown = match contact.display_name.trim() {
        "" => contact
            .address
            .split('@')
            .next()
            .unwrap_or(&contact.address),
        name => name,
    };
    let slug = slugify_subject_for_filename(&format!("Contact: {shown}"));
    let timestamp = chrono::Local::now().format("%Y-%m-%d-%H%M%S");
    let name = if slug.is_empty() {
        format!("draft-{timestamp}")
    } else {
        format!("draft-{timestamp}-{slug}")
    };
    let made = door
        .call(
            "contact.vcard_draft",
            json!({
                "account": app.account_config.name,
                "name": name,
                "address": contact.address,
                "display_name": contact.display_name,
            }),
        )
        .and_then(|answer| {
            serde_json::from_value::<mp_protocol::contact::VcardDraft>(answer).map_err(Into::into)
        });
    let made = match made {
        Ok(made) => made,
        Err(e) => {
            let why = match mp_client::session::refusal(&e) {
                Some(refused) => refused.message.clone(),
                None => format!("{e:#}"),
            };
            app.set_status_level(format!("vCard draft failed: {why}"), StatusLevel::Error);
            return Ok(());
        }
    };
    let recipient = mp_core::addresses::format_recipient(&contact.display_name, &contact.address);

    if let Some(idx) = app.find_mailbox_by_kind(MailboxKind::Drafts) {
        app.invalidate_cache_idx(idx);
    }

    // Hand off to $EDITOR (matches the new-draft flow).
    suspend_terminal(terminal)?;
    let edit_result = edit_file(Path::new(&made.draft.path));
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

    // The daemon names the file, and creates it if the selection went stale
    // (renamed or deleted from elsewhere) rather than dropping the user into
    // $EDITOR on a path that will not save.
    let path = match signature_path(app, &name) {
        Ok(path) => path,
        Err(e) => {
            app.set_status_level(format!("Cannot open signature: {e}"), StatusLevel::Error);
            return Ok(());
        }
    };

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

/// The file of the signature `name`, as the daemon's `signature.read` names
/// it, created through `signature.create` when there is none.
///
/// The one door both editor launches take (the compose wizard's `e` and the
/// signatures overlay's `e` and `n`): the signatures directory is the
/// daemon's, and this process only runs the editor on the path it is given.
fn signature_path(app: &App, name: &str) -> std::result::Result<PathBuf, String> {
    let named = json!({ "name": name });
    let file = match app.signature_call("signature.read", named.clone()) {
        Ok(file) => file,
        Err(e) if e.starts_with("no signature named") => {
            app.signature_call("signature.create", named)?
        }
        Err(e) => return Err(e),
    };
    file["path"]
        .as_str()
        .map(PathBuf::from)
        .ok_or_else(|| "the daemon named no file".to_string())
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
    // The daemon names the file. Missing means the selection went stale
    // (renamed or deleted elsewhere): it is created rather than dropping the
    // user into `$EDITOR` on a path whose parent directory may not exist.
    let path = match signature_path(app, name) {
        Ok(path) => path,
        Err(e) => {
            app.set_status_level(format!("Cannot open signature: {e}"), StatusLevel::Error);
            return Ok(());
        }
    };

    suspend_terminal(terminal)?;
    let edit_result = edit_file(&path);
    resume_terminal(terminal)?;

    match edit_result {
        Ok(()) => app.set_status(format!("Signature '{name}' edited")),
        Err(e) => app.set_status_level(format!("Edit failed: {e}"), StatusLevel::Error),
    }

    app.refresh_signatures_overlay();
    if let Overlay::Signatures(overlay) = &mut app.overlay {
        overlay.select(name);
    }
    Ok(())
}

fn submit_compose_wizard(
    app: &mut App,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
) -> Result<()> {
    let Overlay::Compose(mut wizard) = std::mem::replace(&mut app.overlay, Overlay::None) else {
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

    // Editing an existing draft's recipients/subject is the daemon's
    // `draft.set_headers`, which rewrites the file in place; it does NOT open
    // $EDITOR -- the whole point is a quick, fuzzy-finder edit of the header
    // fields.
    if let ComposeMode::EditDraft { id } = &wizard.mode {
        let id = id.clone();
        submit_header_edit(app, &wizard, &id, &edit);
        return Ok(());
    }

    // A forward keeps the wizard's recipients and subject over the ones the
    // builder derived, which is the whole reason it asks for them first.
    if let ComposeMode::Forward { msg, as_attachment } = wizard.mode {
        let clean = write_draft_and_edit(
            app,
            terminal,
            &MessageAddress::Row(msg.row_id()),
            DraftFromSource::Forward { as_attachment },
            Some(&edit),
            "Forward",
        )?;
        land_after_draft_editor(app, clean);
        return Ok(());
    }
    // A forward from the search overlay, of a stored hit or a server-only
    // one, returns to the overlay as its reply does (PERSO-155).
    if let ComposeMode::SearchForward(forward) = &wizard.mode {
        write_draft_and_edit(
            app,
            terminal,
            &forward.address,
            DraftFromSource::Forward {
                as_attachment: forward.as_attachment,
            },
            Some(&edit),
            "Forward",
        )?;
        app.overlay = Overlay::Search;
        return Ok(());
    }

    let draft_result = match &wizard.mode {
        ComposeMode::New => match app.queries() {
            Some(queries) => create_draft_from_wizard(app, queries, &wizard),
            None => Err(anyhow::anyhow!("no daemon session")),
        },
        ComposeMode::Forward { .. }
        | ComposeMode::SearchForward(_)
        | ComposeMode::EditDraft { .. } => {
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
    // written into the file by `draft.create`, so skip the
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
    let clean = edit_result.is_ok();
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
    land_after_draft_editor(app, clean);
    Ok(())
}

/// The file name a new draft from the wizard is asked for: a timestamp, then
/// the subject's slug, then `-<n>` when an earlier try was taken. The daemon
/// adds the `.md`.
fn wizard_draft_name(subject: &str, timestamp: &str, attempt: usize) -> String {
    let slug = slugify_subject_for_filename(subject);
    let base = if slug.is_empty() {
        format!("draft-{timestamp}")
    } else {
        format!("draft-{timestamp}-{slug}")
    };
    if attempt == 0 {
        base
    } else {
        format!("{base}-{attempt}")
    }
}

/// How many names [`create_draft_from_wizard`] tries before it gives up on a
/// drafts directory that already holds every one of them.
const WIZARD_NAME_ATTEMPTS: usize = 100;

/// The `draft.create` parameters a `New` wizard submits: the recipients and
/// the subject as `headers`, the inline body (#0097), and the signature the
/// selector chose (#0106), none when signatures are globally off, and the
/// account default (which the daemon resolves) when the selector is empty.
fn wizard_draft_params(app: &App, wizard: &ComposeWizard, name: &str) -> Value {
    let mut params = json!({
        "account": app.account_config.name,
        "name": name,
        "headers": {
            "to": wizard.to,
            "cc": wizard.cc,
            "bcc": wizard.bcc,
            "subject": wizard.subject,
        },
    });
    if !wizard.body.trim().is_empty() {
        params["body"] = json!(wizard.body.trim());
    }
    if !app.global_config.email.include_signature {
        params["no_signature"] = json!(true);
    } else if let Some(signature) = &wizard.signature_name {
        params["signature"] = json!(signature);
    }
    params
}

/// Write a `New` wizard's draft through the daemon's `draft.create`, as the
/// desktop's compose dialog does, and answer the path it wrote.
///
/// The daemon owns the drafts directory: it mints the id, writes the
/// frontmatter, the body and the signature block, and refuses a name that is
/// taken, in which case the next `-<n>` is tried, the suffix rule the TUI
/// used when it wrote the file itself.
fn create_draft_from_wizard(
    app: &App,
    queries: &dyn super::queries::Queries,
    wizard: &ComposeWizard,
) -> Result<PathBuf> {
    let timestamp = chrono::Local::now().format("%Y-%m-%d-%H%M%S").to_string();
    for attempt in 0..WIZARD_NAME_ATTEMPTS {
        let name = wizard_draft_name(&wizard.subject, &timestamp, attempt);
        match queries.call("draft.create", wizard_draft_params(app, wizard, &name)) {
            Ok(answer) => {
                let created: mp_protocol::draft::DraftCreated = serde_json::from_value(answer)
                    .map_err(|e| anyhow::anyhow!("reading draft.create: {e}"))?;
                return Ok(PathBuf::from(created.path));
            }
            Err(e) => match mp_client::session::refusal(&e) {
                Some(refused) if refused.message.starts_with("A draft already exists") => {}
                Some(refused) => return Err(anyhow::anyhow!("{}", refused.message)),
                None => return Err(e),
            },
        }
    }
    Err(anyhow::anyhow!(
        "every draft name for this subject is taken at {timestamp}"
    ))
}

/// The compose wizard's header edit of the existing draft `id` (`ce`):
/// `draft.set_headers`, the call the desktop's recipients dialog makes, and
/// a status line naming the outcome.
///
/// The signature block is re-spliced only when the selection changed from
/// what the wizard opened with (#0106) or the selected file was edited from
/// the wizard (#0107), so a plain recipient edit leaves the body untouched.
pub fn submit_header_edit(
    app: &mut App,
    wizard: &ComposeWizard,
    id: &str,
    edit: &DraftRecipientEdit,
) {
    let account = app.account_config.name.clone();
    let sig_changed = wizard.signature_needs_respice();
    let params = set_headers_params(
        &account,
        id,
        edit,
        sig_changed.then(|| {
            (
                app.global_config.email.include_signature,
                wizard.signature_name.clone(),
            )
        }),
    );
    match daemon_door(app).call(mp_protocol::draft::METHOD_DRAFT_SET_HEADERS, params) {
        Ok(_) => {
            // The file changed, so the cached listing is stale; the reload
            // reads it back through `draft.list`, which scans the directory
            // fresh, and the selector is the draft's own id either way.
            if let Some(idx) = app.find_mailbox_by_kind(MailboxKind::Drafts) {
                app.invalidate_cache_idx(idx);
            }
            app.reload_current_mailbox();
            let selector = Selector::for_draft(&account, id);
            app.set_status(if sig_changed {
                format!("Recipients and signature updated: {selector}")
            } else {
                format!("Recipients updated: {selector}")
            });
        }
        Err(e) => {
            let why = match mp_client::session::refusal(&e) {
                Some(refused) => refused.message.clone(),
                None => format!("{e:#}"),
            };
            app.set_status_level(
                format!("Recipient update failed: {why}"),
                StatusLevel::Error,
            );
        }
    }
}

/// The `draft.set_headers` parameters of a header edit: all four fields, and,
/// when the signature changed, the re-splice with the wizard's selection as
/// `(include_signature, name)`. With signatures turned off the block is
/// dropped (`no_signature`), and no name means the account default, which is
/// what `draft.create` resolves for a new draft.
fn set_headers_params(
    account: &str,
    id: &str,
    edit: &DraftRecipientEdit,
    signature: Option<(bool, Option<String>)>,
) -> serde_json::Value {
    let mut params = json!({
        "account": account,
        "id": id,
        "headers": {
            "to": edit.to,
            "cc": edit.cc,
            "bcc": edit.bcc,
            "subject": edit.subject,
        },
    });
    if let Some((include, name)) = signature {
        params["splice_signature"] = json!(true);
        if !include {
            params["no_signature"] = json!(true);
        } else if let Some(name) = name {
            params["signature"] = json!(name);
        }
    }
    params
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

fn slugify_subject_for_filename(subject: &str) -> String {
    let slug: String = subject
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let slug = mp_core::types::collapse_hyphens(&slug);
    // Trim to a reasonable length so filenames don't blow up.
    slug.chars().take(40).collect()
}

/// The overlay's own action set: a server-search hit is not a list row, so it
/// has its own Open / Save / Reply / Forward / Archive.
///
/// All of them run off the store for a hit that resolved to a row, and for one
/// that did not through the same daemon calls with the hit's address, which
/// the daemon answers from the server's copy without ingesting it (PERSO-155),
/// or off the fetch the overlay is already rendering for a reply. Two keys need
/// a local row and say so when there is none: Archive, which moves one, and
/// Enter, which puts the list cursor on one.
fn handle_search_result_action(
    app: &mut App,
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    action: Action,
) -> Result<()> {
    match action {
        Action::SearchResultOpen => {
            // A hit that resolved is opened exactly as the list opens it: the
            // read-only Markdown rendition of its row. One that did not is the
            // same rendition of the server's copy, which the daemon renders
            // without ingesting it (PERSO-155).
            let Some(address) = selected_hit_address(app) else {
                return Ok(());
            };
            open_readonly_view(app, terminal, &address)?;
        }

        Action::SearchResultJump => {
            // Close the overlay and put the list cursor on the hit, the way
            // Apple Mail / Outlook open a search result (#0104).
            let Some(hit) = app.server_search_results.get(app.server_search_index) else {
                return Ok(());
            };
            let Some(msg) = hit.entry.msg else {
                app.server_search_status =
                    Some("Not in the local store; press f to fetch it first".to_string());
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
                app.server_search_status = Some(format!(
                    "Cannot open: mailbox {label} is not in the sidebar"
                ));
                return Ok(());
            };
            app.close_overlay();
            app.open_message(msg, &mailbox);
        }

        Action::SearchResultYankPath => {
            // Materialise the read-only Markdown rendition and copy its path
            // (#0104): the store keeps no per-message .md file, this scratch
            // rendition is the file-shaped answer.
            let Some(address) = selected_hit_address(app) else {
                return Ok(());
            };
            let Some(rendition) = readonly_view(app, &address) else {
                if matches!(address, MessageAddress::Row(_)) {
                    app.server_search_status =
                        Some("Yank failed; see the activity log".to_string());
                }
                return Ok(());
            };
            let shown = rendition.path.display().to_string();
            // The overlay's footer has no level, so only the line is kept.
            let (line, _) = super::clipboard::copy_text(terminal, &shown).status(&shown);
            app.server_search_status = Some(line);
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
            match mp_core::parse::open_file_with_system(&path) {
                Ok(()) => app.set_status("Opened in browser".to_string()),
                Err(e) => app.set_status_level(format!("Open failed: {e}"), StatusLevel::Error),
            }
        }

        Action::SearchResultReply(all) => {
            let what = if all { "Reply-all" } else { "Reply" };
            search_result_draft(app, terminal, DraftFromSource::Reply { all }, what)?;
        }

        Action::SearchResultForward => {
            search_result_draft(
                app,
                terminal,
                DraftFromSource::Forward {
                    as_attachment: false,
                },
                "Forward",
            )?;
        }

        Action::SearchResultForwardAsAttachment => {
            search_result_draft(
                app,
                terminal,
                DraftFromSource::Forward {
                    as_attachment: true,
                },
                "Forward as attachment",
            )?;
        }

        Action::SearchResultExport => {
            if let Some(path) = search_result_eml_file(app) {
                app.open_dir_picker(vec![path]);
            }
        }

        Action::SearchResultArchive => {
            archive_search_hit(app, &daemon_door(app));
        }

        _ => {}
    }
    Ok(())
}

/// The selected server-search hit as its `.eml`, for the overlay's `te` to
/// hand to the directory picker as the list's `te` does, or `None` with the
/// overlay's status line saying why (PERSO-174).
///
/// A hit that resolved is its row's export; a server-only one is addressed as
/// `f` addresses it, and the daemon downloads the server's copy without
/// ingesting it, so the hit stays a hit.
pub fn search_result_eml_file(app: &mut App) -> Option<PathBuf> {
    let address = selected_hit_address(app)?;
    let account = app.account_config.name.clone();
    match commands::eml_file(&daemon_door(app), &account, &address) {
        Ok(path) => Some(path),
        Err(e) => {
            app.server_search_status = Some(format!("Export failed: {e}"));
            None
        }
    }
}

/// Archive the selected server-search hit (`a` in the overlay).
///
/// The hit leaves the results only once the archive went through; when it did
/// not (no Archive mailbox, every `message.archive` refused), it stays, and the
/// error is shown on the overlay's own status line, which is the one the user
/// is looking at.
fn archive_search_hit(app: &mut App, queries: &dyn super::queries::Queries) {
    let hit = app
        .server_search_results
        .get(app.server_search_index)
        .and_then(|r| r.entry.msg);
    let Some(msg) = hit else {
        app.server_search_status =
            Some("Not in the local store; press f to fetch it first".to_string());
        return;
    };

    match commands::archive_msgs(app, queries, vec![msg], false) {
        Ok(_) => {
            // Looked up by ref rather than by the index read before the
            // archive ran, so a refresh in between cannot remove another hit.
            if let Some(index) = app
                .server_search_results
                .iter()
                .position(|r| r.entry.msg == Some(msg))
            {
                app.server_search_results.remove(index);
            }
            if app.server_search_index >= app.server_search_results.len()
                && !app.server_search_results.is_empty()
            {
                app.server_search_index = app.server_search_results.len() - 1;
            }
        }
        Err(error) => app.server_search_status = Some(error),
    }
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
    match kind {
        DraftFromSource::Forward { as_attachment } => {
            search_result_forward(app, as_attachment);
            Ok(())
        }
        // A reply goes straight to the editor, as the list's does: a hit that
        // resolved is addressed by its row, and one that did not travels as
        // the message the overlay holds.
        DraftFromSource::Reply { all } => {
            let msg = hit.entry.msg;
            // Cloned only for the unresolved hit, which is the one case that
            // needs the fetched payload while `app` is borrowed mutably.
            let fetched = msg.is_none().then(|| hit.fetched.clone());
            match (msg, fetched) {
                (Some(msg), _) => {
                    write_draft_and_edit(
                        app,
                        terminal,
                        &MessageAddress::Row(msg.row_id()),
                        kind,
                        None,
                        what,
                    )?;
                    Ok(())
                }
                (None, Some(fetched)) => {
                    write_fetched_reply_and_edit(app, terminal, &fetched, all, what)
                }
                (None, None) => Ok(()),
            }
        }
    }
}

/// The search overlay's `w` and `cF`: the forward wizard over the selected
/// hit, which asks for the recipients first as the list's forward does, for a
/// stored hit and a server-only one alike, as the desktop's forward of either
/// does (PERSO-155). The subject is the hit's own, since a server-only hit has
/// no row to read it from.
pub fn search_result_forward(app: &mut App, as_attachment: bool) {
    let Some(hit) = app.server_search_results.get(app.server_search_index) else {
        return;
    };
    let subject = mp_core::draft::fwd_subject(&hit.fetched.subject);
    let Some(address) = selected_hit_address(app) else {
        return;
    };
    open_compose_wizard(
        app,
        ComposeMode::SearchForward(Box::new(crate::app::SearchForward {
            address,
            subject,
            as_attachment,
        })),
    );
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

    fn search_hit(row_id: i64, subject: &str) -> crate::app::SearchResultEntry {
        let fetched = mp_core::parse::FetchedEmail {
            from: "a@example.com".into(),
            to: "me@example.com".into(),
            cc: None,
            reply_to: None,
            bcc: None,
            subject: subject.into(),
            date: "Mon, 01 Jan 2024 12:00:00 +0000".into(),
            body_text: String::new(),
            html_body: None,
            has_attachments: false,
            server_has_attachments: None,
            message_id: Some(format!("<{row_id}@example.com>")),
            attachments: Vec::new(),
            flags: Default::default(),
            calendar_ics: None,
            event: None,
        };
        let entry = super::super::helpers::fetched_to_email_entry(
            Some(MessageRef::new(row_id)),
            None,
            &fetched,
        );
        crate::app::SearchResultEntry {
            entry,
            fetched,
            source_label: "INBOX".into(),
        }
    }

    /// `a` on a hit when the archive cannot happen (here: no Archive mailbox)
    /// keeps the hit and says why on the overlay's own status line.
    #[test]
    fn archiving_a_search_hit_that_fails_keeps_it_and_reports_on_the_overlay() {
        let mut app = App::default_for_tests();
        app.server_search_results = vec![search_hit(1, "one"), search_hit(2, "two")];
        app.server_search_index = 0;

        archive_search_hit(&mut app, &QueryHandle::closed());

        assert_eq!(app.server_search_results.len(), 2);
        assert_eq!(app.server_search_index, 0);
        assert_eq!(
            app.server_search_status.as_deref(),
            Some("Archive mailbox not configured")
        );
    }

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
            9,
            "the number of {needle}) call sites changed (now at lines {sites:?}); \
             re-derive the Action::suspends_terminal mapping before updating this count"
        );
    }

    /// Once `e`'s editor returned cleanly the keyboard is back on the list,
    /// from the body pane or the headers it was pressed in, as the desktop
    /// client's reader gives its focus back when the embedded editor exits
    /// with code 0 (PERSO-124); the sidebar and the list keep theirs.
    #[test]
    fn a_clean_return_from_e_puts_the_keyboard_back_on_the_list() {
        let mut app = App::default_for_tests();
        for (from, to) in [
            (Focus::Preview, Focus::List),
            (Focus::Headers, Focus::List),
            (Focus::List, Focus::List),
            (Focus::Sidebar, Focus::Sidebar),
        ] {
            app.focus = from;
            return_to_list_after_edit(&mut app);
            assert_eq!(app.focus, to, "from {from:?}");
        }
    }

    /// The editor `r`, `cf` and `cn` open on a fresh draft lands like `e`'s:
    /// a clean exit puts the keyboard back on the list from the headers or the
    /// body pane, and a failed one leaves it where it was (PERSO-129).
    #[test]
    fn a_clean_return_from_a_new_drafts_editor_puts_the_keyboard_back_on_the_list() {
        let mut app = App::default_for_tests();
        for (from, clean, to) in [
            (Focus::Preview, true, Focus::List),
            (Focus::Headers, true, Focus::List),
            (Focus::List, true, Focus::List),
            (Focus::Sidebar, true, Focus::Sidebar),
            (Focus::Preview, false, Focus::Preview),
            (Focus::Headers, false, Focus::Headers),
        ] {
            app.focus = from;
            land_after_draft_editor(&mut app, clean);
            assert_eq!(app.focus, to, "from {from:?}, clean {clean}");
        }
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
                        assert!(!s.addr.trim().is_empty(), "empty address from {cleaned:?}");
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

    /// A door that records every call and answers `draft.create` with the
    /// draft it would have written, refusing the first `taken` names as the
    /// daemon refuses a name already in the drafts directory.
    struct DraftDoor {
        calls: std::cell::RefCell<Vec<(String, Value)>>,
        taken: usize,
    }

    impl super::super::queries::Queries for DraftDoor {
        fn call(&self, method: &str, params: Value) -> anyhow::Result<Value> {
            let mut calls = self.calls.borrow_mut();
            calls.push((method.to_string(), params.clone()));
            let name = params["name"].as_str().unwrap_or_default().to_string();
            if calls.len() <= self.taken {
                return Err(mp_client::session::Refused {
                    method: method.to_string(),
                    error: mp_protocol::RpcError {
                        code: -32602,
                        message: format!("A draft already exists at /drafts/{name}.md"),
                        data: None,
                    },
                }
                .into());
            }
            Ok(json!({
                "account": "work",
                "id": "d1",
                "selector": "mp://work/drafts/d1",
                "path": format!("/drafts/{name}.md"),
                "source": null,
                "subject": params["headers"]["subject"],
            }))
        }
    }

    fn draft_door(taken: usize) -> DraftDoor {
        DraftDoor {
            calls: std::cell::RefCell::new(Vec::new()),
            taken,
        }
    }

    /// A `New` wizard's draft is the daemon's `draft.create`, carrying the
    /// recipients and subject as `headers`, the inline body and the selected
    /// signature, and this process writes no file: the drafts directory is
    /// the daemon's, as it is for the desktop's compose dialog.
    #[test]
    fn a_new_wizard_draft_is_created_by_the_daemon() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::default_for_tests();
        app.account_config.name = "work".to_string();
        app.global_config.email.include_signature = true;
        app.drafts_dir = Some(dir.path().to_path_buf());
        let mut wizard = wizard_with_body("  Thanks, see you Tuesday.  ");
        wizard.cc = "carol@example.com".to_string();
        wizard.signature_name = Some("casual".to_string());

        let door = draft_door(0);
        let path = create_draft_from_wizard(&app, &door, &wizard).unwrap();

        let calls = door.calls.borrow();
        assert_eq!(calls.len(), 1, "one call: {calls:?}");
        let (method, params) = &calls[0];
        assert_eq!(method, "draft.create");
        assert_eq!(params["account"], "work");
        let name = params["name"].as_str().unwrap();
        assert!(
            name.starts_with("draft-") && name.ends_with("-quick-note"),
            "the timestamp-and-slug name the TUI has always used: {name}"
        );
        assert_eq!(
            params["headers"],
            json!({
                "to": "alice@example.com",
                "cc": "carol@example.com",
                "bcc": "",
                "subject": "Quick note",
            })
        );
        assert_eq!(params["body"], "Thanks, see you Tuesday.");
        assert_eq!(params["signature"], "casual");
        assert!(params.get("no_signature").is_none());
        assert_eq!(path, PathBuf::from(format!("/drafts/{name}.md")));
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            0,
            "the TUI writes nothing into the drafts directory"
        );
    }

    /// An empty inline body sends none, which leaves the draft body to the
    /// signature (or blank) and is the signal `submit_compose_wizard` uses to
    /// open `$EDITOR`; signatures turned off globally ask for none, and an
    /// empty selector leaves the account default to the daemon.
    #[test]
    fn the_wizard_asks_for_no_body_and_the_signature_the_config_allows() {
        let mut app = App::default_for_tests();
        app.account_config.name = "work".to_string();
        let wizard = wizard_with_body("   ");

        app.global_config.email.include_signature = false;
        let off = wizard_draft_params(&app, &wizard, "n");
        assert!(off.get("body").is_none(), "{off}");
        assert_eq!(off["no_signature"], true);
        assert!(off.get("signature").is_none());

        app.global_config.email.include_signature = true;
        let default = wizard_draft_params(&app, &wizard, "n");
        assert!(default.get("no_signature").is_none(), "{default}");
        assert!(default.get("signature").is_none(), "{default}");
    }

    /// A name the drafts directory already holds is refused by the daemon and
    /// the next `-<n>` is asked for, the suffix rule the TUI applied when it
    /// wrote the file itself.
    #[test]
    fn a_taken_draft_name_is_retried_with_a_suffix() {
        let app = App::default_for_tests();
        let wizard = wizard_with_body("");
        let door = draft_door(2);
        let path = create_draft_from_wizard(&app, &door, &wizard).unwrap();
        let names: Vec<String> = door
            .calls
            .borrow()
            .iter()
            .map(|(_, params)| params["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(names.len(), 3, "{names:?}");
        assert_eq!(names[1], format!("{}-1", names[0]));
        assert_eq!(names[2], format!("{}-2", names[0]));
        assert_eq!(path, PathBuf::from(format!("/drafts/{}.md", names[2])));
    }

    // -----------------------------------------------------------------------
    // Signatures and contacts through the daemon (the daemon leftovers' TUI
    // follow-up)
    // -----------------------------------------------------------------------

    /// What a recording session answers: the account's signature listing, a
    /// file under a daemon-chosen directory for `signature.read`, and nothing
    /// for a name it does not hold, so `signature.create` is asked.
    struct SignatureDoor {
        calls: std::sync::Arc<std::sync::Mutex<Vec<(String, Value)>>>,
    }

    impl super::super::queries::Queries for SignatureDoor {
        fn call(&self, method: &str, params: Value) -> anyhow::Result<Value> {
            self.calls
                .lock()
                .unwrap()
                .push((method.to_string(), params.clone()));
            let name = params["name"].as_str().unwrap_or_default().to_string();
            match method {
                "signature.list" => Ok(json!({
                    "account": params["account"],
                    "names": ["casual", "work"],
                    "default": "work",
                })),
                "signature.read" if name == "gone" => Err(mp_client::session::Refused {
                    method: method.to_string(),
                    error: mp_protocol::RpcError {
                        code: -32602,
                        message: format!("no signature named '{name}'"),
                        data: None,
                    },
                }
                .into()),
                "signature.read" | "signature.create" => Ok(json!({
                    "name": name,
                    "path": format!("/daemon/signatures/{name}.md"),
                    "content": "",
                })),
                other => Err(anyhow::anyhow!("unexpected {other}")),
            }
        }
    }

    type Calls = std::sync::Arc<std::sync::Mutex<Vec<(String, Value)>>>;

    fn app_with_signature_door() -> (App, Calls) {
        let calls: Calls = Default::default();
        let door_calls = calls.clone();
        let mut app = App::default_for_tests();
        app.account_config.name = "work".to_string();
        app.global_config.email.include_signature = true;
        app.session = Some(crate::session::Session::serving(move || SignatureDoor {
            calls: door_calls,
        }));
        (app, calls)
    }

    fn methods(calls: &Calls) -> Vec<String> {
        calls
            .lock()
            .unwrap()
            .iter()
            .map(|(m, _)| m.clone())
            .collect()
    }

    /// Composing to a contact offers the signatures the daemon's
    /// `signature.list` names and preselects the account default it reports;
    /// the wizard reads no signature file itself.
    #[test]
    fn compose_to_contact_takes_its_signatures_from_the_daemon() {
        let (mut app, calls) = app_with_signature_door();
        open_compose_wizard_seeded(&mut app, "bob@example.com".to_string());
        let wizard = app.compose_wizard().expect("the wizard is open");
        assert_eq!(wizard.available_signatures, ["casual", "work"]);
        assert_eq!(wizard.signature_name.as_deref(), Some("work"));
        assert_eq!(wizard.to, "bob@example.com");
        assert!(
            methods(&calls).contains(&"signature.list".to_string()),
            "{:?}",
            methods(&calls)
        );
    }

    /// The wizard's `e` and the overlay's `e` open the file the daemon names,
    /// and a name it no longer holds is created through `signature.create`
    /// rather than written here.
    #[test]
    fn a_signature_is_edited_at_the_path_the_daemon_names() {
        let (app, calls) = app_with_signature_door();
        assert_eq!(
            signature_path(&app, "work"),
            Ok(PathBuf::from("/daemon/signatures/work.md"))
        );
        assert_eq!(
            signature_path(&app, "gone"),
            Ok(PathBuf::from("/daemon/signatures/gone.md"))
        );
        assert_eq!(
            methods(&calls),
            ["signature.read", "signature.read", "signature.create"]
        );
    }

    /// A wizard opened before the Contacts view asked for the index queues
    /// the daemon's `contact.search` rather than reading the cache file, and
    /// the answer, when it lands, fills the wizard's completion.
    #[test]
    fn a_compose_wizard_completes_from_the_daemon_contact_index() {
        let _data = mp_core::config::test_env::TestDataDir::new();
        // A cache file on disk that a direct read would have picked up.
        let index = mp_core::contacts::ContactIndex {
            account: "work".to_string(),
            contacts: Default::default(),
            built_at: String::new(),
            extractor_version: mp_core::contacts::EXTRACTOR_VERSION,
        };
        let root = mp_core::config::account_dir("work");
        std::fs::create_dir_all(&root).unwrap();
        mp_core::contacts::save_cache(&root, &index).unwrap();

        let (mut app, _calls) = app_with_signature_door();
        open_compose_wizard_seeded(&mut app, String::new());
        assert!(app.compose_wizard().unwrap().contacts.is_none());
        let generation = match app.pending_actions.iter().find_map(|a| match a {
            Action::LoadContacts { generation } => Some(*generation),
            _ => None,
        }) {
            Some(generation) => generation,
            None => panic!("the wizard asks the daemon for the index"),
        };

        let answer = crate::queries::decode_contacts(
            "work",
            &json!({"contacts": [{
                "address": "ada@example.com",
                "display_name": "Ada",
                "sent_to": 3, "sent_cc": 0, "received": 1,
                "first_seen": "2026-01-01T00:00:00Z",
                "last_seen": "2026-02-01T00:00:00Z",
                "score": 1,
            }], "rebuild": null}),
        );
        app.land_contacts(app.active_account, generation, Ok(answer));
        let contacts = app.compose_wizard().unwrap().contacts.as_ref();
        let ada = contacts.and_then(|index| index.contacts.get("ada@example.com"));
        assert_eq!(ada.map(|c| c.sent_to), Some(3));
        assert_eq!(
            ada.map(|c| c.last_seen.as_str()),
            Some("2026-02-01T00:00:00Z")
        );
    }

    /// A header edit asks `draft.set_headers` for all four fields, and for a
    /// re-splice only when the signature changed: with signatures off it drops
    /// the block, with no name it takes the account default.
    #[test]
    fn a_header_edit_asks_the_daemon_for_the_fields_and_the_resplice() {
        let edit = DraftRecipientEdit {
            to: "a@example.com".to_string(),
            cc: String::new(),
            bcc: "b@example.com".to_string(),
            subject: "Hi".to_string(),
        };
        let plain = set_headers_params("work", "d1", &edit, None);
        assert_eq!(
            plain,
            json!({"account": "work", "id": "d1", "headers": {
                "to": "a@example.com", "cc": "", "bcc": "b@example.com", "subject": "Hi",
            }})
        );
        let off = set_headers_params("work", "d1", &edit, Some((false, Some("x".into()))));
        assert_eq!(off["splice_signature"], json!(true));
        assert_eq!(off["no_signature"], json!(true));
        assert!(off.get("signature").is_none());
        let default = set_headers_params("work", "d1", &edit, Some((true, None)));
        assert_eq!(default["splice_signature"], json!(true));
        assert!(default.get("signature").is_none());
        assert!(default.get("no_signature").is_none());
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
        let cfg = mp_core::config::test_env::ConfigDirOverride::new(dir.path());
        cfg.set_config_dir(&dir.path().join("config"));
        let _data = mp_core::config::test_env::TestDataDir::new();
        mp_core::signatures::write("work", "-- \nAlice").unwrap();

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
        // The block the daemon's `signature.read` gives the wizard; this unit
        // test has no daemon, so it reads the file as the daemon would.
        let account = app.account_config.clone();
        let block = || mp_core::config::resolve_signature_markdown(&account, Some("work"));
        let opened_with = block();
        mp_core::draft::rewrite_draft_signature(&path, opened_with.as_deref(), Some("work"))
            .unwrap();
        assert!(
            std::fs::read_to_string(&path).unwrap().contains("Alice"),
            "the draft starts with the pre-edit block"
        );

        // Nothing has happened yet: a plain recipient edit leaves the body be.
        assert!(!app.compose_wizard().unwrap().signature_needs_respice());

        // `$EDITOR` rewrites the file and `edit_compose_signature` records it.
        mp_core::signatures::write("work", "-- \nAlice B., Team Lead").unwrap();
        note_compose_signature_edited(&mut app);
        assert!(
            app.compose_wizard().unwrap().signature_needs_respice(),
            "an in-place file edit still has to reach the draft"
        );

        // What the submit path then asks the daemon for with that decision,
        // and the re-splice `draft.set_headers` runs on it.
        let sig_name = app.compose_wizard().unwrap().signature_name.clone();
        let edit = DraftRecipientEdit {
            to: "bob@example.com".to_string(),
            cc: String::new(),
            bcc: String::new(),
            subject: "Re: hello".to_string(),
        };
        let params = set_headers_params("work", "d1", &edit, Some((true, sig_name.clone())));
        assert_eq!(params["splice_signature"], json!(true));
        assert_eq!(params["signature"], json!("work"));
        mp_core::draft::rewrite_draft_signature(&path, block().as_deref(), sig_name.as_deref())
            .unwrap();

        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("Alice B., Team Lead"), "{content}");
        assert!(!content.contains("\nAlice\n"), "{content}");
    }
}
