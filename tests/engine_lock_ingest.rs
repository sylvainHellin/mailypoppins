//! The engine lock on the ingest path (#0122, plan unit P3b-U1).
//!
//! Until now the per-account engine lock (`<account_dir>/store.lock`,
//! [`mailypoppins::engine_lock`], #0061) guarded the two *queue drains* only:
//! the outbox ([`mailypoppins::outbox::drain_guarded`], #0116) and the mutation
//! queue ([`mailypoppins::pending_ops::drain_account`]). The ingest half of a
//! sync ran unguarded, so `mp sync` in one terminal and an open TUI in another
//! both downloaded and ingested the same window into the same store.
//!
//! This file pins the extension of the lock to that half, and it is **not**
//! feature-gated: it is a change to existing library behaviour and belongs in
//! the default suite. The implementation landed in P3b-U1 / P3b-U2, so every
//! test here runs as part of `cargo test --workspace` (they were `#[ignore]`d
//! while the plan's P3b-U1 note recommended it, and are not any more).
//!
//! # The API this pins
//!
//! ```ignore
//! // src/sync/engine.rs
//! pub async fn run_sync_guarded(
//!     backend: &mut impl SyncBackend,
//!     run: &SyncRun<'_>,
//!     span: &mut TimingSpan,
//! ) -> anyhow::Result<Option<SyncResult>>;
//!
//! pub async fn run_sync_guarded_at(
//!     lock_path: &std::path::Path,
//!     backend: &mut impl SyncBackend,
//!     run: &SyncRun<'_>,
//!     span: &mut TimingSpan,
//! ) -> anyhow::Result<Option<SyncResult>>;
//! ```
//!
//! The pair mirrors [`mailypoppins::outbox::drain_guarded`] /
//! `drain_guarded_at` line for line, and for the same two reasons: the plain
//! form resolves `crate::config::account_dir(run.account).join("store.lock")`
//! itself, so no caller has to know the layout, and the `_at` form is the
//! mechanism, split out so a test can point it at a tempdir exactly as
//! [`mailypoppins::engine_lock::EngineLock::try_acquire_at`] is. `run.account`
//! already carries the account, so `run_sync_guarded` needs no parameter
//! [`mailypoppins::sync::engine::run_sync`] does not have.
//!
//! `Ok(None)` means another process is the engine for this account and this
//! call did nothing: no IMAP session, no ingest, no error. That is a success at
//! every caller, the same reading `drain_guarded`'s `Ok(None)` gets.
//!
//! [`run_sync`] itself stays lock-free, so the fake-backend engine tests in
//! `src/sync/engine.rs` keep working unchanged; nothing in this file touches
//! it except to state that it is still the unguarded mechanism underneath.
//!
//! # Why the lock is taken in-process
//!
//! `flock(2)` is per open file description, not per process: a second
//! `try_acquire_at` on the same path is refused even from the same process,
//! which the unit tests in `src/engine_lock.rs` already pin. So a test holder
//! is just another `EngineLock`, and no spawned process is needed.
//!
//! Nothing in this binary spawns one either. The CLI half of the contract (the
//! exit code and the stdout of a refused `mp sync`) lives in
//! `tests/engine_lock_ingest_cli.rs`, a target of its own: a fork duplicates
//! the `flock`ed descriptors the tests here hold on their own tempdirs, and an
//! inherited copy keeps a sibling's lock alive past the sibling's `drop` until
//! `exec` closes it. That is what made this binary flake about twice in
//! twenty-five runs before the split.

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::Result;
use mailypoppins::engine_lock::EngineLock;
use mailypoppins::ingest::KnownUids;
use mailypoppins::store::{BlobStore, Store};
use mailypoppins::sync::engine::{run_sync_guarded_at, SyncRun};
use mailypoppins::sync::{
    BodyBatch, FetchedMessage, FlagScope, MailboxCoverage, MailboxListing, MailboxState,
    ObservationSink, Observed, ResumePoint, Stop, SyncBackend, SyncResult, SyncTarget,
};
use mailypoppins::timing::TimingSpan;
use mailypoppins::types::{MailboxRole, MessageFlags};
use tempfile::TempDir;

