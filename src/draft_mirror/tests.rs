//! The drafts mirror over an in-memory Drafts folder (#0143).

use std::path::PathBuf;

use super::folder::fake::FakeFolder;
use super::folder::{header_value, DraftsFolder};
use super::push::{self, Mirror};
use super::*;
use crate::config::{AccountConfig, EmailSettings};
use crate::ops::ServerOp;
use crate::pending_ops::{self, OpExecutor};
use crate::store::{draft_sync, BlobStore};

/// One account with a store and a drafts directory under a fresh data dir.
pub(super) struct Fixture {
    _data: crate::config::test_env::TestDataDir,
    pub account: AccountConfig,
    pub store: Store,
    pub email: EmailSettings,
}

impl Fixture {
    pub fn new() -> Fixture {
        let data = crate::config::test_env::TestDataDir::new();
        let account = AccountConfig {
            name: "tum".to_string(),
            default_from: "me@tum.de".to_string(),
            ..Default::default()
        };
        std::fs::create_dir_all(crate::config::drafts_dir("tum")).unwrap();
        let store = Store::open(crate::config::store_path("tum")).unwrap();
        Fixture {
            _data: data,
            account,
            store,
            email: EmailSettings::default(),
        }
    }

    pub fn mirror(&self, now: i64) -> Mirror<'_> {
        Mirror {
            store: &self.store,
            account: &self.account,
            email: &self.email,
            now,
        }
    }

    /// Write `drafts/<name>.md` with `id: <id>` and the given extra
    /// frontmatter lines and body.
    pub fn write(&self, name: &str, id: &str, extra: &str, body: &str) -> PathBuf {
        Self::write_in_current(name, id, extra, body)
    }

    /// [`Fixture::write`] into whichever data dir is current.
    pub fn write_in_current(name: &str, id: &str, extra: &str, body: &str) -> PathBuf {
        let path = crate::config::drafts_dir("tum").join(format!("{name}.md"));
        std::fs::write(
            &path,
            format!(
                "---\nid: \"{id}\"\nto: robin@example.com\ncc:\nbcc: chef@example.com\n\
                 subject: Angebot\nstatus: draft\n{extra}---\n\n{body}\n"
            ),
        )
        .unwrap();
        path
    }

    pub fn row(&self, id: &str) -> Option<draft_sync::DraftSyncRow> {
        draft_sync::get(&self.store, "tum", id).unwrap()
    }
}

fn block_on<F: std::future::Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}

/// The draft's server copy: its id header, its Message-ID, its Bcc and its
/// reply threading, whatever its status and however unfinished.
#[test]
fn a_server_copy_keeps_the_bcc_the_id_and_the_reply_headers() {
    let fx = Fixture::new();
    let path = fx.write(
        "reply",
        "d1",
        "in_reply_to: \"<parent@example.com>\"\n",
        "Danke!",
    );
    let draft = crate::draft::parse_email_draft(&path).unwrap();
    let raw = crate::send::build_server_draft(
        &draft,
        "me@tum.de",
        &fx.email,
        "<mp-draft.1@tum.de>",
        &std::fs::read(&path).unwrap(),
    )
    .unwrap();
    let header = String::from_utf8_lossy(&raw);
    assert_eq!(
        header_value(&raw, "X-Mailypoppins-Draft-Id").as_deref(),
        Some("d1")
    );
    assert_eq!(
        header_value(&raw, "Message-ID").as_deref(),
        Some("<mp-draft.1@tum.de>")
    );
    assert_eq!(
        header_value(&raw, "Bcc").as_deref(),
        Some("chef@example.com")
    );
    assert_eq!(
        header_value(&raw, "In-Reply-To").as_deref(),
        Some("<parent@example.com>")
    );
    assert!(header.contains("Danke!"), "{header}");
    assert!(
        header_value(&raw, "X-Mailypoppins-Draft-Status").is_none(),
        "the status travels inside the carried file, never in a header of its own"
    );

    // No recipient and no subject yet is still a draft the server can hold.
    std::fs::write(
        &path,
        "---\nid: \"d1\"\nto:\nsubject:\nstatus: draft\n---\n\nhalf\n",
    )
    .unwrap();
    let draft = crate::draft::parse_email_draft(&path).unwrap();
    assert!(crate::send::build_server_draft(
        &draft,
        "me@tum.de",
        &fx.email,
        "<x@tum.de>",
        &std::fs::read(&path).unwrap()
    )
    .is_ok());
}

/// The first push mints the draft's `server_message_id` into its file, puts
/// one copy in the folder and records it; a second push of the same bytes
/// does nothing.
#[test]
fn the_first_push_records_the_message_id_and_the_copy() {
    let fx = Fixture::new();
    let path = fx.write("angebot", "d1", "", "Hallo");
    let mut folder = FakeFolder::default();
    block_on(push::push(&fx.mirror(1000), &mut folder, "d1")).unwrap();

    let written = crate::draft::parse_email_draft(&path).unwrap();
    let mid = written
        .frontmatter
        .server_message_id
        .clone()
        .expect("the first push records the copies' Message-ID");
    assert!(
        mid.starts_with("<mp-draft.") && mid.ends_with("@tum.de>"),
        "{mid}"
    );
    assert_eq!(folder.live().len(), 1);
    let copy = &folder.live()[0];
    assert_eq!(
        header_value(&copy.raw, "Message-ID").as_deref(),
        Some(mid.as_str())
    );

    let row = fx.row("d1").expect("a row");
    assert_eq!(
        (row.mailbox.as_str(), row.uid, row.uidvalidity),
        ("Drafts", copy.uid, 7)
    );
    assert_eq!(row.synced_at, 1000);
    assert_eq!(
        row.synced_hash,
        file_hash(&std::fs::read(&path).unwrap()),
        "the hash is the file's after the write"
    );

    block_on(push::push(&fx.mirror(2000), &mut folder, "d1")).unwrap();
    assert_eq!(folder.log, vec!["append".to_string()], "nothing changed");
}

/// An edit replaces the copy: one copy afterwards, under the same
/// Message-ID, and the push leaves the file's bytes alone.
#[test]
fn an_edit_replaces_the_copy_and_leaves_the_file_alone() {
    let fx = Fixture::new();
    let path = fx.write("angebot", "d1", "", "Hallo");
    let mut folder = FakeFolder::default();
    block_on(push::push(&fx.mirror(1000), &mut folder, "d1")).unwrap();
    let first = fx.row("d1").unwrap();

    let edited = std::fs::read_to_string(&path)
        .unwrap()
        .replace("Hallo", "Hallo Robin");
    std::fs::write(&path, &edited).unwrap();
    block_on(push::push(&fx.mirror(2000), &mut folder, "d1")).unwrap();

    assert_eq!(std::fs::read_to_string(&path).unwrap(), edited, "untouched");
    let live = folder.live();
    assert_eq!(live.len(), 1, "one copy: {:?}", folder.log);
    assert!(String::from_utf8_lossy(&live[0].raw).contains("Hallo Robin"));
    let second = fx.row("d1").unwrap();
    assert_ne!(second.uid, first.uid);
    assert_eq!(second.uid, live[0].uid);
    assert_eq!(second.synced_hash, file_hash(edited.as_bytes()));
    assert_eq!(
        folder.log,
        vec![
            "append".to_string(),
            "append".to_string(),
            format!("delete [{}]", first.uid)
        ],
        "APPEND before the delete"
    );
}

/// Without UIDPLUS the old copy is only flagged deleted, which every read of
/// the folder skips.
#[test]
fn without_uidplus_the_old_copy_is_only_flagged() {
    let fx = Fixture::new();
    let path = fx.write("angebot", "d1", "", "Hallo");
    let mut folder = FakeFolder {
        uidplus: false,
        ..FakeFolder::default()
    };
    block_on(push::push(&fx.mirror(1000), &mut folder, "d1")).unwrap();
    std::fs::write(
        &path,
        std::fs::read_to_string(&path)
            .unwrap()
            .replace("Hallo", "Hi"),
    )
    .unwrap();
    block_on(push::push(&fx.mirror(2000), &mut folder, "d1")).unwrap();
    assert_eq!(folder.copies.len(), 2, "the old copy is still there");
    assert_eq!(folder.live().len(), 1, "flagged deleted");
}

/// What owes a push: a draft with no row or whose bytes moved; not a synced
/// one, a conflict copy, a sent draft, nor anything on an account whose
/// mirror is off. A draft with a push queued is not queued twice.
#[test]
fn the_owed_pushes_are_queued_once() {
    let mut fx = Fixture::new();
    fx.write("a", "d-new", "", "neu");
    fx.write("b", "d-conflict", "conflict_of: d-new\n", "alt");
    let sent = fx.write("c", "d-sent", "", "weg");
    std::fs::write(
        &sent,
        std::fs::read_to_string(&sent)
            .unwrap()
            .replace("status: draft", "status: sent"),
    )
    .unwrap();
    let synced = fx.write("d", "d-synced", "", "fertig");
    draft_sync::upsert(
        &fx.store,
        &draft_sync::DraftSyncRow {
            account: "tum".to_string(),
            draft_id: "d-synced".to_string(),
            mailbox: "Drafts".to_string(),
            uid: 1,
            uidvalidity: 7,
            synced_at: 1,
            synced_hash: file_hash(&std::fs::read(&synced).unwrap()),
        },
    )
    .unwrap();

    assert_eq!(push::enqueue_owed(&fx.store, &fx.account).unwrap(), 1);
    assert_eq!(
        push::enqueue_owed(&fx.store, &fx.account).unwrap(),
        0,
        "queued once"
    );
    let queued = pending_ops::queued_ops(&fx.store, "tum").unwrap();
    assert_eq!(queued.len(), 1);
    assert!(matches!(
        &queued[0].op,
        ServerOp::PushDraft { draft_id, hash: Some(_) } if draft_id == "d-new"
    ));
    assert_eq!(queued[0].target_message_id, None);

    std::fs::write(
        &synced,
        "---\nid: \"d-synced\"\nsubject: x\nstatus: draft\n---\n\nmoved\n",
    )
    .unwrap();
    assert_eq!(
        push::enqueue_owed(&fx.store, &fx.account).unwrap(),
        1,
        "it moved"
    );

    fx.account.drafts.mirror = false;
    fx.write("e", "d-off", "", "aus");
    assert_eq!(
        push::enqueue_owed(&fx.store, &fx.account).unwrap(),
        0,
        "the switch is off"
    );
}

/// A delete removes every copy of the Message-ID, and a delete that finds
/// none is the queue's converged replay.
#[test]
fn a_delete_removes_every_copy_and_none_is_converged() {
    let mut folder = FakeFolder::default();
    let raw = b"Message-ID: <mp-draft.9@tum.de>\r\nSubject: x\r\n\r\nbody\r\n";
    folder.put(raw);
    folder.put(raw);
    folder.put(b"Message-ID: <other@tum.de>\r\n\r\nkeep\r\n");
    block_on(push::delete(&mut folder, "Drafts", "<mp-draft.9@tum.de>")).unwrap();
    assert_eq!(folder.live().len(), 1, "the other draft stays");
    let again = block_on(push::delete(&mut folder, "Drafts", "<mp-draft.9@tum.de>")).unwrap_err();
    assert!(crate::ops::NotFoundOnServer::is_in(&again), "{again:#}");
}

