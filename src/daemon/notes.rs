//! The notes on mail (PERSO-315): one file the daemon keeps for every
//! account, and the batch prompt it renders from them.
//!
//! # The file
//!
//! A note is a short instruction the user attached to a received message, to
//! a draft, or to nothing, which asks for a new email; `mp_protocol::note`
//! is its wire shape. Every note lives in `<data_dir>/notes.json` ([`path`]):
//! the user typed it, so it may not live only in a store the daemon drops and
//! rebuilds, and `config.toml` is the user's file, which the daemon never
//! writes. The file is replaced atomically, private to the user, the way the
//! blocked senders are ([`super::senders`]), and a file that does not parse
//! is an error rather than an empty list, so a broken file is never
//! overwritten with one.
//!
//! A note on mail is held by what it is on ([`NoteKey`]): a received message
//! by its account and Message-ID, whatever mailbox holds it, so a note follows
//! its message into the archive and every copy carries it; a draft by its
//! id, the two namespaces kept apart. The mailbox the note was set in is kept
//! beside the key as a hint for the selector an answer prints, never as part
//! of the key. At most one note is on a key: setting another replaces its
//! preset and text and opens it again. When a draft is sent, its note moves
//! onto the Message-ID it went out with ([`follow_sent_draft`]).
//!
//! A note keeps a snapshot of the `From`, the `To` and the subject taken when
//! it was set, so the list and the batch prompt render with no store read and
//! outlive a rebuilt store. A note whose mail is gone stays where it is; the
//! answers mark it orphaned, and only a clear removes it.
//!
//! # Order
//!
//! The file holds the notes in the order they were first set, so newest
//! first is the file read backwards; a note set again keeps its place.
//!
//! Every read-modify-write holds one lock ([`LOCK`]), so two clients setting
//! notes at once never drop one of them. A writer hands its caller's
//! `publish` the list it wrote while it still holds the lock, so the lists
//! the callers publish as `note.changed` leave in the order the file changed
//! and the last one a client hears is the file as it is.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::Context;
use log::warn;
use serde::{Deserialize, Serialize};

use mp_protocol::note::{Note, NotePreset, NoteState};

use crate::selector::{MessageRowRef, Selector};

/// The file name of the notes under the data directory.
pub const NOTES_FILE: &str = "notes.json";

/// Serialises every read-modify-write of the file in the process.
static LOCK: Mutex<()> = Mutex::new(());

fn locked() -> std::sync::MutexGuard<'static, ()> {
    LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// `<data_dir>/notes.json`.
pub fn path() -> PathBuf {
    crate::config::mailypoppins_data_dir().join(NOTES_FILE)
}

/// Why a notes call did not go through.
#[derive(Debug)]
pub enum NotesError {
    /// The caller named something that is not there; one sentence it shows.
    Refused(String),
    /// The file could not be read or written.
    File(anyhow::Error),
}

impl From<anyhow::Error> for NotesError {
    fn from(error: anyhow::Error) -> Self {
        NotesError::File(error)
    }
}

type Result<T> = std::result::Result<T, NotesError>;

/// The file as it lies on disk.
#[derive(Debug, Default, Serialize, Deserialize)]
struct NotesFile {
    #[serde(default)]
    notes: Vec<Stored>,
}

/// What a note on mail is on: a received message or a draft, in namespaces
/// of their own.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum NoteKey {
    /// A received message, by its Message-ID in the `message_key` form,
    /// without angle brackets.
    Message { account: String, message_id: String },
    /// A draft, by its id.
    Draft { account: String, id: String },
}

impl NoteKey {
    /// The key of the message `message_id` names, in either form.
    pub fn message(account: &str, message_id: &str) -> NoteKey {
        NoteKey::Message {
            account: account.to_string(),
            message_id: crate::selector::message_key(message_id).to_string(),
        }
    }

    /// The key of the draft `id`.
    pub fn draft(account: &str, id: &str) -> NoteKey {
        NoteKey::Draft {
            account: account.to_string(),
            id: id.to_string(),
        }
    }

    /// The account the mail is in.
    pub fn account(&self) -> &str {
        match self {
            NoteKey::Message { account, .. } | NoteKey::Draft { account, .. } => account,
        }
    }
}

impl std::fmt::Display for NoteKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NoteKey::Message {
                account,
                message_id,
            } => write!(f, "the message {message_id} of {account}"),
            NoteKey::Draft { account, id } => write!(f, "the draft {id} of {account}"),
        }
    }
}

/// One note as the file holds it: the wire [`Note`] without `orphaned`,
/// which is decided at every answer and never stored, and with what it is on
/// in place of the selector, which an answer decides too.
#[derive(Clone, Debug, Serialize, Deserialize)]
struct Stored {
    id: String,
    /// What the note is on; absent for a note asking for a new email.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    on: Option<NoteKey>,
    /// The mailbox a message note was set in: the hint for the selector an
    /// answer prints, never part of the key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mailbox: Option<String>,
    account: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    preset: Option<String>,
    #[serde(default)]
    text: String,
    state: NoteState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    result: Option<String>,
    created: String,
    updated: String,
    #[serde(default)]
    from: String,
    #[serde(default)]
    to: String,
    #[serde(default)]
    subject: String,
}

impl Stored {
    /// The note on the wire, its selector the one the hint names: the copy
    /// in the mailbox the note was set in, which an answer replaces with a
    /// copy the store still holds.
    fn to_wire(&self) -> Note {
        let (selector, message_id, draft_id) = match &self.on {
            Some(NoteKey::Message {
                account,
                message_id,
            }) => (
                Some(hinted_selector(
                    account,
                    self.mailbox.as_deref(),
                    message_id,
                )),
                Some(message_id.clone()),
                None,
            ),
            Some(NoteKey::Draft { account, id }) => (
                Some(Selector::for_draft(account, id).to_string()),
                None,
                Some(id.clone()),
            ),
            None => (None, None, None),
        };
        Note {
            id: self.id.clone(),
            selector,
            account: self.account.clone(),
            message_id,
            draft_id,
            preset: self.preset.clone(),
            text: self.text.clone(),
            state: self.state,
            result: self.result.clone(),
            created: self.created.clone(),
            updated: self.updated.clone(),
            from: self.from.clone(),
            to: self.to.clone(),
            subject: self.subject.clone(),
            orphaned: false,
        }
    }

