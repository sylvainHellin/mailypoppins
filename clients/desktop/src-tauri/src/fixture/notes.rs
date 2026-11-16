//! The notes on mail (PERSO-315), as the fixture keeps them: in memory, for
//! every account, answering the `note.*` family as `src/daemon/notes.rs` and
//! `src/daemon/methods/note.rs` do, with their refusals' sentences.
//!
//! A note on a received message is held by its account and Message-ID, a
//! note on a draft by its account and id, and a note for a new email by
//! neither; a selector is read for its account and key only, so any copy's
//! selector names the note on the message. The fixture answers what a
//! selector names, and where a note's mail is now, through [`World`], which
//! `fixture.rs` implements over its rows and drafts. Every write answers
//! whether the list changed, which `fixture.rs` publishes as `note.changed`
//! with the whole list.

use mp_protocol::note::{Note, NotePreset, NoteState};
use serde_json::{json, Value};

/// A refusal: the daemon's code and sentence.
pub type Refusal = (i32, String);

fn invalid(why: impl Into<String>) -> Refusal {
    (-32602, why.into())
}

/// What a selector names, with the snapshot a new note takes of it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Target {
    pub account: String,
    /// The full selector of the copy the note is set on.
    pub selector: String,
    /// A received message's Message-ID, without its angle brackets.
    pub message_id: Option<String>,
    /// A draft's id.
    pub draft_id: Option<String>,
    pub from: String,
    pub to: String,
    pub subject: String,
}

/// The rows and drafts the notes are on, as `fixture.rs` holds them.
pub trait World {
    /// The message or draft `selector` names, or the daemon's refusal.
    fn target(&self, account: Option<&str>, selector: &str) -> Result<Target, Refusal>;
    /// The selector of the mail `note` is on now; `None` when it is gone.
    fn locate(&self, note: &Note) -> Option<String>;
    /// Whether the configuration names `account`.
    fn knows(&self, account: &str) -> bool;
}

/// What a selector addresses a note by: its account, and the Message-ID or
/// the draft id; the mailbox and a UID segment do not count.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Key {
    Message { account: String, message_id: String },
    Draft { account: String, id: String },
}

/// The key of `selector`, a full `mp://<account>/<mailbox>/<key>` or, with
/// `account`, `<mailbox>/<key>`.
pub fn key_of(selector: &str, account: Option<&str>) -> Result<Key, Refusal> {
    let refused = || {
        invalid(format!(
            "`{selector}` is not a selector of a message or a draft"
        ))
    };
    let (account, rest) = match selector.strip_prefix("mp://") {
        Some(rest) => {
            let (account, rest) = rest.split_once('/').ok_or_else(refused)?;
            (account.to_string(), rest)
        }
        None => (account.ok_or_else(refused)?.to_string(), selector),
    };
    let (mailbox, key) = rest.split_once('/').ok_or_else(refused)?;
    // A UID segment, `<key>/<uid>`, names the same message.
    let key = key.split('/').next().unwrap_or_default();
    let key = key.trim_start_matches('<').trim_end_matches('>');
    if account.is_empty() || mailbox.is_empty() || key.is_empty() {
        return Err(refused());
    }
    Ok(if mailbox == "drafts" {
        Key::Draft {
            account,
            id: key.to_string(),
        }
    } else {
        Key::Message {
            account,
            message_id: key.to_string(),
        }
    })
}

/// Whether `note` is the one `key` names.
fn is_on(note: &Note, key: &Key) -> bool {
    match key {
        Key::Message {
            account,
            message_id,
        } => note.account == *account && note.message_id.as_deref() == Some(message_id),
        Key::Draft { account, id } => {
            note.account == *account && note.draft_id.as_deref() == Some(id)
        }
    }
}

/// Every note, oldest first, with the clock and the ids it minted.
#[derive(Debug, Default)]
pub struct NoteBook {
    notes: Vec<Note>,
    next_id: u32,
    ticks: u32,
}

