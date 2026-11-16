//! The `note.*` family: the notes on mail (PERSO-315).
//!
//! ```text
//! note.set        {account?, selector?|id?, preset?, text?} -> Note
//! note.set_many   {account?, selectors, preset?, text?}     -> {notes}
//! note.list       {state?}                                  -> {notes}
//! note.get        {account?, selector}                      -> {note}
//! note.done       {account?, selector|id, result}           -> Note
//! note.clear      {account?, selector|id}                   -> {removed}
//! note.clear_done {}                                        -> {removed}
//! note.export     {ids?}                                    -> {text, ids}
//! note.handoff    {ids?}                                    -> {text, ids}
//! note.presets    {}                                        -> {presets}
//! ```
//!
//! The file, its order and the batch prompt are [`crate::daemon::notes`].
//! A selector is read in the grammar `mp show` takes, its account from its
//! own `mp://<account>/` segment or else from `account`, and a selector whose
//! mailbox is `drafts` names a draft. `note.set` resolves it against the
//! store, or the drafts directory for a draft, so a note is only ever set on
//! mail the daemon holds, with the snapshot of its headers; the note is held
//! by the message's account and Message-ID, or the draft's id, and keeps the
//! mailbox it was found in as a hint. `note.get`, `note.done` and
//! `note.clear` read the account and the key off the selector and nothing
//! else, so a selector of any copy, in any mailbox, a stale one included,
//! names the note on its message, and no store is read.
//!
//! Every answer decides `orphaned` and `selector` afresh ([`resolve`]): a
//! note whose draft is no longer in the drafts directory, or whose message no
//! mailbox of the account's store holds, is orphaned; an account whose store
//! cannot be read now orphans nothing, and an account the configuration no
//! longer names orphans every note on its mail. The selector names the copy
//! in the hinted mailbox when there is one, else the account's first copy.
//! Every call that wrote the file publishes `note.changed` with the whole
//! list.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use futures::future::BoxFuture;
use serde::de::DeserializeOwned;
use serde_json::Value;

use mp_protocol::events::KIND_NOTE_CHANGED;
use mp_protocol::note::{
    Note, NoteClearParams, NoteCleared, NoteDoneParams, NoteExport, NoteExportParams,
    NoteGetParams, NoteListParams, NoteListing, NoteLookup, NotePreset, NotePresets,
    NoteSetManyParams, NoteSetParams,
};
use mp_protocol::RpcError;

use super::super::config::ConfigStore;
use super::super::dispatch::{
    CancelToken, ClientCtx, Dispatcher, DomainError, Method, MethodKind, MethodSpec, Outcome,
    ResourceId,
};
use super::super::notes::{self, Content, Key, NoteKey, NotesError, Target};
use super::super::state::events::Event;
use super::super::state::CanonicalState;
use super::{internal, invalid_params, only_params};
use crate::config::AccountConfig;
use crate::selector::{Namespace, Selector, DRAFTS_MAILBOX};
use crate::store::Store;

/// The family, in method-name order.
pub const NOTE_METHOD_SPECS: [MethodSpec; 10] = [
    MethodSpec::new("note.clear", MethodKind::Command, 1),
    MethodSpec::new("note.clear_done", MethodKind::Command, 1),
    MethodSpec::new("note.done", MethodKind::Command, 1),
    MethodSpec::new("note.export", MethodKind::Query, 1),
    MethodSpec::new("note.get", MethodKind::Query, 1),
    MethodSpec::new("note.handoff", MethodKind::Command, 1),
    MethodSpec::new("note.list", MethodKind::Query, 1),
    MethodSpec::new("note.presets", MethodKind::Query, 1),
    MethodSpec::new("note.set", MethodKind::Command, 1),
    MethodSpec::new("note.set_many", MethodKind::Command, 1),
];

/// The resource every writer names: the one list.
const NOTES_RESOURCE: &str = "note:list";

/// Register the family on `dispatcher`.
pub fn register(
    dispatcher: &mut Dispatcher,
    config: Arc<ConfigStore>,
    canonical: Arc<CanonicalState>,
) {
    for spec in NOTE_METHOD_SPECS {
        dispatcher.register(Arc::new(NoteMethod {
            spec,
            config: Arc::clone(&config),
            canonical: Arc::clone(&canonical),
        }));
    }
}

/// One method of the family, selected by its own [`MethodSpec`].
pub struct NoteMethod {
    spec: MethodSpec,
    config: Arc<ConfigStore>,
    canonical: Arc<CanonicalState>,
}

