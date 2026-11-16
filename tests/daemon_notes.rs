//! The notes on mail over a real socket, and `mp notes` in front of them
//! (PERSO-315).
//!
//! The file, its order and the batch prompt are unit-tested beside their code
//! (`src/daemon/notes.rs`, whose snapshot pins the prompt); this file pins
//! what a client sees: each `note.*` answer and refusal, the `note.changed`
//! every write publishes, the `note` a message listing row and a draft row
//! carry, the notes outliving a rebuilt store, and the CLI's verbs.
//!
//! One account with a server and no credentials, on the discard port with the
//! scheduler off, so nothing ever connects; its store holds two Inbox
//! messages, and its drafts directory one draft.

mod support;

use std::path::Path;
use std::time::Duration;

use serde_json::{json, Value};
use tempfile::TempDir;

use mp_client::{ClientError, ClientInfo, ClientKind, Connection, Identity};
use mp_protocol::events::KIND_NOTE_CHANGED;
use mp_protocol::note::{Note, NoteExport, NoteListing, NoteState};

use mailypoppins::ingest::{ingest_message, record_mailbox_cursor, IngestInput, MailboxCursor};
use mailypoppins::store::{BlobStore, Store};

use support::events::next_event;
use support::parity::{socket_path, DaemonFixture};

const DEADLINE: Duration = Duration::from_secs(20);
const ACCOUNT: &str = "alpha";
const THESIS: &str = "mp://alpha/inbox/thesis@tum.de";
const BUDGET: &str = "mp://alpha/inbox/budget@tum.de";
const DRAFT: &str = "mp://alpha/drafts/a3f9c2e1b4d5f607";

const CONFIG: &str = r#"
[[accounts]]
name = "alpha"
default_from = "alpha@example.com"

[accounts.imap]
host = "127.0.0.1"
port = 9
username = "alpha@example.com"
sync_interval_secs = 0

[accounts.mailboxes.inbox]
server = "INBOX"

[accounts.mailboxes.archive]
server = "Archive"
"#;

/// A seeded root and a daemon serving it. The field order is the drop order.
struct Slice {
    daemon: DaemonFixture,
    tmp: TempDir,
}

fn store_path(root: &Path) -> std::path::PathBuf {
    root.join("accounts").join(ACCOUNT).join("store.sqlite3")
}

/// Ingest one Inbox message; its Message-ID is `<id>`.
fn ingest(root: &Path, uid: i64, id: &str, from: &str, subject: &str) {
    ingest_raw(
        root,
        uid,
        &format!(
            "From: {from}\r\nTo: alpha@example.com\r\nSubject: {subject}\r\nMessage-ID: <{id}>\r\nDate: Mon, 16 Nov 2026 09:00:00 +0100\r\n\r\nbody\r\n"
        ),
    );
}

/// Ingest one Inbox message from its raw bytes.
fn ingest_raw(root: &Path, uid: i64, raw: &str) {
    let dir = root.join("accounts").join(ACCOUNT);
    let store = Store::open(store_path(root)).unwrap();
    let blobs = BlobStore::new(dir.join("blobs"));
    let email = mailypoppins::parse::parse_rfc822_to_fetched_email(raw.as_bytes()).unwrap();
    ingest_message(
        &store,
        &blobs,
        &IngestInput {
            account: ACCOUNT,
            mailbox: "inbox",
            uid,
            email: &email,
            raw: Some(raw.as_bytes()),
        },
    )
    .unwrap();
}

/// The configuration, a synced store holding the two messages, and the draft.
fn seed(root: &Path, config: &str) {
    std::fs::write(root.join("config.toml"), config).unwrap();
    let dir = root.join("accounts").join(ACCOUNT);
    std::fs::create_dir_all(dir.join("drafts")).unwrap();
    let store = Store::open(store_path(root)).unwrap();
    record_mailbox_cursor(
        &store,
        ACCOUNT,
        "inbox",
        &MailboxCursor {
            uidvalidity: Some(7),
            last_uid: Some(10),
            ..MailboxCursor::default()
        },
    )
    .unwrap();
    ingest(
        root,
        1,
        "thesis@tum.de",
        "Stefan Fuchs <stefan.fuchs@tum.de>",
        "Master Thesis Presentation",
    );
    ingest(
        root,
        2,
        "budget@tum.de",
        "Jana Weber <jana@tum.de>",
        "Budget",
    );
    std::fs::write(
        dir.join("drafts").join("2026-11-16-review.md"),
        "---\nid: a3f9c2e1b4d5f607\nto: Ada Lovelace <ada@example.com>\nsubject: \"Re: Review\"\nstatus: draft\n---\n\nThanks, Ada.\n",
    )
    .unwrap();
}

impl Slice {
    fn start() -> Slice {
        Slice::with_config(CONFIG)
    }

    fn with_config(config: &str) -> Slice {
        let tmp = TempDir::new().unwrap();
        seed(tmp.path(), config);
        let daemon = DaemonFixture::start(tmp.path());
        Slice { daemon, tmp }
    }

