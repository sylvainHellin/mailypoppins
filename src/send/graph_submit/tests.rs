//! The staged Graph send and its resume against an in-memory mailbox: every
//! crash window is a future dropped at a chosen request, and nothing here
//! touches the network or a real account.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::anyhow;
use serde_json::Value;

use super::*;
use crate::outbox::{Envelope, OutboxState};
use crate::send::tests::{build_with_ancestry, draft_with};

const ACCOUNT: &str = "graph-acct";

// ---------------------------------------------------------------------------
// The fake mailbox
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Create,
    Attach,
    Send,
    SendMime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fault {
    /// The process dies waiting, before the server acted.
    HangBefore,
    /// The server acted, then the process died waiting for the answer.
    HangAfter,
    /// An outright refusal, an HTTP 4xx.
    Refuse,
    /// The server acted and the answer timed out.
    TimeoutAfter,
    /// Graph took the delivering request and answered 504: the message is on
    /// its way (a draft in the Outbox, not yet in Sent Items) until
    /// [`FakeGraph::deliver`].
    Gateway,
    /// Before `send` acted, the draft left Drafts: an earlier send of it was
    /// taken late and is on its way. The send answers 404.
    Vanished,
}

#[derive(Debug, Clone, Default)]
struct Item {
    message_id: String,
    /// Our `Message-ID` in [`crate::graph::MESSAGE_ID_PROPERTY`], on a JSON
    /// draft.
    property: Option<String>,
    mime: Option<Vec<u8>>,
    attachments: Vec<OutgoingAttachment>,
}

impl Item {
    fn is(&self, message_id: &str) -> bool {
        self.message_id == message_id || self.property.as_deref() == Some(message_id)
    }
}

#[derive(Default)]
struct Server {
    next_id: u32,
    drafts: BTreeMap<String, Item>,
    /// Taken by Graph and not yet filed in Sent Items, with the draft id a
    /// staged send had.
    queued: Vec<(Option<String>, Item)>,
    sent: Vec<Item>,
    faults: Vec<(Step, Fault)>,
    discarded: Vec<String>,
    sent_drafts: Vec<String>,
}

impl Server {
    /// Take one message: filed at once, or queued when Graph answered 504.
    fn take(&mut self, draft: Option<String>, item: Item, queue: bool) {
        if queue {
            self.queued.push((draft, item));
            return;
        }
        if let Some(id) = draft {
            self.sent_drafts.push(id);
        }
        self.sent.push(item);
    }
}

#[derive(Default)]
struct FakeGraph(Mutex<Server>);

fn message_id_of_mime(b64: &str) -> (String, Vec<u8>) {
    use base64::Engine;
    let mime = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .expect("standard base64");
    (crate::send::message_id_of(&mime), mime)
}

impl FakeGraph {
    fn with_fault(step: Step, fault: Fault) -> Self {
        let fake = FakeGraph::default();
        fake.0.lock().unwrap().faults.push((step, fault));
        fake
    }

    fn take_fault(&self, step: Step) -> Option<Fault> {
        let mut server = self.0.lock().unwrap();
        let at = server.faults.iter().position(|(s, _)| *s == step)?;
        Some(server.faults.remove(at).1)
    }

    /// Take the next fault of `step` only when it is `fault`.
    fn take_this_fault(&self, step: Step, fault: Fault) -> bool {
        let mut server = self.0.lock().unwrap();
        match server.faults.iter().position(|(s, _)| *s == step) {
            Some(at) if server.faults[at].1 == fault => {
                server.faults.remove(at);
                true
            }
            _ => false,
        }
    }

    /// Run one request: the fault decides whether the server acts and what
    /// the client hears. The action is told whether Graph only queues the
    /// message ([`Fault::Gateway`]).
    async fn act<T: Send>(
        &self,
        step: Step,
        action: impl FnOnce(&mut Server, bool) -> Result<T> + Send,
    ) -> Result<T> {
        let fault = self.take_fault(step);
        match fault {
            Some(Fault::HangBefore) => std::future::pending::<()>().await,
            Some(Fault::Refuse) => {
                return Err(graph_status(400));
            }
            _ => {}
        }
        let value = action(&mut self.0.lock().unwrap(), fault == Some(Fault::Gateway))?;
        match fault {
            Some(Fault::HangAfter) => {
                std::future::pending::<()>().await;
                unreachable!()
            }
            Some(Fault::TimeoutAfter) => Err(anyhow!(outbox::AmbiguousSubmission(
                "operation timed out".to_string()
            ))),
            Some(Fault::Gateway) => Err(graph_status(504)),
            _ => Ok(value),
        }
    }