/// The test executor: the message ops are refused, the draft ops run against
/// a fake folder.
struct FakeExecutor<'f> {
    folder: &'f mut FakeFolder,
    account: AccountConfig,
    email: EmailSettings,
}

impl OpExecutor for FakeExecutor<'_> {
    async fn execute(&mut self, op: &ServerOp) -> anyhow::Result<()> {
        Err(anyhow::anyhow!("no message op in this test: {}", op.kind()))
    }

    async fn execute_draft(&mut self, store: &Store, op: &ServerOp) -> anyhow::Result<()> {
        let mirror = Mirror {
            store,
            account: &self.account,
            email: &self.email,
            now: 5000,
        };
        push::run(&mirror, self.folder, op).await
    }
}

/// A queued push drains through the queue like a queued archive: retired on
/// success; a refused APPEND retries under backoff, parks `failed` past the
/// budget with nothing to roll back, and a later push that succeeds forgets
/// the failure.
#[test]
fn a_queued_push_drains_and_a_refused_one_parks_failed() {
    let fx = Fixture::new();
    let path = fx.write("a", "d1", "", "eins");
    push::enqueue_owed(&fx.store, &fx.account).unwrap();
    let blobs = BlobStore::for_account("tum");
    let mut folder = FakeFolder {
        fail_append: true,
        ..FakeFolder::default()
    };
    let mut now = crate::outbox::unix_now();
    for _ in 0..pending_ops::MAX_ATTEMPTS {
        let mut exec = FakeExecutor {
            folder: &mut folder,
            account: fx.account.clone(),
            email: EmailSettings::default(),
        };
        block_on(pending_ops::drain(&fx.store, &blobs, "tum", &mut exec, now)).unwrap();
        now += 100_000;
    }
    assert!(pending_ops::queued_ops(&fx.store, "tum")
        .unwrap()
        .is_empty());
    assert_eq!(
        pending_ops::failed_draft_pushes(&fx.store, "tum").unwrap(),
        vec!["d1".to_string()]
    );
    assert!(fx.row("d1").is_none());

    folder.fail_append = false;
    assert_eq!(
        push::enqueue_owed(&fx.store, &fx.account).unwrap(),
        0,
        "the parked push is for these bytes: not queued again at every drain"
    );
    assert_eq!(
        pending_ops::failed_ops(&fx.store, "tum").unwrap().len(),
        1,
        "one failed row, not one per drain"
    );
    // The next edit retries.
    std::fs::write(
        &path,
        std::fs::read_to_string(&path)
            .unwrap()
            .replace("eins", "eins, zwei"),
    )
    .unwrap();
    assert_eq!(push::enqueue_owed(&fx.store, &fx.account).unwrap(), 1);
    let mut exec = FakeExecutor {
        folder: &mut folder,
        account: fx.account.clone(),
        email: EmailSettings::default(),
    };
    let result = block_on(pending_ops::drain(&fx.store, &blobs, "tum", &mut exec, now)).unwrap();
    assert_eq!(result.completed, 1);
    assert!(fx.row("d1").is_some());
    assert!(
        pending_ops::failed_draft_pushes(&fx.store, "tum")
            .unwrap()
            .is_empty(),
        "the success forgets the failure"
    );
    assert_eq!(folder.live().len(), 1);
}

/// A draft saved while the server is out of reach reaches it once the server
/// is back, with no edit: a push parked `failed` holds the owed-push scan back
/// only for a cool-down, then one fresh push is queued. A draft has one
/// failed row at a time, and a retry that fails again for the same bytes is
/// not announced a second time.
#[test]
fn a_push_parked_offline_is_retried_after_the_cool_down() {
    let fx = Fixture::new();
    fx.write("a", "d1", "", "eins");
    let blobs = BlobStore::for_account("tum");
    let mut folder = FakeFolder {
        fail_append: true,
        ..FakeFolder::default()
    };
    let mut now = crate::outbox::unix_now();
    // Every drain point queues the owed pushes, then drains.
    let drain_all = |folder: &mut FakeFolder, now: &mut i64| -> usize {
        let mut failed = 0;
        for _ in 0..pending_ops::MAX_ATTEMPTS {
            push::enqueue_owed(&fx.store, &fx.account).unwrap();
            let mut exec = FakeExecutor {
                folder: &mut *folder,
                account: fx.account.clone(),
                email: EmailSettings::default(),
            };
            let result = block_on(pending_ops::drain(
                &fx.store, &blobs, "tum", &mut exec, *now,
            ))
            .unwrap();
            failed += result.failed;
            *now += 100_000;
        }
        failed
    };
    let past_the_cool_down = |store: &Store| {
        store
            .conn()
            .execute(
                "UPDATE pending_ops SET updated = updated - ?1 WHERE state = 'failed'",
                [pending_ops::PUSH_RETRY_COOL_DOWN_SECS + 1],
            )
            .unwrap();
    };

    assert_eq!(
        drain_all(&mut folder, &mut now),
        1,
        "parked and announced once"
    );
    assert_eq!(pending_ops::failed_ops(&fx.store, "tum").unwrap().len(), 1);
    assert_eq!(
        push::enqueue_owed(&fx.store, &fx.account).unwrap(),
        0,
        "not queued again during the cool-down"
    );

    past_the_cool_down(&fx.store);
    assert_eq!(
        drain_all(&mut folder, &mut now),
        0,
        "the retry failed for the same bytes: not announced again"
    );
    assert_eq!(
        pending_ops::failed_ops(&fx.store, "tum").unwrap().len(),
        1,
        "one failed row at a time"
    );
    assert!(fx.row("d1").is_none());

    folder.fail_append = false;
    past_the_cool_down(&fx.store);
    drain_all(&mut folder, &mut now);
    assert!(fx.row("d1").is_some(), "pushed once the server is back");
    assert_eq!(folder.live().len(), 1);
    assert!(pending_ops::failed_ops(&fx.store, "tum")
        .unwrap()
        .is_empty());
}

/// A push for a draft that is gone does nothing and succeeds; a Graph
/// account mirrors nothing.
#[test]
fn a_push_of_a_gone_draft_or_on_graph_does_nothing() {
    let mut fx = Fixture::new();
    let mut folder = FakeFolder::default();
    block_on(push::push(&fx.mirror(1), &mut folder, "missing")).unwrap();
    assert!(folder.log.is_empty());

    fx.write("a", "d1", "", "eins");
    fx.account.auth_method = crate::config::AuthMethod::Graph;
    assert!(!mirror_enabled(&fx.account));
    block_on(push::push(&fx.mirror(1), &mut folder, "d1")).unwrap();
    assert!(folder.log.is_empty());
}

/// A header edit (`draft.set_headers`, which rewrites the frontmatter in
/// place) is a change like any other: a synced draft owes a push afterwards.
#[test]
fn a_header_edit_owes_a_push() {
    let fx = Fixture::new();
    let path = fx.write("a", "d1", "", "eins");
    let mut folder = FakeFolder::default();
    block_on(push::push(&fx.mirror(1), &mut folder, "d1")).unwrap();
    assert_eq!(
        push::enqueue_owed(&fx.store, &fx.account).unwrap(),
        0,
        "synced"
    );
    crate::draft::rewrite_draft_recipients(
        &path,
        &crate::draft::DraftRecipientEdit {
            to: "neu@example.com".to_string(),
            cc: String::new(),
            bcc: String::new(),
            subject: "Angebot".to_string(),
        },
    )
    .unwrap();
    assert_eq!(push::enqueue_owed(&fx.store, &fx.account).unwrap(), 1);
}

/// A discarded draft forgets its row and queues the delete of its server
/// copy in the folder the row named; on an account that does not mirror, or
/// for a draft never pushed, only the row goes.
#[test]
fn a_retired_draft_forgets_its_row_and_queues_the_delete() {
    let mut fx = Fixture::new();
    let path = fx.write("a", "d1", "", "eins");
    let mut folder = FakeFolder::default();
    block_on(push::push(&fx.mirror(1), &mut folder, "d1")).unwrap();
    let row = fx.row("d1").unwrap();
    let mid = crate::draft::parse_email_draft(&path)
        .unwrap()
        .frontmatter
        .server_message_id
        .unwrap();

    assert!(push::forget_retired(&fx.store, &fx.account, "d1", Some(&mid)).unwrap());
    assert!(fx.row("d1").is_none());
    let queued = pending_ops::queued_ops(&fx.store, "tum").unwrap();
    assert_eq!(
        queued[0].op,
        ServerOp::DeleteDraft {
            mailbox: "Drafts".to_string(),
            message_id: mid.clone(),
            uid: Some(row.uid),
            uidvalidity: Some(7),
        }
    );
    block_on(push::run(&fx.mirror(2), &mut folder, &queued[0].op)).unwrap();
    assert!(folder.live().is_empty(), "the copy is gone");

    assert!(!push::forget_retired(&fx.store, &fx.account, "d2", None).unwrap());
    fx.account.drafts.mirror = false;
    assert!(!push::forget_retired(&fx.store, &fx.account, "d3", Some("<x@y>")).unwrap());
    assert_eq!(pending_ops::queued_ops(&fx.store, "tum").unwrap().len(), 1);
}

/// `drafts/.retired/` keeps a draft for 30 days from its retirement, not from
/// its last edit, and then the prune deletes it.
#[test]
fn a_retired_draft_is_pruned_after_thirty_days_from_its_retirement() {
    let fx = Fixture::new();
    let path = fx.write("old", "d1", "", "alt");
    let long_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(90 * 86_400);
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(long_ago)
        .unwrap();
    let retired = crate::draft::retire_draft_files(&path).unwrap().unwrap();
    let now = std::time::SystemTime::now();
    assert_eq!(prune_retired("tum", now), 0, "retired just now");
    let later = now + std::time::Duration::from_secs(31 * 86_400);
    assert_eq!(prune_retired("tum", later), 1);
    assert!(!retired.exists());
}

// ---------------------------------------------------------------------------
// The reconcile (#0143 step 5)
// ---------------------------------------------------------------------------

use super::reconcile::{reconcile, ReconcileReport};

/// A copy of `mid` as Outlook saves an edit of it: one `text/html` part, no
/// source comment, Outlook's reply header above the quote it rewrote.
fn outlook_copy(mid: &str, to: &str, subject: &str, html_body: &str) -> Vec<u8> {
    format!(
        "Message-ID: {mid}\r\nSubject: {subject}\r\nFrom: me@tum.de\r\nTo: {to}\r\n\
         Bcc: chef@example.com\r\nMIME-Version: 1.0\r\n\
         Content-Type: text/html; charset=utf-8\r\n\r\n\
         <html><head><style>p {{ margin: 0 }}</style></head><body>{html_body}\
         <div id=\"divRplyFwdMsg\"><b>From:</b> Robin</div><p>rewritten quote</p></body></html>\r\n"
    )
    .into_bytes()
}