    fn root(&self) -> &Path {
        self.tmp.path()
    }

    /// Stop the daemon, let `change` edit the root it served, and start a
    /// daemon over it again: the store is the daemon's while it runs.
    fn restarted(self, change: impl FnOnce(&Path)) -> Slice {
        let Slice { daemon, tmp } = self;
        daemon.stop();
        change(tmp.path());
        let daemon = DaemonFixture::start(tmp.path());
        Slice { daemon, tmp }
    }

    async fn connect(&self) -> Connection {
        connect(self.root()).await
    }

    /// `mp notes <args>` against the daemon, which must route.
    fn mp(&self, args: &[&str]) -> (String, String, Option<i32>) {
        let out = self.daemon.mp_routed(args);
        (
            String::from_utf8_lossy(&out.stdout).to_string(),
            String::from_utf8_lossy(&out.stderr).to_string(),
            out.status.code(),
        )
    }
}

async fn connect(root: &Path) -> Connection {
    let mut conn = tokio::time::timeout(DEADLINE, Connection::connect(&socket_path(root)))
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(
        DEADLINE,
        conn.initialize(
            ClientInfo {
                kind: ClientKind::Cli,
                app_version: env!("CARGO_PKG_VERSION").to_string(),
            },
            Identity {
                data_dir: root.to_path_buf(),
                config_dir: root.to_path_buf(),
            },
            &[],
            &[],
        ),
    )
    .await
    .unwrap()
    .unwrap();
    conn
}

async fn call(conn: &mut Connection, method: &str, params: Value) -> Result<Value, ClientError> {
    tokio::time::timeout(DEADLINE, conn.call(method, params))
        .await
        .unwrap_or_else(|_| panic!("{method} went unanswered"))
}

async fn ok(conn: &mut Connection, method: &str, params: Value) -> Value {
    call(conn, method, params)
        .await
        .unwrap_or_else(|e| panic!("{method}: {e}"))
}

/// The refusal's code and sentence.
async fn refused(conn: &mut Connection, method: &str, params: Value) -> (i32, String) {
    match call(conn, method, params).await {
        Err(ClientError::Rpc(error)) => (error.code, error.message),
        other => panic!("{method} was not refused: {other:?}"),
    }
}

/// The next `note.changed` on a bootstrapped connection.
async fn next_notes_event(conn: &mut Connection) -> NoteListing {
    loop {
        let event = next_event(conn, DEADLINE, "note.changed").await;
        if event.kind == KIND_NOTE_CHANGED {
            return serde_json::from_value(event.payload).unwrap();
        }
    }
}

fn note(value: Value) -> Note {
    serde_json::from_value(value).unwrap()
}

fn listing(value: Value) -> NoteListing {
    serde_json::from_value(value).unwrap()
}