    fn sent(&self) -> Vec<Item> {
        self.0.lock().unwrap().sent.clone()
    }

    /// Graph finishes what it queued: the messages land in Sent Items.
    fn deliver(&self) {
        let mut server = self.0.lock().unwrap();
        for (draft, item) in std::mem::take(&mut server.queued) {
            server.take(draft, item, false);
        }
    }

    fn drafts(&self) -> Vec<String> {
        self.0.lock().unwrap().drafts.keys().cloned().collect()
    }

    /// A draft created out of band, as an earlier attempt left it.
    fn plant_draft(&self, message_id: &str) -> String {
        let mut server = self.0.lock().unwrap();
        server.next_id += 1;
        let id = format!("planted-{}", server.next_id);
        server.drafts.insert(
            id.clone(),
            Item {
                message_id: message_id.to_string(),
                ..Item::default()
            },
        );
        id
    }
}

/// A delivering request answered with `status`, read the way the real client
/// reads it ([`crate::graph::delivering_status_error`]).
fn graph_status(status: u16) -> anyhow::Error {
    crate::graph::delivering_status_error(
        status,
        false,
        true,
        format!("HTTP {status} from the fake"),
    )
}

impl GraphSendApi for FakeGraph {
    fn send_mime(&self, mime_base64: String) -> impl Future<Output = Result<()>> + Send {
        self.act(Step::SendMime, move |server, queue| {
            let (message_id, mime) = message_id_of_mime(&mime_base64);
            let item = Item {
                message_id,
                mime: Some(mime),
                ..Item::default()
            };
            server.take(None, item, queue);
            Ok(())
        })
    }

    fn create_draft_mime(
        &self,
        mime_base64: String,
    ) -> impl Future<Output = Result<String>> + Send {
        self.act(Step::Create, move |server, _| {
            let (message_id, mime) = message_id_of_mime(&mime_base64);
            server.next_id += 1;
            let id = format!("draft-{}", server.next_id);
            server.drafts.insert(
                id.clone(),
                Item {
                    message_id,
                    mime: Some(mime),
                    ..Item::default()
                },
            );
            Ok(id)
        })
    }

    fn create_draft_json(&self, message: &Value) -> impl Future<Output = Result<String>> + Send {
        let property = message["singleValueExtendedProperties"]
            .as_array()
            .and_then(|props| {
                props
                    .iter()
                    .find(|p| p["id"] == crate::graph::MESSAGE_ID_PROPERTY)
            })
            .and_then(|p| p["value"].as_str())
            .map(str::to_string);
        self.act(Step::Create, move |server, _| {
            server.next_id += 1;
            let id = format!("draft-{}", server.next_id);
            server.drafts.insert(
                id.clone(),
                Item {
                    message_id: format!("<exchange-{}@fake>", server.next_id),
                    property,
                    ..Item::default()
                },
            );
            Ok(id)
        })
    }

    fn add_attachment(
        &self,
        draft_id: &str,
        attachment: &OutgoingAttachment,
    ) -> impl Future<Output = Result<()>> + Send {
        let draft_id = draft_id.to_string();
        let attachment = attachment.clone();
        self.act(Step::Attach, move |server, _| {
            server
                .drafts
                .get_mut(&draft_id)
                .ok_or_else(|| anyhow!("HTTP 404"))?
                .attachments
                .push(attachment);
            Ok(())
        })
    }

    fn send_draft(&self, draft_id: &str) -> impl Future<Output = Result<()>> + Send {
        let draft_id = draft_id.to_string();
        if self.take_this_fault(Step::Send, Fault::Vanished) {
            let mut server = self.0.lock().unwrap();
            if let Some(item) = server.drafts.remove(&draft_id) {
                server.queued.push((Some(draft_id.clone()), item));
            }
        }
        self.act(Step::Send, move |server, queue| {
            let item = server
                .drafts
                .remove(&draft_id)
                .ok_or_else(|| graph_status(404))?;
            server.take(Some(draft_id), item, queue);
            Ok(())
        })
    }

    fn discard_draft(&self, draft_id: &str) -> impl Future<Output = Result<()>> + Send {
        let draft_id = draft_id.to_string();
        async move {
            let mut server = self.0.lock().unwrap();
            server.drafts.remove(&draft_id);
            server.discarded.push(draft_id);
            Ok(())
        }
    }