/// Never called: it exists so the *plain* form of the API is pinned by the
/// compiler too, with the signature the plan states.
///
/// Every test below drives `run_sync_guarded_at`, because a test cannot let
/// `crate::config::account_dir` resolve the lock path (that reads the process's
/// environment, which is shared by every test in the binary). This is what says
/// the caller-facing form takes no parameter [`run_sync`] does not have: the
/// account is already in [`SyncRun`], so `run_sync_guarded` is a drop-in for
/// `run_sync` at every call site, exactly as `drain_guarded` is for `drain`.
#[allow(dead_code)]
async fn the_plain_form_takes_exactly_run_syncs_parameters(
    backend: &mut ScriptedBackend,
    run: &SyncRun<'_>,
    span: &mut TimingSpan,
) -> Result<Option<SyncResult>> {
    mailypoppins::sync::engine::run_sync_guarded(backend, run, span).await
}

// ---------------------------------------------------------------------------
// Fake backends
// ---------------------------------------------------------------------------

/// A backend that must never be reached.
///
/// The "no IMAP session opened" half of the contract: the engine's first call
/// into the transport is `list_targets`, and every session in the real backend
/// is opened inside it or after it ([`mailypoppins::imap_client::ImapBackend`]
/// checks out of the pool per target). A guarded run that was refused the lock
/// must return before this is called, so reaching it is the failure.
struct PanickingBackend;

impl SyncBackend for PanickingBackend {
    type Known = KnownUids;

    fn known(store: &Store, account: &str, target: &SyncTarget) -> Result<KnownUids> {
        mailypoppins::ingest::known_uids_with_cursor(store, account, target.role.as_str())
    }

    async fn list_targets(
        &mut self,
        _targets: &[SyncTarget],
        _limit: usize,
        _knowns: Vec<KnownUids>,
        _stop: Option<&Stop>,
    ) -> Vec<Result<MailboxListing>> {
        panic!(
            "run_sync_guarded opened a fetch while another process held the engine lock: \
             the refusal must happen before the transport is touched"
        );
    }

    async fn next_batch(&mut self, _index: usize) -> Result<BodyBatch> {
        panic!("run_sync_guarded downloaded a body while another process held the engine lock");
    }
}

/// One mailbox's whole fetch as a test scripts it: the listing, every new
/// body, and the coverage the backend reports once the bodies are in.
struct MailboxFetch {
    messages: Vec<FetchedMessage>,
    skipped: usize,
    known_flags: Vec<(i64, MessageFlags)>,
    state: MailboxState,
    vanished: Vec<i64>,
    listed: Vec<i64>,
    uidvalidity_reset: bool,
    enumeration_complete: bool,
    download_incomplete: bool,
    bodies_complete: bool,
    pending_arrival_mark: Option<u32>,
    highest_modseq: Option<mailypoppins::ingest::ModseqMark>,
}

/// Hands back scripted fetches per target, one per pass, ignoring the skip
/// list. The engine tests in `src/sync/engine.rs` use the same shape; this is a
/// copy rather than a shared module because nothing in `tests/` is shared today
/// (there is no `tests/common/`) and the fixture here is four fields wide.
#[derive(Default)]
struct ScriptedBackend {
    passes: HashMap<String, Vec<MailboxFetch>>,
    /// How many times the engine asked for a fetch, so a test can say "no pass
    /// ran" as well as "no session opened".
    calls: usize,
    /// The bodies of each target of the last listing, by index, still to be
    /// handed over, and whether the scripted pass asked for every new UID.
    bodies: Vec<Option<(Vec<FetchedMessage>, bool)>>,
}

/// How many bodies [`ScriptedBackend`] hands over per batch: the IMAP
/// backend's chunk size.
const BATCH: usize = 20;

impl ScriptedBackend {
    fn with(server_name: &str, passes: Vec<MailboxFetch>) -> Self {
        let mut map = HashMap::new();
        map.insert(server_name.to_string(), passes);
        Self {
            passes: map,
            ..Self::default()
        }
    }
}

impl SyncBackend for ScriptedBackend {
    type Known = KnownUids;

    fn known(store: &Store, account: &str, target: &SyncTarget) -> Result<KnownUids> {
        mailypoppins::ingest::known_uids_with_cursor(store, account, target.role.as_str())
    }

