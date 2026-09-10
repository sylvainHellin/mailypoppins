//! The seeded root the Phase 4 send slice is measured against (plan P4-U11).
//!
//! `mp send [-y]`, `mp send --invite …`, `mp send-approved [-y]
//! [--all-accounts]` and `mp outbox list|retry|discard` all answer about one
//! account's drafts and one account's outbox table, so they are all compared
//! against one fixture. It is [`draft_fixture`] - itself [`read_fixture`] plus
//! a drafts directory - with three additions the send slice needs and the
//! earlier slices do not:
//!
//! | axis | where |
//! |---|---|
//! | an account whose transport is Microsoft Graph (`ANO-4`, `SND-05`) | [`GRAPH_ACCOUNT`] |
//! | an account with an SMTP host and no credentials | [`SMTP_ACCOUNT`] |
//! | a seeded outbox in every state `mp outbox list` renders | [`seed_outbox`] |
//!
//! Building on `draft_fixture` rather than beside it keeps one definition of
//! "the valid draft" and "the approved draft" across the slices, and the
//! additions are made here so no listing, dump or search expectation the read
//! and draft slices pin has to move.
//!
//! # No SMTP server, no IMAP server, and what that costs
//!
//! `rg -n 'smtp|TcpListener|MockSmtp' tests/*.rs tests/support/*.rs` finds no
//! server: `tests/outbox_integration.rs` fakes the *Sent mailbox* behind the
//! [`mailypoppins::outbox::SentMailbox`] trait and drives
//! `outbox::drain_guarded_at` in process, and `tests/imip_integration.rs` never
//! sends at all - it parses and ingests. So the premise "reuse the legacy
//! suites' SMTP fake" has nothing to reuse, and the repository's transport
//! (`send::build_smtp_transport`) is TLS-only on both of its branches: implicit
//! TLS on 465, `Tls::Required` STARTTLS otherwise. A plaintext `TcpListener`
//! cannot serve it and a TLS one would need a certificate generator this tree
//! does not depend on.
//!
//! What this fixture does instead is [`FAKE_TRANSPORT_ENV`]: a daemon-side
//! hook, in the shape of the `MAILYPOPPINS_DAEMON_FAKE_SYNC_OUTCOME` hook
//! `tests/daemon_sync_outcome.rs` and `tests/daemon_sync_slice.rs` already use,
//! that serves the SMTP submission and the Sent-mailbox APPEND in process and
//! records every one of them in a log file. That is what makes a *successful*
//! routed send, the partly-delivered outcome (`SND-08`), the sent-copy append
//! (`SND-09`) and the racing-drain twin non-vacuous.
//!
//! The hook lives in the **daemon**, so the pre-daemon oracle cannot see it and
//! no row that uses it is a parity row. Every such row is a routed-side
//! assertion and says so, exactly as `tests/daemon_sync_slice.rs`'s
//! engine-lock row does.
//!
//! # Determinism
//!
//! Two things a send mints are not reproducible: the `Message-ID` (`send.rs`
//! derives it from a uuid and the clock) and an invitation's `UID`
//! (`invite::generate_uid`). Neither is seeded here; the test file masks the
//! one line each of them reaches and says so. Everything the fixture itself
//! writes is fixed: the outbox rows are enqueued in a known order, so their
//! ids are 1..4, their `Message-ID`s are literals, and their `updated` stamps
//! are backdated to [`FIXED_UPDATED`] so `mp outbox list`'s
//! `YYYY-MM-DD HH:MM` column is the same in both binaries and on every run.

#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};

use mailypoppins::outbox::{self, Envelope, OutboxState, RecipientVerdicts, SubmitOutcome};
use mailypoppins::send::RecipientRole;
use mailypoppins::store::{BlobStore, Store};

use super::draft_fixture;
use super::read_fixture;

// ---------------------------------------------------------------------------
// The accounts
// ---------------------------------------------------------------------------