    fn server_copies(
        &self,
        internet_message_id: &str,
    ) -> impl Future<Output = Result<ServerCopies>> + Send {
        let wanted = internet_message_id.to_string();
        async move {
            let server = self.0.lock().unwrap();
            Ok(ServerCopies {
                sent: server.sent.iter().any(|i| i.is(&wanted)),
                drafts: server
                    .drafts
                    .iter()
                    .filter(|(_, i)| i.is(&wanted))
                    .map(|(id, _)| id.clone())
                    .collect(),
            })
        }
    }

    fn draft_state(&self, draft_id: &str) -> impl Future<Output = Result<DraftState>> + Send {
        let draft_id = draft_id.to_string();
        async move {
            let server = self.0.lock().unwrap();
            Ok(if server.drafts.contains_key(&draft_id) {
                DraftState::InDrafts
            } else if server
                .queued
                .iter()
                .any(|(id, _)| id.as_deref() == Some(&draft_id))
            {
                DraftState::Sending
            } else {
                // Filed in Sent Items under a new id, or deleted.
                DraftState::Gone
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// One account's outbox on disk, opened the way a restarted process opens it.
struct Outbox {
    _dir: tempfile::TempDir,
    store: Store,
    blobs: BlobStore,
    drafts: std::path::PathBuf,
}

impl Outbox {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("store.sqlite3")).unwrap();
        let blobs = BlobStore::new(dir.path().join("blobs"));
        let drafts = dir.path().join("drafts");
        std::fs::create_dir(&drafts).unwrap();
        Self {
            _dir: dir,
            store,
            blobs,
            drafts,
        }
    }

    /// The approved draft file a message keyed `id:<id>` was built from.
    fn write_draft(&self, id: &str) -> std::path::PathBuf {
        let path = self.drafts.join(format!("{id}.md"));
        std::fs::write(
            &path,
            format!(
                "---\nid: {id}\nto: bob@example.com\nsubject: Hello\nstatus: approved\n---\n\nBody.\n"
            ),
        )
        .unwrap();
        path
    }

    /// Commit the row, as `DurableSend::begin` does before the first request.
    fn enqueue(&self, built: &BuiltMessage) -> i64 {
        outbox::enqueue(
            &self.store,
            &self.blobs,
            ACCOUNT,
            None,
            &built.message_id,
            &built.raw,
            &Envelope {
                from: built.from.clone(),
                recipients: built.recipients.clone(),
                draft_key: built.draft_key.clone(),
                ..Default::default()
            },
        )
        .unwrap()
    }

    fn row(&self, id: i64) -> OutboxRow {
        outbox::load(&self.store, id).unwrap().unwrap()
    }

    /// The live send's submission and record, without the account directory
    /// `send_durably_graph` opens.
    async fn live<A: GraphSendApi>(&self, id: i64, built: &BuiltMessage, api: &A) -> GraphOutcome {
        let outcome = submit(Some((&self.store, id)), plan(built).unwrap(), api).await;
        record(&self.store, &self.blobs, id, &outcome).unwrap();
        outcome
    }

    async fn resume<A: GraphSendApi>(&self, now: i64, api: &A) {
        resume_rows(&self.store, &self.blobs, ACCOUNT, &self.drafts, now, api).await;
    }
}

/// Run a send until the process "dies": the future is dropped where the
/// fake left it waiting, with whatever it had committed.
async fn crash<F: Future>(send: F) {
    assert!(
        tokio::time::timeout(Duration::from_millis(200), send)
            .await
            .is_err(),
        "the send was meant to die mid-way, and it finished"
    );
}

/// A reply too large for one sendMail: a 3 MB attachment beside a small one.
fn large_reply(key: &str) -> BuiltMessage {
    let tmp = tempfile::tempdir().unwrap();
    let big: Vec<u8> = (0..3_000_000u32).map(|i| (i % 253) as u8).collect();
    std::fs::write(tmp.path().join("plans.pdf"), big).unwrap();
    let mut reply = draft_with(Some(key), tmp.path().join("r.md").to_str().unwrap());
    reply.frontmatter.subject = "Re: Plan".to_string();
    reply.frontmatter.in_reply_to = Some("<parent@example.com>".to_string());
    reply.frontmatter.attachments = Some(vec!["plans.pdf".to_string()]);
    build_with_ancestry(&reply, Some("<root@example.com>"))
}

fn small_message(key: &str) -> BuiltMessage {
    let mut draft = draft_with(Some(key), "/drafts/s.md");
    draft.frontmatter.bcc = Some("dan@example.com".to_string());
    build_with_ancestry(&draft, None)
}

fn header(mime: &[u8], name: &str) -> Option<String> {
    use mailparse::MailHeaderMap;
    mailparse::parse_headers(mime)
        .unwrap()
        .0
        .get_first_value(name)
}

// ---------------------------------------------------------------------------
// Item 1: a large message keeps its threading
// ---------------------------------------------------------------------------

/// The staged send creates one draft from the MIME, adds the moved
/// attachment, sends that draft, and leaves nothing in Drafts; what arrives
/// carries the reply's threading headers and our `Message-ID`.
#[tokio::test]
async fn a_large_reply_is_sent_as_one_threaded_draft_with_its_attachment() {
    let outbox = Outbox::new();
    let built = large_reply("large-ok");
    let id = outbox.enqueue(&built);
    let graph = FakeGraph::default();

    assert_eq!(
        outbox.live(id, &built, &graph).await,
        GraphOutcome::Delivered
    );

    let sent = graph.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].message_id, built.message_id);
    let mime = sent[0].mime.as_deref().expect("created from MIME");
    assert_eq!(
        header(mime, "In-Reply-To").as_deref(),
        Some("<parent@example.com>")
    );
    assert_eq!(
        header(mime, "References").as_deref(),
        Some("<root@example.com> <parent@example.com>")
    );
    assert_eq!(sent[0].attachments.len(), 1);
    assert_eq!(sent[0].attachments[0].name, "plans.pdf");
    assert_eq!(sent[0].attachments[0].bytes.len(), 3_000_000);
    assert!(graph.drafts().is_empty());
    assert_eq!(outbox.row(id).state, OutboxState::Done);
}

// ---------------------------------------------------------------------------
// Item 2: an interrupted Graph send resumes exactly once, with no orphan
// ---------------------------------------------------------------------------

/// Killed while uploading: nothing went out, the row names its draft, and the
/// resume discards that draft and sends a fresh one, once.
#[tokio::test]
async fn a_crash_while_uploading_is_resent_once_and_leaves_no_draft() {
    let outbox = Outbox::new();
    let built = large_reply("crash-upload");
    let id = outbox.enqueue(&built);
    let graph = FakeGraph::with_fault(Step::Attach, Fault::HangBefore);

    crash(outbox.live(id, &built, &graph)).await;
    let row = outbox.row(id);
    assert_eq!(row.state, OutboxState::PendingSend);
    assert_eq!(row.submission_started_at, None, "nothing delivering ran");
    let first = row
        .envelope
        .unwrap()
        .graph_draft
        .expect("the draft is recorded");
    assert_eq!(graph.drafts(), std::slice::from_ref(&first));

    outbox.resume(outbox::unix_now(), &graph).await;

    assert_eq!(graph.sent().len(), 1, "sent exactly once");
    assert_eq!(graph.sent()[0].attachments.len(), 1);
    assert!(graph.drafts().is_empty(), "no orphan draft");
    assert!(graph.0.lock().unwrap().discarded.contains(&first));
    assert_eq!(outbox.row(id).state, OutboxState::Done);
}

/// Killed after the server created the draft but before its id came back:
/// the row names no draft, and the resume still finds it by `Message-ID`.
#[tokio::test]
async fn a_draft_whose_creation_was_never_answered_is_found_and_discarded() {
    let outbox = Outbox::new();
    let built = large_reply("crash-create");
    let id = outbox.enqueue(&built);
    let graph = FakeGraph::with_fault(Step::Create, Fault::HangAfter);

    crash(outbox.live(id, &built, &graph)).await;
    assert_eq!(outbox.row(id).envelope.unwrap().graph_draft, None);
    assert_eq!(
        graph.drafts().len(),
        1,
        "the server holds an unrecorded draft"
    );

    outbox.resume(outbox::unix_now(), &graph).await;

    assert_eq!(graph.sent().len(), 1);
    assert!(graph.drafts().is_empty(), "no orphan draft");
    assert_eq!(outbox.row(id).state, OutboxState::Done);
}

/// Killed after Exchange took the send: the resume finds the copy in Sent
/// Items and records it, without a second send. Both routes.
#[tokio::test]
async fn a_send_the_server_took_before_the_crash_is_recorded_not_repeated() {
    for (built, step) in [
        (large_reply("taken-draft"), Step::Send),
        (small_message("taken-mime"), Step::SendMime),
    ] {
        let outbox = Outbox::new();
        let id = outbox.enqueue(&built);
        let graph = FakeGraph::with_fault(step, Fault::HangAfter);

        crash(outbox.live(id, &built, &graph)).await;
        assert!(outbox.row(id).submission_started_at.is_some());

        outbox.resume(outbox::unix_now(), &graph).await;

        assert_eq!(graph.sent().len(), 1, "{step:?}: never sent twice");
        assert!(graph.drafts().is_empty());
        assert_eq!(outbox.row(id).state, OutboxState::Done, "{step:?}");
    }
}

/// Killed inside a draft's send that Exchange never took: the draft is still
/// in Drafts. Before the settle time the resume waits; after it, it sends
/// that same draft rather than a new one.
#[tokio::test]
async fn a_send_that_never_took_sends_the_same_draft_after_the_settle_time() {
    let outbox = Outbox::new();
    let built = large_reply("never-took");
    let id = outbox.enqueue(&built);
    let graph = FakeGraph::with_fault(Step::Send, Fault::HangBefore);

    crash(outbox.live(id, &built, &graph)).await;
    let row = outbox.row(id);
    let started = row.submission_started_at.expect("the send was entered");
    let draft = row.envelope.unwrap().graph_draft.unwrap();

    outbox.resume(started + 10, &graph).await;
    assert!(graph.sent().is_empty(), "too early to decide");
    assert_eq!(outbox.row(id).state, OutboxState::PendingSend);
    assert_eq!(graph.drafts(), std::slice::from_ref(&draft));

    outbox.resume(started + SEND_SETTLE_SECS, &graph).await;
    assert_eq!(graph.sent().len(), 1);
    assert_eq!(graph.0.lock().unwrap().sent_drafts, [draft]);
    assert!(graph.drafts().is_empty());
    assert_eq!(outbox.row(id).state, OutboxState::Done);
}

/// A sendMail with no answer and, after the settle time, no copy anywhere:
/// nobody can tell, so the row is parked for a human and nothing is sent.
#[tokio::test]
async fn an_unconfirmed_send_with_no_trace_is_parked_not_resent() {
    let outbox = Outbox::new();
    let built = small_message("no-trace");
    let id = outbox.enqueue(&built);
    let graph = FakeGraph::with_fault(Step::SendMime, Fault::HangBefore);

    crash(outbox.live(id, &built, &graph)).await;
    let started = outbox.row(id).submission_started_at.unwrap();

    outbox.resume(started + SEND_SETTLE_SECS, &graph).await;

    assert!(graph.sent().is_empty());
    let row = outbox.row(id);
    assert_eq!(row.state, OutboxState::Failed);
    assert!(
        row.last_error.unwrap().contains("never confirmed"),
        "the reason is explicit"
    );
}

/// A row that never reached Graph (the process died right after the commit)
/// is submitted from its bytes on the next start, Bcc restored from the
/// envelope: the Graph resume ticket #0063 left parked.
#[tokio::test]
async fn a_row_never_submitted_is_sent_from_its_bytes_with_its_bcc() {
    let outbox = Outbox::new();
    let built = small_message("never-submitted");
    let id = outbox.enqueue(&built);
    let graph = FakeGraph::default();

    outbox.resume(outbox::unix_now(), &graph).await;

    let sent = graph.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].message_id, built.message_id);
    assert_eq!(
        header(sent[0].mime.as_deref().unwrap(), "Bcc").as_deref(),
        Some("dan@example.com")
    );
    assert_eq!(outbox.row(id).state, OutboxState::Done);
}