    /// Set the note's instruction again and open it: with `keep`, a field
    /// `content` leaves out keeps its stored value, else it is cleared.
    fn reset(&mut self, content: &Content, keep: bool, now: &str) {
        match (&content.preset, keep) {
            (Some(preset), _) => self.preset = Some(preset.clone()),
            (None, true) => {}
            (None, false) => self.preset = None,
        }
        match (&content.text, keep) {
            (Some(text), _) => self.text = text.clone(),
            (None, true) => {}
            (None, false) => self.text.clear(),
        }
        self.state = NoteState::Open;
        self.result = None;
        self.updated = now.to_string();
    }
}

/// The file at `path`, in the order the notes were set; an absent file holds
/// none.
fn read(path: &Path) -> anyhow::Result<NotesFile> {
    match std::fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .with_context(|| format!("{} is not a notes file", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(NotesFile::default()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

/// Replace the file at `path` atomically, private to the user.
fn write(path: &Path, file: &NotesFile) -> anyhow::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let bytes = serde_json::to_vec_pretty(file).context("serialising the notes")?;
    let parent = path
        .parent()
        .context("the notes file has no parent directory")?;
    crate::config::create_private_dir_all(parent)
        .with_context(|| format!("creating {}", parent.display()))?;
    let tmp = path.with_extension("json.tmp");
    let mut out = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)
        .with_context(|| format!("opening {}", tmp.display()))?;
    out.write_all(&bytes)
        .and_then(|()| out.sync_all())
        .with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(())
}

/// Every note at `path`, newest first, none of them marked orphaned yet.
pub fn load(path: &Path) -> anyhow::Result<Vec<Note>> {
    Ok(listing(&read(path)?))
}

/// Every note `file` holds, newest first.
fn listing(file: &NotesFile) -> Vec<Note> {
    file.notes.iter().rev().map(Stored::to_wire).collect()
}

/// What a writer is handed to announce the list it wrote: called once per
/// write, with every note newest first, while the lock is held.
pub type Publish<'a> = &'a mut dyn FnMut(Vec<Note>);

/// Replace the file at `path` with `file` and hand `publish` what it now
/// holds, both inside the caller's turn of the lock.
fn commit(path: &Path, file: &NotesFile, publish: Publish<'_>) -> anyhow::Result<()> {
    write(path, file)?;
    publish(listing(file));
    Ok(())
}

/// The instruction a set gives a note: a field left out is none on a note
/// set on mail or for a new email, and keeps the stored value on a note set
/// again by its id.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Content {
    /// The key of a configured preset, checked by the caller.
    pub preset: Option<String>,
    /// What the user typed.
    pub text: Option<String>,
}

/// The selector the hint names for a message note: the full
/// `mp://<account>/<mailbox>/<key>`, never the elided form, which a message
/// held in several mailboxes does not resolve.
pub fn hinted_selector(account: &str, mailbox: Option<&str>, message_id: &str) -> String {
    Selector::new(account, mailbox.unwrap_or("inbox"), message_id).to_string()
}

/// What a `set` puts a note on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// A message or a draft, with the mailbox a message was found in and the
    /// snapshot of its headers.
    Mail {
        on: NoteKey,
        mailbox: Option<String>,
        from: String,
        to: String,
        subject: String,
    },
    /// A new email from `account`: always a note of its own.
    NewEmail { account: String },
    /// The note with this id, whatever it is on.
    Id(String),
}

impl Target {
    /// What the target puts its note on, `None` for a new email or an id.
    pub fn on(&self) -> Option<&NoteKey> {
        match self {
            Target::Mail { on, .. } => Some(on),
            Target::NewEmail { .. } | Target::Id(_) => None,
        }
    }
}

/// How a caller names one note it closes or clears.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Key {
    Id(String),
    /// The note on a message or a draft.
    On(NoteKey),
}

impl Key {
    fn names(&self, note: &Stored) -> bool {
        match self {
            Key::Id(id) => note.id == *id,
            Key::On(on) => note.on.as_ref() == Some(on),
        }
    }

    /// The refusal of a key no note answers to.
    fn missing(&self) -> NotesError {
        NotesError::Refused(match self {
            Key::Id(id) => format!("no note has the id {id}"),
            Key::On(on) => format!("no note is on {on}"),
        })
    }
}