/// The default account: first in the file, holds every seeded draft and the
/// seeded outbox, and configures no server at all.
pub const ACCOUNT: &str = read_fixture::ACCOUNT;

/// The second configured account, reachable only by naming it.
pub const OTHER_ACCOUNT: &str = read_fixture::OTHER_ACCOUNT;

/// The configured account with no store on disk, so its outbox has never
/// existed.
pub const STORELESS_ACCOUNT: &str = read_fixture::STORELESS_ACCOUNT;

/// The account whose transport is Microsoft Graph: the one `mp send --invite`
/// refuses outright (`ANO-4`) and the one whose `mp send` preview is the
/// simplified block rather than the draft preview.
pub const GRAPH_ACCOUNT: &str = "graph";

/// The account with an SMTP host configured and no credentials for it, so an
/// invitation gets as far as the confirmation prompt.
pub const SMTP_ACCOUNT: &str = "gamma";

/// An account name no configuration carries.
pub const UNKNOWN_ACCOUNT: &str = read_fixture::UNKNOWN_ACCOUNT;

/// Every configured account, in configuration order, which is the order
/// `mp send-approved --all-accounts` walks them in.
pub const ALL_ACCOUNTS: [&str; 5] = [
    ACCOUNT,
    OTHER_ACCOUNT,
    STORELESS_ACCOUNT,
    GRAPH_ACCOUNT,
    SMTP_ACCOUNT,
];

/// The accounts that have a drafts directory on disk.
pub const SEEDED_ACCOUNTS: [&str; 3] = [ACCOUNT, OTHER_ACCOUNT, GRAPH_ACCOUNT];

// ---------------------------------------------------------------------------
// The drafts
// ---------------------------------------------------------------------------

/// A valid `draft`-status draft: recipient, subject and body all present.
pub const VALID: &str = draft_fixture::VALID;

/// The one draft in `approved` status, which is what `mp send-approved` picks
/// up.
pub const APPROVED: &str = draft_fixture::APPROVED;

/// A draft that parses and does not validate: no subject.
pub const NO_SUBJECT: &str = draft_fixture::NO_SUBJECT;

/// The draft only [`OTHER_ACCOUNT`] holds, for a cross-account selector.
pub const BETA_DRAFT: &str = draft_fixture::BETA_DRAFT;

/// An id in the minted shape that nothing resolves to.
pub const UNKNOWN_DRAFT: &str = draft_fixture::UNKNOWN_ID;

/// The approved draft of [`GRAPH_ACCOUNT`], so the Graph preview has something
/// to preview.
pub const GRAPH_DRAFT: &str = "a1000000000000c1";

/// The canonical selector of a draft, which is the line `mp send` echoes.
pub fn selector(account: &str, id: &str) -> String {
    draft_fixture::selector(account, id)
}

/// `<root>/accounts/<account>/drafts`.
pub fn drafts_dir(root: &Path, account: &str) -> PathBuf {
    draft_fixture::drafts_dir(root, account)
}

/// `<root>/accounts/<account>`.
pub fn account_dir(root: &Path, account: &str) -> PathBuf {
    read_fixture::account_dir(root, account)
}

/// One account's fixture store, opened by the test itself.
pub fn store(root: &Path, account: &str) -> Store {
    read_fixture::store(root, account)
}

/// One account's blob store, laid out where the real account directory keeps
/// it.
pub fn blobs(root: &Path, account: &str) -> BlobStore {
    BlobStore::new(account_dir(root, account).join("blobs"))
}

/// `<root>/accounts/<account>/store.lock`, the file an engine holds and the
/// file a drain takes before it appends.
pub fn engine_lock_path(root: &Path, account: &str) -> PathBuf {
    account_dir(root, account).join("store.lock")
}

// ---------------------------------------------------------------------------
// The outbox
// ---------------------------------------------------------------------------