impl NoteBook {
    /// The presets the fixture's configuration has: the built-in ones.
    pub fn presets() -> Vec<NotePreset> {
        mp_core::config::default_note_presets()
    }

    /// The state of the note on a message or a draft of `account`, what a
    /// listing row carries.
    pub fn state_of(
        &self,
        account: &str,
        message_id: Option<&str>,
        draft_id: Option<&str>,
    ) -> Option<NoteState> {
        let bare = message_id.map(|m| m.trim_start_matches('<').trim_end_matches('>'));
        self.notes
            .iter()
            .find(|n| {
                n.account == account
                    && ((bare.is_some() && n.message_id.as_deref() == bare)
                        || (draft_id.is_some() && n.draft_id.as_deref() == draft_id))
            })
            .map(|n| n.state)
    }

    /// Every note, newest first, each located again: its selector where its
    /// mail is now, or orphaned.
    pub fn listing(&self, world: &dyn World) -> Vec<Note> {
        self.notes.iter().rev().map(|n| located(n, world)).collect()
    }

    fn now(&mut self) -> String {
        self.ticks += 1;
        format!(
            "2026-11-16T09:{:02}:{:02}Z",
            (self.ticks / 60) % 60,
            self.ticks % 60
        )
    }

    fn mint(&mut self) -> String {
        self.next_id += 1;
        format!("f{:07x}", self.next_id)
    }

    fn by_id(&self, id: &str) -> Result<usize, Refusal> {
        self.notes
            .iter()
            .position(|n| n.id == id)
            .ok_or_else(|| invalid(format!("no note has the id {id}")))
    }

    fn by_key(&self, key: &Key, selector: &str) -> Result<usize, Refusal> {
        self.notes
            .iter()
            .position(|n| is_on(n, key))
            .ok_or_else(|| invalid(format!("no note is on {selector}")))
    }

    /// The note `params` addresses by `selector` or by `id`, exactly one.
    fn addressed(&self, params: &Value) -> Result<usize, Refusal> {
        match (params["selector"].as_str(), params["id"].as_str()) {
            (Some(selector), None) => {
                let key = key_of(selector, params["account"].as_str())?;
                self.by_key(&key, selector)
            }
            (None, Some(id)) => self.by_id(id),
            _ => Err(invalid(
                "a note is addressed by selector or by id; send exactly one",
            )),
        }
    }

    /// Set the note on `target`, or a new-email note when it is `None`,
    /// replacing the one already on it and opening it again.
    fn set_on(
        &mut self,
        target: Option<Target>,
        account: &str,
        preset: Option<String>,
        text: String,
    ) -> usize {
        let now = self.now();
        let found = target.as_ref().and_then(|t| {
            let key = match (&t.message_id, &t.draft_id) {
                (Some(m), _) => Key::Message {
                    account: t.account.clone(),
                    message_id: m.clone(),
                },
                (None, Some(d)) => Key::Draft {
                    account: t.account.clone(),
                    id: d.clone(),
                },
                _ => return None,
            };
            self.notes.iter().position(|n| is_on(n, &key))
        });
        let t = target.unwrap_or_default();
        let fresh = Note {
            id: String::new(),
            selector: (!t.selector.is_empty()).then(|| t.selector.clone()),
            account: if t.account.is_empty() {
                account.to_string()
            } else {
                t.account.clone()
            },
            message_id: t.message_id,
            draft_id: t.draft_id,
            preset,
            text,
            state: NoteState::Open,
            result: None,
            created: now.clone(),
            updated: now,
            from: t.from,
            to: t.to,
            subject: t.subject,
            orphaned: false,
        };
        match found {
            Some(at) => {
                let old = &self.notes[at];
                let note = Note {
                    id: old.id.clone(),
                    created: old.created.clone(),
                    ..fresh
                };
                self.notes[at] = note;
                at
            }
            None => {
                let id = self.mint();
                self.notes.push(Note { id, ..fresh });
                self.notes.len() - 1
            }
        }
    }