/// Set `content` on each of `targets`, in one write, and answer the notes
/// set, in the order of `targets`.
///
/// A target that already has a note replaces its preset and text and opens it
/// again, keeping its id and its place; a [`Target::Mail`] also takes the
/// snapshot again, and a [`Target::Id`] keeps the preset or the text
/// `content` leaves out. An id no note has, and a note left with neither a
/// preset nor a text, are refused and nothing is written.
pub fn set(
    path: &Path,
    targets: &[Target],
    content: &Content,
    now: &str,
    publish: Publish<'_>,
) -> Result<Vec<Note>> {
    let _turn = locked();
    let mut file = read(path)?;
    let mut set = Vec::with_capacity(targets.len());
    for target in targets {
        let at = match target {
            Target::Id(id) => Some(
                file.notes
                    .iter()
                    .position(|note| note.id == *id)
                    .ok_or_else(|| Key::Id(id.clone()).missing())?,
            ),
            Target::Mail { on, .. } => file
                .notes
                .iter()
                .position(|note| note.on.as_ref() == Some(on)),
            Target::NewEmail { .. } => None,
        };
        let index = match at {
            Some(index) => {
                let note = &mut file.notes[index];
                note.reset(content, matches!(target, Target::Id(_)), now);
                if let Target::Mail {
                    mailbox,
                    from,
                    to,
                    subject,
                    ..
                } = target
                {
                    note.mailbox = mailbox.clone();
                    note.from = from.clone();
                    note.to = to.clone();
                    note.subject = subject.clone();
                }
                index
            }
            None => {
                let id = mint_id(|id| file.notes.iter().any(|note| note.id == id));
                let (on, mailbox, account, from, to, subject) = match target {
                    Target::Mail {
                        on,
                        mailbox,
                        from,
                        to,
                        subject,
                    } => (
                        Some(on.clone()),
                        mailbox.clone(),
                        on.account().to_string(),
                        from.clone(),
                        to.clone(),
                        subject.clone(),
                    ),
                    Target::NewEmail { account } => (
                        None,
                        None,
                        account.clone(),
                        String::new(),
                        String::new(),
                        String::new(),
                    ),
                    Target::Id(_) => unreachable!("an id target is found or refused above"),
                };
                file.notes.push(Stored {
                    id,
                    on,
                    mailbox,
                    account,
                    preset: content.preset.clone(),
                    text: content.text.clone().unwrap_or_default(),
                    state: NoteState::Open,
                    result: None,
                    created: now.to_string(),
                    updated: now.to_string(),
                    from,
                    to,
                    subject,
                });
                file.notes.len() - 1
            }
        };
        let note = &file.notes[index];
        if note.preset.is_none() && note.text.trim().is_empty() {
            return Err(NotesError::Refused(
                "a note needs a preset or a text".to_string(),
            ));
        }
        set.push(index);
    }
    commit(path, &file, publish)?;
    Ok(set
        .into_iter()
        .map(|index| file.notes[index].to_wire())
        .collect())
}

/// Close the note `key` names with `result`, whatever its state.
pub fn done(path: &Path, key: &Key, result: &str, now: &str, publish: Publish<'_>) -> Result<Note> {
    let _turn = locked();
    let mut file = read(path)?;
    let note = file
        .notes
        .iter_mut()
        .find(|note| key.names(note))
        .ok_or_else(|| key.missing())?;
    note.state = NoteState::Done;
    note.result = Some(result.to_string());
    note.updated = now.to_string();
    let closed = note.to_wire();
    commit(path, &file, publish)?;
    Ok(closed)
}

/// Move the note on the draft `draft_id` of `account` onto the message the
/// draft went out as, `message_id` in either form, so the note and its result
/// follow the mail into Sent rather than being orphaned with the retired
/// draft. The note keeps its id, its state, its result and its snapshot, and
/// takes the Sent mailbox as its hint. Answers whether a note moved; a draft
/// with no note writes nothing.
pub fn follow_sent_draft(
    path: &Path,
    account: &str,
    draft_id: &str,
    message_id: &str,
    now: &str,
    publish: Publish<'_>,
) -> Result<bool> {
    let _turn = locked();
    let mut file = read(path)?;
    let draft = NoteKey::draft(account, draft_id);
    let Some(note) = file
        .notes
        .iter_mut()
        .find(|note| note.on.as_ref() == Some(&draft))
    else {
        return Ok(false);
    };
    note.on = Some(NoteKey::message(account, message_id));
    note.mailbox = Some(crate::catalogue::FolderRole::Sent.as_str().to_string());
    note.updated = now.to_string();
    commit(path, &file, publish)?;
    Ok(true)
}

/// Who hears a change the notes file takes outside a `note.*` call: the
/// `note.*` family installs it when it is registered ([`listen`]), so a send
/// that moves a draft's note publishes `note.changed` as a `note.*` writer
/// does, under the lock.
type Listener = Arc<dyn Fn(Vec<Note>) + Send + Sync>;

static LISTENER: Mutex<Option<Listener>> = Mutex::new(None);

/// Install the process's [`Listener`], replacing any before it.
pub fn listen(listener: Listener) {
    *LISTENER.lock().unwrap_or_else(|p| p.into_inner()) = Some(listener);
}

/// What every send calls once `draft` went out as `message_id` from
/// `account`, beside the voice snapshot's record: the draft's note, if it has
/// one, moves onto the message ([`follow_sent_draft`]) and the listener hears
/// the list. Best effort and logged, as the message is out either way; a
/// draft with no frontmatter id has no note.
pub fn draft_sent(account: &str, draft: &crate::types::EmailDraft, message_id: &str) {
    let Some(id) = draft
        .frontmatter
        .id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
    else {
        return;
    };
    let listener = LISTENER.lock().unwrap_or_else(|p| p.into_inner()).clone();
    let moved = follow_sent_draft(&path(), account, id, message_id, &now(), &mut |notes| {
        if let Some(listener) = &listener {
            listener(notes);
        }
    });
    if let Err(error) = moved {
        let why = match error {
            NotesError::Refused(why) => why,
            NotesError::File(e) => format!("{e:#}"),
        };
        warn!("[notes] the draft {id} of {account} was sent but its note stayed on it: {why}");
    }
}

/// Remove the note `key` names, answering how many went: `0` when none
/// answers to it, which writes nothing.
pub fn clear(path: &Path, key: &Key, publish: Publish<'_>) -> Result<u64> {
    remove(path, |note| key.names(note), publish)
}

/// Remove every done note, answering how many went.
pub fn clear_done(path: &Path, publish: Publish<'_>) -> Result<u64> {
    remove(path, |note| note.state == NoteState::Done, publish)
}