/// The `Message-ID` of the row that was committed and never submitted, which
/// `mp outbox list` annotates with "never submitted".
pub const QUEUED_MID: &str = "<queued@example.com>";

/// The `Message-ID` of the row an ambiguous SMTP failure parked.
pub const FAILED_MID: &str = "<parked@example.com>";

/// The `Message-ID` of the partly delivered row: one recipient has it, one
/// never will (`SND-08`).
pub const PARTIAL_MID: &str = "<partial@example.com>";

/// The `Message-ID` of the row whose SMTP is done and whose Sent copy is not
/// filed yet (`SND-09`).
pub const APPENDING_MID: &str = "<appending@example.com>";

/// The row ids the four seeded rows take in a fresh store, in enqueue order.
pub const QUEUED_ROW: i64 = 1;
pub const FAILED_ROW: i64 = 2;
pub const PARTIAL_ROW: i64 = 3;
pub const APPENDING_ROW: i64 = 4;

/// A row id nothing in the fixture holds.
pub const UNKNOWN_ROW: i64 = 99;

/// The Sent mailbox the appending row targets.
pub const SENT_MAILBOX: &str = "Sent";

/// The recipient every seeded row delivers to.
pub const TO: &str = "bob@example.com";

/// The recipient the partly delivered row's server refused for good.
pub const REJECTED: &str = "carol@example.com";

/// The reason it gave, which `mp outbox list` prints in brackets.
pub const REJECTION: &str = "550 5.1.1 unknown recipient";

/// The sentence an ambiguous SMTP failure left on the parked row.
pub const AMBIGUOUS: &str = "connection reset while waiting for the 250";

/// Every seeded row's `updated`, so `mp outbox list`'s time column is a
/// literal rather than "now".
///
/// 2026-01-02 03:04:05 UTC. The column is rendered in *local* time, which is
/// the same local time for both binaries in one test run, so the value never
/// has to be named - only fixed.
pub const FIXED_UPDATED: i64 = 1_767_322_845;

// ---------------------------------------------------------------------------
// The fake transport
// ---------------------------------------------------------------------------

/// The daemon-side hook that serves SMTP and the Sent-mailbox APPEND in
/// process.
///
/// Its value is a JSON object; every key is optional:
///
/// ```json
/// {
///   "log": "<path>",                       // one line per transport event
///   "reject": {"carol@example.com": "550 …"},
///   "append": "ok" | "fail" | "swallow_ack",
///   "append_delay_ms": 0
/// }
/// ```
///
/// - `log` is opened `O_APPEND` per event, so two concurrent drains cannot
///   lose a line and the count is the ledger `tests/outbox_integration.rs`
///   keeps in memory.
/// - `reject` fails exactly those recipients, with the reason given, and
///   accepts the rest: that is the partly-delivered outcome (`SND-08`) without
///   a server.
/// - `append` decides what the Sent-mailbox APPEND does. `swallow_ack` files
///   the copy and reports a failure, which is the ambiguity the dedup search
///   exists for.
/// - `append_delay_ms` parks inside the APPEND, which is the window a second
///   drain would slip into.
///
/// Set on the **daemon** (`DaemonFixture::start_with`), never on the client:
/// after P4-U12 the client owns no transport.
pub const FAKE_TRANSPORT_ENV: &str = "MAILYPOPPINS_DAEMON_FAKE_TRANSPORT";

/// One event the fake transport recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportEvent {
    /// One recipient's `RCPT TO`/`DATA` verdict.
    Submit {
        message_id: String,
        address: String,
        accepted: bool,
    },
    /// One `APPEND` request that reached the fake Sent mailbox.
    Append { mailbox: String, message_id: String },
    /// One dedup `SEARCH HEADER Message-ID`.
    Search { mailbox: String, message_id: String },
}

/// The hook's value for a transport that accepts everything and files the
/// copy, writing its ledger to `log`.
pub fn fake_transport(log: &Path) -> String {
    serde_json::json!({"log": log.to_string_lossy()}).to_string()
}

