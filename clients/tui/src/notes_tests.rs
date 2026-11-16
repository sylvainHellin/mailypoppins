//! The notes on mail in the TUI (PERSO-315): `n` and its prompt over the
//! list, the row glyph's state, `fN`, the `N` list with its keys, and
//! `note.changed` keeping the rows and an open list live.
//!
//! The daemon is a door that keeps the notes in a vector and answers the
//! `note.*` calls the way `src/daemon/methods/note.rs` does for what these
//! tests read; every call is recorded.

use std::sync::{Arc, Mutex};

use crossterm::event::{KeyCode, KeyEvent};
use serde_json::{json, Value};

use mp_protocol::events::KIND_NOTE_CHANGED;
use mp_protocol::note::{Note, NoteState};
use mp_protocol::state::Bootstrap;
use mp_protocol::EventEnvelope;

use crate::app::{
    Action, App, EmailEntry, MailboxInfo, MailboxKind, MessageRef, NoteStage, NoteTarget, Overlay,
};

const INSTANCE: &str = "notes-tests";

type Calls = Arc<Mutex<Vec<(String, Value)>>>;

/// The daemon's notes and every call made to it.
#[derive(Clone, Default)]
struct Daemon {
    notes: Arc<Mutex<Vec<Note>>>,
    calls: Calls,
}

impl crate::queries::Queries for Daemon {
    fn call(&self, method: &str, params: Value) -> anyhow::Result<Value> {
        self.calls
            .lock()
            .unwrap()
            .push((method.to_string(), params.clone()));
        let mut notes = self.notes.lock().unwrap();
        match method {
            "note.presets" => Ok(crate::app::notes_presets_answer()),
            "note.get" => {
                let selector = params["selector"].as_str().unwrap_or_default();
                let note = notes
                    .iter()
                    .find(|n| n.selector.as_deref() == Some(selector))
                    .cloned();
                Ok(json!({ "note": note }))
            }
            "note.list" => Ok(json!({ "notes": *notes })),
            "note.set" | "note.set_many" => Ok(json!({ "notes": [] })),
            "note.clear" => {
                let before = notes.len();
                let id = params["id"].as_str().unwrap_or_default().to_string();
                notes.retain(|n| n.id != id);
                Ok(json!({ "removed": before - notes.len() }))
            }
            "note.clear_done" => {
                let before = notes.len();
                notes.retain(|n| n.state != NoteState::Done);
                Ok(json!({ "removed": before - notes.len() }))
            }
            other => Err(anyhow::anyhow!("unexpected {other}")),
        }
    }
}

fn note(id: &str, key: &str, state: NoteState) -> Note {
    Note {
        id: id.to_string(),
        selector: Some(format!("mp://work/inbox/{key}")),
        account: "work".to_string(),
        message_id: Some(key.to_string()),
        draft_id: None,
        preset: Some("r".to_string()),
        text: "say yes".to_string(),
        state,
        result: (state == NoteState::Done).then(|| "Drafted a reply".to_string()),
        created: "2026-11-16T10:00:00Z".to_string(),
        updated: "2026-11-16T10:00:00Z".to_string(),
        from: "Stefan <s@x.example>".to_string(),
        to: "me@x.example".to_string(),
        subject: format!("About {key}"),
        orphaned: false,
    }
}

fn received(row: i64, key: &str) -> EmailEntry {
    EmailEntry {
        msg: Some(MessageRef::new(row)),
        selector: Some(format!("mp://work/inbox/{key}")),
        subject: format!("About {key}"),
        status: "inbox".to_string(),
        read: true,
        ..crate::app::entry_from_skip(mp_protocol::draft::DraftSkip {
            path: "x.md".to_string(),
            error: String::new(),
        })
    }
}

