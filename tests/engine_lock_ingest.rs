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
//! is just another `EngineLock`, and no spawned process is needed. The one
//! test that does spawn a process (`mp sync`) needs a real second process for a
//! different reason: it is the exit code and the stdout of the CLI that it
//! pins.

use std::collections::HashMap;
use std::fs;
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use anyhow::Result;
use mailypoppins::engine_lock::EngineLock;
use mailypoppins::ingest::KnownUids;
use mailypoppins::secrets::{EncryptedFileBackend, SecretsBackend};
use mailypoppins::store::{BlobStore, Store};
use mailypoppins::sync::engine::{run_sync_guarded_at, SyncRun};
use mailypoppins::sync::{
    FetchedRaw, MailboxFetch, MailboxState, SyncBackend, SyncResult, SyncTarget,
};
use mailypoppins::timing::TimingSpan;
use mailypoppins::types::{MailboxRole, MessageFlags};
use tempfile::TempDir;

const MP: &str = env!("CARGO_BIN_EXE_mp");

/// The line `mp sync` prints when another process holds the engine lock.
///
/// Pinned here because the plan leaves the wording to this unit. It mirrors
/// what the outbox already says on the same refusal (`[outbox] another engine
/// is draining {account}; leaving the APPENDs to it`), moved from the log to
/// stdout because `mp sync` is an interactive command whose whole output is
/// otherwise a summary of a pass that did not happen. The full line carries the
/// `ℹ` prefix every other informational `mp sync` line uses:
///
/// ```text
/// ℹ Sync skipped: another engine is syncing 'acct'; leaving the ingest to it
/// ```
///
/// The tests assert on the text after the prefix, so the marker and any
/// colouring stay the implementer's business.
fn skip_line(account: &str) -> String {
    format!("Sync skipped: another engine is syncing '{account}'; leaving the ingest to it")
}

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
/// The "no IMAP session opened" half of the contract: the engine's only call
/// into the transport is `fetch_targets`, and every session in the real backend
/// is opened inside it ([`mailypoppins::imap_client::ImapBackend`] checks out
/// of the pool per target). A guarded run that was refused the lock must return
/// before this is called, so reaching it is the failure.
struct PanickingBackend;

impl SyncBackend for PanickingBackend {
    async fn fetch_targets(
        &mut self,
        _targets: &[SyncTarget],
        _limit: usize,
        _knowns: Vec<KnownUids>,
    ) -> Vec<Result<MailboxFetch>> {
        panic!(
            "run_sync_guarded opened a fetch while another process held the engine lock: \
             the refusal must happen before the transport is touched"
        );
    }
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
}

impl ScriptedBackend {
    fn with(server_name: &str, passes: Vec<MailboxFetch>) -> Self {
        let mut map = HashMap::new();
        map.insert(server_name.to_string(), passes);
        Self {
            passes: map,
            calls: 0,
        }
    }
}

impl SyncBackend for ScriptedBackend {
    async fn fetch_targets(
        &mut self,
        targets: &[SyncTarget],
        _limit: usize,
        _knowns: Vec<KnownUids>,
    ) -> Vec<Result<MailboxFetch>> {
        self.calls += 1;
        targets
            .iter()
            .map(|target| {
                self.passes
                    .get_mut(&target.server_name)
                    .filter(|queue| !queue.is_empty())
                    .map(|queue| queue.remove(0))
                    .ok_or_else(|| anyhow::anyhow!("nothing scripted for {}", target.server_name))
            })
            .collect()
    }
}

