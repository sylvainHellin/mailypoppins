//! The drafts mirror over an in-memory Drafts folder (#0143).

use std::path::PathBuf;

use super::folder::fake::FakeFolder;
use super::folder::header_value;
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
    let raw =
        crate::send::build_server_draft(&draft, "me@tum.de", &fx.email, "<mp-draft.1@tum.de>")
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
        header_value(&raw, crate::send::DRAFT_STATUS_HEADER).is_none(),
        "approval stays local while approval_travels() is false"
    );

    // No recipient and no subject yet is still a draft the server can hold.
    std::fs::write(
        &path,
        "---\nid: \"d1\"\nto:\nsubject:\nstatus: draft\n---\n\nhalf\n",
    )
    .unwrap();
    let draft = crate::draft::parse_email_draft(&path).unwrap();
    assert!(crate::send::build_server_draft(&draft, "me@tum.de", &fx.email, "<x@tum.de>").is_ok());
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
    assert_eq!(
        queued[0].op,
        ServerOp::PushDraft {
            draft_id: "d-new".to_string()
        }
    );
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
    fx.write("a", "d1", "", "eins");
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
        1,
        "still owed"
    );
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