/// The account `work` listing `emails` in its Inbox, with a session on a
/// daemon holding `notes`, past the bootstrap at 10.
fn app(emails: Vec<EmailEntry>, notes: Vec<Note>) -> (App, Daemon) {
    let daemon = Daemon::default();
    *daemon.notes.lock().unwrap() = notes;
    let served = daemon.clone();
    let mut app = App::default_for_tests();
    app.account_config.name = "work".to_string();
    app.accounts = vec![crate::app::AccountState::for_tests("work")];
    let mailbox = |label: &str, id: &str, kind: MailboxKind| MailboxInfo {
        label: label.to_string(),
        icon: "",
        id: id.to_string(),
        kind,
        server_name: None,
    };
    app.mailboxes = vec![
        mailbox("Inbox", "inbox", MailboxKind::Inbox),
        mailbox("Sent", "sent", MailboxKind::Sent),
    ];
    app.emails = Arc::new(emails);
    app.email_cache = vec![Some(Arc::clone(&app.emails)), Some(Arc::new(Vec::new()))];
    app.mailbox_counts = vec![app.emails.len(), 0];
    app.rebuild_visible();
    app.session = Some(crate::session::Session::serving(move || served));
    app.apply_bootstrap(&Bootstrap {
        instance_id: INSTANCE.to_string(),
        revision: 10,
        ..Default::default()
    });
    (app, daemon)
}

fn press(app: &mut App, c: char) {
    app.handle_key(KeyEvent::from(KeyCode::Char(c)));
}

fn key(app: &mut App, code: KeyCode) {
    app.handle_key(KeyEvent::from(code));
}

fn typed(app: &mut App, text: &str) {
    for c in text.chars() {
        press(app, c);
    }
}

/// The calls after the ones the bootstrap made, `note.*` only.
fn calls(daemon: &Daemon) -> Vec<(String, Value)> {
    daemon
        .calls
        .lock()
        .unwrap()
        .iter()
        .filter(|(m, _)| m.starts_with("note."))
        .cloned()
        .collect()
}

fn changed(revision: u64, notes: &[Note]) -> EventEnvelope {
    EventEnvelope {
        instance_id: INSTANCE.to_string(),
        revision,
        kind: KIND_NOTE_CHANGED.to_string(),
        payload: json!({ "notes": notes }),
    }
}

/// `n` then a preset that needs no text writes the note at once, with the
/// presets read from the daemon.
#[test]
fn n_then_a_preset_with_no_text_sets_the_note_at_once() {
    let (mut app, daemon) = app(vec![received(1, "a@x")], vec![]);
    press(&mut app, 'n');
    assert!(app.note_prompt.is_some());
    press(&mut app, 's');
    assert!(app.note_prompt.is_none());
    assert_eq!(
        calls(&daemon),
        [
            ("note.presets".to_string(), json!({})),
            (
                "note.set".to_string(),
                json!({"selector": "mp://work/inbox/a@x", "preset": "s", "text": ""})
            ),
        ]
    );
    assert_eq!(app.status_message.as_deref(), Some("Note set"));
}

/// A preset that needs text takes a line, then Enter; Esc writes nothing.
#[test]
fn n_then_reply_takes_a_line_and_esc_writes_nothing() {
    let (mut app, daemon) = app(vec![received(1, "a@x")], vec![]);
    press(&mut app, 'n');
    key(&mut app, KeyCode::Esc);
    assert!(app.note_prompt.is_none());
    press(&mut app, 'n');
    press(&mut app, 'r');
    typed(&mut app, "yes, Tuesday");
    key(&mut app, KeyCode::Enter);
    assert_eq!(
        calls(&daemon).last().cloned(),
        Some((
            "note.set".to_string(),
            json!({"selector": "mp://work/inbox/a@x", "preset": "r", "text": "yes, Tuesday"})
        ))
    );
    let sets = calls(&daemon)
        .iter()
        .filter(|(m, _)| m == "note.set")
        .count();
    assert_eq!(sets, 1, "Esc set nothing");
}