/// The `note` of the Inbox row whose selector is `selector`, as `message.list`
/// answers it.
async fn row_note(conn: &mut Connection, selector: &str) -> Value {
    let listed = ok(
        conn,
        "message.list",
        json!({"account": ACCOUNT, "mailbox": "inbox"}),
    )
    .await;
    let row = listed["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["selector"] == selector)
        .unwrap_or_else(|| panic!("no row {selector} in {listed}"))
        .clone();
    row.get("note").cloned().unwrap_or(Value::Null)
}

/// A note set on a message by an elided selector is held by its canonical
/// one, with the snapshot of its headers; every client hears `note.changed`
/// with the whole list, and the message's listing row, its search hit and
/// its streamed row carry the note's state while the other row carries none.
#[tokio::test]
async fn a_note_on_a_message_marks_its_row_and_every_client_hears_it() {
    let slice = Slice::start();
    let mut watcher = slice.connect().await;
    ok(&mut watcher, "state.bootstrap", json!({})).await;
    let mut conn = slice.connect().await;

    let set = note(
        ok(
            &mut conn,
            "note.set",
            json!({"account": ACCOUNT, "selector": "inbox/thesis@tum.de", "preset": "s"}),
        )
        .await,
    );
    assert_eq!(set.selector.as_deref(), Some(THESIS));
    assert_eq!(set.account, ACCOUNT);
    assert_eq!(set.message_id.as_deref(), Some("thesis@tum.de"));
    assert_eq!(set.draft_id, None);
    assert_eq!(set.state, NoteState::Open);
    assert_eq!(set.from, "Stefan Fuchs <stefan.fuchs@tum.de>");
    assert_eq!(set.subject, "Master Thesis Presentation");
    assert!(!set.orphaned);

    let heard = next_notes_event(&mut watcher).await;
    assert_eq!(heard.notes, std::slice::from_ref(&set));

    assert_eq!(row_note(&mut conn, THESIS).await, json!("open"));
    assert_eq!(row_note(&mut conn, BUDGET).await, Value::Null);
    let hits = ok(
        &mut conn,
        "message.search",
        json!({"account": ACCOUNT, "query": "subject:Thesis"}),
    )
    .await;
    assert_eq!(hits["hits"][0]["note"], json!("open"), "{hits}");

    // A UID-qualified selector, as a listing prints for a twin, names the
    // same note.
    let got = ok(
        &mut conn,
        "note.get",
        json!({"selector": format!("{THESIS}/1")}),
    )
    .await;
    assert_eq!(note(got["note"].clone()).id, set.id);
    let none = ok(&mut conn, "note.get", json!({"selector": BUDGET})).await;
    assert_eq!(none, json!({"note": null}));
}

/// Every notification of one stream until its finish, by method.
async fn stream(conn: &mut Connection, method: &str) -> Vec<(String, Value)> {
    let started = ok(
        conn,
        method,
        json!({"account": ACCOUNT, "mailbox": "inbox"}),
    )
    .await;
    let id = started["operation_id"].clone();
    let mut seen = Vec::new();
    loop {
        let notification = tokio::time::timeout(DEADLINE, conn.next_notification())
            .await
            .expect("the stream goes on")
            .expect("the daemon keeps the connection open");
        if notification.method == mp_protocol::METHOD_STATE_EVENT
            && notification.params["kind"] == json!("operation.finished")
            && notification.params["payload"]["operation_id"] == id
        {
            return seen;
        }
        seen.push((notification.method, notification.params));
    }
}

/// The rows of the two streamed listings carry the note as `message.list`'s
/// do: a flat row by itself, a conversation by its `latest` row.
#[tokio::test]
async fn the_streamed_rows_carry_the_note() {
    use mp_protocol::listing::{ConversationRowsChunk, MessageRowsChunk};
    use mp_protocol::{METHOD_MESSAGE_CONVERSATIONS, METHOD_MESSAGE_ROWS};

    let slice = Slice::start();
    let mut conn = slice.connect().await;
    ok(
        &mut conn,
        "note.set",
        json!({"selector": THESIS, "preset": "s"}),
    )
    .await;
    ok(&mut conn, "state.bootstrap", json!({})).await;

    let mut flat = Vec::new();
    for (method, params) in stream(&mut conn, "message.list_stream").await {
        if method == METHOD_MESSAGE_ROWS {
            let chunk: MessageRowsChunk = serde_json::from_value(params).unwrap();
            flat.extend(chunk.rows.into_iter().map(|row| (row.selector, row.note)));
        }
    }
    flat.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        flat,
        [
            (BUDGET.to_string(), None),
            (THESIS.to_string(), Some(NoteState::Open))
        ]
    );

    let mut grouped = Vec::new();
    for (method, params) in stream(&mut conn, "message.list_conversations").await {
        if method == METHOD_MESSAGE_CONVERSATIONS {
            let chunk: ConversationRowsChunk = serde_json::from_value(params).unwrap();
            grouped.extend(
                chunk
                    .rows
                    .into_iter()
                    .map(|row| (row.latest.selector, row.latest.note)),
            );
        }
    }
    grouped.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(grouped, flat);
}

/// A conversation carries the most pressing note among its members, here
/// the thesis under a later reply that has none, while its `latest` row
/// carries the reply's own; `message.thread` gives each member its note.
#[tokio::test]
async fn a_conversation_and_its_members_carry_their_notes() {
    use mp_protocol::listing::{ConversationRowsChunk, ThreadListing};
    use mp_protocol::METHOD_MESSAGE_CONVERSATIONS;

    let slice = Slice::start();
    let mut conn = slice.connect().await;
    ok(
        &mut conn,
        "note.set",
        json!({"selector": THESIS, "preset": "s"}),
    )
    .await;
    drop(conn);
    let slice = slice.restarted(|root| {
        ingest_raw(
            root,
            3,
            "From: Stefan Fuchs <stefan.fuchs@tum.de>\r\nTo: alpha@example.com\r\nSubject: Re: Master Thesis Presentation\r\nMessage-ID: <reply@tum.de>\r\nIn-Reply-To: <thesis@tum.de>\r\nReferences: <thesis@tum.de>\r\nDate: Mon, 16 Nov 2026 11:00:00 +0100\r\n\r\nbody\r\n",
        );
    });
    let reply = "mp://alpha/inbox/reply@tum.de";
    let mut conn = slice.connect().await;
    ok(&mut conn, "state.bootstrap", json!({})).await;

    let mut rows = Vec::new();
    for (method, params) in stream(&mut conn, "message.list_conversations").await {
        if method == METHOD_MESSAGE_CONVERSATIONS {
            let chunk: ConversationRowsChunk = serde_json::from_value(params).unwrap();
            rows.extend(chunk.rows);
        }
    }
    let thread = rows
        .iter()
        .find(|row| row.latest.selector == reply)
        .unwrap_or_else(|| panic!("no conversation under {reply}: {rows:?}"));
    assert_eq!(thread.count, 2, "{thread:?}");
    assert_eq!(thread.note, Some(NoteState::Open));
    assert_eq!(thread.latest.note, None);
    let budget = rows
        .iter()
        .find(|row| row.latest.selector == BUDGET)
        .unwrap();
    assert_eq!(budget.note, None);

    let listed: ThreadListing = serde_json::from_value(
        ok(
            &mut conn,
            "message.thread",
            json!({"account": ACCOUNT, "selector": reply}),
        )
        .await,
    )
    .unwrap();
    let notes: Vec<(&str, Option<NoteState>)> = listed
        .messages
        .iter()
        .map(|member| (member.selector.as_str(), member.note))
        .collect();
    assert_eq!(notes, [(THESIS, Some(NoteState::Open)), (reply, None)]);
}