fn remove(path: &Path, gone: impl Fn(&Stored) -> bool, publish: Publish<'_>) -> Result<u64> {
    let _turn = locked();
    let mut file = read(path)?;
    let before = file.notes.len();
    file.notes.retain(|note| !gone(note));
    let removed = (before - file.notes.len()) as u64;
    if removed > 0 {
        commit(path, &file, publish)?;
    }
    Ok(removed)
}

/// The notes a batch prompt numbers: the ones `ids` names, in any state and
/// in list order, or every open note when it names none. An id no note has
/// is refused.
pub fn select(notes: &[Note], ids: Option<&[String]>) -> Result<Vec<Note>> {
    let Some(ids) = ids else {
        return Ok(notes
            .iter()
            .filter(|note| note.state == NoteState::Open)
            .cloned()
            .collect());
    };
    if let Some(unknown) = ids.iter().find(|id| !notes.iter().any(|n| n.id == **id)) {
        return Err(Key::Id(unknown.clone()).missing());
    }
    Ok(notes
        .iter()
        .filter(|note| ids.contains(&note.id))
        .cloned()
        .collect())
}

/// The notes [`select`] picks out of the file, and every open one among them
/// marked handed off, in one write: what `note.handoff` renders the prompt
/// from. The answer is the notes newest first, the marks applied, and
/// whether any was open, which is whether the file changed.
pub fn hand_off(
    path: &Path,
    ids: Option<&[String]>,
    now: &str,
    publish: Publish<'_>,
) -> Result<(Vec<Note>, bool)> {
    let _turn = locked();
    let mut file = read(path)?;
    let listed = listing(&file);
    let picked: Vec<String> = select(&listed, ids)?
        .into_iter()
        .map(|note| note.id)
        .collect();
    let mut changed = false;
    for note in file.notes.iter_mut() {
        if picked.contains(&note.id) && note.state == NoteState::Open {
            note.state = NoteState::HandedOff;
            note.updated = now.to_string();
            changed = true;
        }
    }
    if changed {
        commit(path, &file, publish)?;
    }
    let given = file
        .notes
        .iter()
        .rev()
        .filter(|note| picked.contains(&note.id))
        .map(Stored::to_wire)
        .collect();
    Ok((given, changed))
}

/// Now, as a note's `created` and `updated` spell it: RFC 3339 in UTC, to the
/// second.
pub fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// A fresh note id: eight characters, a letter first so it never reads as a
/// number, and none `taken` already.
fn mint_id(taken: impl Fn(&str) -> bool) -> String {
    loop {
        let n = rand::random::<u32>();
        let first = b"abcdef"[(n >> 28) as usize % 6] as char;
        let id = format!("{first}{:07x}", n & 0x0fff_ffff);
        if !taken(&id) {
            return id;
        }
    }
}