/// A live send whose `send` times out is not parked at once: the row stays
/// queued with its marker, and the resume finds the copy and records it.
#[tokio::test]
async fn a_live_send_timeout_is_settled_by_the_mailbox() {
    let outbox = Outbox::new();
    let built = large_reply("live-timeout");
    let id = outbox.enqueue(&built);
    let graph = FakeGraph::with_fault(Step::Send, Fault::TimeoutAfter);

    let outcome = outbox.live(id, &built, &graph).await;
    assert!(
        matches!(outcome, GraphOutcome::Unconfirmed(_)),
        "{outcome:?}"
    );
    let row = outbox.row(id);
    assert_eq!(row.state, OutboxState::PendingSend);
    assert!(row.submission_started_at.is_some());

    outbox.resume(outbox::unix_now(), &graph).await;
    assert_eq!(graph.sent().len(), 1);
    assert_eq!(outbox.row(id).state, OutboxState::Done);
}

/// A refused upload discards the draft at once and leaves the row
/// submittable, one attempt older.
#[tokio::test]
async fn a_refused_upload_discards_its_draft_and_backs_off() {
    let outbox = Outbox::new();
    let built = large_reply("refused-upload");
    let id = outbox.enqueue(&built);
    let graph = FakeGraph::with_fault(Step::Attach, Fault::Refuse);

    let outcome = outbox.live(id, &built, &graph).await;
    assert!(matches!(outcome, GraphOutcome::Clean(_)), "{outcome:?}");
    assert!(graph.drafts().is_empty());
    assert!(graph.sent().is_empty());
    let row = outbox.row(id);
    assert_eq!(row.state, OutboxState::PendingSend);
    assert_eq!(row.attempts, 1);
    assert_eq!(row.submission_started_at, None);
    assert_eq!(row.envelope.unwrap().graph_draft, None);
}