/// The same, with `reject` naming the recipients the server refuses for good.
pub fn fake_transport_rejecting(log: &Path, rejected: &[(&str, &str)]) -> String {
    let mut map = serde_json::Map::new();
    for (address, reason) in rejected {
        map.insert((*address).to_string(), serde_json::json!(reason));
    }
    serde_json::json!({"log": log.to_string_lossy(), "reject": map}).to_string()
}

/// The same, with the APPEND behaving as `append` says and parking for
/// `delay_ms` inside the request.
pub fn fake_transport_appending(log: &Path, append: &str, delay_ms: u64) -> String {
    serde_json::json!({
        "log": log.to_string_lossy(),
        "append": append,
        "append_delay_ms": delay_ms,
    })
    .to_string()
}

/// The ledger file the fake transport writes, under a root the test owns.
pub fn transport_log(root: &Path) -> PathBuf {
    root.join("transport.log")
}

/// Every event the fake transport recorded, in the order it wrote them.
///
/// The line format is the hook's contract: a kind, then its fields, separated
/// by single spaces, one line per event, no field containing a space.
pub fn transport_events(log: &Path) -> Vec<TransportEvent> {
    let text = match fs::read_to_string(log) {
        Ok(text) => text,
        Err(_) => return Vec::new(),
    };
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let field: Vec<&str> = line.split_whitespace().collect();
            match field.as_slice() {
                ["submit", message_id, address, verdict] => TransportEvent::Submit {
                    message_id: (*message_id).to_string(),
                    address: (*address).to_string(),
                    accepted: *verdict == "accepted",
                },
                ["append", mailbox, message_id] => TransportEvent::Append {
                    mailbox: (*mailbox).to_string(),
                    message_id: (*message_id).to_string(),
                },
                ["search", mailbox, message_id] => TransportEvent::Search {
                    mailbox: (*mailbox).to_string(),
                    message_id: (*message_id).to_string(),
                },
                _ => panic!("the fake transport wrote a line nothing parses: {line:?}"),
            }
        })
        .collect()
}

/// How many `APPEND`s the fake transport served for `message_id`.
pub fn appends_of(log: &Path, message_id: &str) -> usize {
    transport_events(log)
        .into_iter()
        .filter(|event| matches!(event, TransportEvent::Append { message_id: mid, .. } if mid == message_id))
        .count()
}

/// How many `APPEND`s the fake transport served in total.
pub fn appends(log: &Path) -> usize {
    transport_events(log)
        .into_iter()
        .filter(|event| matches!(event, TransportEvent::Append { .. }))
        .count()
}

/// How many dedup searches it served.
pub fn searches(log: &Path) -> usize {
    transport_events(log)
        .into_iter()
        .filter(|event| matches!(event, TransportEvent::Search { .. }))
        .count()
}

// ---------------------------------------------------------------------------
// Seeding
// ---------------------------------------------------------------------------

/// The two accounts this fixture adds to [`read_fixture::CONFIG`], appended so
/// `alpha` is still first and still the default.
pub const EXTRA_ACCOUNTS: &str = r#"
[[accounts]]
name = "graph"
default_from = "graph@example.com"
auth_method = "graph"

[accounts.oauth2]
client_id = "11111111-1111-1111-1111-111111111111"
tenant_id = "22222222-2222-2222-2222-222222222222"

[[accounts]]
name = "gamma"
default_from = "gamma@example.com"

[accounts.smtp]
host = "127.0.0.1"
port = 9
username = "gamma@example.com"
"#;