/// Setting a note again replaces it and opens it again; `note.done` closes it
/// with its result, which the row shows as `done`; `note.clear` removes it and
/// a second clear removes nothing, which publishes nothing.
#[tokio::test]
async fn a_note_is_replaced_closed_and_cleared() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;
    let first = note(
        ok(
            &mut conn,
            "note.set",
            json!({"selector": THESIS, "preset": "s"}),
        )
        .await,
    );
    ok(
        &mut conn,
        "note.done",
        json!({"selector": THESIS, "result": "three lines"}),
    )
    .await;
    let again = note(
        ok(
            &mut conn,
            "note.set",
            json!({"selector": THESIS, "preset": "r", "text": "Yes, Thursday works."}),
        )
        .await,
    );
    assert_eq!(again.id, first.id);
    assert_eq!((again.state, again.result), (NoteState::Open, None));
    assert_eq!(again.text, "Yes, Thursday works.");

    let closed = note(
        ok(
            &mut conn,
            "note.done",
            json!({"id": first.id, "result": "Drafted the reply"}),
        )
        .await,
    );
    assert_eq!(closed.state, NoteState::Done);
    assert_eq!(closed.result.as_deref(), Some("Drafted the reply"));
    assert_eq!(row_note(&mut conn, THESIS).await, json!("done"));

    let mut watcher = slice.connect().await;
    ok(&mut watcher, "state.bootstrap", json!({})).await;
    let cleared = ok(&mut conn, "note.clear", json!({"selector": THESIS})).await;
    assert_eq!(cleared, json!({"removed": 1}));
    assert!(next_notes_event(&mut watcher).await.notes.is_empty());
    let cleared = ok(&mut conn, "note.clear", json!({"selector": THESIS})).await;
    assert_eq!(cleared, json!({"removed": 0}));
    assert_eq!(row_note(&mut conn, THESIS).await, Value::Null);
    support::events::assert_no_domain_event_within(
        &mut watcher,
        Duration::from_millis(300),
        "a clear of nothing",
    )
    .await;
}

/// The refusals name what is wrong: no preset and no text, a preset the
/// configuration lacks, mail the store does not hold, a new email with no
/// account, an id no note has, a note addressed twice, an empty result.
#[tokio::test]
async fn the_refusals_say_why() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;
    let cases = [
        (
            "note.set",
            json!({"selector": THESIS}),
            "a note needs a preset or a text",
        ),
        (
            "note.set",
            json!({"selector": THESIS, "preset": "q"}),
            "preset q is not configured; the presets are r, d, s, e, p, t, f",
        ),
        (
            "note.set",
            json!({"selector": "mp://alpha/inbox/nobody@tum.de", "text": "x"}),
            "no match for nobody@tum.de",
        ),
        (
            "note.set",
            json!({"text": "write to Ada"}),
            "a note for a new email names the account it goes out from",
        ),
        (
            "note.set",
            json!({"id": "zzzzzzzz", "text": "x"}),
            "no note has the id zzzzzzzz",
        ),
        (
            "note.set",
            json!({"selector": THESIS, "id": "zzzzzzzz", "text": "x"}),
            "selector and id are two addresses",
        ),
        (
            "note.set",
            json!({"selector": "mp://beta/inbox/x@y", "text": "x"}),
            "no account named beta is configured",
        ),
        (
            "note.done",
            json!({"selector": THESIS, "result": " "}),
            "result is the line a client shows",
        ),
        (
            "note.done",
            json!({"selector": THESIS, "result": "x"}),
            "no note is on the message thesis@tum.de of alpha",
        ),
        (
            "note.clear",
            json!({}),
            "a note is addressed by selector or by id",
        ),
        (
            "note.export",
            json!({"ids": ["zzzzzzzz"]}),
            "no note has the id zzzzzzzz",
        ),
        (
            "note.list",
            json!({"state": "closed"}),
            "the parameters of note.list",
        ),
        (
            "note.list",
            json!({"all": true}),
            "note.list has no all parameter",
        ),
    ];
    for (method, params, sentence) in cases {
        let (code, message) = refused(&mut conn, method, params.clone()).await;
        assert!(
            message.contains(sentence),
            "{method} {params}: {message:?} does not say {sentence:?}"
        );
        let expected = if sentence.starts_with("no account named") {
            -32005
        } else {
            -32602
        };
        assert_eq!(code, expected, "{method} {params}: {message}");
    }
    let listed = listing(ok(&mut conn, "note.list", json!({})).await);
    assert!(listed.notes.is_empty(), "a refusal wrote nothing");
}