/// On a `v` selection the note goes to every selected row in one call.
#[test]
fn n_on_a_selection_sets_the_note_on_each_row() {
    let (mut app, daemon) = app(
        vec![received(1, "a@x"), received(2, "b@x"), received(3, "c@x")],
        vec![],
    );
    press(&mut app, 'v');
    press(&mut app, 'j');
    press(&mut app, 'j');
    press(&mut app, 'v');
    press(&mut app, 'n');
    press(&mut app, ' ');
    typed(&mut app, "file it");
    key(&mut app, KeyCode::Enter);
    assert_eq!(
        calls(&daemon).last().cloned(),
        Some((
            "note.set_many".to_string(),
            json!({
                "selectors": ["mp://work/inbox/a@x", "mp://work/inbox/c@x"],
                "text": "file it"
            })
        ))
    );
    assert_eq!(
        app.status_message.as_deref(),
        Some("Note set on 2 messages")
    );
}

/// `n` on a noted row reads its note and opens on its text; a done note
/// opens on the presets, where Backspace clears it.
#[test]
fn n_on_a_noted_row_edits_it_and_on_a_done_one_offers_clear() {
    let mut open = received(1, "a@x");
    open.note = Some(NoteState::Open);
    let mut done = received(2, "b@x");
    done.note = Some(NoteState::Done);
    let (mut app, daemon) = app(
        vec![open, done],
        vec![
            note("n1", "a@x", NoteState::Open),
            note("n2", "b@x", NoteState::Done),
        ],
    );
    press(&mut app, 'n');
    let prompt = app.note_prompt.clone().expect("the prompt");
    assert_eq!(
        prompt.stage,
        NoteStage::Text {
            preset: Some("r".to_string()),
            text: "say yes".to_string()
        }
    );
    typed(&mut app, "!");
    key(&mut app, KeyCode::Enter);
    assert_eq!(
        calls(&daemon).last().cloned(),
        Some((
            "note.set".to_string(),
            json!({"selector": "mp://work/inbox/a@x", "preset": "r", "text": "say yes!"})
        ))
    );

    press(&mut app, 'j');
    press(&mut app, 'n');
    let prompt = app.note_prompt.clone().expect("the prompt");
    assert_eq!(prompt.stage, NoteStage::Choose);
    assert!(prompt.choices().starts_with("done: Drafted a reply"));
    key(&mut app, KeyCode::Backspace);
    assert_eq!(
        calls(&daemon).last().cloned(),
        Some(("note.clear".to_string(), json!({"id": "n2"})))
    );
    assert_eq!(app.status_message.as_deref(), Some("Note cleared"));
}

/// `note.changed` gives each row its note's state, a message by its key in
/// any mailbox; `fN` then lists only the noted rows.
#[test]
fn note_changed_patches_the_rows_and_fn_lists_the_noted() {
    let (mut app, _daemon) = app(vec![received(1, "a@x"), received(2, "b@x")], vec![]);
    let mut archived = note("n1", "b@x", NoteState::HandedOff);
    archived.selector = Some("mp://work/Archive/b@x".to_string());
    assert_eq!(
        app.apply_event(&changed(11, &[archived])),
        crate::events::Applied::Rows
    );
    let states: Vec<Option<NoteState>> = app.emails.iter().map(|e| e.note).collect();
    assert_eq!(states, [None, Some(NoteState::HandedOff)]);
    assert_eq!(
        app.email_cache[0].as_ref().map(|c| c[1].note),
        Some(Some(NoteState::HandedOff)),
        "the cached list is the patched one"
    );
    assert!(app.email_cache[1].is_none(), "another mailbox reads again");

    press(&mut app, 'f');
    press(&mut app, 'N');
    assert!(app.noted_only);
    assert_eq!(app.visible.len(), 1);
    assert_eq!(app.status_message.as_deref(), Some("Noted only (1)"));
    // Its note cleared elsewhere, the row leaves the noted view.
    app.apply_event(&changed(12, &[]));
    assert!(app.visible.is_empty());
    press(&mut app, 'f');
    press(&mut app, 'N');
    assert_eq!(app.visible.len(), 2);
}