/// Write the configuration, the stores, the drafts and the outbox under
/// `root`.
///
/// Call it before starting a daemon against the same root: the daemon loads
/// `config.toml` once, at startup.
pub fn seed(root: &Path) {
    draft_fixture::seed(root);
    fs::write(
        root.join("config.toml"),
        format!("{}{EXTRA_ACCOUNTS}", read_fixture::CONFIG),
    )
    .expect("write config.toml");

    // A store per added account, so a refusal is about the command rather than
    // about an account directory that is not there.
    for account in [GRAPH_ACCOUNT, SMTP_ACCOUNT] {
        let dir = account_dir(root, account);
        fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("create {}: {e}", dir.display()));
        drop(Store::open(dir.join("store.sqlite3")).expect("open the added account's store"));
    }

    seed_graph_draft(root);
    seed_outbox(root);
}

/// One approved draft for [`GRAPH_ACCOUNT`], so `mp send` over a Graph
/// transport has a draft to preview and `mp send-approved` has a batch.
fn seed_graph_draft(root: &Path) {
    let dir = drafts_dir(root, GRAPH_ACCOUNT);
    fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("create {}: {e}", dir.display()));
    let path = dir.join("graph.md");
    fs::write(
        &path,
        draft_fixture::document(
            GRAPH_DRAFT,
            "robin@example.com",
            "Über Graph",
            "approved",
            "Sent through Graph.\n",
        ),
    )
    .unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
}

/// The four outbox rows of [`ACCOUNT`], one per line shape `mp outbox list`
/// renders.
///
/// The order is the id order the listing prints, and every transition is made
/// through `outbox`'s own API rather than by SQL, so a row's encoded envelope
/// is the encoding the reader expects rather than this fixture's guess at it.
/// Only `updated` is written by hand, and only to make the time column a
/// literal.
pub fn seed_outbox(root: &Path) {
    let store = store(root, ACCOUNT);
    let blobs = blobs(root, ACCOUNT);

    // 1. Committed, never submitted: the row `mp outbox list` says the next
    //    sync will send.
    enqueue(&store, &blobs, QUEUED_MID, Some(SENT_MAILBOX));

    // 2. A submission that died without a verdict: parked, never re-sent
    //    automatically, and the only row `mp outbox retry` may re-arm.
    let failed = enqueue(&store, &blobs, FAILED_MID, Some(SENT_MAILBOX));
    outbox::mark_submission_started(&store, failed).expect("mark the parked row");
    outbox::record_submission(
        &store,
        &blobs,
        failed,
        &SubmitOutcome::Ambiguous(AMBIGUOUS.to_string()),
    )
    .expect("park the row");

    // 3. Partly delivered (SND-08): one recipient has it, one never will. No
    //    target mailbox, so the row completed on the 250 and is `done` - and
    //    stays listed because it kept a note.
    let partial = enqueue(&store, &blobs, PARTIAL_MID, None);
    outbox::record_submission(
        &store,
        &blobs,
        partial,
        &SubmitOutcome::PerRecipient(RecipientVerdicts {
            delivered: vec![TO.to_string()],
            rejected: vec![(REJECTED.to_string(), REJECTION.to_string())],
            ..Default::default()
        }),
    )
    .expect("record the partial delivery");

    // 4. SMTP done, Sent copy outstanding (SND-09).
    let appending = enqueue(&store, &blobs, APPENDING_MID, Some(SENT_MAILBOX));
    outbox::record_submission(&store, &blobs, appending, &SubmitOutcome::Accepted)
        .expect("accept the appending row");

    backdate(&store);
}

/// One queued row with the fixture's envelope.
fn enqueue(store: &Store, blobs: &BlobStore, message_id: &str, target: Option<&str>) -> i64 {
    outbox::enqueue(
        store,
        blobs,
        ACCOUNT,
        target,
        message_id,
        &raw(message_id),
        &envelope(),
    )
    .unwrap_or_else(|e| panic!("enqueue {message_id}: {e}"))
}

/// The envelope every seeded row carries: one recipient who gets it and one
/// the partial row's server refuses.
fn envelope() -> Envelope {
    Envelope {
        from: "alpha@example.com".to_string(),
        recipients: vec![
            (TO.to_string(), RecipientRole::To),
            (REJECTED.to_string(), RecipientRole::To),
        ],
        ..Default::default()
    }
}

