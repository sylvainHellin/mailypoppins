//! The body `message.get` answers for a row whose body blob is gone
//! (PERSO-205).
//!
//! The retention sweep evicts body blobs and keeps every row, on the promise
//! that the store is a cache and an evicted body comes back when its message
//! is opened. It comes back here, in the daemon, so `mp show`, `mp read`, the
//! TUI preview and the desktop reader all get it through the one
//! `message.get`, from the first source that has it:
//!
//! 1. the blobs the sweep never evicts: the raw message of an IMAP row, the
//!    HTML of a Graph row ([`read::rederive_body`]), with no network at all;
//! 2. the server: that one message by its `Message-ID`, through the recent
//!    copies the other server reads share ([`ServerCopies`]).
//!
//! A body that hashes to the row's own `body_blob` is stored again
//! ([`crate::ingest::restore_body`]), so the next open reads it from the store,
//! and it counts against the cap like any blob. Only the opened message is
//! fetched, never its mailbox, so a store held at its cap does not download
//! again everything it evicted, the churn #0060 rejected a sync-side refill for.
//!
//! # The server leg does not hold the answer
//!
//! A reader asks for the body of the message under its cursor, and the daemon
//! answers one connection's requests one at a time, so a `message.get` that
//! waited seconds for a server held every other call of that client behind
//! it: the TUI's frame, which reads the preview before it draws, froze for up
//! to the client's 30-second budget. So the second source runs on a task of
//! its own ([`BodyFetches`]) and `message.get` answers at once with a `null`
//! body and `body_pending`, the sentence a reader shows meanwhile. When the
//! fetch is over, stored or failed, the daemon publishes
//! `message.body_fetched` `{account, row_id}` and a client showing the row
//! reads it again. Its outcome is remembered for [`REMEMBERED_FOR`], so that
//! re-read is answered at once, and so is every selection of the row in that
//! time: moving the cursor back and forth offline asks the server once, not
//! once per move. `mp show` is a one-shot command with nothing else to do,
//! so it asks with `wait_for_body` and the answer waits for the fetch.
//!
//! # Two downloads at a time per account
//!
//! Holding `j` over evicted rows opens one after the other, each a fetch of
//! its own, and each download is a fresh IMAP login or Graph client. Graph
//! throttles a mailbox above about four concurrent requests with a 429, and a
//! throttled fetch would then be remembered as unavailable. So at most
//! [`PER_ACCOUNT`] downloads of one account run at once and the others wait
//! their turn, their rows answering `body_pending` meanwhile like any fetch
//! in progress.
//!
//! When neither source answers (offline, a server error, a message the server
//! no longer has) the body stays `null` and `body_unavailable` carries the
//! sentence a reader shows in its place.
//!
//! [`ServerCopies`]: super::super::server_copies::ServerCopies

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use log::{info, warn};
use tokio::sync::{OnceCell, Semaphore};

use crate::config::AccountConfig;
use crate::secrets::SecretsBackendKind;
use crate::store::read::{self, MessageRow};
use crate::store::{BlobStore, Store};

use super::super::server_copies::ServerCopies;
use super::message_eml::ServerLeg;

/// How long the outcome of a server fetch is answered without asking the
/// server again: long enough that a cursor moving back and forth offline asks
/// once, short enough that a network that came back is tried again soon.
pub const REMEMBERED_FOR: Duration = Duration::from_secs(60);

/// How many fetches are remembered at most: the rows a user opened in the
/// last minute, a body each.
const CAPACITY: usize = 32;

/// How many server downloads of one account's bodies run at once: below the
/// four concurrent requests Graph serves a mailbox before it throttles, with
/// room for the sync and the other server reads beside them.
const PER_ACCOUNT: usize = 2;

/// The sentence a reader shows while the body is on its way from the server.
pub const BODY_PENDING: &str =
    "The body of this message is not available locally and is being fetched from the server";

/// What tells the clients a fetch is over: `message.body_fetched` through the
/// canonical state in the daemon, a counter in a test.
pub type Announce = Arc<dyn Fn(&str, i64) + Send + Sync>;

