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
}

#[derive(Debug, Clone)]
struct Item {
    message_id: String,
    mime: Option<Vec<u8>>,
    attachments: Vec<OutgoingAttachment>,
}

#[derive(Default)]
struct Server {
    next_id: u32,
    drafts: BTreeMap<String, Item>,
    sent: Vec<Item>,
    faults: Vec<(Step, Fault)>,
    discarded: Vec<String>,
    sent_drafts: Vec<String>,
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

    /// Run one request: the fault decides whether the server acts and what
    /// the client hears.
    async fn act<T: Send>(
        &self,
        step: Step,
        action: impl FnOnce(&mut Server) -> Result<T> + Send,
    ) -> Result<T> {
        let fault = self.take_fault(step);
        match fault {
            Some(Fault::HangBefore) => std::future::pending::<()>().await,
            Some(Fault::Refuse) => return Err(anyhow!("HTTP 400 from the fake")),
            _ => {}
        }
        let value = action(&mut self.0.lock().unwrap())?;
        match fault {
            Some(Fault::HangAfter) => {
                std::future::pending::<()>().await;
                unreachable!()
            }
            Some(Fault::TimeoutAfter) => Err(anyhow!(outbox::AmbiguousSubmission(
                "operation timed out".to_string()
            ))),
            _ => Ok(value),
        }
    }

    fn sent(&self) -> Vec<Item> {
        self.0.lock().unwrap().sent.clone()
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
                mime: None,
                attachments: Vec::new(),
            },
        );
        id
    }
}

impl GraphSendApi for FakeGraph {
    fn send_mime(&self, mime_base64: String) -> impl Future<Output = Result<()>> + Send {
        self.act(Step::SendMime, move |server| {
            let (message_id, mime) = message_id_of_mime(&mime_base64);
            server.sent.push(Item {
                message_id,
                mime: Some(mime),
                attachments: Vec::new(),
            });
            Ok(())
        })
    }

    fn create_draft_mime(
        &self,
        mime_base64: String,
    ) -> impl Future<Output = Result<String>> + Send {
        self.act(Step::Create, move |server| {
            let (message_id, mime) = message_id_of_mime(&mime_base64);
            server.next_id += 1;
            let id = format!("draft-{}", server.next_id);
            server.drafts.insert(
                id.clone(),
                Item {
                    message_id,
                    mime: Some(mime),
                    attachments: Vec::new(),
                },
            );
            Ok(id)
        })
    }

    fn create_draft_json(&self, _message: &Value) -> impl Future<Output = Result<String>> + Send {
        self.act(Step::Create, move |server| {
            server.next_id += 1;
            let id = format!("draft-{}", server.next_id);
            server.drafts.insert(
                id.clone(),
                Item {
                    message_id: format!("<exchange-{}@fake>", server.next_id),
                    mime: None,
                    attachments: Vec::new(),
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
        self.act(Step::Attach, move |server| {
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
        self.act(Step::Send, move |server| {
            let item = server
                .drafts
                .remove(&draft_id)
                .ok_or_else(|| anyhow!("HTTP 404"))?;
            server.sent.push(item);
            server.sent_drafts.push(draft_id);
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
                sent: server.sent.iter().any(|i| i.message_id == wanted),
                drafts: server
                    .drafts
                    .iter()
                    .filter(|(_, i)| i.message_id == wanted)
                    .map(|(id, _)| id.clone())
                    .collect(),
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
}

impl Outbox {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("store.sqlite3")).unwrap();
        let blobs = BlobStore::new(dir.path().join("blobs"));
        Self {
            _dir: dir,
            store,
            blobs,
        }
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
        resume_rows(&self.store, &self.blobs, ACCOUNT, now, api).await;
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
        mime: None,
        attachments: Vec::new(),
    });
    graph.plant_draft(&built.message_id);

    outbox.resume(outbox::unix_now(), &graph).await;

    assert_eq!(graph.sent().len(), 1);
    assert!(graph.drafts().is_empty());
    assert_eq!(outbox.row(id).state, OutboxState::Done);
}