fn mid_of(path: &std::path::Path) -> String {
    crate::draft::parse_email_draft(path)
        .unwrap()
        .frontmatter
        .server_message_id
        .unwrap()
}

fn reconciled(fx: &Fixture, folder: &mut FakeFolder) -> ReconcileReport {
    block_on(reconcile(&fx.mirror(9000), folder)).unwrap()
}

/// Losing the store loses the rows and nothing else: the reconcile finds each
/// draft's copy by its Message-ID, sees the copy is the file, and writes the
/// row back, with no second copy and no conflict.
#[test]
fn a_lost_row_is_recovered_from_the_copy_that_is_the_file() {
    let fx = Fixture::new();
    let path = fx.write("a", "d1", "", "eins");
    let mut folder = FakeFolder::default();
    block_on(push::push(&fx.mirror(1), &mut folder, "d1")).unwrap();
    let uid = fx.row("d1").unwrap().uid;
    draft_sync::delete(&fx.store, "tum", "d1").unwrap();

    let report = reconciled(&fx, &mut folder);
    assert_eq!(report.recovered, 1, "{report:?}");
    assert_eq!(report.conflicts, 0);
    assert_eq!(fx.row("d1").unwrap().uid, uid);
    assert_eq!(folder.live().len(), 1);
    assert_eq!(
        push::enqueue_owed(&fx.store, &fx.account).unwrap(),
        0,
        "synced"
    );

    // A push before the reconcile ran reconciles instead of duplicating.
    draft_sync::delete(&fx.store, "tum", "d1").unwrap();
    block_on(push::push(&fx.mirror(2), &mut folder, "d1")).unwrap();
    assert_eq!(folder.live().len(), 1, "{:?}", folder.log);
    assert!(fx.row("d1").is_some());
    let _ = path;
}

/// An edit in Outlook reaches the file: the recipients and the subject from
/// the copy's headers, the text above the quote from its HTML in Markdown,
/// and the quote below the marker as the file had it. An approved draft is
/// demoted, and the row names the edited copy.
#[test]
fn an_outlook_edit_is_pulled_above_the_quote_and_demotes_an_approval() {
    let fx = Fixture::new();
    let path = fx.write(
        "re",
        "d1",
        "",
        "Danke!\n\n<!-- mp:quote -->\n\n> Robin wrote:\n> Original",
    );
    let mut folder = FakeFolder::default();
    block_on(push::push(&fx.mirror(1), &mut folder, "d1")).unwrap();
    let approved = std::fs::read_to_string(&path)
        .unwrap()
        .replace("status: draft", "status: approved");
    std::fs::write(&path, approved).unwrap();
    block_on(push::push(&fx.mirror(2), &mut folder, "d1")).unwrap();
    let mid = mid_of(&path);
    let old = folder.live()[0].uid;
    folder.remove(old);
    let edited = folder.put(&outlook_copy(
        &mid,
        "robin@example.com, kim@example.com",
        "Angebot (neu)",
        "<p>Danke, <b>gerne</b>!</p><ul><li>eins</li><li>zwei</li></ul>",
    ));

    let report = reconciled(&fx, &mut folder);
    assert_eq!((report.pulled, report.conflicts), (1, 0), "{report:?}");
    let draft = crate::draft::parse_email_draft(&path).unwrap();
    assert_eq!(
        draft.frontmatter.to.as_deref(),
        Some("robin@example.com, kim@example.com")
    );
    assert_eq!(draft.frontmatter.subject, "Angebot (neu)");
    assert_eq!(
        draft.frontmatter.status,
        crate::types::EmailStatus::Draft,
        "demoted"
    );
    assert_eq!(
        draft.frontmatter.server_message_id.as_deref(),
        Some(mid.as_str())
    );
    let body = &draft.body_markdown;
    assert!(body.contains("Danke, **gerne**!"), "{body}");
    assert!(body.contains("- eins\n- zwei"), "{body}");
    assert!(!body.contains("rewritten quote"), "{body}");
    assert!(
        !body.contains("margin"),
        "the style block is skipped: {body}"
    );
    assert!(
        body.contains("<!-- mp:quote -->\n\n> Robin wrote:"),
        "{body}"
    );
    let row = fx.row("d1").unwrap();
    assert_eq!(row.uid, edited);
    assert_eq!(row.synced_hash, file_hash(&std::fs::read(&path).unwrap()));
    assert_eq!(
        push::enqueue_owed(&fx.store, &fx.account).unwrap(),
        0,
        "no echo push"
    );
}

/// Both sides changed since they last agreed: the draft takes the server's
/// version and the local text is kept in `<name>.conflict.md` under a fresh
/// id, which is never pushed.
#[test]
fn an_edit_on_both_sides_keeps_the_local_text_as_a_conflict_copy() {
    let fx = Fixture::new();
    let path = fx.write("angebot", "d1", "", "Original");
    let mut folder = FakeFolder::default();
    block_on(push::push(&fx.mirror(1), &mut folder, "d1")).unwrap();
    let mid = mid_of(&path);
    folder.remove(folder.live()[0].uid);
    folder.put(&outlook_copy(
        &mid,
        "robin@example.com",
        "Angebot",
        "<p>Server text</p>",
    ));
    let local = std::fs::read_to_string(&path)
        .unwrap()
        .replace("Original", "Local text");
    std::fs::write(&path, &local).unwrap();

    let report = reconciled(&fx, &mut folder);
    assert_eq!(report.conflicts, 1, "{report:?}");
    let draft = crate::draft::parse_email_draft(&path).unwrap();
    assert!(draft.body_markdown.contains("Server text"));
    let copy_path = path.with_file_name("angebot.conflict.md");
    let copy = crate::draft::parse_email_draft(&copy_path).unwrap();
    assert!(copy.body_markdown.contains("Local text"));
    assert_eq!(copy.frontmatter.conflict_of.as_deref(), Some("d1"));
    assert_ne!(copy.frontmatter.id.as_deref(), Some("d1"), "a fresh id");
    assert_eq!(copy.frontmatter.server_message_id, None);
    assert_eq!(
        push::enqueue_owed(&fx.store, &fx.account).unwrap(),
        0,
        "never pushed"
    );
}

/// A copy sent or deleted in another client takes the draft with it, once two
/// reconciles in a row have missed it: the file moves to `drafts/.retired/`
/// under a name that says why.
#[test]
fn a_copy_gone_elsewhere_retires_the_draft() {
    let fx = Fixture::new();
    let path = fx.write("angebot", "d1", "", "eins");
    let mut folder = FakeFolder::default();
    block_on(push::push(&fx.mirror(1), &mut folder, "d1")).unwrap();
    folder.remove(folder.live()[0].uid);

    assert_eq!(reconciled(&fx, &mut folder).retired, 0, "missed once");
    let report = reconciled(&fx, &mut folder);
    assert_eq!(report.retired, 1);
    assert!(!path.exists());
    assert!(path
        .parent()
        .unwrap()
        .join(crate::draft::RETIRED_DIR)
        .join("angebot.copy-gone.md")
        .exists());
    assert!(fx.row("d1").is_none());
    assert_eq!(reconciled(&fx, &mut folder), ReconcileReport::default());
}

/// Apple Mail saves an edit under a new Message-ID. The one new copy with the
/// draft's subject, recipients and reply target is the draft, relinked and
/// pulled rather than retired and imported twice.
#[test]
fn an_apple_mail_edit_under_a_new_message_id_is_relinked() {
    let fx = Fixture::new();
    let path = fx.write("angebot", "d1", "", "eins");
    let mut folder = FakeFolder::default();
    block_on(push::push(&fx.mirror(1), &mut folder, "d1")).unwrap();
    folder.remove(folder.live()[0].uid);
    folder.put(&outlook_copy(
        "<0DC386F6-APPLE@tum.de>",
        "robin@example.com",
        "Angebot",
        "<p>Apple text</p>",
    ));

    let report = reconciled(&fx, &mut folder);
    assert_eq!((report.pulled, report.retired), (1, 0), "{report:?}");
    assert_eq!(mid_of(&path), "<0DC386F6-APPLE@tum.de>");
    assert!(crate::draft::parse_email_draft(&path)
        .unwrap()
        .body_markdown
        .contains("Apple text"));
}

/// A draft mp pushed on the other machine arrives whole, under its own id,
/// with a row, and converges: a second reconcile changes nothing.
#[test]
fn a_draft_from_the_other_machine_is_imported_under_its_id() {
    let fx = Fixture::new();
    let mut folder = FakeFolder::default();
    let theirs = {
        // The other machine: its own data dir, the same server folder.
        let other = Fixture::new();
        let path = other.write("fern", "d-remote", "", "Von drüben");
        block_on(push::push(&other.mirror(1), &mut folder, "d-remote")).unwrap();
        std::fs::read(path).unwrap()
    };
    let report = reconciled(&fx, &mut folder);
    assert_eq!(report.imported, 1, "{report:?}");
    let local = find_local("tum", "d-remote").expect("imported under its id");
    assert!(local.draft.body_markdown.contains("Von drüben"));
    assert_eq!(
        local.draft.frontmatter.bcc.as_deref(),
        Some("chef@example.com"),
        "the Bcc comes back from the copy's own header"
    );
    assert_eq!(
        String::from_utf8_lossy(&local.bytes),
        String::from_utf8_lossy(&theirs),
        "byte for byte"
    );
    assert!(fx.row("d-remote").is_some());
    assert_eq!(reconciled(&fx, &mut folder), ReconcileReport::default());
    assert_eq!(push::enqueue_owed(&fx.store, &fx.account).unwrap(), 0);
}

/// A copy whose draft was retired here is deleted, never imported back; a
/// draft removed by hand, missing from two reconciles in a row, keeps its
/// copy's text in `.retired/` before the copy goes.
#[test]
fn a_retired_drafts_copy_is_deleted_and_a_hand_removed_one_is_kept() {
    let fx = Fixture::new();
    let gone = fx.write("gone", "d-gone", "", "weg");
    let removed = fx.write("removed", "d-hand", "", "von Hand");
    let mut folder = FakeFolder::default();
    block_on(push::push(&fx.mirror(1), &mut folder, "d-gone")).unwrap();
    block_on(push::push(&fx.mirror(1), &mut folder, "d-hand")).unwrap();
    crate::draft::retire_draft_files(&gone).unwrap();
    std::fs::remove_file(&removed).unwrap();

    let first = reconciled(&fx, &mut folder);
    assert_eq!((first.deleted, first.imported), (1, 0), "{first:?}");
    assert!(fx.row("d-hand").is_some(), "missed once: kept");
    let report = reconciled(&fx, &mut folder);
    assert_eq!(report.deleted, 1, "{report:?}");
    assert_eq!(report.imported, 0);
    assert!(folder.live().is_empty());
    let kept = crate::config::drafts_dir("tum")
        .join(crate::draft::RETIRED_DIR)
        .join("d-hand.md");
    assert!(std::fs::read_to_string(kept).unwrap().contains("von Hand"));
    assert!(fx.row("d-hand").is_none() && fx.row("d-gone").is_none());
}