impl Method for NoteMethod {
    fn spec(&self) -> MethodSpec {
        self.spec
    }

    fn call<'a>(
        &'a self,
        _ctx: &'a ClientCtx,
        params: Value,
        _cancel: CancelToken,
    ) -> BoxFuture<'a, Result<Outcome, DomainError>> {
        Box::pin(async move {
            match self.spec.name {
                "note.set" => self.set(&params).await,
                "note.set_many" => self.set_many(&params).await,
                "note.list" => self.list(&params).await,
                "note.get" => self.get(&params).await,
                "note.done" => self.done(&params).await,
                "note.clear" => self.clear(&params).await,
                "note.clear_done" => self.clear_done(&params).await,
                "note.export" => self.export(&params, false).await,
                "note.handoff" => self.export(&params, true).await,
                _ => self.presets(&params),
            }
        })
    }
}

/// What a writer hands the notes file to announce the list it wrote: it
/// decides each note's selector and orphan mark, publishes `note.changed`
/// while the file's lock is still held, so two writers' events leave in the
/// order their writes did, and keeps the list for the answer.
struct Announcer {
    canonical: Arc<CanonicalState>,
    accounts: Arc<Vec<AccountConfig>>,
    published: Option<Vec<Note>>,
}

impl Announcer {
    fn publish(&mut self, mut notes: Vec<Note>) {
        resolve(&mut notes, &self.accounts);
        self.canonical.publish(note_changed(notes.clone()));
        self.published = Some(notes);
    }

    /// The list after the call: the one published, or the file read now
    /// when the call wrote nothing.
    fn listing(self, path: &std::path::Path) -> Result<Vec<Note>, DomainError> {
        match self.published {
            Some(notes) => Ok(notes),
            None => listed(path, &self.accounts),
        }
    }
}

impl NoteMethod {
    /// An announcer for one writer over the live configuration's accounts.
    fn announcer(&self, accounts: &Arc<Vec<AccountConfig>>) -> Announcer {
        Announcer {
            canonical: Arc::clone(&self.canonical),
            accounts: Arc::clone(accounts),
            published: None,
        }
    }

    async fn set(&self, params: &Value) -> Result<Outcome, DomainError> {
        let request: NoteSetParams = params_of(
            self.spec.name,
            params,
            &["account", "selector", "id", "preset", "text"],
        )?;
        let content = content(&request.preset, &request.text, &self.configured_presets())?;
        let accounts = self.config.accounts();
        let mut announcer = self.announcer(&accounts);
        let path = notes::path();
        let answer = blocking(move || {
            let target = match (&request.selector, &request.id) {
                (Some(_), Some(_)) => {
                    return Err(invalid_params(
                        "selector and id are two addresses; send at most one",
                    )
                    .into())
                }
                (Some(selector), None) => {
                    resolve_target(selector, request.account.as_deref(), &accounts)?
                }
                (None, Some(id)) => Target::Id(id.clone()),
                (None, None) => {
                    let account = request.account.as_deref().ok_or_else(|| {
                        invalid_params(
                            "a note for a new email names the account it goes out from; \
                             send account, or a selector or an id",
                        )
                    })?;
                    super::account::configured_account(&accounts, account)?;
                    Target::NewEmail {
                        account: account.to_string(),
                    }
                }
            };
            let set = notes::set(&path, &[target], &content, &notes::now(), &mut |notes| {
                announcer.publish(notes)
            })
            .map_err(refusal)?;
            Ok(pick(&announcer.listing(&path)?, &set).remove(0))
        })
        .await?;
        self.command(&answer)
    }

    async fn set_many(&self, params: &Value) -> Result<Outcome, DomainError> {
        let request: NoteSetManyParams = params_of(
            self.spec.name,
            params,
            &["account", "selectors", "preset", "text"],
        )?;
        if request.selectors.is_empty() {
            return Err(invalid_params("selectors names no message or draft").into());
        }
        let content = content(&request.preset, &request.text, &self.configured_presets())?;
        let accounts = self.config.accounts();
        let mut announcer = self.announcer(&accounts);
        let path = notes::path();
        let answer = blocking(move || {
            let mut targets: Vec<Target> = Vec::new();
            for selector in &request.selectors {
                let target = resolve_target(selector, request.account.as_deref(), &accounts)?;
                // Two selectors of one message, in two mailboxes or not, are
                // one note.
                if !targets.iter().any(|known| known.on() == target.on()) {
                    targets.push(target);
                }
            }
            let set = notes::set(&path, &targets, &content, &notes::now(), &mut |notes| {
                announcer.publish(notes)
            })
            .map_err(refusal)?;
            Ok(NoteListing {
                notes: pick(&announcer.listing(&path)?, &set),
            })
        })
        .await?;
        self.command(&answer)
    }