/// The batch prompt for `notes`, in their order: what `note.export` and
/// `note.handoff` answer, rendered here alone so every client hands an agent
/// the same text. `""` for no notes.
///
/// A preset note reads `<label>: <instruction>` from `presets`, the
/// configuration's, followed by the user's text; a preset the configuration
/// no longer has is named by its key.
pub fn render(notes: &[Note], presets: &[NotePreset]) -> String {
    if notes.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    out.push_str(&match notes.len() {
        1 => "I have a note on one email.\n".to_string(),
        n => format!("I have notes on {n} emails.\n"),
    });
    out.push_str(
        "Read each with `mp show <selector>` (a draft: `mp path <selector>` gives its file), \
         carry out the instruction, and close it with \
         `mp notes done <selector> --result \"...\"`.\n\
         To answer an email, write the draft with `mp reply <selector> --body-file -` \
         (`--all` to reply to all); to pass one on, with \
         `mp forward <selector> --body-file -`, then add the recipients in the file \
         `mp path` gives for the draft it prints.\n\
         A note without a selector asks for a new email: write it as its item says, then \
         close it with `mp notes done --id <id> --result \"...\"`.\n\
         Never send or approve anything.\n\n",
    );
    for (index, note) in notes.iter().enumerate() {
        let mut lines: Vec<String> = Vec::new();
        match &note.selector {
            Some(selector) => {
                lines.push(selector.clone());
                lines.push(headline(note));
                if note.orphaned {
                    lines.push(
                        "This selector names nothing any more; find the mail with `mp search`, \
                         or report that as the result."
                            .to_string(),
                    );
                }
            }
            None => {
                lines.push(format!(
                    "New email from account {} (note {})",
                    note.account, note.id
                ));
                lines.push(format!(
                    "Write it with `mp -A {} new <name> --to <address> --subject \"...\" \
                     --body-file -`.",
                    note.account
                ));
            }
        }
        if let Some(key) = &note.preset {
            lines.push(match presets.iter().find(|preset| preset.key == *key) {
                Some(preset) => format!("{}: {}", preset.label, preset.instruction),
                None => format!("Preset {key}, which the configuration no longer has."),
            });
        }
        lines.extend(note.text.lines().map(str::to_string));
        let number = format!("{}. ", index + 1);
        let indent = " ".repeat(number.len());
        for (at, line) in lines.iter().enumerate() {
            let lead = if at == 0 { &number } else { &indent };
            out.push_str(lead);
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// The line under a selector: who the mail is from, or who a draft is to,
/// and its subject.
fn headline(note: &Note) -> String {
    let who = if note.draft_id.is_some() {
        let to = note.to.trim();
        if to.is_empty() {
            "Draft with no recipient yet".to_string()
        } else {
            format!("Draft to {to}")
        }
    } else {
        format!("From {}", crate::parse::readable_sender(&note.from))
    };
    format!("{who}, subject \"{}\"", note.subject)
}

/// The states of the notes on mail, by account and then by Message-ID or
/// draft id: what a listing row's `note` is read from, one file read per
/// listing.
#[derive(Debug, Default)]
pub struct NoteMarks {
    messages: HashMap<String, HashMap<String, NoteState>>,
    drafts: HashMap<String, HashMap<String, NoteState>>,
}

impl NoteMarks {
    /// The marks of the notes in the file at `path`. A file that does not
    /// read marks nothing and says so in the log: a listing is not refused
    /// over its glyphs.
    pub fn load(path: &Path) -> NoteMarks {
        let file = match read(path) {
            Ok(file) => file,
            Err(e) => {
                warn!("[notes] listing with no note marks: {e:#}");
                return NoteMarks::default();
            }
        };
        let mut marks = NoteMarks::default();
        for note in file.notes {
            let (map, account, key) = match note.on {
                Some(NoteKey::Message {
                    account,
                    message_id,
                }) => (&mut marks.messages, account, message_id),
                Some(NoteKey::Draft { account, id }) => (&mut marks.drafts, account, id),
                None => continue,
            };
            map.entry(account).or_default().insert(key, note.state);
        }
        marks
    }

    /// The state of the note on `row`, a message of `account`, in whatever
    /// mailbox the row is.
    pub fn of_message<R: MessageRowRef>(&self, account: &str, row: &R) -> Option<NoteState> {
        self.of_message_id(account, row.message_id())
    }

    /// The state of the note on the message `message_id` of `account`, in
    /// either form.
    pub fn of_message_id(&self, account: &str, message_id: &str) -> Option<NoteState> {
        self.messages
            .get(account)?
            .get(crate::selector::message_key(message_id))
            .copied()
    }

    /// The state of the note on the draft `id` of `account`.
    pub fn of_draft(&self, account: &str, id: &str) -> Option<NoteState> {
        self.drafts.get(account)?.get(id).copied()
    }

    /// The most pressing state among the notes on the messages
    /// `message_ids` of `account`, a conversation's members: open before
    /// handed off before done, absent when none has a note.
    pub fn of_conversation<'a>(
        &self,
        account: &str,
        message_ids: impl IntoIterator<Item = &'a str>,
    ) -> Option<NoteState> {
        let notes = self.messages.get(account)?;
        message_ids
            .into_iter()
            .filter_map(|id| notes.get(crate::selector::message_key(id)).copied())
            .min_by_key(|state| state.urgency())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: &str = "2026-11-16T09:30:00Z";
    const LATER: &str = "2026-11-16T10:05:00Z";

    /// A message of `tum`, found in the mailbox `selector` names.
    fn mail(selector: &str, subject: &str) -> Target {
        let parts = crate::selector::parse(selector).unwrap();
        Target::Mail {
            on: NoteKey::message("tum", &parts.key),
            mailbox: parts.mailbox,
            from: "Stefan Fuchs <stefan.fuchs@tum.de>".to_string(),
            to: "sylvain@tum.de".to_string(),
            subject: subject.to_string(),
        }
    }

    fn content(preset: Option<&str>, text: &str) -> Content {
        Content {
            preset: preset.map(str::to_string),
            text: Some(text.to_string()),
        }
    }

    fn file() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(NOTES_FILE);
        (dir, path)
    }

    fn refused<T: std::fmt::Debug>(result: Result<T>) -> String {
        match result {
            Err(NotesError::Refused(why)) => why,
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    /// A note is set on a message, listed newest first, and a second set on
    /// the same message, found in another mailbox this time, replaces its
    /// instruction, its hint and its snapshot and opens it again in its place,
    /// keeping its id; a new email is always a note of its own.
    #[test]
    fn a_message_holds_one_note_and_setting_it_again_reopens_it() {
        let (_dir, path) = file();
        let first = set(
            &path,
            &[mail("mp://tum/inbox/a", "A")],
            &content(Some("s"), ""),
            NOW,
            &mut |_| {},
        )
        .unwrap()
        .remove(0);
        assert_eq!(first.state, NoteState::Open);
        assert_eq!(first.id.len(), 8);
        assert!(first.id.starts_with(|c: char| c.is_ascii_lowercase()));
        set(
            &path,
            &[mail("mp://tum/inbox/b", "B")],
            &content(None, "x"),
            NOW,
            &mut |_| {},
        )
        .unwrap();
        done(
            &path,
            &Key::Id(first.id.clone()),
            "summed up",
            NOW,
            &mut |_| {},
        )
        .unwrap();

        let again = set(
            &path,
            &[mail("mp://tum/archive/a", "A, renamed")],
            &content(Some("r"), "yes"),
            LATER,
            &mut |_| {},
        )
        .unwrap()
        .remove(0);
        assert_eq!(again.id, first.id);
        assert_eq!(again.state, NoteState::Open);
        assert_eq!(again.result, None);
        assert_eq!(again.preset.as_deref(), Some("r"));
        assert_eq!(again.subject, "A, renamed");
        assert_eq!(again.selector.as_deref(), Some("mp://tum/archive/a"));
        assert_eq!(again.message_id.as_deref(), Some("a"));
        assert_eq!(again.created, NOW);
        assert_eq!(again.updated, LATER);

        let new = NewEmailNote::set(&path);
        let other = NewEmailNote::set(&path);
        assert_ne!(new, other);
        let order: Vec<String> = load(&path)
            .unwrap()
            .into_iter()
            .map(|note| note.selector.unwrap_or(note.id))
            .collect();
        assert_eq!(
            order,
            [
                other,
                new,
                "mp://tum/inbox/b".into(),
                "mp://tum/archive/a".into()
            ]
        );
    }

    /// A new-email note, by its id.
    struct NewEmailNote;

    impl NewEmailNote {
        fn set(path: &Path) -> String {
            set(
                path,
                &[Target::NewEmail {
                    account: "tum".to_string(),
                }],
                &content(None, "write to Ada"),
                NOW,
                &mut |_| {},
            )
            .unwrap()
            .remove(0)
            .id
        }
    }

    /// A note set again by its id keeps what it is on; an unknown id is
    /// refused and writes nothing.
    #[test]
    fn a_note_is_set_again_by_id_and_an_unknown_id_is_refused() {
        let (_dir, path) = file();
        let id = NewEmailNote::set(&path);
        let edited = set(
            &path,
            &[Target::Id(id.clone())],
            &content(None, "to Ada and Bo"),
            LATER,
            &mut |_| {},
        )
        .unwrap()
        .remove(0);
        assert_eq!(edited.text, "to Ada and Bo");
        assert_eq!(edited.selector, None);
        let before = std::fs::read(&path).unwrap();
        let why = refused(set(
            &path,
            &[mail("mp://tum/inbox/a", "A"), Target::Id("zzzzzzzz".into())],
            &content(None, "x"),
            NOW,
            &mut |_| {},
        ));
        assert_eq!(why, "no note has the id zzzzzzzz");
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    /// A note set again by its id keeps the field the call leaves out: a text
    /// alone keeps the preset, a preset alone keeps the text, and a note left
    /// with neither is refused. A note set again on its mail replaces both.
    #[test]
    fn an_edit_by_id_keeps_what_it_leaves_out() {
        let (_dir, path) = file();
        let note = set(
            &path,
            &[mail("mp://tum/inbox/a", "A")],
            &content(Some("r"), "yes"),
            NOW,
            &mut |_| {},
        )
        .unwrap()
        .remove(0);
        let edit = |preset: Option<&str>, text: Option<&str>| {
            set(
                &path,
                &[Target::Id(note.id.clone())],
                &Content {
                    preset: preset.map(str::to_string),
                    text: text.map(str::to_string),
                },
                LATER,
                &mut |_| {},
            )
        };
        let edited = edit(None, Some("yes, Thursday")).unwrap().remove(0);
        assert_eq!(
            (edited.preset.as_deref(), edited.text.as_str()),
            (Some("r"), "yes, Thursday")
        );
        let edited = edit(Some("d"), None).unwrap().remove(0);
        assert_eq!(
            (edited.preset.as_deref(), edited.text.as_str()),
            (Some("d"), "yes, Thursday")
        );
        let replaced = set(
            &path,
            &[mail("mp://tum/inbox/a", "A")],
            &Content {
                preset: None,
                text: Some("only this".to_string()),
            },
            LATER,
            &mut |_| {},
        )
        .unwrap()
        .remove(0);
        assert_eq!(
            (replaced.preset, replaced.text.as_str()),
            (None, "only this")
        );
        let before = std::fs::read(&path).unwrap();
        assert_eq!(
            refused(edit(None, Some(""))),
            "a note needs a preset or a text"
        );
        assert_eq!(
            refused(set(
                &path,
                &[mail("mp://tum/inbox/b", "B")],
                &Content::default(),
                LATER,
                &mut |_| {},
            )),
            "a note needs a preset or a text"
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
    }

    /// `done` closes by selector or id with the result; `clear` removes one,
    /// `0` for a key nothing answers to; `clear_done` removes the done ones.
    #[test]
    fn done_closes_and_clear_removes() {
        let (_dir, path) = file();
        set(
            &path,
            &[mail("mp://tum/inbox/a", "A"), mail("mp://tum/inbox/b", "B")],
            &content(Some("s"), ""),
            NOW,
            &mut |_| {},
        )
        .unwrap();
        let closed = done(
            &path,
            &Key::On(NoteKey::message("tum", "a")),
            "three lines",
            LATER,
            &mut |_| {},
        )
        .unwrap();
        assert_eq!(closed.state, NoteState::Done);
        assert_eq!(closed.result.as_deref(), Some("three lines"));
        assert_eq!(
            refused(done(
                &path,
                &Key::On(NoteKey::message("tum", "<c>")),
                "x",
                NOW,
                &mut |_| {},
            )),
            "no note is on the message c of tum"
        );
        assert_eq!(
            clear(&path, &Key::Id("zzzzzzzz".into()), &mut |_| {}).unwrap(),
            0
        );
        assert_eq!(clear_done(&path, &mut |_| {}).unwrap(), 1);
        assert_eq!(clear_done(&path, &mut |_| {}).unwrap(), 0);
        assert_eq!(
            clear(&path, &Key::On(NoteKey::message("tum", "b")), &mut |_| {}).unwrap(),
            1
        );
        assert!(load(&path).unwrap().is_empty());
    }

    /// The export picks the open notes, or the ones named in any state; the
    /// hand-off marks only the open ones among them handed off.
    #[test]
    fn a_hand_off_marks_the_open_notes_it_gives() {
        let (_dir, path) = file();
        let notes = set(
            &path,
            &[
                mail("mp://tum/inbox/a", "A"),
                mail("mp://tum/inbox/b", "B"),
                mail("mp://tum/inbox/c", "C"),
            ],
            &content(Some("s"), ""),
            NOW,
            &mut |_| {},
        )
        .unwrap();
        done(
            &path,
            &Key::Id(notes[2].id.clone()),
            "done",
            NOW,
            &mut |_| {},
        )
        .unwrap();
        let listed = load(&path).unwrap();
        assert_eq!(select(&listed, None).unwrap().len(), 2);
        let why = refused(select(&listed, Some(&["nope0000".to_string()])));
        assert_eq!(why, "no note has the id nope0000");

        let (given, changed) = hand_off(
            &path,
            Some(&[notes[0].id.clone(), notes[2].id.clone()]),
            LATER,
            &mut |_| {},
        )
        .unwrap();
        assert!(changed);
        let states: Vec<NoteState> = given.iter().map(|note| note.state).collect();
        assert_eq!(states, [NoteState::Done, NoteState::HandedOff]);
        let (open, _) = hand_off(&path, None, LATER, &mut |_| {}).unwrap();
        assert_eq!(open.len(), 1);
        assert_eq!(open[0].id, notes[1].id);
        assert_eq!(
            hand_off(&path, None, LATER, &mut |_| {}).unwrap(),
            (Vec::new(), false)
        );
        let (again, changed) =
            hand_off(&path, Some(&[notes[2].id.clone()]), LATER, &mut |_| {}).unwrap();
        assert_eq!((again.len(), changed), (1, false));
    }

    /// A sent draft's note moves onto the message it went out as, keeping
    /// its id, its state and its snapshot and naming the Sent copy; a draft
    /// with no note, or another account's draft, moves nothing.
    #[test]
    fn a_sent_drafts_note_follows_its_message() {
        let (_dir, path) = file();
        let review = set(
            &path,
            &[Target::Mail {
                on: NoteKey::draft("tum", "d1"),
                mailbox: None,
                from: "sylvain@tum.de".to_string(),
                to: "ada@example.com".to_string(),
                subject: "Re: Review".to_string(),
            }],
            &content(Some("p"), ""),
            NOW,
            &mut |_| {},
        )
        .unwrap()
        .remove(0);
        let mut heard = Vec::new();
        let follow = |account: &str, id: &str, heard: &mut Vec<Vec<Note>>| {
            follow_sent_draft(&path, account, id, "<sent-1@tum.de>", LATER, &mut |notes| {
                heard.push(notes)
            })
            .unwrap()
        };
        assert!(!follow("tum", "d2", &mut heard));
        assert!(!follow("perso", "d1", &mut heard));
        assert!(heard.is_empty());
        assert!(follow("tum", "d1", &mut heard));
        let moved = &heard[0][0];
        assert_eq!(moved.id, review.id);
        assert_eq!(moved.message_id.as_deref(), Some("sent-1@tum.de"));
        assert_eq!(moved.draft_id, None);
        assert_eq!(
            moved.selector.as_deref(),
            Some("mp://tum/sent/sent-1@tum.de")
        );
        assert_eq!(
            (moved.state, moved.subject.as_str()),
            (NoteState::Open, "Re: Review")
        );
        assert_eq!(moved.updated, LATER);
        let marks = NoteMarks::load(&path);
        assert_eq!(marks.of_draft("tum", "d1"), None);
        assert_eq!(
            marks.of_message_id("tum", "sent-1@tum.de"),
            Some(NoteState::Open)
        );
        assert!(!follow("tum", "d1", &mut heard));
    }

    /// Writers racing each other publish in the order they wrote: each list
    /// handed to `publish` holds one note more than the one before, and the
    /// last is the file as it is, so the last `note.changed` a client hears
    /// is never a stale list.
    #[test]
    fn racing_writers_publish_in_the_order_they_wrote() {
        let (_dir, path) = file();
        let published = std::sync::Mutex::new(Vec::<Vec<Note>>::new());
        std::thread::scope(|scope| {
            for n in 0..16 {
                let (path, published) = (&path, &published);
                scope.spawn(move || {
                    set(
                        path,
                        &[mail(&format!("mp://tum/inbox/m{n}"), "M")],
                        &content(None, "x"),
                        NOW,
                        &mut |notes| published.lock().unwrap().push(notes),
                    )
                    .unwrap();
                });
            }
        });
        let published = published.into_inner().unwrap();
        let sizes: Vec<usize> = published.iter().map(Vec::len).collect();
        assert_eq!(sizes, (1..=16).collect::<Vec<_>>());
        let ids = |notes: &[Note]| notes.iter().map(|n| n.id.clone()).collect::<Vec<_>>();
        assert_eq!(ids(published.last().unwrap()), ids(&load(&path).unwrap()));
    }

    /// A file that does not parse is an error for every edit and is left as
    /// it is, and marks nothing in a listing.
    #[test]
    fn a_broken_file_is_never_overwritten() {
        let (_dir, path) = file();
        std::fs::write(&path, b"{ not json").unwrap();
        assert!(matches!(
            set(
                &path,
                &[mail("mp://tum/inbox/a", "A")],
                &content(None, "x"),
                NOW,
                &mut |_| {},
            ),
            Err(NotesError::File(_))
        ));
        assert!(matches!(
            clear_done(&path, &mut |_| {}),
            Err(NotesError::File(_))
        ));
        assert_eq!(std::fs::read(&path).unwrap(), b"{ not json");
        assert_eq!(NoteMarks::load(&path).of_message_id("tum", "a"), None);
    }

    /// The file is private to the user and holds no `orphaned`.
    #[test]
    fn the_file_is_private_and_stores_no_orphaned_flag() {
        use std::os::unix::fs::PermissionsExt;
        let (_dir, path) = file();
        set(
            &path,
            &[mail("mp://tum/inbox/a", "A")],
            &content(None, "x"),
            NOW,
            &mut |_| {},
        )
        .unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("orphaned"), "{text}");
    }

    /// A listing row is marked by its account and Message-ID, in whatever
    /// mailbox it is and whichever form its Message-ID takes, and a draft row
    /// by its id; another account's row with the same Message-ID is not.
    #[test]
    fn a_row_is_marked_by_its_message_id_in_any_mailbox() {
        struct Row(&'static str, &'static str, i64);
        impl MessageRowRef for Row {
            fn mailbox(&self) -> &str {
                self.0
            }
            fn message_id(&self) -> &str {
                self.1
            }
            fn uid(&self) -> i64 {
                self.2
            }
        }
        let (_dir, path) = file();
        set(
            &path,
            &[mail("mp://tum/inbox/a@x", "A")],
            &content(None, "x"),
            NOW,
            &mut |_| {},
        )
        .unwrap();
        set(
            &path,
            &[Target::Mail {
                on: NoteKey::draft("tum", "d1"),
                mailbox: None,
                from: String::new(),
                to: String::new(),
                subject: String::new(),
            }],
            &content(Some("p"), ""),
            NOW,
            &mut |_| {},
        )
        .unwrap();
        let marks = NoteMarks::load(&path);
        for row in [
            Row("inbox", "<a@x>", 7),
            Row("archive", "<a@x>", 3),
            Row("sent", "a@x", 9),
        ] {
            assert_eq!(
                marks.of_message("tum", &row),
                Some(NoteState::Open),
                "{}",
                row.0
            );
        }
        assert_eq!(marks.of_message("perso", &Row("inbox", "<a@x>", 7)), None);
        assert_eq!(marks.of_message("tum", &Row("inbox", "<b@x>", 7)), None);
        assert_eq!(marks.of_draft("tum", "d1"), Some(NoteState::Open));
        assert_eq!(marks.of_draft("tum", "a@x"), None);
        assert_eq!(marks.of_message_id("tum", "d1"), None);
    }

    /// A conversation carries the most pressing note among its members:
    /// open before handed off before done.
    #[test]
    fn a_conversation_carries_its_most_pressing_note() {
        let (_dir, path) = file();
        let notes = set(
            &path,
            &[
                mail("mp://tum/inbox/a", "A"),
                mail("mp://tum/sent/b", "B"),
                mail("mp://tum/inbox/c", "C"),
            ],
            &content(Some("s"), ""),
            NOW,
            &mut |_| {},
        )
        .unwrap();
        done(
            &path,
            &Key::Id(notes[0].id.clone()),
            "done",
            NOW,
            &mut |_| {},
        )
        .unwrap();
        hand_off(&path, Some(&[notes[1].id.clone()]), NOW, &mut |_| {}).unwrap();
        let marks = NoteMarks::load(&path);
        let of = |ids: &[&str]| marks.of_conversation("tum", ids.iter().copied());
        assert_eq!(of(&["<a>", "<b>", "<c>"]), Some(NoteState::Open));
        assert_eq!(of(&["<a>", "<b>", "<z>"]), Some(NoteState::HandedOff));
        assert_eq!(of(&["<a>", "<z>"]), Some(NoteState::Done));
        assert_eq!(of(&["<z>"]), None);
        assert_eq!(marks.of_conversation("perso", ["<a>"]), None);
    }

    /// The batch prompt every client hands an agent, over a received
    /// message, a draft whose target is gone, a new email with a two-line
    /// text, and a preset the configuration dropped.
    #[test]
    fn the_batch_prompt() {
        let note = |id: &str, selector: Option<&str>, preset: Option<&str>, text: &str| {
            let key = selector.map(|s| crate::selector::parse(s).unwrap());
            let draft = key
                .as_ref()
                .is_some_and(|parts| parts.mailbox.as_deref() == Some("drafts"));
            let key = key.map(|parts| parts.key);
            Note {
                id: id.to_string(),
                selector: selector.map(str::to_string),
                account: "tum".to_string(),
                message_id: key.clone().filter(|_| !draft),
                draft_id: key.filter(|_| draft),
                preset: preset.map(str::to_string),
                text: text.to_string(),
                state: NoteState::Open,
                result: None,
                created: NOW.to_string(),
                updated: NOW.to_string(),
                from: String::new(),
                to: String::new(),
                subject: String::new(),
                orphaned: false,
            }
        };
        let notes = vec![
            note(
                "e5a7712c",
                None,
                None,
                "Ask Ada whether the review moves to Friday.\nCopy Bo.",
            ),
            Note {
                from: "\"Fuchs, Stefan\" <stefan.fuchs@tum.de>".to_string(),
                to: "sylvain@tum.de".to_string(),
                subject: "Master Thesis Presentation".to_string(),
                ..note(
                    "c41f09ab",
                    Some("mp://tum/inbox/thesis-42@tum.de"),
                    Some("s"),
                    "",
                )
            },
            Note {
                from: "sylvain@tum.de".to_string(),
                to: "Ada Lovelace <ada@example.com>".to_string(),
                subject: "Re: Review".to_string(),
                orphaned: true,
                ..note(
                    "d0e83b21",
                    Some("mp://tum/drafts/a3f9c2e1b4d5f607"),
                    Some("p"),
                    "",
                )
            },
            Note {
                from: "Jana Weber <jana.weber@tum.de>".to_string(),
                subject: "Budget".to_string(),
                ..note(
                    "f2b6a0c9",
                    Some("mp://tum/inbox/budget-7@tum.de"),
                    Some("q"),
                    "Say yes.",
                )
            },
        ];
        let presets = crate::config::default_note_presets();
        insta::assert_snapshot!(render(&notes, &presets));
        assert_eq!(render(&[], &presets), "");
        assert!(render(&notes[..1], &presets).starts_with("I have a note on one email.\n"));
    }

    /// The committed `note.export` fixture is what this renderer writes for
    /// the notes of `note.list`'s fixture, so a client reading the fixture
    /// reads the daemon's text.
    #[test]
    fn the_export_fixture_is_the_rendered_prompt() {
        let frame = |raw: &str| serde_json::from_str::<serde_json::Value>(raw).unwrap();
        let listed: mp_protocol::note::NoteListing = serde_json::from_value(
            frame(include_str!(
                "../../crates/mp-protocol/fixtures/note.list.response.json"
            ))["result"]
                .clone(),
        )
        .unwrap();
        let exported: mp_protocol::note::NoteExport = serde_json::from_value(
            frame(include_str!(
                "../../crates/mp-protocol/fixtures/note.export.response.json"
            ))["result"]
                .clone(),
        )
        .unwrap();
        let open = select(&listed.notes, None).unwrap();
        let ids: Vec<String> = open.iter().map(|note| note.id.clone()).collect();
        assert_eq!(ids, exported.ids);
        assert_eq!(
            render(&open, &crate::config::default_note_presets()),
            exported.text
        );
    }
}