/// A push that died between its APPEND and its delete leaves two copies; the
/// newer one is the file, so the reconcile records it and removes the older,
/// with no conflict.
#[test]
fn a_crash_between_append_and_delete_leaves_one_copy_after_the_reconcile() {
    let fx = Fixture::new();
    let path = fx.write("angebot", "d1", "", "eins");
    let mut folder = FakeFolder::default();
    block_on(push::push(&fx.mirror(1), &mut folder, "d1")).unwrap();
    let edited = std::fs::read_to_string(&path)
        .unwrap()
        .replace("eins", "zwei");
    std::fs::write(&path, &edited).unwrap();
    // The APPEND of the edit landed; the delete and the row did not.
    let local = find_local("tum", "d1").unwrap();
    let raw = crate::send::build_server_draft(
        &local.draft,
        "me@tum.de",
        &fx.email,
        &mid_of(&path),
        edited.as_bytes(),
    )
    .unwrap();
    let newer = folder.put(&raw);

    let report = reconciled(&fx, &mut folder);
    assert_eq!(report.conflicts, 0, "{report:?}");
    assert_eq!(folder.live().len(), 1);
    assert_eq!(fx.row("d1").unwrap().uid, newer);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), edited);
}

// ---------------------------------------------------------------------------
// Foreign drafts (#0143 step 6)
// ---------------------------------------------------------------------------

/// A draft started in Outlook, as it saves one: a single `text/html` part in
/// Latin-1 and a PDF beside it.
fn outlook_new_draft() -> Vec<u8> {
    let mut raw = b"Message-ID: <AM0PR-outlook-1@tum.de>\r\nSubject: Termin\r\n\
From: me@tum.de\r\nTo: Robin <robin@example.com>\r\nCc: kim@example.com\r\n\
Bcc: chef@example.com\r\nIn-Reply-To: <parent@example.com>\r\nMIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=\"b1\"\r\n\r\n--b1\r\n\
Content-Type: text/html; charset=iso-8859-1\r\nContent-Transfer-Encoding: 8bit\r\n\r\n\
<html><body><p>Gr\xfc\xdfe, <i>morgen</i>?</p></body></html>\r\n--b1\r\n\
Content-Type: application/pdf; name=\"plan.pdf\"\r\n\
Content-Disposition: attachment; filename=\"plan.pdf\"\r\n\
Content-Transfer-Encoding: base64\r\n\r\nJVBERi0xLjQ=\r\n--b1--\r\n"
        .to_vec();
    raw.shrink_to_fit();
    raw
}

/// A draft started in another client becomes a Markdown file under a fresh
/// id, linked by its own Message-ID, with its recipients, its reply target
/// and its attachment as a file; it is not pushed until it is edited here,
/// and a second reconcile changes nothing.
#[test]
fn a_draft_started_in_outlook_is_imported_as_a_markdown_file() {
    let fx = Fixture::new();
    let mut folder = FakeFolder::default();
    let uid = folder.put(&outlook_new_draft());

    let report = reconciled(&fx, &mut folder);
    assert_eq!(report.imported, 1, "{report:?}");
    let local = local_drafts("tum").pop().expect("one draft");
    let fm = &local.draft.frontmatter;
    assert_eq!(
        fm.server_message_id.as_deref(),
        Some("<AM0PR-outlook-1@tum.de>")
    );
    assert_eq!(fm.to.as_deref(), Some("Robin <robin@example.com>"));
    assert_eq!(fm.cc.as_deref(), Some("kim@example.com"));
    assert_eq!(fm.bcc.as_deref(), Some("chef@example.com"));
    assert_eq!(fm.subject, "Termin");
    assert_eq!(fm.in_reply_to.as_deref(), Some("<parent@example.com>"));
    assert_eq!(fm.status, crate::types::EmailStatus::Draft);
    assert!(
        local.draft.body_markdown.contains("Grüße, *morgen*?"),
        "{}",
        local.draft.body_markdown
    );
    let attachments = fm.attachments.clone().unwrap_or_default();
    assert_eq!(attachments.len(), 1, "{attachments:?}");
    assert!(attachments[0].ends_with("plan.pdf") && std::path::Path::new(&attachments[0]).exists());
    assert_eq!(fx.row(&local.id).unwrap().uid, uid);
    assert_eq!(
        push::enqueue_owed(&fx.store, &fx.account).unwrap(),
        0,
        "not pushed yet"
    );
    assert_eq!(reconciled(&fx, &mut folder), ReconcileReport::default());

    // The first edit here replaces Outlook's copy with mp's rendering.
    let edited = std::fs::read_to_string(&local.path)
        .unwrap()
        .replace("morgen", "übermorgen");
    std::fs::write(&local.path, edited).unwrap();
    assert_eq!(push::enqueue_owed(&fx.store, &fx.account).unwrap(), 1);
    block_on(push::push(&fx.mirror(2), &mut folder, &local.id)).unwrap();
    assert_eq!(folder.live().len(), 1);
    assert!(String::from_utf8_lossy(&folder.live()[0].raw).contains("mp:copy v2"));
}

/// A reply started in Apple Mail keeps its quoted thread below a quote
/// marker, so a later pull of an edit keeps the thread as the file has it.
#[test]
fn a_foreign_reply_keeps_its_quote_below_a_marker() {
    let fx = Fixture::new();
    let mut folder = FakeFolder::default();
    folder.put(
        b"Message-ID: <APPLE-2@icloud.com>\r\nSubject: Re: Plan\r\nTo: robin@example.com\r\n\
MIME-Version: 1.0\r\nContent-Type: text/html; charset=utf-8\r\n\r\n\
<html><body><div>Klingt gut.</div><blockquote type=\"cite\"><div>Der Plan</div></blockquote>\
</body></html>\r\n",
    );
    reconciled(&fx, &mut folder);
    let local = local_drafts("tum").pop().expect("imported");
    let body = &local.draft.body_markdown;
    let marker = body
        .find(crate::draft::QUOTE_MARKER)
        .expect("a quote marker");
    assert!(body[..marker].contains("Klingt gut."), "{body}");
    assert!(body[marker..].contains("Der Plan"), "{body}");
}

/// A local edit pushed before the reconcile has seen an Outlook edit does not
/// bury it: the push reconciles instead, so the draft takes Outlook's text
/// and the local text is a conflict copy, and only then is a push owed.
#[test]
fn a_push_over_an_unseen_outlook_edit_reconciles_first() {
    let fx = Fixture::new();
    let path = fx.write("angebot", "d1", "", "Original");
    let mut folder = FakeFolder::default();
    block_on(push::push(&fx.mirror(1), &mut folder, "d1")).unwrap();
    let mid = mid_of(&path);
    folder.remove(folder.live()[0].uid);
    folder.put(&outlook_copy(
        &mid,
        "robin@example.com",
        "Angebot",
        "<p>Outlook text</p>",
    ));
    let local = std::fs::read_to_string(&path)
        .unwrap()
        .replace("Original", "Local text");
    std::fs::write(&path, local).unwrap();

    block_on(push::push(&fx.mirror(2), &mut folder, "d1")).unwrap();
    assert!(crate::draft::parse_email_draft(&path)
        .unwrap()
        .body_markdown
        .contains("Outlook text"));
    let copy = path.with_file_name("angebot.conflict.md");
    assert!(std::fs::read_to_string(copy)
        .unwrap()
        .contains("Local text"));
    assert_eq!(folder.live().len(), 1, "Outlook's copy is the draft's now");
}

/// An older copy edited elsewhere that a newer copy superseded is kept as a
/// conflict copy before it goes, never deleted unread.
#[test]
fn an_older_foreign_copy_is_kept_as_a_conflict_copy() {
    let fx = Fixture::new();
    let path = fx.write("angebot", "d1", "", "eins");
    let mut folder = FakeFolder::default();
    block_on(push::push(&fx.mirror(1), &mut folder, "d1")).unwrap();
    let mid = mid_of(&path);
    let ours = folder.live()[0].uid;
    // A foreign copy older than ours, which only a race can leave.
    folder.copies.insert(
        0,
        super::folder::fake::Copy {
            uid: ours - 50,
            raw: outlook_copy(&mid, "robin@example.com", "Angebot", "<p>Fremd</p>"),
            deleted: false,
        },
    );
    let report = reconciled(&fx, &mut folder);
    assert_eq!((report.conflicts, report.deleted), (1, 1), "{report:?}");
    let copy = path.with_file_name("angebot.conflict.md");
    assert!(std::fs::read_to_string(copy).unwrap().contains("Fremd"));
    assert_eq!(folder.live().len(), 1);
}

// ---------------------------------------------------------------------------
// Review findings of the mirror (#0143)
// ---------------------------------------------------------------------------

/// One machine of a pair sharing a server folder: its own data dir, store and
/// drafts directory, entered for the length of a guard.
struct Machine {
    store: Store,
    account: AccountConfig,
    email: EmailSettings,
    dir: tempfile::TempDir,
}

impl Machine {
    fn new() -> Machine {
        let dir = tempfile::tempdir().unwrap();
        let _guard = crate::config::test_env::DataDirOverride::set(dir.path());
        std::fs::create_dir_all(crate::config::drafts_dir("tum")).unwrap();
        let store = Store::open(crate::config::store_path("tum")).unwrap();
        Machine {
            store,
            account: AccountConfig {
                name: "tum".to_string(),
                default_from: "me@tum.de".to_string(),
                ..Default::default()
            },
            email: EmailSettings::default(),
            dir,
        }
    }

    fn enter(&self) -> crate::config::test_env::DataDirOverride {
        crate::config::test_env::DataDirOverride::set(self.dir.path())
    }

    fn mirror(&self) -> Mirror<'_> {
        Mirror {
            store: &self.store,
            account: &self.account,
            email: &self.email,
            now: 9000,
        }
    }

    fn reconcile(&self, folder: &mut FakeFolder) -> ReconcileReport {
        let _guard = self.enter();
        block_on(reconcile(&self.mirror(), folder)).unwrap()
    }

    /// Queue every owed push and drain the queue against `folder`.
    fn drain(&self, folder: &mut FakeFolder) {
        let _guard = self.enter();
        push::enqueue_owed(&self.store, &self.account).unwrap();
        let blobs = BlobStore::for_account("tum");
        let mut exec = FakeExecutor {
            folder,
            account: self.account.clone(),
            email: EmailSettings::default(),
        };
        block_on(pending_ops::drain(
            &self.store,
            &blobs,
            "tum",
            &mut exec,
            crate::outbox::unix_now(),
        ))
        .unwrap();
    }

    fn has(&self, id: &str) -> bool {
        let _guard = self.enter();
        find_local("tum", id).is_some()
    }
}