/// A row this process is still sending has no marker while its draft fills;
/// a resume that ran then must leave it alone, or it would send it twice.
#[tokio::test]
async fn a_row_being_sent_in_this_process_is_left_to_its_send() {
    let outbox = Outbox::new();
    let built = small_message("in-flight");
    let id = outbox.enqueue(&built);
    let graph = FakeGraph::default();

    let held = crate::send::SendAdmission::claim(ACCOUNT, built.draft_key.as_deref().unwrap())
        .expect("free");
    outbox.resume(outbox::unix_now(), &graph).await;
    assert!(graph.sent().is_empty());
    assert_eq!(outbox.row(id).state, OutboxState::PendingSend);

    drop(held);
    outbox.resume(outbox::unix_now(), &graph).await;
    assert_eq!(graph.sent().len(), 1);
}

/// An operator `retry` of a message that did arrive: the resume finds it in
/// Sent Items, records it, and sends nothing; a leftover draft goes too.
#[tokio::test]
async fn a_retried_row_already_in_sent_items_is_not_sent_again() {
    let outbox = Outbox::new();
    let built = small_message("already-sent");
    let id = outbox.enqueue(&built);
    let graph = FakeGraph::default();
    graph.0.lock().unwrap().sent.push(Item {
        message_id: built.message_id.clone(),
        ..Item::default()
    });
    graph.plant_draft(&built.message_id);

    outbox.resume(outbox::unix_now(), &graph).await;

    assert_eq!(graph.sent().len(), 1);
    assert!(graph.drafts().is_empty());
    assert_eq!(outbox.row(id).state, OutboxState::Done);
}