/// `N` lists every note; `d` drops one, `D` the done ones, `y` and `x`
/// queue the copy and the hand-off, Esc closes; an open list takes the list
/// a `note.changed` carries.
#[test]
fn the_notes_list_drops_copies_and_hands_off() {
    let (mut app, daemon) = app(
        vec![received(1, "a@x")],
        vec![
            note("n1", "a@x", NoteState::Open),
            note("n2", "b@x", NoteState::Done),
        ],
    );
    press(&mut app, 'N');
    let Overlay::Notes(overlay) = &app.overlay else {
        panic!("the notes list is not open");
    };
    assert_eq!(overlay.notes.len(), 2);
    assert_eq!(overlay.open_count(), 1);
    assert_eq!(overlay.instruction(&overlay.notes[0]), "Reply: say yes");
    press(&mut app, 'y');
    assert!(matches!(
        app.pending_actions.back(),
        Some(Action::CopyNotesExport)
    ));
    press(&mut app, 'x');
    assert!(matches!(
        app.pending_actions.back(),
        Some(Action::HandOffNotes)
    ));
    press(&mut app, 'j');
    press(&mut app, 'd');
    assert_eq!(
        calls(&daemon).last().cloned(),
        Some(("note.clear".to_string(), json!({"id": "n2"})))
    );
    press(&mut app, 'D');
    assert_eq!(
        calls(&daemon).last().cloned(),
        Some(("note.clear_done".to_string(), json!({})))
    );
    app.apply_event(&changed(11, &[note("n1", "a@x", NoteState::Open)]));
    match &app.overlay {
        Overlay::Notes(overlay) => {
            assert_eq!(overlay.notes.len(), 1);
            assert_eq!(overlay.selected, 0);
        }
        _ => panic!("the list closed"),
    }
    key(&mut app, KeyCode::Esc);
    assert!(matches!(app.overlay, Overlay::None));
}

/// `a` asks for a new email from the active account; `e` edits the cursor
/// note by its selector, its text pre-filled.
#[test]
fn the_notes_list_adds_a_new_email_note_and_edits_one() {
    let (mut app, daemon) = app(
        vec![received(1, "a@x")],
        vec![note("n1", "a@x", NoteState::Open)],
    );
    press(&mut app, 'N');
    press(&mut app, 'a');
    match &app.overlay {
        Overlay::Notes(overlay) => assert_eq!(
            overlay.prompt.as_ref().map(|p| &p.target),
            Some(&NoteTarget::NewEmail("work".to_string()))
        ),
        _ => panic!("the list closed"),
    }
    press(&mut app, ' ');
    typed(&mut app, "ask Anna for the slides");
    key(&mut app, KeyCode::Enter);
    assert_eq!(
        calls(&daemon).last().cloned(),
        Some((
            "note.set".to_string(),
            json!({"account": "work", "text": "ask Anna for the slides"})
        ))
    );
    press(&mut app, 'e');
    typed(&mut app, " now");
    key(&mut app, KeyCode::Enter);
    assert_eq!(
        calls(&daemon).last().cloned(),
        Some((
            "note.set".to_string(),
            json!({"selector": "mp://work/inbox/a@x", "preset": "r", "text": "say yes now"})
        ))
    );
    assert!(
        matches!(app.overlay, Overlay::Notes(_)),
        "the list stays open"
    );
}

/// Enter on a note puts the list cursor on its mail and closes the list.
#[test]
fn enter_on_a_note_opens_its_mail() {
    let (mut app, _daemon) = app(
        vec![received(1, "a@x"), received(2, "b@x")],
        vec![note("n1", "b@x", NoteState::Open)],
    );
    press(&mut app, 'N');
    key(&mut app, KeyCode::Enter);
    assert!(matches!(app.overlay, Overlay::None));
    assert_eq!(app.list_index, 1);
    assert!(app.pending_select_selector.is_none());
}

/// The three new keys are in the help, under the words the website shows.
#[test]
fn the_keys_are_in_the_help() {
    let sections = crate::app::help_sections();
    let find = |title: &str, keys: &str| {
        sections
            .iter()
            .find(|(t, _)| *t == title)
            .and_then(|(_, rows)| rows.iter().find(|(k, _)| *k == keys))
            .map(|(_, d)| *d)
    };
    assert!(find("MESSAGE (list, headers, body)", "n").is_some());
    assert!(find("GLOBAL", "N").is_some());
    assert!(find("EMAIL LIST", "fN").is_some());
    assert!(find("NOTES", "x").is_some());
}