/// The server copy mp would write of the draft file `text`, under `mid`.
fn server_copy_of(text: &str, mid: &str) -> Vec<u8> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("copy.md");
    std::fs::write(&path, text).unwrap();
    let draft = crate::draft::parse_email_draft(&path).unwrap();
    crate::send::build_server_draft(
        &draft,
        "me@tum.de",
        &EmailSettings::default(),
        mid,
        text.as_bytes(),
    )
    .unwrap()
}

/// A draft whose frontmatter is half typed on one machine is still that
/// machine's draft: its copy is not deleted as if removed by hand, the other
/// machine does not retire it, and once the YAML is whole again both
/// machines hold it and the server one copy (the cascade of review finding
/// B1).
#[test]
fn a_half_typed_frontmatter_never_cascades_into_a_retire_on_both_machines() {
    let mut folder = FakeFolder::default();
    let m1 = Machine::new();
    let m2 = Machine::new();
    let path = {
        let _guard = m1.enter();
        let path = Fixture::write_in_current("angebot", "d1", "", "Hallo");
        block_on(push::push(&m1.mirror(), &mut folder, "d1")).unwrap();
        path
    };
    assert_eq!(m2.reconcile(&mut folder).imported, 1);

    let whole = std::fs::read_to_string(&path).unwrap();
    let half = whole.replace("to: robin@example.com", "to: [robin@example.com");
    std::fs::write(&path, &half).unwrap();
    for _ in 0..3 {
        m1.reconcile(&mut folder);
        m1.drain(&mut folder);
        m2.reconcile(&mut folder);
    }
    std::fs::write(&path, &whole).unwrap();
    for _ in 0..3 {
        m1.drain(&mut folder);
        m1.reconcile(&mut folder);
        m2.reconcile(&mut folder);
    }

    assert!(
        m1.has("d1"),
        "the draft is still in drafts/ on the first machine"
    );
    assert!(m2.has("d1"), "and on the second");
    assert_eq!(folder.live().len(), 1, "one copy: {:?}", folder.log);
}

/// A copy one listing did not show is not gone: a draft is retired only when
/// two reconciles in a row under one UIDVALIDITY miss its copy.
#[test]
fn a_copy_missing_from_one_listing_does_not_retire_its_draft() {
    let fx = Fixture::new();
    let path = fx.write("angebot", "d1", "", "eins");
    let mut folder = FakeFolder::default();
    block_on(push::push(&fx.mirror(1), &mut folder, "d1")).unwrap();
    let uid = folder.live()[0].uid;

    folder.hidden = vec![uid];
    assert_eq!(reconciled(&fx, &mut folder).retired, 0);
    assert!(
        path.exists(),
        "one listing without the copy retires nothing"
    );
    folder.hidden.clear();
    assert_eq!(reconciled(&fx, &mut folder), ReconcileReport::default());

    folder.hidden = vec![uid];
    assert_eq!(
        reconciled(&fx, &mut folder).retired,
        0,
        "the count restarted"
    );
    assert_eq!(
        reconciled(&fx, &mut folder).retired,
        1,
        "missed twice in a row"
    );
    assert!(!path.exists());
}

/// A draft retired because its copy went is no tombstone: when a copy under
/// its `Message-ID` comes back (the other machine pushed it again), the copy
/// is not deleted, and the draft returns to `drafts/` with the copy's text.
#[test]
fn a_returning_copy_brings_back_a_draft_retired_for_its_absence() {
    let fx = Fixture::new();
    let path = fx.write("angebot", "d1", "", "eins");
    let mut folder = FakeFolder::default();
    block_on(push::push(&fx.mirror(1), &mut folder, "d1")).unwrap();
    let mid = mid_of(&path);
    let text = std::fs::read_to_string(&path).unwrap();
    folder.remove(folder.live()[0].uid);
    reconciled(&fx, &mut folder);
    reconciled(&fx, &mut folder);
    assert!(
        !path.exists(),
        "retired after two reconciles without its copy"
    );

    folder.put(&server_copy_of(&text.replace("eins", "zwei"), &mid));
    let report = reconciled(&fx, &mut folder);
    assert_eq!(folder.live().len(), 1, "the copy stays: {:?}", folder.log);
    assert_eq!(report.deleted, 0, "{report:?}");
    let back = find_local("tum", "d1").expect("the draft is back in drafts/");
    assert!(back.draft.body_markdown.contains("zwei"));
    assert_eq!(fx.row("d1").unwrap().uid, folder.live()[0].uid);
}

/// The listing after every sync carries only the headers a copy is linked
/// by, never the draft file a copy carries in its source headers, which only
/// a fetch of the one copy a pull or an import reads brings.
#[test]
fn the_listing_carries_no_draft_source() {
    let fx = Fixture::new();
    fx.write("angebot", "d1", "", &"lang ".repeat(2000));
    let mut folder = FakeFolder::default();
    block_on(push::push(&fx.mirror(1), &mut folder, "d1")).unwrap();
    let uid = folder.live()[0].uid;
    let heads = block_on(folder.heads()).unwrap();
    let header = &heads[0].header;
    assert!(header_value(header, "Message-ID").is_some());
    assert!(header_value(header, crate::send::DRAFT_ID_HEADER).is_some());
    assert!(header_value(header, "Subject").is_some());
    assert!(
        header_value(header, super::carrier::SOURCE_HEADER).is_none(),
        "{} bytes of header",
        header.len()
    );
    assert!(header.len() < 1024, "{} bytes of header", header.len());
    let raw = block_on(folder.fetch(uid)).unwrap().unwrap();
    assert!(header_value(&raw, super::carrier::SOURCE_HEADER).is_some());
}

/// A second draft of one file name retired because its copy went is no
/// tombstone either: its retired name counts after the suffix, so a copy that
/// comes back still brings it back instead of being deleted.
#[test]
fn a_second_copy_gone_retirement_of_one_name_still_comes_back() {
    let fx = Fixture::new();
    let retired = crate::config::drafts_dir("tum").join(crate::draft::RETIRED_DIR);
    std::fs::create_dir_all(&retired).unwrap();
    std::fs::write(
        retired.join("angebot.copy-gone.md"),
        "---\nid: \"d0\"\nto: robin@example.com\nsubject: Alt\nstatus: draft\n---\n\nalt\n",
    )
    .unwrap();
    let path = fx.write("angebot", "d1", "", "eins");
    let mut folder = FakeFolder::default();
    block_on(push::push(&fx.mirror(1), &mut folder, "d1")).unwrap();
    let mid = mid_of(&path);
    let text = std::fs::read_to_string(&path).unwrap();
    folder.remove(folder.live()[0].uid);
    reconciled(&fx, &mut folder);
    reconciled(&fx, &mut folder);
    assert!(!path.exists(), "retired after two reconciles");
    assert!(retired.join("angebot.copy-gone-2.md").exists());

    folder.put(&server_copy_of(&text.replace("eins", "zwei"), &mid));
    let report = reconciled(&fx, &mut folder);
    assert_eq!(report.deleted, 0, "{report:?} {:?}", folder.log);
    assert_eq!(folder.live().len(), 1, "the copy stays");
    let back = find_local("tum", "d1").expect("the draft is back in drafts/");
    assert_eq!(back.path, path, "under its old name");
    assert!(back.draft.body_markdown.contains("zwei"));
}

/// A save that lands while the reconcile waits on the server is never
/// overwritten by the pull it was racing: the pull skips the draft, and the
/// next reconcile sees both sides changed and keeps the save in a conflict
/// copy (review finding B2).
#[test]
fn a_save_during_the_fetch_is_never_overwritten_by_a_pull() {
    let fx = Fixture::new();
    let path = fx.write("angebot", "d1", "", "Original");
    let mut folder = FakeFolder::default();
    block_on(push::push(&fx.mirror(1), &mut folder, "d1")).unwrap();
    let mid = mid_of(&path);
    folder.remove(folder.live()[0].uid);
    folder.put(&outlook_copy(
        &mid,
        "robin@example.com",
        "Angebot",
        "<p>Server text</p>",
    ));
    folder.touch_on_fetch = Some((path.clone(), "\nTyped while the mirror waited\n".into()));

    reconciled(&fx, &mut folder);
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains("Typed while the mirror waited"), "{text}");

    let report = reconciled(&fx, &mut folder);
    assert_eq!(report.conflicts, 1, "{report:?}");
    let copy = std::fs::read_to_string(path.with_file_name("angebot.conflict.md")).unwrap();
    assert!(copy.contains("Typed while the mirror waited"), "{copy}");
    assert!(std::fs::read_to_string(&path)
        .unwrap()
        .contains("Server text"));
}

/// A reply quotes the original's HTML verbatim, so a sender can plant what
/// looks like mp's own carrier in it. The other machine reads the draft the
/// user wrote, never the planted one (review finding B3).
#[test]
fn a_carrier_planted_in_a_quoted_message_is_never_read_as_the_draft() {
    use base64::Engine;
    let mut folder = FakeFolder::default();
    let m1 = Machine::new();
    let m2 = Machine::new();
    let crafted = "---\nid: \"d-evil\"\nto: attacker@evil.example\nsubject: x\n\
                   status: approved\nattachments:\n  - /etc/passwd\n---\n\nhi\n";
    let planted = format!(
        "<p>Hello</p><!-- mp:source v1 {} --><!-- mp:copy v2 file=00 src=00 -->",
        base64::engine::general_purpose::STANDARD.encode(crafted)
    );
    {
        let _guard = m1.enter();
        let path = Fixture::write_in_current(
            "re",
            "d1",
            "",
            "Danke!\n\n<!-- mp:quote -->\n\n> Robin wrote:\n> Hello",
        );
        std::fs::write(path.with_extension("html"), &planted).unwrap();
        block_on(push::push(&m1.mirror(), &mut folder, "d1")).unwrap();
    }
    m2.reconcile(&mut folder);
    assert!(!m2.has("d-evil"), "the planted draft is not imported");
    let _guard = m2.enter();
    for local in local_drafts("tum") {
        let attachments = local.draft.frontmatter.attachments.unwrap_or_default();
        assert!(
            !attachments.iter().any(|a| a.contains("passwd")),
            "{attachments:?}"
        );
        assert_ne!(
            local.draft.frontmatter.to.as_deref(),
            Some("attacker@evil.example")
        );
    }
}