    async fn list_targets(
        &mut self,
        targets: &[SyncTarget],
        _limit: usize,
        _knowns: Vec<KnownUids>,
        _stop: Option<&Stop>,
    ) -> Vec<Result<MailboxListing>> {
        self.calls += 1;
        self.bodies.clear();
        let mut out = Vec::with_capacity(targets.len());
        for target in targets {
            let next = self
                .passes
                .get_mut(&target.server_name)
                .filter(|queue| !queue.is_empty())
                .map(|queue| queue.remove(0));
            let Some(fetch) = next else {
                self.bodies.push(None);
                out.push(Err(anyhow::anyhow!(
                    "nothing scripted for {}",
                    target.server_name
                )));
                continue;
            };
            let covered = MailboxCoverage {
                download_incomplete: fetch.download_incomplete,
                pending_arrival_mark: fetch.pending_arrival_mark,
                resume: fetch.highest_modseq.map(ResumePoint::Modseq),
                backlog: false,
            };
            // A scripted body the engine did not download leaves the mailbox
            // short, whatever the script says of a pass that got them all.
            let scripted: Vec<i64> = fetch.messages.iter().map(|m| m.uid).collect();
            let coverage = move |downloaded: &[i64], _: bool| MailboxCoverage {
                download_incomplete: covered.download_incomplete
                    || scripted.iter().any(|uid| !downloaded.contains(uid)),
                ..covered
            };
            self.bodies
                .push(Some((fetch.messages, fetch.bodies_complete)));
            out.push(Ok(MailboxListing {
                skipped: fetch.skipped,
                known_flags: fetch.known_flags,
                observed: Observed::Selected(fetch.state),
                resume_stale: false,
                flag_scope: FlagScope::Whole,
                vanished: fetch.vanished,
                listed: fetch.listed,
                uidvalidity_reset: fetch.uidvalidity_reset,
                enumeration_complete: fetch.enumeration_complete,
                coverage: Box::new(coverage),
            }));
        }
        out
    }

    async fn next_batch(&mut self, index: usize) -> Result<BodyBatch> {
        let Some(Some((messages, complete))) = self.bodies.get_mut(index) else {
            panic!("the engine asked for the bodies of a target without a listing");
        };
        if messages.is_empty() {
            return Ok(BodyBatch::Done {
                complete: *complete,
            });
        }
        // Newest first, as the IMAP backend takes its chunks off the end of
        // the ascending new UIDs; a script lists them oldest first.
        let newest = messages.len().saturating_sub(BATCH);
        Ok(BodyBatch::Bodies(messages.split_off(newest)))
    }
}

/// A complete pass: the whole mailbox enumerated, everything owed downloaded.
/// `listed` is derived from the scripted UIDs, so the #0112 rebind gate is
/// exercised rather than degraded; a test that wants the degradation clears
/// `enumeration_complete` or sets `uidvalidity_reset`.
fn fetch(messages: Vec<(i64, Vec<u8>)>) -> MailboxFetch {
    let mut listed: Vec<i64> = messages.iter().map(|(uid, _)| *uid).collect();
    listed.sort_unstable();
    MailboxFetch {
        messages: messages
            .into_iter()
            .map(|(uid, raw)| FetchedMessage::raw(uid, raw, MessageFlags::default()))
            .collect(),
        skipped: 0,
        known_flags: Vec::new(),
        state: MailboxState {
            uid_validity: Some(7),
            uid_next: Some(9000),
            exists: 0,
        },
        vanished: Vec::new(),
        listed,
        uidvalidity_reset: false,
        enumeration_complete: true,
        download_incomplete: false,
        bodies_complete: true,
        pending_arrival_mark: None,
        highest_modseq: None,
    }
}

/// The same pass after the server renumbered: the listing says nothing about
/// which message wears which UID, so the engine degrades to the unconditional
/// rebind ingest always did.
fn reset_fetch(messages: Vec<(i64, Vec<u8>)>) -> MailboxFetch {
    MailboxFetch {
        uidvalidity_reset: true,
        ..fetch(messages)
    }
}

fn sent_target() -> Vec<SyncTarget> {
    vec![SyncTarget::new(MailboxRole::Sent, "Sent")]
}

