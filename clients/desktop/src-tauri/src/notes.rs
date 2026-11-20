//! The notes on mail (PERSO-315): the TUI's `n` prompt and its `N` list.
//!
//! Every command is one `note.*` call on the daemon, which keeps the notes
//! and renders the batch prompt a hand-off gives the agent: `note.presets`
//! for the prompt's buttons, `note.set` and `note.set_many` for a note on one
//! mail or several, `note.get` for the reader's band, `note.list`,
//! `note.clear` and `note.clear_done` for the list, and `note.export` and
//! `note.handoff` for the prompt. A refusal is the daemon's `-32602` carrying
//! its sentence, as a `protocol` error with no code. Every writer publishes
//! `note.changed` with the whole list, which the event channel hands the
//! webview as every other event. The fixture answers the same methods with
//! the same sentences (`fixture/notes.rs`).

use std::time::Duration;

use mp_protocol::note::{
    Note, NoteCleared, NoteExport, NoteListing, NoteLookup, NotePresets, NoteSetManyParams,
    NoteSetParams, METHOD_NOTE_CLEAR, METHOD_NOTE_CLEAR_DONE, METHOD_NOTE_EXPORT, METHOD_NOTE_GET,
    METHOD_NOTE_HANDOFF, METHOD_NOTE_LIST, METHOD_NOTE_PRESETS, METHOD_NOTE_SET,
    METHOD_NOTE_SET_MANY,
};
use serde_json::{json, Value};
use tauri::State;

use crate::commands::{decode, with_door};
use crate::error::{refusal_sentence, rpc_code, Addressing, GuiError};
use crate::session::{Door, SessionHandle};

/// One note call: a write is one file write in the daemon, a read one read.
const NOTE_BUDGET: Duration = Duration::from_secs(15);

/// One `note.*` call: a `-32602` refusal carries the daemon's sentence as a
/// `protocol` error; anything else as every call's.
fn note_call<T: serde::de::DeserializeOwned>(
    door: &Door,
    method: &str,
    params: Value,
) -> Result<T, GuiError> {
    let answer = door.call_within(method, params, NOTE_BUDGET).map_err(|e| {
        let text = format!("{e:#}");
        match (rpc_code(&text), refusal_sentence(&text)) {
            (Some(-32602), Some(sentence)) => GuiError::protocol(sentence),
            _ => GuiError::from_call_text(&text, Addressing::Params),
        }
    })?;
    decode(method, answer)
}

/// The presets the prompt offers, in its order.
pub fn note_presets_on(door: &Door) -> Result<NotePresets, GuiError> {
    note_call(door, METHOD_NOTE_PRESETS, json!({}))
}

/// Every note, newest first.
pub fn note_list_on(door: &Door) -> Result<NoteListing, GuiError> {
    note_call(door, METHOD_NOTE_LIST, json!({}))
}

/// The note on the message or draft `selector` names, if any.
pub fn note_get_on(door: &Door, selector: &str) -> Result<NoteLookup, GuiError> {
    note_call(door, METHOD_NOTE_GET, json!({"selector": selector}))
}

/// `params` as the wire's JSON.
fn params_of(method: &str, params: &impl serde::Serialize) -> Result<Value, GuiError> {
    serde_json::to_value(params)
        .map_err(|e| GuiError::internal(format!("the {method} params did not encode: {e}")))
}

/// Add a note, or replace the one on its selector or id.
pub fn note_set_on(door: &Door, params: &NoteSetParams) -> Result<Note, GuiError> {
    note_call(door, METHOD_NOTE_SET, params_of(METHOD_NOTE_SET, params)?)
}

/// The same note on each of `params.selectors`.
pub fn note_set_many_on(door: &Door, params: &NoteSetManyParams) -> Result<NoteListing, GuiError> {
    note_call(
        door,
        METHOD_NOTE_SET_MANY,
        params_of(METHOD_NOTE_SET_MANY, params)?,
    )
}

/// Remove the note `id` names.
pub fn note_clear_on(door: &Door, id: &str) -> Result<NoteCleared, GuiError> {
    note_call(door, METHOD_NOTE_CLEAR, json!({"id": id}))
}

/// Remove every done note.
pub fn note_clear_done_on(door: &Door) -> Result<NoteCleared, GuiError> {
    note_call(door, METHOD_NOTE_CLEAR_DONE, json!({}))
}

fn ids_param(ids: Option<Vec<String>>) -> Value {
    match ids {
        Some(ids) => json!({"ids": ids}),
        None => json!({}),
    }
}

/// The batch prompt for `ids`, or for every open note; changes nothing.
pub fn note_export_on(door: &Door, ids: Option<Vec<String>>) -> Result<NoteExport, GuiError> {
    note_call(door, METHOD_NOTE_EXPORT, ids_param(ids))
}

/// The batch prompt for `ids`, or for every open note, marking the open
/// ones among them handed off.
pub fn note_handoff_on(door: &Door, ids: Option<Vec<String>>) -> Result<NoteExport, GuiError> {
    note_call(door, METHOD_NOTE_HANDOFF, ids_param(ids))
}

// ---------------------------------------------------------------------------
// Tauri commands
// ---------------------------------------------------------------------------