/// Sent unedited from Outlook, Apple Mail or the Proton app, a server copy
/// reaches its recipients as it is: no part of it may carry the draft file,
/// and nothing mp carries for itself may carry the Bcc (review finding B4).
#[test]
fn no_part_of_a_server_copy_carries_the_draft_file_or_its_bcc() {
    use base64::Engine;
    let fx = Fixture::new();
    let path = fx.write("angebot", "d1", "", "Hallo");
    let mut folder = FakeFolder::default();
    block_on(push::push(&fx.mirror(1), &mut folder, "d1")).unwrap();
    let raw = folder.live()[0].raw.clone();
    let file = std::fs::read(&path).unwrap();
    let email = crate::parse::parse_rfc822_to_fetched_email(&raw).unwrap();
    let html = email.html_body.clone().unwrap_or_default();
    let encoded = base64::engine::general_purpose::STANDARD.encode(&file);
    assert!(!html.contains(&encoded[..24]), "{html}");
    assert!(!html.contains("chef@example.com"), "{html}");
    assert!(!email.body_text.contains("chef@example.com"));

    let (headers, _) = mailparse::parse_headers(&raw).unwrap();
    for header in headers.iter().filter(|h| {
        h.get_key()
            .to_ascii_lowercase()
            .starts_with("x-mailypoppins")
    }) {
        let value = header.get_value();
        let payload = value.split_whitespace().last().unwrap_or_default();
        if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(payload) {
            assert!(
                !String::from_utf8_lossy(&bytes).contains("chef@example.com"),
                "{} carries the Bcc",
                header.get_key()
            );
        }
    }
}

/// Another machine's push over a draft this one has not touched since is a
/// plain pull, with no conflict copy (review finding N1).
#[test]
fn another_machine_pushing_over_an_unchanged_draft_is_a_plain_pull() {
    let fx = Fixture::new();
    let path = fx.write("angebot", "d1", "", "eins");
    let mut folder = FakeFolder::default();
    block_on(push::push(&fx.mirror(1), &mut folder, "d1")).unwrap();
    let mid = mid_of(&path);
    let theirs = std::fs::read_to_string(&path)
        .unwrap()
        .replace("eins", "zwei vom anderen Rechner");
    folder.put(&server_copy_of(&theirs, &mid));

    let report = reconciled(&fx, &mut folder);
    assert_eq!((report.pulled, report.conflicts), (1, 0), "{report:?}");
    assert!(std::fs::read_to_string(&path)
        .unwrap()
        .contains("zwei vom anderen Rechner"));
    assert_eq!(folder.live().len(), 1);
}

/// A rebuilt store loses the row of a draft imported from Outlook; the
/// reconcile renders the copy as a pull would, finds the file, and records
/// the row rather than writing a conflict copy (review finding N2).
#[test]
fn a_lost_row_of_an_imported_foreign_draft_is_recovered_without_a_conflict() {
    let fx = Fixture::new();
    let mut folder = FakeFolder::default();
    folder.put(&outlook_new_draft());
    assert_eq!(reconciled(&fx, &mut folder).imported, 1);
    let local = local_drafts("tum").pop().unwrap();
    draft_sync::delete(&fx.store, "tum", &local.id).unwrap();

    let report = reconciled(&fx, &mut folder);
    assert_eq!((report.recovered, report.conflicts), (1, 0), "{report:?}");
    assert!(fx.row(&local.id).is_some());
    assert_eq!(local_drafts("tum").len(), 1, "no conflict copy");
    assert_eq!(push::enqueue_owed(&fx.store, &fx.account).unwrap(), 0);
}

/// A pull waits while an undo-send hold is armed for the draft, as it waits
/// for an open outbox row, and lands once the hold is gone.
#[test]
fn a_pull_waits_for_an_armed_undo_send_hold() {
    let fx = Fixture::new();
    let path = fx.write("angebot", "d-held", "", "Original");
    let mut folder = FakeFolder::default();
    block_on(push::push(&fx.mirror(1), &mut folder, "d-held")).unwrap();
    let mid = mid_of(&path);
    folder.remove(folder.live()[0].uid);
    folder.put(&outlook_copy(
        &mid,
        "robin@example.com",
        "Angebot",
        "<p>Server text</p>",
    ));

    super::holds::arm("tum", Some("d-held"));
    let held = reconciled(&fx, &mut folder);
    super::holds::release("tum", Some("d-held"));
    assert_eq!(held.pulled, 0, "{held:?}");
    assert!(std::fs::read_to_string(&path).unwrap().contains("Original"));

    assert_eq!(reconciled(&fx, &mut folder).pulled, 1);
    assert!(std::fs::read_to_string(&path)
        .unwrap()
        .contains("Server text"));
}

/// `raw` as Proton Bridge stores an APPEND: without mp's own headers.
fn without_mp_headers(raw: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(raw);
    let (head, body) = text.split_once("\r\n\r\n").unwrap();
    let mut kept: Vec<&str> = Vec::new();
    let mut dropping = false;
    for line in head.split("\r\n") {
        if line.starts_with(' ') || line.starts_with('\t') {
            if !dropping {
                kept.push(line);
            }
            continue;
        }
        dropping = line.to_ascii_lowercase().starts_with("x-mailypoppins");
        if !dropping {
            kept.push(line);
        }
    }
    format!("{}\r\n\r\n{body}", kept.join("\r\n")).into_bytes()
}

/// A copy whose server dropped mp's headers, as Proton Bridge does, still
/// carries its mark: a lost row is recovered without a conflict, and the
/// other machine imports the draft through its HTML, Bcc included.
#[test]
fn a_copy_without_mps_headers_still_recovers_and_imports() {
    let mut folder = FakeFolder::default();
    let m1 = Machine::new();
    let m2 = Machine::new();
    {
        let _guard = m1.enter();
        Fixture::write_in_current("angebot", "d1", "", "Hallo Robin");
        block_on(push::push(&m1.mirror(), &mut folder, "d1")).unwrap();
        draft_sync::delete(&m1.store, "tum", "d1").unwrap();
    }
    for copy in &mut folder.copies {
        copy.raw = without_mp_headers(&copy.raw);
    }
    assert!(!String::from_utf8_lossy(&folder.copies[0].raw).contains("X-Mailypoppins"));

    let report = m1.reconcile(&mut folder);
    assert_eq!((report.recovered, report.conflicts), (1, 0), "{report:?}");

    assert_eq!(m2.reconcile(&mut folder).imported, 1);
    let _guard = m2.enter();
    let local = local_drafts("tum").pop().unwrap();
    assert!(local.draft.body_markdown.contains("Hallo Robin"));
    assert_eq!(
        local.draft.frontmatter.bcc.as_deref(),
        Some("chef@example.com")
    );
}

/// A copy whose carried draft names another `Message-ID` than the copy's own
/// is not mp's: it is imported as a foreign draft under a fresh id.
#[test]
fn a_carried_draft_naming_another_message_id_is_read_as_foreign() {
    let fx = Fixture::new();
    let mut folder = FakeFolder::default();
    let text = "---\nid: \"d-evil\"\nto: a@example.com\nsubject: x\nstatus: draft\n\
                server_message_id: \"<other@example.com>\"\n---\n\nhi\n";
    folder.put(&server_copy_of(text, "<mine@example.com>"));
    assert_eq!(reconciled(&fx, &mut folder).imported, 1);
    assert!(find_local("tum", "d-evil").is_none());
    let local = local_drafts("tum").pop().unwrap();
    assert_eq!(
        local.draft.frontmatter.server_message_id.as_deref(),
        Some("<mine@example.com>")
    );
}

// ---------------------------------------------------------------------------
// Approval travels (Sylvain, 2026-10-23)
// ---------------------------------------------------------------------------

fn approve(path: &std::path::Path) {
    let text = std::fs::read_to_string(path)
        .unwrap()
        .replace("status: draft", "status: approved");
    std::fs::write(path, text).unwrap();
}

fn status_of(machine: &Machine, id: &str) -> crate::types::EmailStatus {
    let _guard = machine.enter();
    find_local("tum", id)
        .unwrap_or_else(|| panic!("{id} is here"))
        .draft
        .frontmatter
        .status
}

/// A draft approved on one machine is approved on the other: through the
/// import of a copy it has never seen, and through the pull of one it holds
/// as a draft.
#[test]
fn an_approval_made_on_one_machine_is_approved_on_the_other() {
    use crate::types::EmailStatus;
    let mut folder = FakeFolder::default();
    let m1 = Machine::new();
    let m2 = Machine::new();
    let path = {
        let _guard = m1.enter();
        let early = Fixture::write_in_current("early", "d-early", "", "Schon fertig");
        approve(&early);
        Fixture::write_in_current("angebot", "d1", "", "Hallo Robin")
    };
    m1.drain(&mut folder);
    assert_eq!(m2.reconcile(&mut folder).imported, 2);
    assert_eq!(
        status_of(&m2, "d-early"),
        EmailStatus::Approved,
        "imported approved"
    );
    assert_eq!(status_of(&m2, "d1"), EmailStatus::Draft);

    {
        let _guard = m1.enter();
        approve(&path);
    }
    m1.drain(&mut folder);
    assert_eq!(m2.reconcile(&mut folder).pulled, 1);
    assert_eq!(
        status_of(&m2, "d1"),
        EmailStatus::Approved,
        "pulled approved"
    );
    m2.drain(&mut folder);
    assert_eq!(folder.live().len(), 2, "no echo push: {:?}", folder.log);
}

/// An edit in another client of a copy whose approval travelled demotes the
/// draft on both machines: nobody approved the text Outlook wrote.
#[test]
fn an_edit_in_another_client_demotes_a_travelled_approval() {
    use crate::types::EmailStatus;
    let mut folder = FakeFolder::default();
    let m1 = Machine::new();
    let m2 = Machine::new();
    let mid = {
        let _guard = m1.enter();
        let path = Fixture::write_in_current("angebot", "d1", "", "Hallo Robin");
        approve(&path);
        path
    };
    m1.drain(&mut folder);
    m2.reconcile(&mut folder);
    assert_eq!(status_of(&m2, "d1"), EmailStatus::Approved);

    let mid = {
        let _guard = m1.enter();
        mid_of(&mid)
    };
    let uid = folder.live()[0].uid;
    folder.remove(uid);
    folder.put(&outlook_copy(
        &mid,
        "robin@example.com",
        "Angebot",
        "<p>Hallo Robin, in Outlook umgeschrieben</p>",
    ));
    assert_eq!(m1.reconcile(&mut folder).pulled, 1);
    assert_eq!(m2.reconcile(&mut folder).pulled, 1);
    assert_eq!(
        status_of(&m1, "d1"),
        EmailStatus::Draft,
        "demoted where approved"
    );
    assert_eq!(
        status_of(&m2, "d1"),
        EmailStatus::Draft,
        "demoted where it travelled"
    );
}

