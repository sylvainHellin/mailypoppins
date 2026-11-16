//! The `note.*` family: notes on mail for a coding agent (PERSO-315).
//!
//! A note is a short instruction the user attaches to a received message, to
//! a draft, or to nothing, which asks for a new email. The daemon keeps every
//! note in one file for every account, and renders the batch prompt a client
//! hands to an agent; the agent works through `mp` and closes each note with
//! a result the clients show on the mail. A note never sends or approves
//! anything.
//!
//! The writers are commands that publish `note.changed` ([`NoteListing`], the
//! whole list), so every client redraws its glyphs and its list from one
//! payload; `note.list`, `note.get`, `note.export` and `note.presets` are the
//! queries. A listing row carries the state of the note on its message
//! ([`NoteState`], `MessageListRow::note`, `DraftEntry::note`), so the list
//! glyph needs no second call.

use serde::{Deserialize, Serialize};

/// `note.set`: add a note, or replace the one on its message, draft or id.
pub const METHOD_NOTE_SET: &str = "note.set";

/// `note.set_many`: the same note on each of several selectors.
pub const METHOD_NOTE_SET_MANY: &str = "note.set_many";

/// `note.list`: every note, newest first.
pub const METHOD_NOTE_LIST: &str = "note.list";

/// `note.get`: the note on one message or draft.
pub const METHOD_NOTE_GET: &str = "note.get";

/// `note.done`: close a note with its result.
pub const METHOD_NOTE_DONE: &str = "note.done";

/// `note.clear`: remove one note.
pub const METHOD_NOTE_CLEAR: &str = "note.clear";

/// `note.clear_done`: remove every done note.
pub const METHOD_NOTE_CLEAR_DONE: &str = "note.clear_done";

/// `note.export`: the batch prompt for the open notes, changing nothing.
pub const METHOD_NOTE_EXPORT: &str = "note.export";

/// `note.handoff`: the batch prompt, marking its open notes handed off.
pub const METHOD_NOTE_HANDOFF: &str = "note.handoff";

/// `note.presets`: the configured presets, in prompt order.
pub const METHOD_NOTE_PRESETS: &str = "note.presets";

/// Where a note stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum NoteState {
    /// Set, and not yet given to an agent.
    Open,
    /// In a batch prompt `note.handoff` rendered.
    HandedOff,
    /// Closed by `note.done`, with its result.
    Done,
}

impl NoteState {
    /// The word the wire spells the state with.
    pub fn as_str(self) -> &'static str {
        match self {
            NoteState::Open => "open",
            NoteState::HandedOff => "handed_off",
            NoteState::Done => "done",
        }
    }

    /// How pressing the state is, lowest first: open, then handed off, then
    /// done. A conversation carries the most pressing state among its
    /// members' notes.
    pub fn urgency(self) -> u8 {
        match self {
            NoteState::Open => 0,
            NoteState::HandedOff => 1,
            NoteState::Done => 2,
        }
    }
}

/// One note, as every `note.*` answer and `note.changed` carry it.
///
/// A note on a received message is held by the message's identity, its
/// account and Message-ID, not by a mailbox: it follows the message when it
/// is archived or moved, and every copy of the message carries it. A note on
/// a draft is held by the draft's id, and a note asking for a new email by
/// neither. A client matches a note against its rows by `account` with
/// `message_id` or `draft_id`, never by `selector`.
///
/// `from`, `to` and `subject` are a snapshot the daemon took from the message
/// or the draft when the note was set, so a list and the export render without
/// a store read and survive a store rebuild.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct Note {
    /// The daemon's id of the note: eight characters, a letter first.
    pub id: String,
    /// A full `mp://<account>/<mailbox>/<key>` selector of the message or
    /// draft the note is on, decided at every answer: the copy in the mailbox
    /// the note was set in when one is still there, else the first copy the
    /// account holds, with a UID segment when that mailbox holds the message
    /// twice. Absent for a note asking for a new email.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub selector: Option<String>,
    /// The account the note belongs to: the message's or the draft's, or the
    /// one a new email goes out from.
    pub account: String,
    /// The Message-ID of the received message the note is on, without its
    /// angle brackets; absent for a draft or a new email.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub message_id: Option<String>,
    /// The id of the draft the note is on; absent for a message or a new
    /// email.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub draft_id: Option<String>,
    /// The key of the preset the note uses, absent for free text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub preset: Option<String>,
    /// What the user typed, `""` for a preset that needs no text.
    #[serde(default)]
    pub text: String,
    /// Where the note stands.
    pub state: NoteState,
    /// What the agent reported when it closed the note; absent until then.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub result: Option<String>,
    /// When the note was first set, RFC 3339 in UTC.
    pub created: String,
    /// When it last changed, RFC 3339 in UTC.
    pub updated: String,
    /// The `From:` of the message when the note was set, the account's own
    /// address for a draft, `""` for a new email.
    #[serde(default)]
    pub from: String,
    /// The `To:` of the message or draft when the note was set.
    #[serde(default)]
    pub to: String,
    /// The subject of the message or draft when the note was set.
    #[serde(default)]
    pub subject: String,
    /// Whether the mail the note is on is gone: a draft deleted, a message no
    /// mailbox of its account holds any more, or an account the configuration
    /// no longer names. Decided at every answer; such a note stays until it is
    /// cleared.
    #[serde(default)]
    pub orphaned: bool,
}