// ---------------------------------------------------------------------------
// The review of d5617984: a 5xx is not a refusal, and a resumed send retires
// its draft file
// ---------------------------------------------------------------------------

/// The clock a resume runs at once the backoff of one clean failure is over,
/// well inside the settle time.
fn after_backoff() -> i64 {
    outbox::unix_now() + outbox::backoff_secs(1) + 1
}

/// sendMail answered 504 after Graph took the message, which it files in Sent
/// Items later: the row keeps its marker, the resume waits instead of sending
/// a second copy, and records the first once it shows up.
#[tokio::test]
async fn a_5xx_on_send_mail_after_graph_took_it_is_not_sent_again() {
    let outbox = Outbox::new();
    let built = small_message("gateway-mime");
    let id = outbox.enqueue(&built);
    let graph = FakeGraph::with_fault(Step::SendMime, Fault::Gateway);

    let outcome = outbox.live(id, &built, &graph).await;
    assert!(
        matches!(outcome, GraphOutcome::Unconfirmed(_)),
        "{outcome:?}"
    );
    assert!(
        outbox.row(id).submission_started_at.is_some(),
        "the marker stays"
    );

    outbox.resume(after_backoff(), &graph).await;
    assert!(graph.sent().is_empty(), "nothing sent a second time");
    assert_eq!(graph.0.lock().unwrap().queued.len(), 1);
    assert_eq!(outbox.row(id).state, OutboxState::PendingSend);

    graph.deliver();
    outbox.resume(after_backoff(), &graph).await;
    assert_eq!(graph.sent().len(), 1, "sent exactly once");
    assert_eq!(outbox.row(id).state, OutboxState::Done);
}