/// `note.set_many` sets one note on each mail, a draft's row carries its
/// note, a new-email note is edited by id, and `note.list` filters by state.
#[tokio::test]
async fn several_notes_a_draft_and_a_new_email() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;
    let many = listing(
        ok(
            &mut conn,
            "note.set_many",
            json!({"selectors": [THESIS, BUDGET, THESIS], "preset": "r", "text": "Yes."}),
        )
        .await,
    );
    let selectors: Vec<_> = many.notes.iter().map(|n| n.selector.as_deref()).collect();
    assert_eq!(selectors, [Some(THESIS), Some(BUDGET)]);

    let review = note(
        ok(
            &mut conn,
            "note.set",
            json!({"account": ACCOUNT, "selector": "drafts/a3f9c2e1b4d5f607", "preset": "p"}),
        )
        .await,
    );
    assert_eq!(review.selector.as_deref(), Some(DRAFT));
    assert_eq!(review.draft_id.as_deref(), Some("a3f9c2e1b4d5f607"));
    assert_eq!(review.message_id, None);
    assert_eq!(review.to, "Ada Lovelace <ada@example.com>");
    assert_eq!(review.from, "alpha@example.com");
    assert_eq!(review.subject, "Re: Review");
    let drafts = ok(&mut conn, "draft.list", json!({"account": ACCOUNT})).await;
    assert_eq!(drafts["drafts"][0]["note"], json!("open"), "{drafts}");

    let new = note(
        ok(
            &mut conn,
            "note.set",
            json!({"account": ACCOUNT, "text": "Ask Ada about Friday."}),
        )
        .await,
    );
    assert_eq!((new.selector.as_deref(), new.from.as_str()), (None, ""));
    let edited = note(
        ok(
            &mut conn,
            "note.set",
            json!({"id": new.id, "text": "Ask Ada and Bo about Friday."}),
        )
        .await,
    );
    assert_eq!(edited.id, new.id);
    assert_eq!(edited.text, "Ask Ada and Bo about Friday.");

    ok(
        &mut conn,
        "note.done",
        json!({"selector": BUDGET, "result": "Replied"}),
    )
    .await;
    let ids =
        |value: Value| -> Vec<String> { listing(value).notes.into_iter().map(|n| n.id).collect() };
    assert_eq!(
        ids(ok(&mut conn, "note.list", json!({})).await),
        [
            new.id.clone(),
            review.id.clone(),
            many.notes[1].id.clone(),
            many.notes[0].id.clone()
        ],
        "newest first"
    );
    assert_eq!(
        ids(ok(&mut conn, "note.list", json!({"state": "done"})).await),
        [many.notes[1].id.clone()]
    );
    assert_eq!(
        ok(&mut conn, "note.clear_done", json!({})).await,
        json!({"removed": 1})
    );
}

/// `note.export` renders the open notes and changes nothing; `note.handoff`
/// renders the same text and marks them handed off, which the next export no
/// longer gives; named ids are given whatever their state.
#[tokio::test]
async fn the_export_changes_nothing_and_the_hand_off_marks_its_notes() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;
    let thesis = note(
        ok(
            &mut conn,
            "note.set",
            json!({"selector": THESIS, "preset": "s"}),
        )
        .await,
    );
    let new = note(
        ok(
            &mut conn,
            "note.set",
            json!({"account": ACCOUNT, "text": "Ask Ada about Friday."}),
        )
        .await,
    );

    let export: NoteExport =
        serde_json::from_value(ok(&mut conn, "note.export", json!({})).await).unwrap();
    assert_eq!(export.ids, [new.id.clone(), thesis.id.clone()]);
    assert!(export.text.starts_with("I have notes on 2 emails.\n"));
    assert!(export.text.contains(&format!(
        "1. New email from account alpha (note {})\n   Ask Ada about Friday.\n2. {THESIS}\n   From Stefan Fuchs <stefan.fuchs@tum.de>, subject \"Master Thesis Presentation\"\n   Summarize: ",
        new.id
    )), "{}", export.text);
    let listed = listing(ok(&mut conn, "note.list", json!({})).await);
    assert!(listed.notes.iter().all(|n| n.state == NoteState::Open));

    let mut watcher = slice.connect().await;
    ok(&mut watcher, "state.bootstrap", json!({})).await;
    let handed: NoteExport =
        serde_json::from_value(ok(&mut conn, "note.handoff", json!({})).await).unwrap();
    assert_eq!(handed, export);
    let heard = next_notes_event(&mut watcher).await;
    assert!(heard.notes.iter().all(|n| n.state == NoteState::HandedOff));
    assert_eq!(row_note(&mut conn, THESIS).await, json!("handed_off"));

    let empty: NoteExport =
        serde_json::from_value(ok(&mut conn, "note.export", json!({})).await).unwrap();
    assert_eq!(empty, NoteExport::default());
    let named: NoteExport =
        serde_json::from_value(ok(&mut conn, "note.export", json!({"ids": [thesis.id]})).await)
            .unwrap();
    assert_eq!(named.ids, [thesis.id]);
}