/// A message the server holds several copies of: the #0112 corpus, copied from
/// `tests/store_ingest_integration.rs` so the four invariants below are the
/// same messages, driven through the engine instead of through `ingest`.
fn duplicated() -> Vec<u8> {
    b"From: a@example.com\r\n\
      To: b@example.com\r\n\
      Subject: Re: Webseite LOC\r\n\
      Message-ID: <dup@example.com>\r\n\
      Date: Thu, 7 Aug 2025 10:00:00 +0000\r\n\
      \r\n\
      duplicated body\r\n"
        .to_vec()
}

// ---------------------------------------------------------------------------
// Fixture
// ---------------------------------------------------------------------------

struct Fixture {
    _tmp: TempDir,
    dir: PathBuf,
    store: Store,
    blobs: BlobStore,
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().to_path_buf();
        let store = Store::open(dir.join("store.sqlite3")).unwrap();
        let blobs = BlobStore::new(dir.join("blobs"));
        Self {
            _tmp: tmp,
            dir,
            store,
            blobs,
        }
    }

    /// Where the engine lock lives: beside the store, exactly as
    /// `crate::config::account_dir(account).join("store.lock")` puts it.
    fn lock_path(&self) -> PathBuf {
        self.dir.join("store.lock")
    }

    /// One guarded pass. `Ok(None)` is the refusal.
    async fn guarded(
        &self,
        backend: &mut impl SyncBackend,
        targets: &[SyncTarget],
    ) -> Result<Option<SyncResult>> {
        let run = SyncRun {
            store: &self.store,
            blobs: &self.blobs,
            account: "acct",
            targets,
            limit: usize::MAX,
            dry_run: false,
            stop: None,
            observations: &ObservationSink::default(),
        };
        let mut span = TimingSpan::new("engine_lock_ingest");
        run_sync_guarded_at(&self.lock_path(), backend, &run, &mut span).await
    }

    /// `(row id, uid)` for the mailbox, lowest UID first: what a row count
    /// cannot say, which is whether a message kept its row or was written into
    /// a new one.
    fn rows(&self, mailbox: &str) -> Vec<(i64, i64)> {
        let conn = self.store.conn();
        let mut stmt = conn
            .prepare(
                "SELECT id, uid FROM messages WHERE account = 'acct' AND mailbox = ?1 \
                 ORDER BY uid",
            )
            .unwrap();
        let out = stmt
            .query_map([mailbox], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        out
    }

    fn uids(&self, mailbox: &str) -> Vec<i64> {
        self.rows(mailbox).into_iter().map(|(_, uid)| uid).collect()
    }

    fn message_rows(&self) -> i64 {
        self.store
            .conn()
            .query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))
            .unwrap()
    }
}

// ---------------------------------------------------------------------------
// 1. The refusal
// ---------------------------------------------------------------------------

/// The defect this unit closes: a second engine ingesting into a store another
/// engine is already writing. A process that cannot take the lock does nothing
/// at all, and says so with `Ok(None)` rather than an error.
///
/// The backend panics on any call, so this also pins the half the return value
/// cannot express: the refusal is decided *before* the transport is touched, so
/// a refused sync costs no server traffic, no session and no login.
#[tokio::test]
async fn a_held_lock_makes_the_guarded_sync_refuse_without_opening_a_session() {
    let f = Fixture::new();
    let _holder = EngineLock::try_acquire_at(&f.lock_path(), "acct")
        .unwrap()
        .expect("the test process is the first holder");

    let mut backend = PanickingBackend;
    let refused = f
        .guarded(&mut backend, &sent_target())
        .await
        .expect("a refused sync is not an error");

    assert!(
        refused.is_none(),
        "a process that cannot take the engine lock must return Ok(None), got {refused:?}"
    );
    assert_eq!(f.message_rows(), 0, "a refused sync must ingest nothing");
}