/// A draft's `/send` answered 502 after Graph took it: the draft is not
/// discarded, no second draft is created, and the item in the Outbox past
/// the settle time is waited for rather than parked.
#[tokio::test]
async fn a_5xx_on_a_drafts_send_keeps_the_draft_and_is_not_sent_again() {
    let outbox = Outbox::new();
    let built = large_reply("gateway-draft");
    let id = outbox.enqueue(&built);
    let graph = FakeGraph::with_fault(Step::Send, Fault::Gateway);

    let outcome = outbox.live(id, &built, &graph).await;
    assert!(
        matches!(outcome, GraphOutcome::Unconfirmed(_)),
        "{outcome:?}"
    );
    let row = outbox.row(id);
    let started = row.submission_started_at.expect("the marker stays");
    assert!(
        row.envelope.unwrap().graph_draft.is_some(),
        "the draft stays recorded"
    );
    assert!(
        graph.0.lock().unwrap().discarded.is_empty(),
        "nothing discarded"
    );

    outbox.resume(after_backoff(), &graph).await;
    outbox.resume(started + SEND_SETTLE_SECS, &graph).await;
    assert!(graph.sent().is_empty());
    assert!(graph.drafts().is_empty(), "no second draft was created");
    assert_eq!(
        outbox.row(id).state,
        OutboxState::PendingSend,
        "a draft on its way is waited for, not parked"
    );

    graph.deliver();
    outbox.resume(started + SEND_SETTLE_SECS, &graph).await;
    assert_eq!(graph.sent().len(), 1, "sent exactly once");
    assert_eq!(outbox.row(id).state, OutboxState::Done);
}

/// The settle time passed with the draft still in Drafts, so the resume sends
/// it again; by then the first send was taken late and moved it, and the
/// resend gets a 404. That is no proof of anything: the row waits for Sent
/// Items rather than creating a fresh message.
#[tokio::test]
async fn a_resent_draft_that_left_drafts_is_not_replaced_by_a_new_message() {
    let outbox = Outbox::new();
    let built = large_reply("resend-404");
    let id = outbox.enqueue(&built);
    let graph = FakeGraph::with_fault(Step::Send, Fault::HangBefore);

    crash(outbox.live(id, &built, &graph)).await;
    let started = outbox.row(id).submission_started_at.unwrap();
    graph
        .0
        .lock()
        .unwrap()
        .faults
        .push((Step::Send, Fault::Vanished));
    let created = graph.0.lock().unwrap().next_id;

    outbox.resume(started + SEND_SETTLE_SECS, &graph).await;
    let row = outbox.row(id);
    assert_eq!(row.state, OutboxState::PendingSend);
    assert!(
        row.submission_started_at.is_some(),
        "the 404 keeps the marker"
    );

    let later = started + SEND_SETTLE_SECS + outbox::backoff_secs(1) + 1;
    outbox.resume(later, &graph).await;
    assert_eq!(graph.0.lock().unwrap().next_id, created, "no new draft");
    assert!(graph.sent().is_empty());
    assert_eq!(outbox.row(id).state, OutboxState::PendingSend);

    graph.deliver();
    outbox.resume(started + SEND_SETTLE_SECS, &graph).await;
    assert_eq!(graph.sent().len(), 1, "sent exactly once");
    assert_eq!(outbox.row(id).state, OutboxState::Done);
}

/// A message whose body alone is over the MIME limit, so it goes as JSON.
fn large_json_message(key: &str) -> BuiltMessage {
    let mut draft = draft_with(Some(key), "/drafts/j.md");
    draft.body_markdown = "word ".repeat(600_000);
    let built = build_with_ancestry(&draft, None);
    assert!(
        matches!(
            plan(&built).unwrap(),
            GraphPlan::Draft {
                body: DraftBody::Json(_),
                ..
            }
        ),
        "the fixture goes as JSON"
    );
    built
}

/// The JSON resource carries our `Message-ID` in the extended property the
/// lookups filter on.
#[test]
fn a_json_draft_is_stamped_with_our_message_id() {
    let built = large_json_message("json-stamp");
    let GraphPlan::Draft {
        body: DraftBody::Json(message),
        ..
    } = plan(&built).unwrap()
    else {
        unreachable!()
    };
    assert_eq!(
        message["singleValueExtendedProperties"][0]["id"],
        crate::graph::MESSAGE_ID_PROPERTY
    );
    assert_eq!(
        message["singleValueExtendedProperties"][0]["value"],
        built.message_id.as_str()
    );
}

/// Killed after Graph created a JSON draft and before its id came back: the
/// draft is not under our `Message-ID`, and the resume still finds it by the
/// stamped property and discards it.
#[tokio::test]
async fn a_json_draft_whose_creation_was_never_answered_is_found_and_discarded() {
    let outbox = Outbox::new();
    let built = large_json_message("json-crash-create");
    let id = outbox.enqueue(&built);
    let graph = FakeGraph::with_fault(Step::Create, Fault::HangAfter);

    crash(outbox.live(id, &built, &graph)).await;
    assert_eq!(graph.drafts().len(), 1);

    outbox.resume(outbox::unix_now(), &graph).await;

    assert_eq!(graph.sent().len(), 1);
    assert!(graph.drafts().is_empty(), "no orphan draft");
    assert_eq!(outbox.row(id).state, OutboxState::Done);
}