    async fn list(&self, params: &Value) -> Result<Outcome, DomainError> {
        let request: NoteListParams = params_of(self.spec.name, params, &["state"])?;
        let accounts = self.config.accounts();
        let path = notes::path();
        let mut notes = blocking(move || listed(&path, &accounts)).await?;
        if let Some(state) = request.state {
            notes.retain(|note| note.state == state);
        }
        Ok(Outcome::query(to_value(
            self.spec.name,
            &NoteListing { notes },
        )?))
    }

    async fn get(&self, params: &Value) -> Result<Outcome, DomainError> {
        let request: NoteGetParams = params_of(self.spec.name, params, &["account", "selector"])?;
        let accounts = self.config.accounts();
        let path = notes::path();
        let note = blocking(move || {
            let on = key_on(&request.selector, request.account.as_deref())?;
            Ok(listed(&path, &accounts)?
                .into_iter()
                .find(|note| is_on(note, &on)))
        })
        .await?;
        Ok(Outcome::query(to_value(
            self.spec.name,
            &NoteLookup { note },
        )?))
    }

    async fn done(&self, params: &Value) -> Result<Outcome, DomainError> {
        let request: NoteDoneParams = params_of(
            self.spec.name,
            params,
            &["account", "selector", "id", "result"],
        )?;
        if request.result.trim().is_empty() {
            return Err(invalid_params(
                "result is the line a client shows on the mail, and may not be empty",
            )
            .into());
        }
        let accounts = self.config.accounts();
        let mut announcer = self.announcer(&accounts);
        let path = notes::path();
        let answer = blocking(move || {
            let key = key_of(
                request.selector.as_deref(),
                request.id.as_deref(),
                request.account.as_deref(),
            )?;
            let closed = notes::done(&path, &key, &request.result, &notes::now(), &mut |notes| {
                announcer.publish(notes)
            })
            .map_err(refusal)?;
            Ok(pick(&announcer.listing(&path)?, &[closed]).remove(0))
        })
        .await?;
        self.command(&answer)
    }

    async fn clear(&self, params: &Value) -> Result<Outcome, DomainError> {
        let request: NoteClearParams =
            params_of(self.spec.name, params, &["account", "selector", "id"])?;
        let accounts = self.config.accounts();
        let mut announcer = self.announcer(&accounts);
        let path = notes::path();
        let removed = blocking(move || {
            let key = key_of(
                request.selector.as_deref(),
                request.id.as_deref(),
                request.account.as_deref(),
            )?;
            notes::clear(&path, &key, &mut |notes| announcer.publish(notes)).map_err(refusal)
        })
        .await?;
        self.command(&NoteCleared { removed })
    }

    async fn clear_done(&self, params: &Value) -> Result<Outcome, DomainError> {
        let _: Value = params_of(self.spec.name, params, &[])?;
        let accounts = self.config.accounts();
        let mut announcer = self.announcer(&accounts);
        let path = notes::path();
        let removed = blocking(move || {
            notes::clear_done(&path, &mut |notes| announcer.publish(notes)).map_err(refusal)
        })
        .await?;
        self.command(&NoteCleared { removed })
    }

    /// `note.export`, and with `hand_off` `note.handoff`, which marks the
    /// open notes it gives handed off.
    async fn export(&self, params: &Value, hand_off: bool) -> Result<Outcome, DomainError> {
        let request: NoteExportParams = params_of(self.spec.name, params, &["ids"])?;
        let presets = self.configured_presets();
        let accounts = self.config.accounts();
        let mut announcer = self.announcer(&accounts);
        let path = notes::path();
        let answer = blocking(move || {
            let ids = request.ids.as_deref();
            let picked = if hand_off {
                let (given, _) = notes::hand_off(&path, ids, &notes::now(), &mut |notes| {
                    announcer.publish(notes)
                })
                .map_err(refusal)?;
                pick(&announcer.listing(&path)?, &given)
            } else {
                notes::select(&listed(&path, &accounts)?, ids).map_err(refusal)?
            };
            Ok(NoteExport {
                text: notes::render(&picked, &presets),
                ids: picked.into_iter().map(|note| note.id).collect(),
            })
        })
        .await?;
        if hand_off {
            self.command(&answer)
        } else {
            Ok(Outcome::query(to_value(self.spec.name, &answer)?))
        }
    }