/// What `message.get` answers for a row whose body blob is gone.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Recovered {
    /// The body, from a blob of the row's own or from the server.
    Body(String),
    /// Asked of the server; `message.body_fetched` says when it is over.
    Pending,
    /// The sentence a reader shows in place of a body that could not be had.
    Unavailable(String),
}

impl From<Result<String, String>> for Recovered {
    fn from(outcome: Result<String, String>) -> Self {
        match outcome {
            Ok(body) => Recovered::Body(body),
            Err(why) => Recovered::Unavailable(why),
        }
    }
}

/// What a server fetch settles to: the body, or why there is none.
type Outcome = Result<String, String>;

/// One row's fetch: running while its cell is empty, remembered once filled.
struct Entry {
    account: String,
    row_id: i64,
    message_id: String,
    started: Instant,
    settled: Option<Instant>,
    outcome: Arc<OnceCell<Outcome>>,
}

/// The server fetches of evicted bodies this daemon is running or ran in
/// the last [`REMEMBERED_FOR`], by account, row and `Message-ID`.
pub struct BodyFetches {
    copies: Arc<ServerCopies>,
    announce: Announce,
    remember: Duration,
    entries: Mutex<VecDeque<Entry>>,
    /// The [`PER_ACCOUNT`] download slots of each account, by name.
    slots: Mutex<HashMap<String, Arc<Semaphore>>>,
}

impl BodyFetches {
    /// The daemon's table, downloading through `copies` and telling the
    /// clients through `announce`.
    pub fn new(copies: Arc<ServerCopies>, announce: Announce) -> Self {
        Self::remembering(REMEMBERED_FOR, copies, announce)
    }

    /// A table that remembers an outcome for `remember`, for the tests that
    /// exercise the expiry.
    pub fn remembering(remember: Duration, copies: Arc<ServerCopies>, announce: Announce) -> Self {
        Self {
            copies,
            announce,
            remember,
            entries: Mutex::new(VecDeque::new()),
            slots: Mutex::new(HashMap::new()),
        }
    }

    /// The download slots of `account`, made on its first fetch.
    fn slots(&self, account: &str) -> Arc<Semaphore> {
        let mut slots = self
            .slots
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Arc::clone(
            slots
                .entry(account.to_string())
                .or_insert_with(|| Arc::new(Semaphore::new(PER_ACCOUNT))),
        )
    }

    /// The cell of `row`'s fetch, and whether it was made by this call, with
    /// every expired entry dropped and the oldest one past [`CAPACITY`]
    /// evicted.
    ///
    /// A fetch still running after ten times [`REMEMBERED_FOR`] is dropped
    /// too, so a task that never settled cannot leave its row pending.
    fn cell(&self, account: &str, row: &MessageRow) -> (Arc<OnceCell<Outcome>>, bool) {
        let mut entries = self.lock();
        let remember = self.remember;
        entries.retain(|entry| match entry.settled {
            Some(at) => at.elapsed() < remember,
            None => entry.started.elapsed() < remember * 10,
        });
        if let Some(entry) = entries.iter().find(|entry| {
            entry.account == account && entry.row_id == row.id && entry.message_id == row.message_id
        }) {
            return (Arc::clone(&entry.outcome), false);
        }
        while entries.len() >= CAPACITY {
            entries.pop_front();
        }
        let outcome = Arc::new(OnceCell::new());
        entries.push_back(Entry {
            account: account.to_string(),
            row_id: row.id,
            message_id: row.message_id.clone(),
            started: Instant::now(),
            settled: None,
            outcome: Arc::clone(&outcome),
        });
        (outcome, true)
    }

    /// Start the remembered time of the fetch behind `outcome`.
    fn settle(&self, outcome: &Arc<OnceCell<Outcome>>) {
        if let Some(entry) = self
            .lock()
            .iter_mut()
            .find(|entry| Arc::ptr_eq(&entry.outcome, outcome))
        {
            entry.settled.get_or_insert_with(Instant::now);
        }
    }