/// The rows `alpha` holds for one Message-ID, in every mailbox.
fn copies_of(root: &Path, message_id: &str) -> Vec<mailypoppins::store::read::MessageRow> {
    let store = Store::open(store_path(root)).unwrap();
    mailypoppins::store::read::find_by_message_id(&store, ACCOUNT, message_id).unwrap()
}

/// A note is on the message, not on a mailbox: with the message archived,
/// the note is not orphaned, its selector names the archived copy, the
/// archived row carries it, and the agent's stale Inbox selector still reads
/// and closes it.
#[tokio::test]
async fn a_note_follows_its_message_into_the_archive() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;
    let set = note(
        ok(
            &mut conn,
            "note.set",
            json!({"selector": THESIS, "preset": "s"}),
        )
        .await,
    );
    drop(conn);
    let slice = slice.restarted(|root| {
        let store = Store::open(store_path(root)).unwrap();
        let row = &copies_of(root, "<thesis@tum.de>")[0];
        mailypoppins::store::write::move_row(&store, row.id, "archive")
            .unwrap()
            .expect("the row moves");
    });
    let archived = "mp://alpha/archive/thesis@tum.de";
    let mut conn = slice.connect().await;

    let listed = listing(ok(&mut conn, "note.list", json!({})).await);
    assert_eq!(listed.notes.len(), 1);
    assert!(!listed.notes[0].orphaned, "{listed:?}");
    assert_eq!(listed.notes[0].selector.as_deref(), Some(archived));
    let rows = ok(
        &mut conn,
        "message.list",
        json!({"account": ACCOUNT, "mailbox": "archive"}),
    )
    .await;
    assert_eq!(rows["messages"][0]["selector"], json!(archived), "{rows}");
    assert_eq!(rows["messages"][0]["note"], json!("open"), "{rows}");

    let got = ok(&mut conn, "note.get", json!({"selector": THESIS})).await;
    assert_eq!(note(got["note"].clone()).id, set.id);
    drop(conn);
    let (out, err, code) = slice.mp(&["notes", "done", THESIS, "--result", "Summed up"]);
    assert_eq!(code, Some(0), "{out}{err}");
    assert!(out.contains(archived), "{out}");
    let mut conn = slice.connect().await;
    let listed = listing(ok(&mut conn, "note.list", json!({})).await);
    assert_eq!(listed.notes[0].state, NoteState::Done);
    assert_eq!(listed.notes[0].result.as_deref(), Some("Summed up"));
}

/// A message no mailbox of its account holds any more orphans its note,
/// which stays, still read by its selector.
#[tokio::test]
async fn a_note_whose_message_is_gone_everywhere_is_orphaned() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;
    ok(
        &mut conn,
        "note.set",
        json!({"selector": THESIS, "preset": "s"}),
    )
    .await;
    drop(conn);
    let slice = slice.restarted(|root| {
        let store = Store::open(store_path(root)).unwrap();
        let blobs = BlobStore::new(root.join("accounts").join(ACCOUNT).join("blobs"));
        for row in copies_of(root, "<thesis@tum.de>") {
            mailypoppins::store::write::delete_row(&store, &blobs, row.id).unwrap();
        }
    });
    let mut conn = slice.connect().await;
    let listed = listing(ok(&mut conn, "note.list", json!({})).await);
    assert_eq!(listed.notes.len(), 1);
    assert!(listed.notes[0].orphaned, "{listed:?}");
    assert_eq!(listed.notes[0].selector.as_deref(), Some(THESIS));
    let got = ok(&mut conn, "note.get", json!({"selector": THESIS})).await;
    assert_eq!(got["note"]["orphaned"], json!(true), "{got}");
}

/// An account the configuration no longer names orphans every note on its
/// mail; the notes stay.
#[tokio::test]
async fn dropping_the_account_orphans_its_notes() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;
    ok(
        &mut conn,
        "note.set",
        json!({"selector": THESIS, "preset": "s"}),
    )
    .await;
    ok(
        &mut conn,
        "note.set",
        json!({"selector": DRAFT, "preset": "p"}),
    )
    .await;
    drop(conn);
    let slice = slice.restarted(|root| {
        std::fs::write(
            root.join("config.toml"),
            CONFIG
                .replace("\"alpha\"", "\"beta\"")
                .replace("alpha@", "beta@"),
        )
        .unwrap();
    });
    let mut conn = slice.connect().await;
    let listed = listing(ok(&mut conn, "note.list", json!({})).await);
    let orphaned: Vec<bool> = listed.notes.iter().map(|n| n.orphaned).collect();
    assert_eq!(orphaned, [true, true], "{listed:?}");
}