    /// One `note.*` call: the answer, and whether the list changed.
    pub fn call(
        &mut self,
        method: &str,
        params: &Value,
        world: &dyn World,
    ) -> Result<(Value, bool), Refusal> {
        match method {
            "note.presets" => Ok((json!({ "presets": Self::presets() }), false)),
            "note.list" => {
                let state: Option<NoteState> = match &params["state"] {
                    Value::Null => None,
                    state => Some(serde_json::from_value(state.clone()).map_err(|_| {
                        invalid(format!("state is open, handed_off or done, not {state}"))
                    })?),
                };
                let notes: Vec<Note> = self
                    .listing(world)
                    .into_iter()
                    .filter(|n| state.is_none_or(|s| n.state == s))
                    .collect();
                Ok((json!({ "notes": notes }), false))
            }
            "note.get" => {
                let selector = params["selector"]
                    .as_str()
                    .ok_or_else(|| invalid("missing string param `selector`"))?;
                let key = key_of(selector, params["account"].as_str())?;
                let note = self
                    .notes
                    .iter()
                    .find(|n| is_on(n, &key))
                    .map(|n| located(n, world));
                Ok((json!({ "note": note }), false))
            }
            "note.set" => {
                let (preset, text) = content(params)?;
                let at = match (params["selector"].as_str(), params["id"].as_str()) {
                    (Some(_), Some(_)) => {
                        return Err(invalid(
                            "selector and id are two addresses; send at most one",
                        ))
                    }
                    (Some(selector), None) => {
                        need_content(&preset, &text)?;
                        let target = world.target(params["account"].as_str(), selector)?;
                        let account = target.account.clone();
                        self.set_on(Some(target), &account, preset, text.unwrap_or_default())
                    }
                    (None, Some(id)) => {
                        // By id the stored preset or text stays where the
                        // request leaves it out.
                        let at = self.by_id(id)?;
                        let old = &self.notes[at];
                        let preset = preset.or_else(|| old.preset.clone());
                        let text = text.unwrap_or_else(|| old.text.clone());
                        need_content(&preset, &Some(text.clone()))?;
                        let now = self.now();
                        let note = &mut self.notes[at];
                        note.preset = preset;
                        note.text = text;
                        note.state = NoteState::Open;
                        note.result = None;
                        note.updated = now;
                        at
                    }
                    (None, None) => {
                        need_content(&preset, &text)?;
                        let account = params["account"].as_str().ok_or_else(|| {
                            invalid(
                                "a note for a new email names the account it goes out from; \
                                 send account, or a selector or an id",
                            )
                        })?;
                        if !world.knows(account) {
                            return Err((-32005, format!("account_unknown: {account}")));
                        }
                        self.set_on(None, account, preset, text.unwrap_or_default())
                    }
                };
                Ok((json!(located(&self.notes[at], world)), true))
            }
            "note.set_many" => {
                let (preset, text) = content(params)?;
                need_content(&preset, &text)?;
                let selectors: Vec<&str> = params["selectors"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect();
                if selectors.is_empty() {
                    return Err(invalid("selectors names no message or draft"));
                }
                // Every selector resolves before anything is written.
                let targets = selectors
                    .iter()
                    .map(|s| world.target(params["account"].as_str(), s))
                    .collect::<Result<Vec<_>, _>>()?;
                let mut set: Vec<usize> = Vec::new();
                for target in targets {
                    let account = target.account.clone();
                    let at = self.set_on(
                        Some(target),
                        &account,
                        preset.clone(),
                        text.clone().unwrap_or_default(),
                    );
                    if !set.contains(&at) {
                        set.push(at);
                    }
                }
                let notes: Vec<Note> = set
                    .iter()
                    .map(|at| located(&self.notes[*at], world))
                    .collect();
                Ok((json!({ "notes": notes }), true))
            }
            "note.done" => {
                let result = params["result"].as_str().unwrap_or_default();
                if result.trim().is_empty() {
                    return Err(invalid(
                        "result is the line a client shows on the mail, and may not be empty",
                    ));
                }
                let at = self.addressed(params)?;
                let now = self.now();
                let note = &mut self.notes[at];
                note.state = NoteState::Done;
                note.result = Some(result.to_string());
                note.updated = now;
                Ok((json!(located(&self.notes[at], world)), true))
            }
            "note.clear" => {
                let at = match self.addressed(params) {
                    Ok(at) => Some(at),
                    // A selector with no note on it removes nothing.
                    Err(_) if params["selector"].is_string() && params["id"].is_null() => None,
                    Err(e) => return Err(e),
                };
                if let Some(at) = at {
                    self.notes.remove(at);
                }
                Ok((json!({ "removed": u64::from(at.is_some()) }), at.is_some()))
            }
            "note.clear_done" => {
                let before = self.notes.len();
                self.notes.retain(|n| n.state != NoteState::Done);
                let removed = (before - self.notes.len()) as u64;
                Ok((json!({ "removed": removed }), removed > 0))
            }
            "note.export" | "note.handoff" => {
                let listed = self.listing(world);
                let picked: Vec<Note> = match params["ids"].as_array() {
                    None => listed
                        .into_iter()
                        .filter(|n| n.state == NoteState::Open)
                        .collect(),
                    Some(ids) => {
                        let ids: Vec<&str> = ids.iter().filter_map(Value::as_str).collect();
                        if let Some(unknown) =
                            ids.iter().find(|id| !listed.iter().any(|n| n.id == **id))
                        {
                            return Err(invalid(format!("no note has the id {unknown}")));
                        }
                        listed
                            .into_iter()
                            .filter(|n| ids.contains(&n.id.as_str()))
                            .collect()
                    }
                };
                let text = render(&picked, &Self::presets());
                let ids: Vec<String> = picked.iter().map(|n| n.id.clone()).collect();
                let mut changed = false;
                if method == "note.handoff" {
                    let now = self.now();
                    for note in self.notes.iter_mut() {
                        if ids.contains(&note.id) && note.state == NoteState::Open {
                            note.state = NoteState::HandedOff;
                            note.updated = now.clone();
                            changed = true;
                        }
                    }
                }
                Ok((json!({ "text": text, "ids": ids }), changed))
            }
            other => Err((-32601, format!("unknown method {other}"))),
        }
    }
}

/// `note` with its selector decided again, or marked orphaned.
fn located(note: &Note, world: &dyn World) -> Note {
    let mut note = note.clone();
    if note.message_id.is_none() && note.draft_id.is_none() {
        return note;
    }
    match world.locate(&note) {
        Some(selector) => {
            note.selector = Some(selector);
            note.orphaned = false;
        }
        None => note.orphaned = true,
    }
    note
}

/// The preset and the text of a set: a configured preset, the text with its
/// trailing white space trimmed.
fn content(params: &Value) -> Result<(Option<String>, Option<String>), Refusal> {
    let preset = params["preset"].as_str().map(str::to_string);
    if let Some(key) = &preset {
        let presets = NoteBook::presets();
        if !presets.iter().any(|p| p.key == *key) {
            let keys: Vec<&str> = presets.iter().map(|p| p.key.as_str()).collect();
            return Err(invalid(format!(
                "preset {key} is not configured; the presets are {}",
                keys.join(", ")
            )));
        }
    }
    let text = params["text"].as_str().map(|t| t.trim_end().to_string());
    Ok((preset, text))
}

/// A note needs a preset or a text.
fn need_content(preset: &Option<String>, text: &Option<String>) -> Result<(), Refusal> {
    if preset.is_none() && text.as_deref().is_none_or(|t| t.trim().is_empty()) {
        return Err(invalid("a note needs a preset or a text"));
    }
    Ok(())
}

/// The batch prompt, as the daemon's `notes::render` words it.
pub fn render(notes: &[Note], presets: &[NotePreset]) -> String {
    if notes.is_empty() {
        return String::new();
    }
    let mut out = match notes.len() {
        1 => "I have a note on one email.\n".to_string(),
        n => format!("I have notes on {n} emails.\n"),
    };
    out.push_str(
        "Read each with `mp show <selector>` (a draft: `mp path <selector>` gives its file), \
         carry out the instruction, and close it with `mp notes done <selector> --result \"...\"`.\n\
         A note without a selector asks for a new email: write it as its item says, then close it \
         with `mp notes done --id <id> --result \"...\"`.\n\
         Never send or approve anything.\n\n",
    );
    for (index, note) in notes.iter().enumerate() {
        let mut lines: Vec<String> = Vec::new();
        match &note.selector {
            Some(selector) => {
                lines.push(selector.clone());
                let who = if note.draft_id.is_some() {
                    format!("Draft to {}", note.to)
                } else {
                    format!("From {}", note.from)
                };
                lines.push(format!("{who}, subject \"{}\"", note.subject));
            }
            None => lines.push(format!(
                "New email from account {} (note {})",
                note.account, note.id
            )),
        }
        if let Some(key) = &note.preset {
            lines.push(match presets.iter().find(|p| p.key == *key) {
                Some(p) => format!("{}: {}", p.label, p.instruction),
                None => format!("Preset {key}, which the configuration no longer has."),
            });
        }
        lines.extend(note.text.lines().map(str::to_string));
        let number = format!("{}. ", index + 1);
        let indent = " ".repeat(number.len());
        for (at, line) in lines.iter().enumerate() {
            out.push_str(if at == 0 { &number } else { &indent });
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One message in `work`'s Inbox and one draft.
    struct Mail;

    impl World for Mail {
        fn target(&self, account: Option<&str>, selector: &str) -> Result<Target, Refusal> {
            match key_of(selector, account)? {
                Key::Message {
                    account,
                    message_id,
                } if account == "work" && message_id == "a@x" => Ok(Target {
                    account,
                    selector: "mp://work/inbox/a@x".to_string(),
                    message_id: Some(message_id),
                    draft_id: None,
                    from: "Ann <ann@x>".to_string(),
                    to: "me@example.com".to_string(),
                    subject: "Lunch".to_string(),
                }),
                Key::Draft { account, id } if account == "work" && id == "d1" => Ok(Target {
                    account,
                    selector: "mp://work/drafts/d1".to_string(),
                    message_id: None,
                    draft_id: Some(id),
                    from: "Me <me@example.com>".to_string(),
                    to: "bob@x".to_string(),
                    subject: "Offsite".to_string(),
                }),
                _ => Err(invalid(format!("no message matches {selector}"))),
            }
        }

        fn locate(&self, note: &Note) -> Option<String> {
            match (&note.message_id, &note.draft_id) {
                (Some(m), _) if m == "a@x" => Some("mp://work/archive/a@x".to_string()),
                (None, Some(d)) if d == "d1" => Some("mp://work/drafts/d1".to_string()),
                _ => None,
            }
        }

        fn knows(&self, account: &str) -> bool {
            account == "work"
        }
    }

    fn call(book: &mut NoteBook, method: &str, params: Value) -> Result<(Value, bool), Refusal> {
        book.call(method, &params, &Mail)
    }

    #[test]
    fn a_set_on_any_copy_is_the_one_note_on_the_message_and_follows_it() {
        let mut book = NoteBook::default();
        let (note, changed) = call(
            &mut book,
            "note.set",
            json!({"selector": "mp://work/inbox/a@x", "preset": "s"}),
        )
        .expect("set");
        assert!(changed);
        assert_eq!(note["selector"], "mp://work/archive/a@x", "located again");
        assert_eq!(note["state"], "open");
        let (again, _) = call(
            &mut book,
            "note.set",
            json!({"selector": "mp://work/archive/<a@x>/7", "text": "ask Bob"}),
        )
        .expect("replace");
        assert_eq!(again["id"], note["id"]);
        assert!(
            again.get("preset").is_none(),
            "a set by selector replaces both"
        );
        assert_eq!(
            book.state_of("work", Some("<a@x>"), None),
            Some(NoteState::Open)
        );
    }

    #[test]
    fn a_set_by_id_keeps_what_it_leaves_out_and_reopens() {
        let mut book = NoteBook::default();
        let (note, _) = call(
            &mut book,
            "note.set",
            json!({"selector": "mp://work/drafts/d1", "preset": "p", "text": "x"}),
        )
        .unwrap();
        let id = note["id"].as_str().unwrap().to_string();
        call(&mut book, "note.done", json!({"id": id, "result": "fine"})).unwrap();
        let (edited, _) = call(&mut book, "note.set", json!({"id": id, "text": "y"})).unwrap();
        assert_eq!(edited["preset"], "p");
        assert_eq!(edited["text"], "y");
        assert_eq!(edited["state"], "open");
        assert!(edited.get("result").is_none());
    }

    #[test]
    fn refusals_carry_the_daemons_sentences() {
        let mut book = NoteBook::default();
        let none = call(
            &mut book,
            "note.set",
            json!({"selector": "mp://work/inbox/a@x"}),
        );
        assert_eq!(none.unwrap_err().1, "a note needs a preset or a text");
        let unknown = call(
            &mut book,
            "note.set",
            json!({"selector": "mp://work/inbox/a@x", "preset": "z"}),
        );
        assert!(unknown
            .unwrap_err()
            .1
            .starts_with("preset z is not configured"));
        let account = call(&mut book, "note.set", json!({"text": "hi"}));
        assert!(account.unwrap_err().1.starts_with("a note for a new email"));
        let id = call(&mut book, "note.done", json!({"id": "nope", "result": "r"}));
        assert_eq!(id.unwrap_err().1, "no note has the id nope");
    }

    #[test]
    fn handoff_marks_the_open_notes_and_export_changes_nothing() {
        let mut book = NoteBook::default();
        call(
            &mut book,
            "note.set",
            json!({"account": "work", "text": "Ask for the room"}),
        )
        .unwrap();
        call(
            &mut book,
            "note.set_many",
            json!({"selectors": ["mp://work/inbox/a@x", "mp://work/drafts/d1"], "preset": "s"}),
        )
        .unwrap();
        let (export, changed) = call(&mut book, "note.export", json!({})).unwrap();
        assert!(!changed);
        assert_eq!(export["ids"].as_array().unwrap().len(), 3);
        assert!(export["text"]
            .as_str()
            .unwrap()
            .starts_with("I have notes on 3 emails."));
        let ids = export["ids"].clone();
        let (handed, changed) = call(&mut book, "note.handoff", json!({"ids": ids})).unwrap();
        assert!(changed);
        assert_eq!(handed["text"], export["text"]);
        let (open, _) = call(&mut book, "note.export", json!({})).unwrap();
        assert_eq!(open["text"], "");
        assert_eq!(
            book.state_of("work", None, Some("d1")),
            Some(NoteState::HandedOff)
        );
    }

    #[test]
    fn clear_and_clear_done_remove_and_a_gone_mail_is_orphaned() {
        let mut book = NoteBook::default();
        call(
            &mut book,
            "note.set",
            json!({"selector": "mp://work/inbox/a@x", "preset": "s"}),
        )
        .unwrap();
        call(
            &mut book,
            "note.set",
            json!({"selector": "mp://work/drafts/d1", "preset": "p"}),
        )
        .unwrap();
        call(
            &mut book,
            "note.done",
            json!({"selector": "mp://work/inbox/a@x", "result": "done"}),
        )
        .unwrap();
        let (cleared, changed) = call(&mut book, "note.clear_done", json!({})).unwrap();
        assert_eq!(cleared["removed"], 1);
        assert!(changed);
        let (nothing, changed) = call(
            &mut book,
            "note.clear",
            json!({"selector": "mp://work/inbox/a@x"}),
        )
        .unwrap();
        assert_eq!(nothing["removed"], 0);
        assert!(!changed);
        // A note whose draft the world no longer holds stays, orphaned.
        book.notes[0].draft_id = Some("gone".to_string());
        let listed = book.listing(&Mail);
        assert!(listed[0].orphaned);
    }
}