/// One preset, as `[notes] presets` configures it and `note.presets` answers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct NotePreset {
    /// The one character a client's note prompt takes for it.
    pub key: String,
    /// The word a prompt and a list show.
    pub label: String,
    /// Whether a client's prompt waits for a line of text after the key; the
    /// daemon takes a note with no text either way.
    #[serde(default)]
    pub needs_text: bool,
    /// The instruction the batch prompt gives the agent.
    pub instruction: String,
}

/// The `params` of `note.set`.
///
/// A note is addressed by `selector` (the message or draft it is on), by `id`
/// (a note already set, a new-email one included), or by neither, which adds
/// a note asking for a new email from `account`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct NoteSetParams {
    /// The account an elided selector, or a new email, belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub account: Option<String>,
    /// The message or draft, in the grammar `mp show` takes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub selector: Option<String>,
    /// A note already set, to replace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub id: Option<String>,
    /// The key of a configured preset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub preset: Option<String>,
    /// The user's text; a note has a preset, a text, or both.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub text: Option<String>,
}

/// The `params` of `note.set_many`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct NoteSetManyParams {
    /// The account an elided selector belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub account: Option<String>,
    /// The messages or drafts, at least one.
    pub selectors: Vec<String>,
    /// The key of a configured preset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub preset: Option<String>,
    /// The user's text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub text: Option<String>,
}

/// The `params` of `note.list`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct NoteListParams {
    /// Only the notes in this state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub state: Option<NoteState>,
}

/// The `params` of `note.get`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct NoteGetParams {
    /// The account an elided selector belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub account: Option<String>,
    /// The message or draft. Of a message selector only the account and the
    /// key count: any copy's selector, in any mailbox and with or without a
    /// UID segment, names the note on the message.
    pub selector: String,
}

/// The `params` of `note.clear`: a note by `selector` or by `id`, exactly one
/// of them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct NoteClearParams {
    /// The account an elided selector belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub account: Option<String>,
    /// The message or draft the note is on, read as `note.get` reads it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub selector: Option<String>,
    /// The note's id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub id: Option<String>,
}

/// The `params` of `note.done`: a note by `selector` or by `id`, exactly one
/// of them, and its result.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct NoteDoneParams {
    /// The account an elided selector belongs to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub account: Option<String>,
    /// The message or draft the note is on, read as `note.get` reads it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub selector: Option<String>,
    /// The note's id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub id: Option<String>,
    /// What the agent did, the line a client shows on the mail.
    pub result: String,
}

/// The `params` of `note.export` and `note.handoff`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct NoteExportParams {
    /// The notes to render, in any state; absent for every open note.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "ts", ts(optional))]
    pub ids: Option<Vec<String>>,
}

/// The `result` of `note.list` and `note.set_many`, and the payload of
/// `note.changed`: notes, newest first.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct NoteListing {
    pub notes: Vec<Note>,
}

/// The `result` of `note.get`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct NoteLookup {
    /// The note on the selector, `null` when it has none.
    pub note: Option<Note>,
}

/// The `result` of `note.clear` and `note.clear_done`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct NoteCleared {
    /// How many notes the call removed: `0` or `1` for `note.clear`.
    pub removed: u64,
}

/// The `result` of `note.export` and `note.handoff`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct NoteExport {
    /// The batch prompt, `""` when there is no note to give.
    pub text: String,
    /// The notes the prompt numbers, in its order.
    pub ids: Vec<String>,
}