/// Approval comes only from a copy mp pushed and nobody edited since: a
/// status header planted on a foreign copy, mp's mark without the carried
/// file, or carrier headers on a copy without mp's mark all import a draft.
#[test]
fn a_planted_approval_on_a_foreign_copy_is_ignored() {
    use crate::types::EmailStatus;
    use base64::Engine;
    let fx = Fixture::new();
    let mut folder = FakeFolder::default();
    let approved = "---\nid: \"d-planted\"\nto: robin@example.com\nsubject: Termin\n\
                    status: approved\nserver_message_id: \"<planted-3@tum.de>\"\n---\n\nja\n";
    let foreign = |mid: &str, extra_headers: &str, html: &str| -> Vec<u8> {
        format!(
            "Message-ID: <{mid}>\r\nSubject: Termin\r\nFrom: me@tum.de\r\n\
             To: robin@example.com\r\n{extra_headers}MIME-Version: 1.0\r\n\
             Content-Type: text/html; charset=utf-8\r\n\r\n<html><body>{html}</body></html>\r\n"
        )
        .into_bytes()
    };
    // A status header of mp's name on a copy Outlook wrote.
    folder.put(&foreign(
        "planted-1@tum.de",
        "X-Mailypoppins-Draft-Status: approved\r\n",
        "<p>ja</p>",
    ));
    // mp's mark, naming an approved file, with no file carried.
    folder.put(&foreign(
        "planted-2@tum.de",
        "",
        &format!(
            "<p>ja</p>{}",
            crate::parse::draft_copy_comment(&file_hash(approved.as_bytes()), None)
        ),
    ));
    // The carried file of an approved draft, on a copy without mp's mark.
    let encoded = base64::engine::general_purpose::STANDARD.encode(approved);
    folder.put(&foreign(
        "planted-3@tum.de",
        &format!("{}: 1/1 {encoded}\r\n", super::carrier::SOURCE_HEADER),
        "<p>ja</p>",
    ));

    assert_eq!(reconciled(&fx, &mut folder).imported, 3);
    assert!(
        find_local("tum", "d-planted").is_none(),
        "not under the planted id"
    );
    for local in local_drafts("tum") {
        assert_eq!(
            local.draft.frontmatter.status,
            EmailStatus::Draft,
            "{}",
            local.path.display()
        );
    }
}

/// Proton Bridge drops mp's headers at the APPEND, so the other machine reads
/// the copy through its HTML: the draft arrives, and its approval does not.
#[test]
fn on_proton_an_approval_does_not_travel() {
    use crate::types::EmailStatus;
    let mut folder = FakeFolder::default();
    let m1 = Machine::new();
    let m2 = Machine::new();
    let path = {
        let _guard = m1.enter();
        Fixture::write_in_current("angebot", "d1", "", "Hallo Robin")
    };
    m1.drain(&mut folder);
    let bridged = |folder: &mut FakeFolder| {
        for copy in &mut folder.copies {
            copy.raw = without_mp_headers(&copy.raw);
        }
    };
    bridged(&mut folder);
    assert_eq!(m2.reconcile(&mut folder).imported, 1);
    let theirs = {
        let _guard = m2.enter();
        local_drafts("tum").pop().unwrap().id
    };

    {
        let _guard = m1.enter();
        approve(&path);
    }
    m1.drain(&mut folder);
    bridged(&mut folder);
    let report = m2.reconcile(&mut folder);
    assert_eq!(report.pulled, 1, "{report:?}");
    assert_eq!(status_of(&m2, &theirs), EmailStatus::Draft);
    assert_eq!(
        status_of(&m1, "d1"),
        EmailStatus::Approved,
        "still approved where it was"
    );
}

/// An attachment the copy does not carry, a file missing when the other
/// machine pushed, means the draft here is not the one approved there; one
/// the copy carries, written here under the stable attachments directory,
/// keeps the approval.
#[test]
fn an_approval_travels_only_with_every_attachment() {
    use crate::types::EmailStatus;
    let mut folder = FakeFolder::default();
    let m1 = Machine::new();
    let m2 = Machine::new();
    let carried = m1.dir.path().join("plan.pdf");
    std::fs::write(&carried, b"%PDF-1.4 plan").unwrap();
    {
        let _guard = m1.enter();
        let with = Fixture::write_in_current(
            "with",
            "d-with",
            &format!("attachments:\n  - {}\n", carried.display()),
            "Anbei der Plan",
        );
        approve(&with);
        let without = Fixture::write_in_current(
            "without",
            "d-without",
            "attachments:\n  - /nonexistent/mp-test/gone.pdf\n",
            "Anbei nichts",
        );
        approve(&without);
    }
    m1.drain(&mut folder);
    // The other machine has none of the first machine's files.
    std::fs::remove_file(&carried).unwrap();

    assert_eq!(m2.reconcile(&mut folder).imported, 2);
    assert_eq!(status_of(&m2, "d-with"), EmailStatus::Approved);
    assert_eq!(status_of(&m2, "d-without"), EmailStatus::Draft);
}

/// An attachment entry that resolves here to a file whose bytes are not the
/// ones the copy carries under that name is not what was approved there: the
/// draft lands as a draft. The same name with the same bytes keeps the
/// approval.
#[test]
fn an_approval_travels_only_when_the_local_attachment_bytes_match() {
    use crate::types::EmailStatus;
    let mut folder = FakeFolder::default();
    let m1 = Machine::new();
    let m2 = Machine::new();
    let shared = tempfile::tempdir().unwrap();
    let same = shared.path().join("same.pdf");
    let differs = shared.path().join("plan.pdf");
    std::fs::write(&same, b"%PDF-1.4 same").unwrap();
    std::fs::write(&differs, b"%PDF-1.4 approved plan").unwrap();
    {
        let _guard = m1.enter();
        let a = Fixture::write_in_current(
            "same",
            "d-same",
            &format!("attachments:\n  - {}\n", same.display()),
            "Anbei",
        );
        approve(&a);
        let b = Fixture::write_in_current(
            "differs",
            "d-differs",
            &format!("attachments:\n  - {}\n", differs.display()),
            "Anbei der Plan",
        );
        approve(&b);
    }
    m1.drain(&mut folder);
    // The path resolves on the other machine too, to another file.
    std::fs::write(&differs, b"%PDF-1.4 a plan nobody approved").unwrap();

    assert_eq!(m2.reconcile(&mut folder).imported, 2);
    assert_eq!(status_of(&m2, "d-same"), EmailStatus::Approved);
    assert_eq!(status_of(&m2, "d-differs"), EmailStatus::Draft);
}

// ---------------------------------------------------------------------------
// The send guard: one draft, two machines, one send
// ---------------------------------------------------------------------------

fn guard(
    store: &Store,
    account: &AccountConfig,
    folder: &mut FakeFolder,
    id: &str,
) -> Result<(), String> {
    let draft = find_local("tum", id).unwrap().draft;
    block_on(send_guard::claim_for_send(store, account, folder, &draft))
        .map_err(|e| format!("{e:#}"))
}

/// The approved draft on both machines: the first send claims the server
/// copy and goes ahead, the second finds it gone and is refused, so the draft
/// is sent once.
#[test]
fn two_machines_sending_one_approved_draft_send_it_once() {
    use crate::types::EmailStatus;
    let mut folder = FakeFolder::default();
    let m1 = Machine::new();
    let m2 = Machine::new();
    {
        let _guard = m1.enter();
        let path = Fixture::write_in_current("angebot", "d1", "", "Hallo Robin");
        approve(&path);
    }
    m1.drain(&mut folder);
    assert_eq!(m2.reconcile(&mut folder).imported, 1);
    assert_eq!(status_of(&m2, "d1"), EmailStatus::Approved);

    {
        let _guard = m1.enter();
        guard(&m1.store, &m1.account, &mut folder, "d1").expect("the first send goes ahead");
        assert!(
            draft_sync::get(&m1.store, "tum", "d1").unwrap().is_none(),
            "row forgotten"
        );
    }
    assert!(
        folder.live().is_empty(),
        "the copy is claimed: {:?}",
        folder.log
    );

    let _guard = m2.enter();
    let refused = guard(&m2.store, &m2.account, &mut folder, "d1").unwrap_err();
    assert!(
        refused.contains("sent or discarded on another machine"),
        "{refused}"
    );
    assert!(
        find_local("tum", "d1").is_some(),
        "the file is the reconcile's to retire"
    );
    assert!(
        draft_sync::get(&m2.store, "tum", "d1").unwrap().is_some(),
        "the row stays, so the reconcile counts the missing copy"
    );
}

/// A copy another session expunged while this one claimed it may be the other
/// machine sending or another client expunging the copy this one flagged: the
/// send is refused with its outcome unknown, and the draft is neither retired
/// nor refused again as sent elsewhere.
#[test]
fn a_copy_claimed_by_another_session_refuses_the_send_with_its_outcome_unknown() {
    let fx = Fixture::new();
    let path = fx.write("angebot", "d1", "", "eins");
    let mut folder = FakeFolder::default();
    block_on(push::push(&fx.mirror(1), &mut folder, "d1")).unwrap();
    folder.claimed_elsewhere = true;

    let refused = guard(&fx.store, &fx.account, &mut folder, "d1").unwrap_err();
    assert!(refused.contains("another session removed"), "{refused}");
    assert!(refused.contains("outcome unknown"), "{refused}");
    assert!(refused.contains("check Sent and Drafts"), "{refused}");
    assert!(fx.row("d1").is_none());
    for _ in 0..2 {
        let report = reconciled(&fx, &mut folder);
        assert_eq!(report.retired, 0, "{report:?}");
    }
    assert!(path.exists(), "never retired as copy-gone");
    guard(&fx.store, &fx.account, &mut folder, "d1").expect("the user's retry goes ahead");
}

/// A connection that drops once the claim has removed the copy leaves a draft
/// a retry sends, that is never retired as sent elsewhere, and that the
/// owed-push scan puts back in Drafts.
#[test]
fn a_claim_that_fails_after_its_store_leaves_a_draft_a_retry_sends() {
    let fx = Fixture::new();
    let path = fx.write("angebot", "d1", "", "eins");
    let mut folder = FakeFolder::default();
    block_on(push::push(&fx.mirror(1), &mut folder, "d1")).unwrap();
    folder.fail_claim = true;

    let refused = guard(&fx.store, &fx.account, &mut folder, "d1").unwrap_err();
    assert!(refused.contains("outcome unknown"), "{refused}");
    assert!(refused.contains("send again or discard"), "{refused}");
    assert!(folder.live().is_empty(), "the copy went with the claim");
    folder.fail_claim = false;

    let retry = guard(&fx.store, &fx.account, &mut folder, "d1");
    assert!(
        retry.is_ok(),
        "a retry is not refused as sent elsewhere: {retry:?}"
    );
    for _ in 0..2 {
        let report = reconciled(&fx, &mut folder);
        assert_eq!(report.retired, 0, "{report:?}");
    }
    assert!(path.exists(), "never retired as copy-gone");
    assert_eq!(push::enqueue_owed(&fx.store, &fx.account).unwrap(), 1);
    block_on(push::push(&fx.mirror(2), &mut folder, "d1")).unwrap();
    assert_eq!(folder.live().len(), 1, "back in Drafts: {:?}", folder.log);
    guard(&fx.store, &fx.account, &mut folder, "d1").expect("the copy pushed again is claimed");
    assert!(folder.live().is_empty());
}