    /// `note.presets`, which reads the live configuration and nothing else.
    fn presets(&self, params: &Value) -> Result<Outcome, DomainError> {
        let _: Value = params_of(self.spec.name, params, &[])?;
        let answer = NotePresets {
            presets: self.configured_presets(),
        };
        Ok(Outcome::query(to_value(self.spec.name, &answer)?))
    }

    /// The presets of the live configuration, in prompt order.
    fn configured_presets(&self) -> Vec<NotePreset> {
        self.config.snapshot().config.notes.presets.clone()
    }

    /// The outcome of a writer, whose `note.changed`, when it wrote, is
    /// already published: the answer at the revision the daemon is at.
    fn command<T: serde::Serialize>(&self, answer: &T) -> Result<Outcome, DomainError> {
        Ok(Outcome::command(
            to_value(self.spec.name, answer)?,
            self.canonical.revision().get(),
            vec![ResourceId::new(NOTES_RESOURCE)],
        ))
    }
}

/// The `note.changed` of a list that moved.
pub fn note_changed(notes: Vec<Note>) -> Event {
    Event::Lifecycle {
        kind: KIND_NOTE_CHANGED,
        payload: serde_json::to_value(NoteListing { notes })
            .unwrap_or_else(|_| serde_json::json!({"notes": []})),
    }
}

/// The parameters as their wire type, after refusing one the method does not
/// take.
fn params_of<T: DeserializeOwned>(
    method: &str,
    params: &Value,
    allowed: &[&str],
) -> Result<T, DomainError> {
    only_params(method, params, allowed)?;
    let params = match params {
        Value::Null => Value::Object(Default::default()),
        other => other.clone(),
    };
    serde_json::from_value(params)
        .map_err(|e| invalid_params(format!("the parameters of {method}: {e}")).into())
}

/// The instruction of a set: a configured preset, a text, or both.
fn content(
    preset: &Option<String>,
    text: &Option<String>,
    presets: &[NotePreset],
) -> Result<Content, DomainError> {
    if let Some(key) = preset {
        if !presets.iter().any(|preset| preset.key == *key) {
            let keys: Vec<&str> = presets.iter().map(|preset| preset.key.as_str()).collect();
            return Err(invalid_params(format!(
                "preset {key} is not configured; the presets are {}",
                keys.join(", ")
            ))
            .into());
        }
    }
    let text = text.as_deref().unwrap_or_default().trim_end().to_string();
    if preset.is_none() && text.trim().is_empty() {
        return Err(invalid_params("a note needs a preset or a text").into());
    }
    Ok(Content {
        preset: preset.clone(),
        text,
    })
}

/// The selector query `selector` asks, in the namespace its mailbox names.
fn query_of(
    selector: &str,
    account: Option<&str>,
) -> Result<crate::selector::SelectorQuery, RpcError> {
    let parts = crate::selector::parse(selector).map_err(|e| invalid_params(format!("{e:#}")))?;
    let namespace = if parts.mailbox.as_deref() == Some(DRAFTS_MAILBOX) {
        Namespace::Drafts
    } else {
        Namespace::Received
    };
    crate::selector::parse_in(selector, namespace, account.unwrap_or_default(), None)
        .map_err(|e| invalid_params(format!("{e:#}")))
}

/// The message or draft `selector` names, resolved, with the snapshot a note
/// keeps of it.
fn resolve_target(
    selector: &str,
    account: Option<&str>,
    accounts: &[AccountConfig],
) -> Result<Target, DomainError> {
    let query = query_of(selector, account)?;
    let configured = super::account::configured_account(accounts, &query.account)?;
    if query.namespace == Namespace::Drafts {
        let row = super::draft::resolve(&query.account, &query.key)?;
        return Ok(Target::Mail {
            on: NoteKey::draft(&query.account, &row.id),
            mailbox: None,
            from: configured.default_from.clone(),
            to: row.to.unwrap_or_default(),
            subject: row.subject.unwrap_or_default(),
        });
    }
    let row = received(&query, accounts)?;
    Ok(Target::Mail {
        on: NoteKey::message(&query.account, &row.message_id),
        mailbox: Some(row.mailbox),
        from: row.from.unwrap_or_default(),
        to: row.to.unwrap_or_default(),
        subject: row.subject.unwrap_or_default(),
    })
}