/// The `result` of `note.presets`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct NotePresets {
    /// The presets, in the order a prompt lists them.
    pub presets: Vec<NotePreset>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(raw: &str) -> serde_json::Value {
        serde_json::from_str(raw).expect("JSON")
    }

    /// The committed `note.*` fixtures decode into their wire types whole.
    #[test]
    fn the_note_fixtures_decode() {
        let params: NoteSetParams = serde_json::from_value(
            frame(include_str!("../fixtures/note.set.request.json"))["params"].clone(),
        )
        .expect("note.set params");
        let set: Note = serde_json::from_value(
            frame(include_str!("../fixtures/note.set.response.json"))["result"].clone(),
        )
        .expect("note.set");
        assert_eq!(set.selector, params.selector);
        assert_eq!(set.preset, params.preset);
        assert_eq!(set.state, NoteState::Open);
        let many: NoteSetManyParams = serde_json::from_value(
            frame(include_str!("../fixtures/note.set_many.request.json"))["params"].clone(),
        )
        .expect("note.set_many params");
        let set_many: NoteListing = serde_json::from_value(
            frame(include_str!("../fixtures/note.set_many.response.json"))["result"].clone(),
        )
        .expect("note.set_many");
        assert_eq!(set_many.notes.len(), many.selectors.len());
        let _: NoteListParams = serde_json::from_value(
            frame(include_str!("../fixtures/note.list.request.json"))["params"].clone(),
        )
        .expect("note.list params");
        let listed: NoteListing = serde_json::from_value(
            frame(include_str!("../fixtures/note.list.response.json"))["result"].clone(),
        )
        .expect("note.list");
        assert!(listed.notes.iter().any(|note| note.selector.is_none()));
        assert!(listed.notes.iter().any(|note| note.orphaned));
        let _: NoteGetParams = serde_json::from_value(
            frame(include_str!("../fixtures/note.get.request.json"))["params"].clone(),
        )
        .expect("note.get params");
        let got: NoteLookup = serde_json::from_value(
            frame(include_str!("../fixtures/note.get.response.json"))["result"].clone(),
        )
        .expect("note.get");
        assert!(got.note.is_some());
        let done: NoteDoneParams = serde_json::from_value(
            frame(include_str!("../fixtures/note.done.request.json"))["params"].clone(),
        )
        .expect("note.done params");
        let closed: Note = serde_json::from_value(
            frame(include_str!("../fixtures/note.done.response.json"))["result"].clone(),
        )
        .expect("note.done");
        assert_eq!(closed.state, NoteState::Done);
        assert_eq!(closed.result, Some(done.result));
        let _: NoteClearParams = serde_json::from_value(
            frame(include_str!("../fixtures/note.clear.request.json"))["params"].clone(),
        )
        .expect("note.clear params");
        for raw in [
            include_str!("../fixtures/note.clear.response.json"),
            include_str!("../fixtures/note.clear_done.response.json"),
        ] {
            let _: NoteCleared =
                serde_json::from_value(frame(raw)["result"].clone()).expect("note.clear*");
        }
        let export: NoteExportParams = serde_json::from_value(
            frame(include_str!("../fixtures/note.handoff.request.json"))["params"].clone(),
        )
        .expect("note.handoff params");
        let handed: NoteExport = serde_json::from_value(
            frame(include_str!("../fixtures/note.handoff.response.json"))["result"].clone(),
        )
        .expect("note.handoff");
        assert_eq!(Some(handed.ids.clone()), export.ids);
        let exported: NoteExport = serde_json::from_value(
            frame(include_str!("../fixtures/note.export.response.json"))["result"].clone(),
        )
        .expect("note.export");
        assert_eq!(exported.text, handed.text);
        let presets: NotePresets = serde_json::from_value(
            frame(include_str!("../fixtures/note.presets.response.json"))["result"].clone(),
        )
        .expect("note.presets");
        assert_eq!(presets.presets[0].key, "r");
        let changed: NoteListing = serde_json::from_value(
            frame(include_str!("../fixtures/notification.note_changed.json"))["params"]["payload"]
                .clone(),
        )
        .expect("note.changed");
        assert_eq!(changed, listed);
    }

    /// A state is spelled on the wire as `as_str` spells it.
    #[test]
    fn a_state_serialises_as_its_word() {
        for state in [NoteState::Open, NoteState::HandedOff, NoteState::Done] {
            assert_eq!(
                serde_json::to_value(state).unwrap(),
                serde_json::json!(state.as_str())
            );
        }
    }
}