/// A JSON send that never took: the recorded draft, read by its id, is still
/// in Drafts after the settle time, and that same draft is sent.
#[tokio::test]
async fn a_json_send_that_never_took_sends_the_same_draft() {
    let outbox = Outbox::new();
    let built = large_json_message("json-never-took");
    let id = outbox.enqueue(&built);
    let graph = FakeGraph::with_fault(Step::Send, Fault::HangBefore);

    crash(outbox.live(id, &built, &graph)).await;
    let row = outbox.row(id);
    let started = row.submission_started_at.unwrap();
    let draft = row.envelope.unwrap().graph_draft.unwrap();
    // Read by its id, the draft is found even where no lookup by
    // `Message-ID` or property would see it.
    graph
        .0
        .lock()
        .unwrap()
        .drafts
        .get_mut(&draft)
        .unwrap()
        .property = None;

    outbox.resume(started + SEND_SETTLE_SECS, &graph).await;

    assert_eq!(graph.0.lock().unwrap().sent_drafts, [draft]);
    assert!(graph.drafts().is_empty());
    assert_eq!(outbox.row(id).state, OutboxState::Done);
}

/// A resume that sends the message retires the draft file it was built
/// from, as the live send does, so `mp send-approved` cannot send it again.
#[tokio::test]
async fn a_resumed_send_retires_its_draft_file() {
    let outbox = Outbox::new();
    let built = large_reply("retire-after-resume");
    let file = outbox.write_draft("retire-after-resume");
    let id = outbox.enqueue(&built);
    let graph = FakeGraph::with_fault(Step::Attach, Fault::Refuse);

    outbox.live(id, &built, &graph).await;
    assert!(
        file.exists(),
        "a clean failure leaves the file for the resume"
    );

    outbox.resume(after_backoff(), &graph).await;

    assert_eq!(graph.sent().len(), 1);
    assert_eq!(outbox.row(id).state, OutboxState::Done);
    assert!(!file.exists(), "the sent draft is retired");
}

/// An unconfirmed send found in Sent Items by the resume retires the file
/// too, and a row still unsettled leaves it alone.
#[tokio::test]
async fn an_unconfirmed_send_found_in_sent_items_retires_its_draft_file() {
    let outbox = Outbox::new();
    let built = small_message("retire-unconfirmed");
    let file = outbox.write_draft("retire-unconfirmed");
    let id = outbox.enqueue(&built);
    let graph = FakeGraph::with_fault(Step::SendMime, Fault::Gateway);

    outbox.live(id, &built, &graph).await;
    outbox.resume(after_backoff(), &graph).await;
    assert!(file.exists(), "nothing is known yet");

    graph.deliver();
    outbox.resume(after_backoff(), &graph).await;
    assert_eq!(outbox.row(id).state, OutboxState::Done);
    assert!(!file.exists(), "the sent draft is retired");
}

/// Discarding a row removes the drafts it left on the server: the recorded
/// one and any under its `Message-ID`, but never an item on its way out.
#[tokio::test]
async fn discarding_a_row_removes_its_server_drafts_and_nothing_in_flight() {
    let outbox = Outbox::new();
    let built = large_reply("discard-row");
    let id = outbox.enqueue(&built);
    let graph = FakeGraph::with_fault(Step::Send, Fault::HangBefore);
    crash(outbox.live(id, &built, &graph)).await;
    // The recorded draft as no lookup sees it (a JSON draft from before the
    // stamp): only its recorded id names it.
    for item in graph.0.lock().unwrap().drafts.values_mut() {
        item.message_id = "<exchange-own@fake>".to_string();
    }
    graph.plant_draft(&built.message_id);
    assert_eq!(graph.drafts().len(), 2);

    discard_row_drafts(&graph, &outbox.row(id)).await.unwrap();
    assert!(graph.drafts().is_empty(), "no orphan draft");

    // A recorded draft that left Drafts is the message itself.
    let built = large_reply("discard-in-flight");
    let id = outbox.enqueue(&built);
    let graph = FakeGraph::with_fault(Step::Send, Fault::Gateway);
    outbox.live(id, &built, &graph).await;
    discard_row_drafts(&graph, &outbox.row(id)).await.unwrap();
    assert!(graph.0.lock().unwrap().discarded.is_empty());
}