#[tauri::command(rename_all = "snake_case")]
pub async fn note_presets(session: State<'_, SessionHandle>) -> Result<NotePresets, GuiError> {
    with_door(&session, move |_, door| note_presets_on(door)).await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn note_list(session: State<'_, SessionHandle>) -> Result<NoteListing, GuiError> {
    with_door(&session, move |_, door| note_list_on(door)).await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn note_get(
    session: State<'_, SessionHandle>,
    selector: String,
) -> Result<NoteLookup, GuiError> {
    with_door(&session, move |_, door| note_get_on(door, &selector)).await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn note_set(
    session: State<'_, SessionHandle>,
    params: NoteSetParams,
) -> Result<Note, GuiError> {
    with_door(&session, move |_, door| note_set_on(door, &params)).await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn note_set_many(
    session: State<'_, SessionHandle>,
    params: NoteSetManyParams,
) -> Result<NoteListing, GuiError> {
    with_door(&session, move |_, door| note_set_many_on(door, &params)).await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn note_clear(
    session: State<'_, SessionHandle>,
    id: String,
) -> Result<NoteCleared, GuiError> {
    with_door(&session, move |_, door| note_clear_on(door, &id)).await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn note_clear_done(session: State<'_, SessionHandle>) -> Result<NoteCleared, GuiError> {
    with_door(&session, move |_, door| note_clear_done_on(door)).await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn note_export(
    session: State<'_, SessionHandle>,
    ids: Option<Vec<String>>,
) -> Result<NoteExport, GuiError> {
    with_door(&session, move |_, door| note_export_on(door, ids)).await
}

#[tauri::command(rename_all = "snake_case")]
pub async fn note_handoff(
    session: State<'_, SessionHandle>,
    ids: Option<Vec<String>>,
) -> Result<NoteExport, GuiError> {
    with_door(&session, move |_, door| note_handoff_on(door, ids)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::Fixture;
    use mp_client::events::Incoming;
    use std::sync::mpsc::Receiver;
    use std::sync::Arc;

    fn fixture_door() -> (Door, Receiver<Incoming>) {
        let (tx, rx) = std::sync::mpsc::channel();
        let fixture = Arc::new(Fixture::load(tx).expect("fixture"));
        (Door::Fixture(fixture), rx)
    }

    /// The `(kind, payload)` of every event posted so far.
    fn drained(rx: &Receiver<Incoming>) -> Vec<(String, Value)> {
        std::iter::from_fn(|| rx.try_recv().ok())
            .filter_map(|i| match i {
                Incoming::Event(e) => Some((e.kind, e.payload)),
                _ => None,
            })
            .collect()
    }

    /// The first message of `work`'s Inbox, as the fixture lists it.
    fn first_inbox_selector(d: &Door) -> String {
        let listed = d
            .call_within(
                "message.list",
                json!({"account": "work", "mailbox": "inbox"}),
                NOTE_BUDGET,
            )
            .expect("list");
        listed["messages"][0]["selector"]
            .as_str()
            .expect("selector")
            .to_string()
    }

    #[test]
    fn a_note_set_on_a_message_marks_its_row_and_publishes_the_list() {
        let (d, rx) = fixture_door();
        assert_eq!(note_presets_on(&d).expect("presets").presets[0].key, "r");
        let selector = first_inbox_selector(&d);
        let set = note_set_on(
            &d,
            &NoteSetParams {
                selector: Some(selector.clone()),
                preset: Some("s".to_string()),
                ..Default::default()
            },
        )
        .expect("set");
        assert_eq!(set.state, mp_protocol::note::NoteState::Open);
        let events = drained(&rx);
        assert!(events
            .iter()
            .any(|(kind, payload)| kind == "note.changed"
                && payload["notes"][0]["id"] == json!(set.id)));
        let listed = d
            .call_within(
                "message.list",
                json!({"account": "work", "mailbox": "inbox"}),
                NOTE_BUDGET,
            )
            .expect("list");
        assert_eq!(listed["messages"][0]["note"], "open");
        let got = note_get_on(&d, &selector).expect("get");
        assert_eq!(got.note.map(|n| n.id), Some(set.id));
    }

    #[test]
    fn a_handoff_marks_the_exported_notes_and_a_refusal_is_the_daemons_sentence() {
        let (d, _rx) = fixture_door();
        let refused = note_set_on(
            &d,
            &NoteSetParams {
                account: Some("work".to_string()),
                ..Default::default()
            },
        )
        .expect_err("refused");
        match refused {
            GuiError::Protocol { message, code } => {
                assert_eq!(message, "a note needs a preset or a text");
                assert_eq!(code, None);
            }
            other => panic!("{other:?}"),
        }
        let new = note_set_on(
            &d,
            &NoteSetParams {
                account: Some("work".to_string()),
                text: Some("Ask for the room".to_string()),
                ..Default::default()
            },
        )
        .expect("new-mail note");
        let export = note_export_on(&d, None).expect("export");
        assert_eq!(export.ids, std::slice::from_ref(&new.id));
        let handed = note_handoff_on(&d, Some(export.ids.clone())).expect("handoff");
        assert_eq!(handed.text, export.text);
        let listed = note_list_on(&d).expect("list");
        assert_eq!(
            listed.notes[0].state,
            mp_protocol::note::NoteState::HandedOff
        );
        assert_eq!(note_clear_on(&d, &new.id).expect("clear").removed, 1);
        assert_eq!(note_clear_done_on(&d).expect("clear done").removed, 0);
    }
}