/// A complete pass: the whole mailbox enumerated, everything owed downloaded.
/// `listed` is derived from the scripted UIDs, so the #0112 rebind gate is
/// exercised rather than degraded; a test that wants the degradation clears
/// `enumeration_complete` or sets `uidvalidity_reset`.
fn fetch(messages: Vec<(u32, Vec<u8>)>) -> MailboxFetch {
    let mut listed: Vec<u32> = messages.iter().map(|(uid, _)| *uid).collect();
    listed.sort_unstable();
    MailboxFetch {
        messages: messages
            .into_iter()
            .map(|(uid, raw)| FetchedRaw {
                uid,
                raw,
                flags: MessageFlags::default(),
            })
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
fn reset_fetch(messages: Vec<(u32, Vec<u8>)>) -> MailboxFetch {
    MailboxFetch {
        uidvalidity_reset: true,
        ..fetch(messages)
    }
}

fn sent_target() -> Vec<SyncTarget> {
    vec![SyncTarget {
        role: MailboxRole::Sent,
        server_name: "Sent".into(),
    }]
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
// 2. The refusal at the CLI
// ---------------------------------------------------------------------------

/// `mp sync` treats the refusal as a success: exit 0, one line saying the sync
/// was skipped and why, and no summary claiming a pass that never ran.
///
/// The sandbox account points IMAP at a listener this test owns, and the test
/// asserts afterwards that nothing ever connected to it: that is the "no IMAP
/// session" half of the contract, asserted at the CLI rather than through a
/// fake backend. The password is seeded into the sandbox's own encrypted
/// secrets file so a run that does connect gets as far as the socket rather
/// than dying on a missing credential, which leaves the guard's *placement*
/// free (before or after `ImapConfig::load`) and pins only its effect.
///
/// Without the guard this command exits 0 too: a per-target fetch failure is a
/// warning, so today's run prints `✓ Synced: 0 email(s) ingested` after a
/// refused connection. The skip line and the accepted-connection count are what
/// make the test non-vacuous.
#[test]
fn mp_sync_exits_zero_and_says_it_skipped_when_another_process_holds_the_lock() {
    let tmp = tempfile::tempdir().unwrap();
    let config_dir = tmp.path().join("config");
    let data_dir = tmp.path().join("data");
    let account_dir = data_dir.join("accounts").join("acct");
    fs::create_dir_all(&config_dir).unwrap();
    fs::create_dir_all(&account_dir).unwrap();

    // The default backend is the machine-keyed encrypted file under the config
    // directory, which the sandbox owns. A host with no readable machine ID
    // cannot seed it; skip there rather than fail, as
    // `tests/secrets_integration.rs` does.
    let Ok(secrets) = EncryptedFileBackend::open(config_dir.join("secrets.enc")) else {
        return;
    };
    secrets.set("imap-password-acct", "hunter2").unwrap();
    secrets.set("smtp-password-acct", "hunter2").unwrap();

    // A listener the test owns rather than a dead port, so "nothing connected"
    // is a fact this test can check instead of infer. It closes every
    // connection immediately: a client that does connect fails its TLS
    // handshake at once instead of blocking the suite on a server that never
    // answers.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind the sentinel IMAP port");
    let port = listener.local_addr().unwrap().port();
    let connections = Arc::new(AtomicUsize::new(0));
    {
        let seen = Arc::clone(&connections);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                match stream {
                    Ok(stream) => {
                        seen.fetch_add(1, Ordering::SeqCst);
                        drop(stream);
                    }
                    Err(_) => break,
                }
            }
        });
    }

    fs::write(
        config_dir.join("config.toml"),
        format!(
            r#"
[[accounts]]
name = "acct"
default_from = "acct@example.com"

[accounts.imap]
host = "127.0.0.1"
port = {port}
username = "acct@example.com"

[accounts.mailboxes.inbox]
server = "INBOX"
"#
        ),
    )
    .unwrap();
    // A store the sync would have written into, so the run fails on the lock
    // rather than on an absent account directory.
    drop(Store::open(account_dir.join("store.sqlite3")).unwrap());

    let _holder = EngineLock::try_acquire_at(&account_dir.join("store.lock"), "acct")
        .unwrap()
        .expect("the test process holds the lock the child will be refused");

    let out = Command::new(MP)
        .arg("sync")
        .env("HOME", tmp.path())
        .env("MAILYPOPPINS_CONFIG_DIR", &config_dir)
        .env("MAILYPOPPINS_DATA_DIR", &data_dir)
        .env("NO_COLOR", "1")
        .output()
        .expect("mp must run");

    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();

    assert_eq!(
        out.status.code(),
        Some(0),
        "a refused sync is a success, not a failure\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains(&skip_line("acct")),
        "`mp sync` must say why it did nothing; expected {:?}\nstdout: {stdout}",
        skip_line("acct")
    );
    assert!(
        !stdout.contains("Synced:"),
        "a skipped sync must not report a pass it never ran\nstdout: {stdout}"
    );

    assert_eq!(
        connections.load(Ordering::SeqCst),
        0,
        "a refused sync must open no IMAP session, but the sentinel port was connected to"
    );
}

// ---------------------------------------------------------------------------
// 3. The #0112 invariants, driven through the guard
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
    };
    let mut span = TimingSpan::new("engine_lock_ingest");
    let result = mailypoppins::sync::engine::run_sync(&mut backend, &run, &mut span)
        .await
        .expect("the unguarded mechanism takes no lock and cannot be refused");

    assert_eq!(result.saved, 1);
}