/// The other side of the same rule: the holder is not slowed down or refused by
/// the guard. It runs the pass it would have run unguarded and reports the real
/// [`SyncResult`], not `None`.
///
/// It also releases the lock when the pass is over: the guard is scoped to the
/// call, exactly as `drain_guarded`'s is, so the next process is the engine
/// without waiting for this one to exit.
#[tokio::test]
async fn the_lock_holder_runs_the_pass_and_releases_the_lock() {
    let f = Fixture::new();
    let mut backend = ScriptedBackend::with("Sent", vec![fetch(vec![(6540, duplicated())])]);

    let result = f
        .guarded(&mut backend, &sent_target())
        .await
        .unwrap()
        .expect("nothing holds the lock, so this call is the engine");

    assert_eq!(result.saved, 1, "the holder ingests normally");
    assert_eq!(backend.calls, 1, "the holder opens exactly one pass");
    assert_eq!(f.uids("sent"), vec![6540]);

    assert!(
        EngineLock::try_acquire_at(&f.lock_path(), "acct")
            .unwrap()
            .is_some(),
        "the guard must release the lock when the pass returns, not hold it for the process"
    );
}

/// A refusal leaves nothing behind: no half-written cursor, no marker, no
/// poisoned state. The pass that runs after the holder goes away ingests the
/// window the refused one never looked at, as if the refusal had not happened.
///
/// This is what makes `Ok(None)` a success rather than a deferred failure: the
/// work happens, in the other process or on the next tick.
#[tokio::test]
async fn a_refusal_is_a_success_and_the_next_pass_after_it_ingests_normally() {
    let f = Fixture::new();

    let holder = EngineLock::try_acquire_at(&f.lock_path(), "acct")
        .unwrap()
        .unwrap();
    let mut refused_backend = PanickingBackend;
    assert!(f
        .guarded(&mut refused_backend, &sent_target())
        .await
        .unwrap()
        .is_none());
    drop(holder);

    let mut backend = ScriptedBackend::with("Sent", vec![fetch(vec![(6540, duplicated())])]);
    let result = f
        .guarded(&mut backend, &sent_target())
        .await
        .unwrap()
        .unwrap();

    assert_eq!(
        result.saved, 1,
        "the pass after a refusal is an ordinary first pass"
    );
    assert_eq!(f.uids("sent"), vec![6540]);
}

// ---------------------------------------------------------------------------
// 2. The #0112 invariants, driven through the guard
// ---------------------------------------------------------------------------
//
// The four tests named in the plan live in `tests/store_ingest_integration.rs`
// and drive `ingest` directly. Here they are the same four properties driven
// through `run_sync_guarded`, which is the path that will carry them once the
// lock moves onto the ingest half: the guard must change what a pass is allowed
// to start, and nothing about what a pass that starts does.

/// N UIDs the server lists for one `Message-ID` are N messages in that mailbox
/// and owe N rows. Collapsing them onto one row is what left the other copies
/// out of the skip list and made the fetch re-download them every pass forever.
#[tokio::test]
async fn n_listed_copies_of_one_message_id_get_n_rows_under_the_lock() {
    let f = Fixture::new();
    let raw = duplicated();
    let mut backend = ScriptedBackend::with(
        "Sent",
        vec![fetch(vec![
            (6540, raw.clone()),
            (6542, raw.clone()),
            (6543, raw),
        ])],
    );

    let result = f
        .guarded(&mut backend, &sent_target())
        .await
        .unwrap()
        .unwrap();

    assert_eq!(result.saved, 3, "three listed copies are three new rows");
    assert_eq!(
        result.uid_rebound, 0,
        "a UID the server is still listing is not a renumbering"
    );
    assert_eq!(f.uids("sent"), vec![6540, 6542, 6543]);
    assert_eq!(f.message_rows(), 3);
}

/// The other half of the same rule: ingest never rebinds twice onto one row, so
/// the copies stay put once they have their rows. The pass-2 half of "the same
/// mailbox synced twice reports nothing new".
#[tokio::test]
async fn a_second_pass_over_the_same_copies_changes_no_row_under_the_lock() {
    let f = Fixture::new();
    let raw = duplicated();
    let pass = || fetch(vec![(6540, raw.clone()), (6542, raw.clone())]);
    let mut backend = ScriptedBackend::with("Sent", vec![pass(), pass()]);

    f.guarded(&mut backend, &sent_target())
        .await
        .unwrap()
        .unwrap();
    let before = f.rows("sent");
    let second = f
        .guarded(&mut backend, &sent_target())
        .await
        .unwrap()
        .unwrap();

    assert_eq!(second.saved, 0, "the second pass inserts nothing");
    assert_eq!(second.uid_rebound, 0, "and moves nothing");
    assert_eq!(f.rows("sent"), before, "no row id and no uid moved");
    assert_eq!(f.message_rows(), 2);
}