/// A server that cannot be reached, or offers no UIDPLUS, says nothing about
/// the other machine: the send is refused rather than risked.
#[test]
fn an_unreachable_drafts_folder_refuses_the_send() {
    let fx = Fixture::new();
    fx.write("angebot", "d1", "", "eins");
    let mut folder = FakeFolder::default();
    block_on(push::push(&fx.mirror(1), &mut folder, "d1")).unwrap();

    folder.fail_select = true;
    let refused = guard(&fx.store, &fx.account, &mut folder, "d1").unwrap_err();
    assert!(refused.contains("could not be reached"), "{refused}");
    assert!(refused.contains("nothing was sent"), "{refused}");

    folder.fail_select = false;
    folder.uidplus = false;
    let refused = guard(&fx.store, &fx.account, &mut folder, "d1").unwrap_err();
    assert!(refused.contains("UIDPLUS"), "{refused}");
    assert_eq!(folder.live().len(), 1, "the copy is untouched");
    assert!(fx.row("d1").is_some());
}

/// A draft never pushed, and any draft of an account that does not mirror,
/// passes without a word to the server.
#[test]
fn a_draft_with_no_server_copy_passes_the_guard_untouched() {
    let mut fx = Fixture::new();
    fx.write("angebot", "d1", "", "eins");
    let mut folder = FakeFolder {
        fail_select: true,
        ..FakeFolder::default()
    };
    guard(&fx.store, &fx.account, &mut folder, "d1").expect("never pushed");

    fx.write(
        "pushed",
        "d2",
        "server_message_id: \"<mp-draft.2@tum.de>\"\n",
        "zwei",
    );
    fx.account.drafts.mirror = false;
    guard(&fx.store, &fx.account, &mut folder, "d2").expect("mirror off");
    assert!(folder.log.is_empty(), "{:?}", folder.log);
}

/// A send that fails after its claim leaves a draft with no copy and no row:
/// the owed-push scan pushes it again, and the reconcile never retires it as
/// a draft sent elsewhere.
#[test]
fn a_send_that_fails_after_its_claim_is_pushed_again_and_never_retired() {
    let fx = Fixture::new();
    let path = fx.write("angebot", "d1", "", "eins");
    let mut folder = FakeFolder::default();
    block_on(push::push(&fx.mirror(1), &mut folder, "d1")).unwrap();
    guard(&fx.store, &fx.account, &mut folder, "d1").unwrap();
    assert!(folder.live().is_empty());

    // The SMTP submission failed: the file stays where it was.
    for _ in 0..2 {
        let report = reconciled(&fx, &mut folder);
        assert_eq!(report.retired, 0, "{report:?}");
    }
    assert!(path.exists(), "never retired");
    assert_eq!(
        push::enqueue_owed(&fx.store, &fx.account).unwrap(),
        1,
        "a push is owed"
    );
    block_on(push::push(&fx.mirror(2), &mut folder, "d1")).unwrap();
    assert_eq!(folder.live().len(), 1, "pushed again: {:?}", folder.log);
    assert!(fx.row("d1").is_some());
}

/// The TUM incident of 2026-10-23: the home server imported a draft at its
/// first copy, the Mac replaced that copy twice, approved it and sent it.
/// The home server's send key then approves its own file, which owes a push,
/// and sends. Whichever runs first, the push or the claim, nothing is pushed
/// again and nothing is sent: a push that finds no copy where its row named
/// one was sent or discarded elsewhere, and the reconcile retires the file.
fn sent_on_the_other_machine() -> (FakeFolder, Machine, Machine, PathBuf) {
    let mut folder = FakeFolder::default();
    let mac = Machine::new();
    let home = Machine::new();
    let path = {
        let _guard = mac.enter();
        Fixture::write_in_current("angebot", "d1", "", "Hallo Robin")
    };
    mac.drain(&mut folder);
    assert_eq!(home.reconcile(&mut folder).imported, 1);
    let home_path = {
        let _guard = home.enter();
        find_local("tum", "d1").unwrap().path
    };
    std::fs::write(
        &path,
        std::fs::read_to_string(&path)
            .unwrap()
            .replace("Hallo Robin", "Hallo Robin, anbei"),
    )
    .unwrap();
    mac.drain(&mut folder);
    approve(&path);
    mac.drain(&mut folder);
    {
        let _guard = mac.enter();
        guard(&mac.store, &mac.account, &mut folder, "d1").expect("the Mac sends it");
    }
    assert!(folder.live().is_empty(), "{:?}", folder.log);
    (folder, mac, home, home_path)
}

#[test]
fn a_draft_sent_elsewhere_is_never_pushed_again_when_approved_then_sent() {
    let (mut folder, _mac, home, home_path) = sent_on_the_other_machine();

    // The send key: approve, then send; the guard refuses.
    approve(&home_path);
    {
        let _guard = home.enter();
        let refused = guard(&home.store, &home.account, &mut folder, "d1").unwrap_err();
        assert!(
            refused.contains("sent or discarded on another machine"),
            "{refused}"
        );
    }
    // The drain the approval and the send asked for.
    home.drain(&mut folder);
    assert!(
        folder.live().is_empty(),
        "the refused draft is not pushed back: {:?}",
        folder.log
    );
    for _ in 0..2 {
        home.reconcile(&mut folder);
        home.drain(&mut folder);
    }
    assert!(!home.has("d1"), "retired as copy-gone");
    assert!(folder.live().is_empty(), "{:?}", folder.log);
}

#[test]
fn a_push_before_the_claim_never_hands_the_claim_a_fresh_copy_of_a_sent_draft() {
    let (mut folder, _mac, home, home_path) = sent_on_the_other_machine();

    // The approval's push drains before the send takes the runtime's turn.
    approve(&home_path);
    home.drain(&mut folder);
    assert!(
        folder.live().is_empty(),
        "no fresh copy for the claim to find: {:?}",
        folder.log
    );
    let _guard = home.enter();
    let refused = guard(&home.store, &home.account, &mut folder, "d1").unwrap_err();
    assert!(
        refused.contains("sent or discarded on another machine"),
        "{refused}"
    );
}

/// A draft retired because its copy went and then moved back into `drafts/`
/// by hand has no row: that is the deliberate way to keep it, and it is
/// pushed as a new copy.
#[test]
fn a_copy_gone_draft_moved_back_by_hand_is_pushed_as_new() {
    let (mut folder, _mac, home, home_path) = sent_on_the_other_machine();
    approve(&home_path);
    for _ in 0..2 {
        home.reconcile(&mut folder);
    }
    assert!(!home.has("d1"));
    let _guard = home.enter();
    let retired = crate::config::drafts_dir("tum")
        .join(crate::draft::RETIRED_DIR)
        .join(format!(
            "{}.copy-gone.md",
            home_path.file_stem().unwrap().to_string_lossy()
        ));
    std::fs::rename(&retired, &home_path).unwrap();
    drop(_guard);
    home.drain(&mut folder);
    assert_eq!(folder.live().len(), 1, "pushed as new: {:?}", folder.log);
}

// ---------------------------------------------------------------------------
// A forward started in Outlook mobile (TUM Drafts, 2026-10-23)
// ---------------------------------------------------------------------------

/// A forward as Outlook mobile saves it before a recipient is chosen: no `To`
/// header, `FW:` in the subject, `In-Reply-To` naming the forwarded message,
/// one `text/html` part whose quoted message sits in
/// `div#mail-editor-reference-message-container` under a bordered From/Sent
/// block.
fn outlook_mobile_forward() -> Vec<u8> {
    b"From: Sylvain Hellin <me@tum.de>\r\nSubject: FW: Master's thesis\r\n\
Message-ID: <71174d11-mobile@tum.de>\r\nReferences: <root@tum.de> <parent@tum.de>\r\n\
In-Reply-To: <parent@tum.de>\r\nMIME-Version: 1.0\r\n\
Content-Type: text/html; charset=utf-8\r\n\r\n\
<html><body><div dir=\"ltr\">Zur Info, siehe unten.</div>\
<div id=\"ms-outlook-mobile-signature\" dir=\"ltr\"><div><b>Sylvain</b></div></div>\
<div id=\"mail-editor-reference-message-container\">\
<div style=\"padding: 3pt 0in 0in; border-width: 1pt medium medium; border-style: solid none none;\">\
<div><b>From: </b>Tina<br><b>Sent: </b>Wednesday<br><b>To: </b>Sylvain<br>\
<b>Subject: </b>AW: Master's thesis<br></div></div>\
<div id=\"mail-editor-reference-message-body\"><p>Die Arbeit ist eingereicht.</p></div>\
</div></body></html>\r\n"
        .to_vec()
}

/// The forward imports as a forward: the quoted message below mp's quote
/// marker, an empty `to:` rather than a placeholder address, and
/// `forwarded_from:` rather than `in_reply_to:`, so the send flags the source
/// `$Forwarded` and does not thread the forward into its conversation.
#[test]
fn an_outlook_mobile_forward_imports_as_a_forward_with_its_quote_marked() {
    let fx = Fixture::new();
    let mut folder = FakeFolder::default();
    folder.put(&outlook_mobile_forward());

    assert_eq!(reconciled(&fx, &mut folder).imported, 1);
    let local = local_drafts("tum").pop().expect("imported");
    let fm = &local.draft.frontmatter;
    assert!(
        fm.to.as_deref().is_none_or(|to| to.trim().is_empty()),
        "no To header is an empty to: {:?}",
        fm.to
    );
    assert_eq!(fm.subject, "FW: Master's thesis");
    assert_eq!(fm.forwarded_from.as_deref(), Some("<parent@tum.de>"));
    assert_eq!(fm.in_reply_to, None, "a forward answers nothing");

    let body = &local.draft.body_markdown;
    let marker = body
        .find(crate::draft::QUOTE_MARKER)
        .unwrap_or_else(|| panic!("a quote marker: {body}"));
    assert!(body[..marker].contains("Zur Info"), "{body}");
    assert!(!body[..marker].contains("eingereicht"), "{body}");
    assert!(
        body[marker..].contains("Die Arbeit ist eingereicht."),
        "{body}"
    );

    // A reply under the same headers stays a reply.
    let fx = Fixture::new();
    let mut folder = FakeFolder::default();
    let reply = String::from_utf8(outlook_mobile_forward())
        .unwrap()
        .replace("Subject: FW: ", "Subject: AW: ")
        .replace("71174d11-mobile", "71174d12-mobile");
    folder.put(reply.as_bytes());
    reconciled(&fx, &mut folder);
    let local = local_drafts("tum").pop().expect("imported");
    assert_eq!(
        local.draft.frontmatter.in_reply_to.as_deref(),
        Some("<parent@tum.de>")
    );
    assert_eq!(local.draft.frontmatter.forwarded_from, None);
}