/// The bytes a row holds, which nothing in this slice prints.
fn raw(message_id: &str) -> Vec<u8> {
    format!(
        "From: alpha@example.com\r\n\
         To: {TO}\r\n\
         Subject: Queued\r\n\
         Date: Mon, 01 Jan 2024 12:00:00 +0000\r\n\
         Message-ID: {message_id}\r\n\
         \r\n\
         Body.\r\n"
    )
    .into_bytes()
}

/// Put every seeded row's `updated` at [`FIXED_UPDATED`].
///
/// `mp outbox list` renders it as `YYYY-MM-DD HH:MM`, so a row stamped "now"
/// would make the listing differ between the two binaries whenever a run
/// crossed a minute boundary.
fn backdate(store: &Store) {
    store
        .conn()
        .execute(
            "UPDATE outbox SET updated = ?1, created = ?1 WHERE account = ?2",
            rusqlite::params![FIXED_UPDATED, ACCOUNT],
        )
        .expect("backdate the seeded outbox rows");
}

/// The state of one row now, or `None` when it is gone.
pub fn row_state(root: &Path, account: &str, id: i64) -> Option<OutboxState> {
    let store = store(root, account);
    outbox::load(&store, id)
        .expect("load an outbox row")
        .map(|row| row.state)
}

/// Every row id `mp outbox list` would show for `account`, in listing order.
pub fn listed_rows(root: &Path, account: &str) -> Vec<i64> {
    let store = store(root, account);
    outbox::unfinished_rows(&store, account)
        .expect("list the unfinished rows")
        .into_iter()
        .map(|row| row.id)
        .collect()
}

/// The `status:` line of one draft file, which is what a send moves to `sent`.
pub fn draft_status(root: &Path, account: &str, file: &str) -> String {
    let path = drafts_dir(root, account).join(file);
    let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    text.lines()
        .find_map(|line| line.strip_prefix("status: "))
        .unwrap_or_else(|| panic!("{} has no status: line", path.display()))
        .trim()
        .to_string()
}

// ---------------------------------------------------------------------------
// Pristine runs
// ---------------------------------------------------------------------------

/// A copy of every account directory, restorable in place.
///
/// Half of this slice writes: a send retires a draft, enqueues an outbox row
/// and releases a blob; a discard deletes a row. A parity comparison of a
/// writing command therefore cannot run the two binaries one after the other
/// over one root - the second would see the first one's work - and it cannot
/// use two roots either, because the paths in the output would differ.
///
/// So the whole `accounts/` tree is copied before a run and put back
/// afterwards. Restoring puts modification times back too and touches only the
/// files that changed, for the reason `draft_fixture::Stash` gives: the drafts
/// index orders by `mtime DESC, id ASC`, and a restore that rewrote every file
/// would hand the second binary a different order than the first one saw.
///
/// **The daemon must not be running while `restore` runs.** It holds the
/// SQLite store open in WAL mode, and putting a different `store.sqlite3`
/// underneath an open connection is undefined. The test file stops it and
/// starts it again around each mutating comparison.
pub struct Pristine {
    root: PathBuf,
    files: std::collections::BTreeMap<PathBuf, Saved>,
}

struct Saved {
    bytes: Vec<u8>,
    modified: std::time::SystemTime,
}

impl Pristine {
    /// Take a copy of `root`'s `accounts/` tree.
    pub fn take(root: &Path) -> Pristine {
        let mut files = std::collections::BTreeMap::new();
        for path in walk(&root.join("accounts")) {
            let bytes = fs::read(&path).unwrap_or_else(|e| panic!("stash {}: {e}", path.display()));
            let modified = fs::metadata(&path)
                .and_then(|meta| meta.modified())
                .unwrap_or_else(|e| panic!("stat {}: {e}", path.display()));
            files.insert(path, Saved { bytes, modified });
        }
        Pristine {
            root: root.to_path_buf(),
            files,
        }
    }