/// The unconditional policy is still reachable and still collapses, which is
/// what a UIDVALIDITY reset needs: the row follows its message onto a UID the
/// renumbered server is listing, even though the listing names it.
#[tokio::test]
async fn the_unconditional_policy_still_rebinds_onto_a_listed_uid_under_the_lock() {
    let f = Fixture::new();
    let raw = duplicated();
    let mut backend = ScriptedBackend::with(
        "Sent",
        vec![
            fetch(vec![(6540, raw.clone())]),
            reset_fetch(vec![(6542, raw)]),
        ],
    );

    let first = f
        .guarded(&mut backend, &sent_target())
        .await
        .unwrap()
        .unwrap();
    let rows_before = f.rows("sent");
    let second = f
        .guarded(&mut backend, &sent_target())
        .await
        .unwrap()
        .unwrap();

    assert_eq!(first.saved, 1);
    assert_eq!(
        second.saved, 0,
        "the renumbered copy is the same message, not a new one"
    );
    assert_eq!(second.uid_rebound, 1);
    assert_eq!(second.uidvalidity_resets, 1);
    assert_eq!(f.message_rows(), 1, "the row followed its message");
    assert_eq!(
        f.rows("sent"),
        vec![(rows_before[0].0, 6542)],
        "same row id, new uid"
    );
}

/// A reset pass has no usable listing, so it rebinds through an empty one. That
/// must not put every copy back on the lowest-id row: a candidate this pass has
/// already moved onto a UID is off limits for the rest of it, which is how N
/// copies survive a renumbering as N rows.
#[tokio::test]
async fn a_reset_pass_maps_n_copies_onto_n_rows_under_the_lock() {
    let f = Fixture::new();
    let raw = duplicated();
    let mut backend = ScriptedBackend::with(
        "Sent",
        vec![
            fetch(vec![(6540, raw.clone()), (6542, raw.clone())]),
            reset_fetch(vec![(11, raw.clone()), (12, raw)]),
        ],
    );

    f.guarded(&mut backend, &sent_target())
        .await
        .unwrap()
        .unwrap();
    let before = f.rows("sent");
    let second = f
        .guarded(&mut backend, &sent_target())
        .await
        .unwrap()
        .unwrap();

    assert_eq!(second.uid_rebound, 2, "both copies followed their own row");
    assert_eq!(second.saved, 0, "a renumbering downloads no new message");
    assert_eq!(f.message_rows(), 2, "a renumbering must not lose a copy");
    assert_eq!(f.uids("sent"), vec![11, 12]);
    assert_eq!(
        f.rows("sent").iter().map(|(id, _)| *id).collect::<Vec<_>>(),
        before.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
        "the second copy must not land on the first row"
    );
}

/// `run_sync` stays lock-free, so the fake-backend engine tests in
/// `src/sync/engine.rs` keep working unchanged: P3b-U2 adds a guarded wrapper
/// around it rather than changing the signature of what it wraps. Driving the
/// unguarded form while a holder is live is what proves it: it ingests, where
/// the guarded form would have refused.
#[tokio::test]
async fn run_sync_itself_stays_lock_free() {
    let f = Fixture::new();
    let _holder = EngineLock::try_acquire_at(&f.lock_path(), "acct")
        .unwrap()
        .unwrap();

    let mut backend = ScriptedBackend::with("Sent", vec![fetch(vec![(6540, duplicated())])]);
    let run = SyncRun {
        store: &f.store,
        blobs: &f.blobs,
        account: "acct",
        targets: &sent_target(),
        limit: usize::MAX,
        dry_run: false,
        stop: None,
        observations: &ObservationSink::default(),
    };
    let mut span = TimingSpan::new("engine_lock_ingest");
    let result = mailypoppins::sync::engine::run_sync(&mut backend, &run, &mut span)
        .await
        .expect("the unguarded mechanism takes no lock and cannot be refused");

    assert_eq!(result.saved, 1);
}