/// A draft's note follows the draft when it is sent: it moves onto the
/// Message-ID the send went out with, keeping its id and state, and every
/// client hears `note.changed` with it.
#[tokio::test]
async fn a_sent_drafts_note_follows_the_message_it_went_out_as() {
    use support::send_fixture as fixture;

    let tmp = TempDir::new().unwrap();
    fixture::seed(tmp.path());
    support::parity::pin_font_size(tmp.path());
    let hook = fixture::fake_transport(&fixture::transport_log(tmp.path()));
    let daemon =
        DaemonFixture::start_with(tmp.path(), None, &[(fixture::FAKE_TRANSPORT_ENV, &hook)]);
    let mut conn = connect(tmp.path()).await;
    let draft = fixture::selector(fixture::ACCOUNT, fixture::APPROVED);
    let set = note(
        ok(
            &mut conn,
            "note.set",
            json!({"selector": draft, "preset": "p"}),
        )
        .await,
    );
    assert_eq!(set.draft_id.as_deref(), Some(fixture::APPROVED));
    let mut watcher = connect(tmp.path()).await;
    ok(&mut watcher, "state.bootstrap", json!({})).await;

    let started = ok(
        &mut conn,
        "send.draft",
        json!({"account": fixture::ACCOUNT, "selector": draft}),
    )
    .await;
    let id = started["operation_id"].clone();
    let settled = loop {
        let status = ok(&mut conn, "operation.status", json!({"operation_id": id})).await;
        if !matches!(status["state"].as_str(), Some("queued" | "running")) {
            break status;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    let sent = settled["result"]["message_id"]
        .as_str()
        .unwrap_or_else(|| panic!("the send names its Message-ID: {settled}"))
        .to_string();
    let key = sent.trim_start_matches('<').trim_end_matches('>');

    let heard = next_notes_event(&mut watcher).await;
    assert_eq!(heard.notes.len(), 1, "{heard:?}");
    let moved = &heard.notes[0];
    assert_eq!(moved.id, set.id);
    assert_eq!(moved.message_id.as_deref(), Some(key));
    assert_eq!(moved.draft_id, None);
    assert_eq!(moved.state, NoteState::Open);
    assert_eq!(moved.subject, set.subject);
    // The send files its copy in the store's Sent mailbox, which the note
    // now names.
    assert!(!moved.orphaned, "{moved:?}");
    assert_eq!(
        moved.selector,
        Some(format!("mp://{}/sent/{key}", fixture::ACCOUNT))
    );
    let listed = listing(ok(&mut conn, "note.list", json!({})).await);
    assert_eq!(listed.notes, heard.notes);
    let got = ok(
        &mut conn,
        "note.get",
        json!({"selector": format!("mp://{}/sent/{key}", fixture::ACCOUNT)}),
    )
    .await;
    assert_eq!(got["note"]["id"], json!(set.id), "{got}");
    drop(daemon);
}

/// The notes outlive the store: with the store deleted and the daemon started
/// again, as a failed integrity check would have it, every note is still
/// listed with the snapshot it took. A store that is not there yet orphans
/// nothing, since the next sync may bring the message back; a draft whose
/// file is gone orphans its note, and nothing is deleted.
#[tokio::test]
async fn the_notes_survive_a_store_rebuild() {
    let slice = Slice::start();
    let mut conn = slice.connect().await;
    let thesis = note(
        ok(
            &mut conn,
            "note.set",
            json!({"selector": THESIS, "preset": "s"}),
        )
        .await,
    );
    let review = note(
        ok(
            &mut conn,
            "note.set",
            json!({"selector": DRAFT, "preset": "p"}),
        )
        .await,
    );
    drop(conn);
    let Slice { daemon, tmp } = slice;
    daemon.stop();

    for file in ["store.sqlite3", "store.sqlite3-wal", "store.sqlite3-shm"] {
        let path = tmp.path().join("accounts").join(ACCOUNT).join(file);
        if path.exists() {
            std::fs::remove_file(path).unwrap();
        }
    }
    std::fs::remove_file(
        tmp.path()
            .join("accounts")
            .join(ACCOUNT)
            .join("drafts")
            .join("2026-11-16-review.md"),
    )
    .unwrap();
    let daemon = DaemonFixture::start(tmp.path());
    let mut conn = connect(tmp.path()).await;
    let listed = listing(ok(&mut conn, "note.list", json!({})).await);
    let orphaned: Vec<bool> = listed.notes.iter().map(|n| n.orphaned).collect();
    assert_eq!(orphaned, [true, false], "{listed:?}");
    let survived: Vec<Note> = listed
        .notes
        .into_iter()
        .map(|n| Note {
            orphaned: false,
            ..n
        })
        .collect();
    assert_eq!(survived, [review, thesis]);
    drop(daemon);
}

/// `note.presets` answers the configuration's presets in its order, which
/// replace the built-in ones, and `config.validate` refuses two presets
/// sharing a key.
#[tokio::test]
async fn the_presets_come_from_the_configuration() {
    let config = format!(
        "{CONFIG}\n[notes]\npresets = [\n  {{ key = \"x\", label = \"Translate\", instruction = \"Translate it into German.\" }},\n]\n"
    );
    let slice = Slice::with_config(&config);
    let mut conn = slice.connect().await;
    let presets = ok(&mut conn, "note.presets", json!({})).await;
    assert_eq!(
        presets,
        json!({"presets": [{"key": "x", "label": "Translate", "needs_text": false, "instruction": "Translate it into German."}]})
    );
    let (_, why) = refused(
        &mut conn,
        "note.set",
        json!({"selector": THESIS, "preset": "s"}),
    )
    .await;
    assert!(why.contains("the presets are x"), "{why}");

    let doubled = format!(
        "{CONFIG}\n[notes]\npresets = [\n  {{ key = \"x\", label = \"A\", instruction = \"A.\" }},\n  {{ key = \"x\", label = \"B\", instruction = \"B.\" }},\n]\n"
    );
    let verdict = ok(&mut conn, "config.validate", json!({"toml": doubled})).await;
    assert_eq!(verdict["ok"], json!(false), "{verdict}");
    assert!(
        verdict["errors"][0]["message"]
            .as_str()
            .unwrap()
            .contains("two presets share the key \"x\""),
        "{verdict}"
    );
}

/// `mp notes` routes every verb through the daemon: add on two selectors,
/// new, edit, list, export with and without the hand-off, done, clear and
/// presets, each printing the daemon's answer; a refusal is the daemon's
/// sentence and exit 1.
#[test]
fn mp_notes_round_trip() {
    let slice = Slice::start();
    let (out, err, code) = slice.mp(&[
        "notes",
        "add",
        "inbox/thesis@tum.de",
        BUDGET,
        "--preset",
        "r",
        "--text",
        "Yes, Thursday works.",
    ]);
    assert_eq!(code, Some(0), "{out}{err}");
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 2, "{out}");
    assert!(
        lines[0].ends_with(&format!("open        {THESIS}")),
        "{out}"
    );
    assert!(
        lines[1].ends_with(&format!("open        {BUDGET}")),
        "{out}"
    );

    let (out, _, code) = slice.mp(&["notes", "new", "--text", "Ask Ada about Friday."]);
    assert_eq!(code, Some(0));
    assert!(out.contains("new email from alpha"), "{out}");
    let new_id = out.split_whitespace().nth(1).unwrap().to_string();
    let (out, _, code) = slice.mp(&["notes", "edit", &new_id, "--text", "Ask Ada and Bo."]);
    assert_eq!(code, Some(0), "{out}");

    let (out, _, _) = slice.mp(&["notes", "list"]);
    assert!(
        out.starts_with(&format!(
            "{new_id}  open        new email from alpha\n    Ask Ada and Bo.\n"
        )),
        "{out}"
    );
    assert!(
        out.contains(&format!(
            "{THESIS}\n    Master Thesis Presentation, from Stefan Fuchs <stefan.fuchs@tum.de>\n    [r] Yes, Thursday works.\n"
        )),
        "{out}"
    );

    let (exported, _, code) = slice.mp(&["notes", "export"]);
    assert_eq!(code, Some(0));
    assert!(
        exported.starts_with("I have notes on 3 emails.\n"),
        "{exported}"
    );
    let (json_out, _, _) = slice.mp(&["notes", "export", "--json"]);
    let answer: Value = serde_json::from_str(&json_out).unwrap();
    assert_eq!(answer["text"], json!(exported));
    let (handed, _, _) = slice.mp(&["notes", "export", "--handoff"]);
    assert_eq!(handed, exported);
    let (out, err, _) = slice.mp(&["notes", "export"]);
    assert_eq!(out, "");
    assert!(err.ends_with("No open notes to export\n"), "{err}");

    let (out, _, code) = slice.mp(&["notes", "done", THESIS, "--result", "Drafted the reply"]);
    assert_eq!(code, Some(0));
    assert!(out.contains("Done: ") && out.contains("done  "), "{out}");
    let (out, _, code) = slice.mp(&["notes", "done", "--id", &new_id, "--result", "Wrote it"]);
    assert_eq!(code, Some(0), "{out}");
    let (out, _, _) = slice.mp(&["notes", "list", "--state", "done"]);
    assert!(out.contains("    = Drafted the reply\n"), "{out}");
    let (json_out, _, _) = slice.mp(&["notes", "list", "--json"]);
    let listed: NoteListing = serde_json::from_str(&json_out).unwrap();
    assert_eq!(listed.notes.len(), 3);

    let (out, _, _) = slice.mp(&["notes", "clear", BUDGET]);
    assert_eq!(out, "\u{2713} Cleared 1 note\n");
    let (out, _, _) = slice.mp(&["notes", "clear", "--done"]);
    assert_eq!(out, "\u{2713} Cleared 2 notes\n");
    let (out, _, _) = slice.mp(&["notes", "clear", "--id", &new_id]);
    assert_eq!(out, "No note to clear\n");
    let (out, _, _) = slice.mp(&["notes", "list"]);
    assert_eq!(out, "No notes\n");

    let (out, _, _) = slice.mp(&["notes", "presets"]);
    assert!(
        out.starts_with("r  Reply (+ text): Draft a reply in my voice from this gist.\n"),
        "{out}"
    );
    let (_, err, code) = slice.mp(&["notes", "add", THESIS, "--preset", "q"]);
    assert_eq!(code, Some(1));
    assert!(err.contains("preset q is not configured"), "{err}");
}