/// The one stored message `query` names, or the resolver's refusal.
fn received(
    query: &crate::selector::SelectorQuery,
    accounts: &[AccountConfig],
) -> Result<crate::store::read::MessageRow, DomainError> {
    super::account::ready_account(accounts, &query.account)?;
    let store = Store::open(crate::config::store_path(&query.account))
        .map_err(|e| internal(format!("opening the store of {}: {e:#}", query.account)))?;
    crate::selector::resolve_received(&store, query)
        .map(|(row, _)| row)
        .map_err(|e| invalid_params(format!("{e:#}")).into())
}

/// What a note on `selector` is held by: its account and its key, a message's
/// Message-ID or a draft's id. The mailbox and the UID segment of a message
/// selector are not read, so the selector of any copy, in any mailbox, names
/// the note on its message; nothing is resolved against the store.
fn key_on(selector: &str, account: Option<&str>) -> Result<NoteKey, DomainError> {
    let query = query_of(selector, account)?;
    Ok(match query.namespace {
        Namespace::Drafts => NoteKey::draft(&query.account, &query.key),
        Namespace::Received => NoteKey::message(&query.account, &query.key),
    })
}

/// Whether `note` is on the mail `on` names.
fn is_on(note: &Note, on: &NoteKey) -> bool {
    match on {
        NoteKey::Message {
            account,
            message_id,
        } => note.account == *account && note.message_id.as_deref() == Some(message_id.as_str()),
        NoteKey::Draft { account, id } => {
            note.account == *account && note.draft_id.as_deref() == Some(id.as_str())
        }
    }
}

/// The note a `note.done` or `note.clear` names: by selector or by id,
/// exactly one.
fn key_of(
    selector: Option<&str>,
    id: Option<&str>,
    account: Option<&str>,
) -> Result<Key, DomainError> {
    match (selector, id) {
        (Some(selector), None) => Ok(Key::On(key_on(selector, account)?)),
        (None, Some(id)) => Ok(Key::Id(id.to_string())),
        _ => {
            Err(invalid_params("a note is addressed by selector or by id; send exactly one").into())
        }
    }
}

/// Every note, newest first, each with its selector and `orphaned` decided.
fn listed(path: &std::path::Path, accounts: &[AccountConfig]) -> Result<Vec<Note>, DomainError> {
    let mut notes = notes::load(path).map_err(|e| DomainError::from(internal(format!("{e:#}"))))?;
    resolve(&mut notes, accounts);
    Ok(notes)
}

/// The notes of `listing` that `wanted` names, in `wanted`'s order, so an
/// answer carries the selector and the orphan mark the listing decided.
fn pick(listing: &[Note], wanted: &[Note]) -> Vec<Note> {
    wanted
        .iter()
        .map(|note| {
            listing
                .iter()
                .find(|listed| listed.id == note.id)
                .cloned()
                .unwrap_or_else(|| note.clone())
        })
        .collect()
}

/// Decide each note's `selector` and `orphaned` against the mail as it is
/// now, reading each account's store and drafts directory at most once.
///
/// A draft note is orphaned when its id is not in the drafts index. A message
/// note is orphaned when no mailbox of its account holds its Message-ID, in
/// either form; otherwise its selector names the copy in the mailbox it was
/// set in when that mailbox still holds one, else the first copy, with a UID
/// segment when that mailbox holds the message twice. A note of an account
/// the configuration no longer names is orphaned, and one whose store cannot
/// be read now is not, keeping the selector of the mailbox it was set in.
pub fn resolve(notes: &mut [Note], accounts: &[AccountConfig]) {
    let mut stores: HashMap<String, Option<Store>> = HashMap::new();
    let mut drafts: HashMap<String, HashSet<String>> = HashMap::new();
    for note in notes.iter_mut() {
        if note.message_id.is_none() && note.draft_id.is_none() {
            continue;
        }
        if !accounts
            .iter()
            .any(|configured| configured.name == note.account)
        {
            note.orphaned = true;
            continue;
        }
        if let Some(id) = &note.draft_id {
            let ids = drafts.entry(note.account.clone()).or_insert_with(|| {
                crate::store::drafts::index_dir(&crate::config::drafts_dir(&note.account))
                    .0
                    .into_iter()
                    .map(|row| row.id)
                    .collect()
            });
            note.orphaned = !ids.contains(id);
            continue;
        }
        let Some(key) = note.message_id.clone() else {
            continue;
        };
        let store = stores.entry(note.account.clone()).or_insert_with(|| {
            super::account::ready_account(accounts, &note.account)
                .ok()
                .and_then(|_| Store::open(crate::config::store_path(&note.account)).ok())
        });
        let Some(store) = store else {
            continue;
        };
        match copies(store, &note.account, &key) {
            Ok(rows) if rows.is_empty() => note.orphaned = true,
            Ok(rows) => {
                let hint = note
                    .selector
                    .as_deref()
                    .and_then(|selector| crate::selector::parse(selector).ok())
                    .and_then(|parts| parts.mailbox);
                let row = rows
                    .iter()
                    .find(|row| Some(&row.mailbox) == hint.as_ref())
                    .unwrap_or(&rows[0]);
                let twins = crate::selector::Twins::among(rows.iter());
                note.selector = Some(Selector::for_listed(&note.account, row, &twins).to_string());
                note.orphaned = false;
            }
            Err(e) => {
                log::warn!(
                    "[notes] reading the message {key} in the store of {}: {e:#}",
                    note.account
                );
            }
        }
    }
}