    /// Put the tree back the way [`Pristine::take`] found it.
    pub fn restore(&self) {
        for path in walk(&self.root.join("accounts")) {
            if !self.files.contains_key(&path) {
                fs::remove_file(&path).unwrap_or_else(|e| panic!("remove {}: {e}", path.display()));
            }
        }
        for (path, saved) in &self.files {
            if fs::read(path).is_ok_and(|current| current == saved.bytes) {
                continue;
            }
            let parent = path.parent().expect("a stashed path has a parent");
            fs::create_dir_all(parent)
                .unwrap_or_else(|e| panic!("create {}: {e}", parent.display()));
            fs::write(path, &saved.bytes)
                .unwrap_or_else(|e| panic!("restore {}: {e}", path.display()));
            let file = fs::OpenOptions::new()
                .write(true)
                .open(path)
                .unwrap_or_else(|e| panic!("reopen {}: {e}", path.display()));
            let times = fs::FileTimes::new().set_modified(saved.modified);
            file.set_times(times)
                .unwrap_or_else(|e| panic!("restore the mtime of {}: {e}", path.display()));
        }
    }
}

/// Every file below `dir`, in path order, or nothing when it is not there.
fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(walk(&path));
        } else {
            out.push(path);
        }
    }
    out.sort();
    out
}

// ---------------------------------------------------------------------------
// The refusals, verbatim
// ---------------------------------------------------------------------------

/// `mp send` with neither a selector nor `--invite`.
pub const SEND_NEEDS_A_SELECTOR: &str = "`mp send` needs a draft selector, or use `--invite` to \
                                         send a calendar invitation";

/// `mp send --invite` on a Graph account: the refusal `ANO-4` records, made
/// before anything else about the invitation is looked at.
pub const GRAPH_INVITE_REFUSAL: &str =
    "`mp send --invite` is not supported for Graph accounts yet (Graph calendar send is tracked \
     by #0036, blocked on #0035). Use an SMTP-configured account.";

/// `mp send --invite` with no `--subject`.
pub const INVITE_NEEDS_A_SUBJECT: &str = "--invite requires --subject (used as the event summary)";

/// `mp send --invite` with no `--start`.
pub const INVITE_NEEDS_A_START: &str = "--invite requires --start";

/// `mp send --invite` with neither `--to` nor `--cc`.
pub const INVITE_NEEDS_A_RECIPIENT: &str = "--invite requires at least one recipient via --to/--cc";

/// The prompt `mp send` and `mp send-approved` print, and the answer a closed
/// stdin gives.
pub const SEND_PROMPT: &str = "Send this email? [y/N] ";

/// The prompt `mp send --invite` prints.
pub const INVITE_PROMPT: &str = "Send this invitation? [y/N] ";

/// What either of them prints when the answer is not yes.
pub const CANCELLED: &str = "Cancelled.";

/// `mp outbox <anything>` for an account whose store was never created.
pub fn never_queued_line(account: &str) -> String {
    format!("  · nothing has been queued for {account} yet")
}

/// `mp outbox list` for an account whose outbox has nothing to say.
pub fn outbox_clear_line(account: &str) -> String {
    format!("  ✓ the outbox for {account} is clear")
}

/// `mp outbox discard` naming a row that is not there.
pub fn no_such_row(id: i64) -> String {
    format!("no outbox row {id}")
}

/// `mp send-approved` for an account with no approved draft.
pub fn no_approved_drafts(account: &str) -> String {
    format!("No approved drafts for {account}")
}

/// The summary line `mp send-approved` ends each account with.
pub fn approved_summary(account: &str, sent: usize, failed: usize) -> String {
    format!("Summary {account}: {sent} sent, {failed} failed")
}