    /// Run `row`'s fetch into `outcome` once one of its account's download
    /// slots is free, or wait for the one already running or waiting, and
    /// announce its end when this call was the one that ran it.
    async fn run(
        &self,
        outcome: &Arc<OnceCell<Outcome>>,
        account: &AccountConfig,
        secrets: SecretsBackendKind,
        row: &MessageRow,
    ) -> Outcome {
        let mut ran = false;
        let settled = outcome
            .get_or_init(|| {
                ran = true;
                let slots = self.slots(&account.name);
                async move {
                    // The semaphore is never closed, so a slot always comes.
                    let _slot = slots.acquire_owned().await;
                    from_server(&self.copies, account, secrets, row).await
                }
            })
            .await
            .clone();
        self.settle(outcome);
        if ran {
            (self.announce)(&account.name, row.id);
        }
        settled
    }

    /// The entries, whatever a panicking holder left: they are a cache, and
    /// a lost one costs a download, never a wrong answer.
    fn lock(&self) -> MutexGuard<'_, VecDeque<Entry>> {
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// What a body recovery needs beside the account: the backend its
/// credentials are in, and the daemon's fetches.
#[derive(Clone, Copy)]
pub(super) struct BodyLeg<'a> {
    pub(super) secrets: SecretsBackendKind,
    pub(super) fetches: &'a Arc<BodyFetches>,
}

/// The body of `row`, whose body blob is gone: read back from a blob of its
/// own at once, else from the server, waited for when `wait` and otherwise
/// [`Recovered::Pending`] while a task of its own asks.
pub(super) async fn recover(
    account: &AccountConfig,
    row: &MessageRow,
    leg: BodyLeg<'_>,
    wait: bool,
) -> Recovered {
    let name = account.name.as_str();
    let local = match open(name) {
        Ok(store) => {
            let blobs = BlobStore::new(crate::config::blobs_dir(name));
            read::rederive_body(&store, &blobs, row.id)
        }
        Err(why) => return Recovered::Unavailable(why),
    };
    if let Some(body) = local {
        keep(name, row, &body);
        return Recovered::Body(body);
    }
    let (outcome, fresh) = leg.fetches.cell(name, row);
    if let Some(settled) = outcome.get() {
        return settled.clone().into();
    }
    if wait {
        return leg
            .fetches
            .run(&outcome, account, leg.secrets, row)
            .await
            .into();
    }
    if fresh {
        let fetches = Arc::clone(leg.fetches);
        let (account, row, secrets) = (account.clone(), row.clone(), leg.secrets);
        // The outcome is in the cell for the re-read the announcement asks
        // for; the task has no one to hand it to.
        tokio::spawn(async move {
            let _remembered = fetches.run(&outcome, &account, secrets, &row).await;
        });
    }
    Recovered::Pending
}

/// The body of `row` as the server sends it again, stored when it is the one
/// the row records, or the sentence a reader shows when it could not be had.
async fn from_server(
    copies: &ServerCopies,
    account: &AccountConfig,
    secrets: SecretsBackendKind,
    row: &MessageRow,
) -> Outcome {
    let server = ServerLeg { secrets, copies };
    let raw = server
        .download(account, &row.message_id, &row.mailbox)
        .await
        .map_err(|e| unavailable(&e.message))?;
    let body = crate::parse::parse_rfc822_to_fetched_email(&raw)
        .map(|email| email.body_text)
        .ok_or_else(|| unavailable("the server's copy does not parse as a message"))?;
    keep(&account.name, row, &body);
    Ok(body)
}

/// Store `body` again for `row` when it is the one the row records; a
/// failure costs the next open another derivation and is only logged.
fn keep(name: &str, row: &MessageRow, body: &str) {
    let kept = Store::open(crate::config::store_path(name)).and_then(|store| {
        let blobs = BlobStore::new(crate::config::blobs_dir(name));
        crate::ingest::restore_body(&store, &blobs, row.id, body)
    });
    match kept {
        Ok(true) => info!(
            "[store] restored the evicted body of {name}:{}/{}",
            row.mailbox, row.uid
        ),
        Ok(false) => info!(
            "[store] the body found again for {name}:{}/{} is not the one stored; shown, not kept",
            row.mailbox, row.uid
        ),
        Err(e) => warn!(
            "[store] restoring the evicted body of {name}:{}/{}: {e:#}",
            row.mailbox, row.uid
        ),
    }
}

fn open(name: &str) -> Result<Store, String> {
    Store::open(crate::config::store_path(name))
        .map_err(|e| unavailable(&format!("opening the store of {name}: {e:#}")))
}