/// Every row `account`'s store holds for the Message-ID `key`, in any
/// mailbox, under the bracketed form ingest stores or the bare one.
fn copies(
    store: &Store,
    account: &str,
    key: &str,
) -> anyhow::Result<Vec<crate::store::read::MessageRow>> {
    let rows = crate::store::read::find_by_message_id(store, account, &format!("<{key}>"))?;
    if !rows.is_empty() {
        return Ok(rows);
    }
    crate::store::read::find_by_message_id(store, account, key)
}

/// A notes-file refusal as the caller's error, a file that will not read or
/// write as the daemon's.
fn refusal(error: NotesError) -> DomainError {
    match error {
        NotesError::Refused(why) => invalid_params(why).into(),
        NotesError::File(e) => internal(format!("{e:#}")).into(),
    }
}

/// Run a file or store operation off the dispatcher's thread.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, DomainError> + Send + 'static,
) -> Result<T, DomainError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|e| DomainError::internal(format!("the notes task {e}")))?
}

fn to_value<T: serde::Serialize>(method: &str, answer: &T) -> Result<Value, DomainError> {
    serde_json::to_value(answer).map_err(|e| internal(format!("serialising {method}: {e}")).into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::dispatch::CancelScope;

    /// The family, in method-name order, durable; the four reads queries and
    /// the six writers commands.
    #[test]
    fn the_family_declares_ten_methods_in_name_order() {
        use mp_protocol::note::*;
        let names: Vec<&str> = NOTE_METHOD_SPECS.iter().map(|s| s.name).collect();
        assert_eq!(
            names,
            [
                METHOD_NOTE_CLEAR,
                METHOD_NOTE_CLEAR_DONE,
                METHOD_NOTE_DONE,
                METHOD_NOTE_EXPORT,
                METHOD_NOTE_GET,
                METHOD_NOTE_HANDOFF,
                METHOD_NOTE_LIST,
                METHOD_NOTE_PRESETS,
                METHOD_NOTE_SET,
                METHOD_NOTE_SET_MANY,
            ]
        );
        for spec in NOTE_METHOD_SPECS {
            assert_eq!(spec.cancel_scope, CancelScope::Durable, "{}", spec.name);
            let query = matches!(
                spec.name,
                "note.export" | "note.get" | "note.list" | "note.presets"
            );
            let kind = if query {
                MethodKind::Query
            } else {
                MethodKind::Command
            };
            assert_eq!(spec.kind, kind, "{}", spec.name);
        }
    }

    /// A set needs a configured preset or a text; trailing blank lines go.
    #[test]
    fn a_note_needs_a_configured_preset_or_a_text() {
        let presets = crate::config::default_note_presets();
        let refused = |preset: Option<&str>, text: Option<&str>| {
            let error = content(
                &preset.map(str::to_string),
                &text.map(str::to_string),
                &presets,
            )
            .unwrap_err();
            format!("{error:?}")
        };
        assert!(refused(None, None).contains("a note needs a preset or a text"));
        assert!(refused(None, Some("  \n")).contains("a note needs a preset or a text"));
        assert!(refused(Some("q"), None)
            .contains("preset q is not configured; the presets are r, d, s, e, p, t, f"));
        let ok = content(&Some("s".into()), &None, &presets).unwrap();
        assert_eq!(ok.text, "");
        let ok = content(&None, &Some("say yes\n\n".into()), &presets).unwrap();
        assert_eq!(ok.text, "say yes");
    }
}