/// The sentence a reader shows in place of a body it could not get back.
fn unavailable(reason: &str) -> String {
    format!(
        "The body of this message is not available locally, and the server did not \
         send it again: {reason}"
    )
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Value};

    use crate::config::AccountConfig;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use std::sync::Mutex;
    use std::time::Duration;

    use crate::daemon::server_copies::{Fetcher, ServerCopies};
    use crate::ingest::{ingest_message, IngestInput};
    use crate::store::blobs::BlobHash;
    use crate::store::sweep::{sweep, SweepOptions};
    use crate::store::{BlobStore, Store};

    use super::{Announce, BodyFetches, BodyLeg, BODY_PENDING, REMEMBERED_FOR};

    /// A plain-text message as a server hands it over.
    const RAW: &[u8] = b"From: Alice <alice@example.com>\r\n\
To: me@example.com\r\n\
Subject: Evicted\r\n\
Date: Wed, 12 Nov 2025 09:00:00 +0100\r\n\
Message-ID: <evicted@example.com>\r\n\
MIME-Version: 1.0\r\n\
Content-Type: text/plain; charset=utf-8\r\n\
\r\n\
The body the retention sweep evicts.\r\n";

    /// One account's store under a temp data root, holding the message of
    /// [`RAW`] in its inbox with the raw message when `with_raw`, and its body
    /// evicted by the retention sweep as a store over its cap does it: a first
    /// over-cap sweep warns, the second evicts.
    struct Evicted {
        _data: crate::config::test_env::DataDirOverride,
        _root: tempfile::TempDir,
        accounts: Vec<AccountConfig>,
        row_id: i64,
        body_hash: String,
        /// Every row of the fixture, [`Self::row_id`] first.
        row_ids: Vec<i64>,
    }

    fn evicted(with_raw: bool) -> Evicted {
        evicted_all(&[RAW], with_raw)
    }

    /// The message numbered `n`, a body and a `Message-ID` of its own.
    fn numbered(n: usize) -> Vec<u8> {
        format!(
            "From: Alice <alice@example.com>\r\n\
             To: me@example.com\r\n\
             Subject: Evicted {n}\r\n\
             Date: Wed, 12 Nov 2025 09:00:00 +0100\r\n\
             Message-ID: <evicted-{n}@example.com>\r\n\
             MIME-Version: 1.0\r\n\
             Content-Type: text/plain; charset=utf-8\r\n\
             \r\n\
             The body number {n} the retention sweep evicts.\r\n"
        )
        .into_bytes()
    }

    /// [`evicted`] with a row for each of `raws`, under uids from 7 up.
    fn evicted_all(raws: &[&[u8]], with_raw: bool) -> Evicted {
        let root = tempfile::tempdir().expect("tempdir");
        let data = crate::config::test_env::DataDirOverride::set(root.path());
        crate::config::create_private_dir_all(crate::config::account_dir("tum")).expect("dir");
        let store = Store::open_account("tum").expect("the store");
        let blobs = BlobStore::new(crate::config::blobs_dir("tum"));
        let mut row_ids = Vec::new();
        for (uid, raw) in (7..).zip(raws) {
            let mut email = crate::parse::parse_rfc822_to_fetched_email(raw).expect("parses");
            email.flags.seen = true;
            let outcome = ingest_message(
                &store,
                &blobs,
                &IngestInput {
                    account: "tum",
                    mailbox: "inbox",
                    uid,
                    email: &email,
                    raw: with_raw.then_some(*raw),
                },
            )
            .expect("ingests");
            row_ids.push(outcome.row_id);
        }
        let row_id = row_ids[0];
        let body_hash = crate::store::read::find_by_id(&store, row_id)
            .expect("reads")
            .expect("the row")
            .body_blob
            .expect("a body blob");

        let policy = crate::config::RetentionPolicy {
            max_disk_bytes: 1,
            ..Default::default()
        };
        let evict = SweepOptions {
            dry_run: false,
            force: true,
        };
        sweep(&store, &blobs, &policy, evict).expect("the first sweep warns");
        sweep(&store, &blobs, &policy, evict).expect("the second sweep evicts");
        let hash = crate::store::blobs::BlobHash::parse(&body_hash).expect("a hash");
        assert!(!blobs.contains(&hash), "the sweep evicted the body");
        Evicted {
            _data: data,
            _root: root,
            accounts: vec![AccountConfig {
                name: "tum".to_string(),
                ..Default::default()
            }],
            row_id,
            body_hash,
            row_ids,
        }
    }

    /// The daemon's side of a test: the recent copies over `fetcher`, the
    /// body fetches over them, and every `message.body_fetched` they
    /// announced, by row.
    struct Daemon {
        fetches: Arc<BodyFetches>,
        announced: Arc<Mutex<Vec<(String, i64)>>>,
        settled: Arc<tokio::sync::Notify>,
    }

    impl Daemon {
        fn over(fetcher: Fetcher) -> Daemon {
            Daemon::remembering(REMEMBERED_FOR, fetcher)
        }

        fn remembering(remember: Duration, fetcher: Fetcher) -> Daemon {
            let announced = Arc::new(Mutex::new(Vec::new()));
            let settled = Arc::new(tokio::sync::Notify::new());
            let announce: Announce = {
                let announced = Arc::clone(&announced);
                let settled = Arc::clone(&settled);
                Arc::new(move |account: &str, row_id| {
                    announced
                        .lock()
                        .expect("the announcements")
                        .push((account.to_string(), row_id));
                    settled.notify_one();
                })
            };
            let copies = Arc::new(ServerCopies::new(fetcher));
            Daemon {
                fetches: Arc::new(BodyFetches::remembering(remember, copies, announce)),
                announced,
                settled,
            }
        }

        fn announced(&self) -> Vec<(String, i64)> {
            self.announced.lock().expect("the announcements").clone()
        }

        /// Until the fetch running in the background announced its end.
        async fn settled(&self) {
            tokio::time::timeout(Duration::from_secs(5), self.settled.notified())
                .await
                .expect("the fetch announced its end");
        }
    }

    impl Evicted {
        /// `message.get` of the row, its body asked for, through `daemon`.
        async fn get(&self, daemon: &Daemon) -> Value {
            self.get_with(daemon, json!({})).await
        }

        /// [`Self::get`] with the parameters of `extra` added.
        async fn get_with(&self, daemon: &Daemon, extra: Value) -> Value {
            self.get_row(daemon, self.row_id, extra).await
        }

        /// `message.get` of `row_id`, with the parameters of `extra`.
        async fn get_row(&self, daemon: &Daemon, row_id: i64, extra: Value) -> Value {
            let leg = BodyLeg {
                secrets: Default::default(),
                fetches: &daemon.fetches,
            };
            let mut params = json!({"account": "tum", "row_id": row_id});
            for (key, value) in extra.as_object().expect("an object") {
                params[key] = value.clone();
            }
            super::super::message::get(&params, &self.accounts, leg)
                .await
                .expect("message.get answers")
        }

        /// Whether the body blob is on disk, and its refcount.
        fn stored(&self) -> (bool, i64) {
            let store = Store::open_account("tum").expect("the store");
            let blobs = BlobStore::new(crate::config::blobs_dir("tum"));
            let hash = BlobHash::parse(&self.body_hash).expect("a hash");
            (
                blobs.contains(&hash),
                crate::store::blobs::refcount(store.conn(), &hash).expect("refcount"),
            )
        }
    }

    /// A fetcher that fails as an offline server does, counting its calls.
    fn offline() -> (Fetcher, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&calls);
        let fetcher: Fetcher = Arc::new(move |_request| {
            counted.fetch_add(1, Ordering::SeqCst);
            Box::pin(async {
                Err(super::super::internal(
                    "connecting to imap.example.com:993: network is unreachable",
                ))
            })
        });
        (fetcher, calls)
    }

    const BODY: &str = "The body the retention sweep evicts.\r\n";

    /// A server that holds every download until the test releases it,
    /// answering the [`numbered`] message the request asks for and counting
    /// the downloads started and the most in flight at once.
    struct Holding {
        release: Arc<tokio::sync::Semaphore>,
        started: Arc<AtomicUsize>,
        most: Arc<AtomicUsize>,
    }

    fn holding() -> (Fetcher, Holding) {
        let release = Arc::new(tokio::sync::Semaphore::new(0));
        let started = Arc::new(AtomicUsize::new(0));
        let most = Arc::new(AtomicUsize::new(0));
        let in_flight = Arc::new(AtomicUsize::new(0));
        let server = Holding {
            release: Arc::clone(&release),
            started: Arc::clone(&started),
            most: Arc::clone(&most),
        };
        let fetcher: Fetcher = Arc::new(move |request| {
            let (release, started) = (Arc::clone(&release), Arc::clone(&started));
            let (most, in_flight) = (Arc::clone(&most), Arc::clone(&in_flight));
            Box::pin(async move {
                started.fetch_add(1, Ordering::SeqCst);
                let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                most.fetch_max(now, Ordering::SeqCst);
                release.acquire().await.expect("open").forget();
                in_flight.fetch_sub(1, Ordering::SeqCst);
                let n = (0..)
                    .find(|n| request.message_id.contains(&format!("evicted-{n}@")))
                    .expect("a numbered message");
                Ok(numbered(n))
            })
        });
        (fetcher, server)
    }

    /// Until `done` holds, polled while the daemon's tasks run.
    async fn until(what: &str, done: impl Fn() -> bool) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !done() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{what}"));
    }

    /// Opening a message whose body the sweep evicted shows its body: the
    /// row's raw message is never evicted, so the body is read back from it
    /// with no server at all, and stored again under the row's own hash with
    /// its reference, so the next open reads the store. The row's flags are
    /// left as they were.
    #[tokio::test]
    async fn an_evicted_body_is_back_when_the_message_is_opened() {
        let fx = evicted(true);
        let (fetcher, downloads) = offline();
        let daemon = Daemon::over(fetcher);

        let shown = fx.get(&daemon).await;
        assert_eq!(shown["body"], json!(BODY), "{shown}");
        assert!(shown.get("body_unavailable").is_none(), "{shown}");
        assert!(shown.get("body_pending").is_none(), "{shown}");
        assert_eq!(shown["flags"], json!(["read"]), "{shown}");
        assert_eq!(fx.stored(), (true, 1), "the body is stored again");
        assert_eq!(fx.get(&daemon).await["body"], json!(BODY));
        assert_eq!(downloads.load(Ordering::SeqCst), 0, "no server leg");
        assert!(daemon.announced().is_empty(), "nothing to announce");
    }

    /// A row with no blob to read its body back from (a Graph message sent as
    /// plain text keeps no HTML) asks the server for that one message, once,
    /// without holding the answer: `message.get` says the body is on its way,
    /// `message.body_fetched` says when it is stored, and the next open reads
    /// it from the store.
    #[tokio::test]
    async fn a_body_no_blob_holds_is_fetched_from_the_server_once() {
        let fx = evicted(false);
        let (fetcher, downloads) = crate::daemon::server_copies::tests::counting(RAW);
        let daemon = Daemon::over(fetcher);

        let shown = fx.get(&daemon).await;
        assert!(shown["body"].is_null(), "{shown}");
        assert_eq!(shown["body_pending"], json!(BODY_PENDING), "{shown}");
        assert!(shown.get("body_unavailable").is_none(), "{shown}");
        daemon.settled().await;
        assert_eq!(daemon.announced(), vec![("tum".to_string(), fx.row_id)]);
        assert_eq!(fx.stored(), (true, 1), "the body is stored again");

        let shown = fx.get(&daemon).await;
        assert_eq!(shown["body"], json!(BODY), "{shown}");
        assert!(shown.get("body_pending").is_none(), "{shown}");
        // A fresh daemon, so a second download could not be a remembered one.
        let (fetcher, again) = crate::daemon::server_copies::tests::counting(RAW);
        assert_eq!(fx.get(&Daemon::over(fetcher)).await["body"], json!(BODY));
        assert_eq!(downloads.load(Ordering::SeqCst), 1);
        assert_eq!(again.load(Ordering::SeqCst), 0);
    }

    /// `wait_for_body`, which `mp show` sends, answers the body the server
    /// sent once it is in, with nothing pending.
    #[tokio::test]
    async fn waiting_for_the_body_answers_it() {
        let fx = evicted(false);
        let (fetcher, downloads) = crate::daemon::server_copies::tests::counting(RAW);
        let daemon = Daemon::over(fetcher);

        let shown = fx.get_with(&daemon, json!({"wait_for_body": true})).await;
        assert_eq!(shown["body"], json!(BODY), "{shown}");
        assert!(shown.get("body_pending").is_none(), "{shown}");
        assert_eq!(downloads.load(Ordering::SeqCst), 1);
        assert_eq!(fx.stored(), (true, 1), "the body is stored again");
    }

    /// Holding `j` over evicted rows opens one after the other, and each
    /// asks the server for its body; a mailbox serves at most two of those
    /// downloads at once (Graph throttles above about four concurrent
    /// requests, and a throttled fetch is remembered as unavailable), the
    /// others wait their turn while their readers are told the body is on
    /// its way, and every body arrives once the server answers.
    #[tokio::test]
    async fn at_most_two_bodies_are_fetched_at_once_per_account() {
        let raws: Vec<Vec<u8>> = (0..5).map(numbered).collect();
        let raws: Vec<&[u8]> = raws.iter().map(Vec::as_slice).collect();
        let fx = evicted_all(&raws, false);
        let (fetcher, server) = holding();
        let daemon = Daemon::over(fetcher);

        for &row_id in &fx.row_ids {
            let shown = fx.get_row(&daemon, row_id, json!({})).await;
            assert_eq!(shown["body_pending"], json!(BODY_PENDING), "{shown}");
        }
        until("two downloads started", || {
            server.started.load(Ordering::SeqCst) >= 2
        })
        .await;
        // Time for any further download to start, were it allowed to.
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(server.started.load(Ordering::SeqCst), 2, "two at a time");
        for &row_id in &fx.row_ids {
            let shown = fx.get_row(&daemon, row_id, json!({})).await;
            assert_eq!(shown["body_pending"], json!(BODY_PENDING), "{shown}");
        }

        server.release.add_permits(fx.row_ids.len());
        until("every fetch announced its end", || {
            daemon.announced().len() == fx.row_ids.len()
        })
        .await;
        assert_eq!(server.started.load(Ordering::SeqCst), 5);
        assert_eq!(server.most.load(Ordering::SeqCst), 2, "never more than two");
        for (n, &row_id) in fx.row_ids.iter().enumerate() {
            let shown = fx.get_row(&daemon, row_id, json!({})).await;
            let body = format!("The body number {n} the retention sweep evicts.\r\n");
            assert_eq!(shown["body"], json!(body), "{shown}");
        }
    }

    /// Offline, the body stays `null` and the reader is told why in a
    /// sentence of its own once the fetch is over, never handed an empty
    /// body; nothing is stored. The failure is remembered, so opening the row
    /// again answers the sentence at once and asks the server nothing, until
    /// the remembered time is up.
    #[tokio::test]
    async fn an_unreachable_server_answers_a_sentence_and_no_body() {
        let fx = evicted(false);
        let (fetcher, downloads) = offline();
        let daemon = Daemon::over(fetcher);

        assert!(fx.get(&daemon).await.get("body_pending").is_some());
        daemon.settled().await;
        for _ in 0..3 {
            let shown = fx.get(&daemon).await;
            assert!(shown["body"].is_null(), "{shown}");
            assert!(shown.get("body_pending").is_none(), "{shown}");
            let why = shown["body_unavailable"].as_str().expect("a reason");
            assert!(why.contains("not available locally"), "{why}");
            assert!(why.contains("network is unreachable"), "{why}");
        }
        let waited = fx.get_with(&daemon, json!({"wait_for_body": true})).await;
        assert!(waited["body_unavailable"].is_string(), "{waited}");
        assert_eq!(downloads.load(Ordering::SeqCst), 1, "asked once");
        assert_eq!(daemon.announced().len(), 1);
        assert_eq!(fx.stored(), (false, 0), "nothing is stored");

        // Past the remembered time the server is asked again.
        let (fetcher, again) = offline();
        let forgetful = Daemon::remembering(Duration::ZERO, fetcher);
        assert!(fx.get(&forgetful).await.get("body_pending").is_some());
        forgetful.settled().await;
        assert!(fx.get(&forgetful).await.get("body_pending").is_some());
        forgetful.settled().await;
        assert_eq!(again.load(Ordering::SeqCst), 2);
    }
}
