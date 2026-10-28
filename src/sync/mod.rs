//! The transport-independent half of the receive path: the sync types every
//! backend speaks, and the [`SyncBackend`] seam the engine is driven through.
//!
//! Before #0059 these types lived in `imap_client`, and `graph.rs` imported
//! them from there: the abstraction existed, spelled as a dependency on the
//! other transport's module. They live here now, and the orchestration that
//! consumes them lives in [`engine`], so it can be driven offline by a fake
//! backend in tests (`engine`'s test module) instead of only against a server.
//!
//! What the seam is *for*, beyond the tests:
//!
//! - #0041 wants one IMAP session held across mailboxes and a CONDSTORE
//!   `HIGHESTMODSEQ` remembered between passes. Both are backend state, and
//!   the trait takes `&mut self` so a backend may keep them.
//! - #0042 wants Graph's `deltaLink` remembered between passes, which is
//!   the same shape of state behind the same `&mut self`.
//!
//! Neither is implemented here. The parity half of #0059, one orchestration
//! serving both transports, is parked with the Graph backend: `graph.rs` still
//! runs its own loop, and the engine below is driven by the IMAP backend only.

pub mod engine;
pub mod tick;

use anyhow::Result;

use crate::ingest::KnownUids;
use crate::types::{MailboxRole, MessageFlags};

pub use engine::run_sync;

/// Where a pass asks its caller whether it has been told to stop (PERSO-127).
///
/// Every point is a boundary at which nothing is half done: the transport has
/// not been asked yet, a body chunk has been collected in full, or one mailbox
/// has been ingested and the next has not been touched.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StopPoint {
    /// Before the transport is asked for anything.
    Fetch,
    /// Before the next `UID FETCH` of one mailbox's body pass.
    Chunk,
    /// Before the target at this index, in target order, is ingested.
    Mailbox(usize),
}

/// The question a cancellable pass asks at every [`StopPoint`]: `true` means
/// stop there.
///
/// A closure rather than the daemon's cancel token, so the engine and the
/// transport stay ignorant of who asks for the stop; the daemon builds one
/// over its operation's token (`src/daemon/methods/sync.rs`). A pass with no
/// `Stop` runs to the end, which is what every caller outside the daemon's
/// `sync.quick` and `sync.full` gets.
#[derive(Clone)]
pub struct Stop(std::sync::Arc<dyn Fn(StopPoint) -> bool + Send + Sync>);

impl Stop {
    /// Wrap `asks`, which answers whether the pass must stop at a point.
    pub fn new(asks: impl Fn(StopPoint) -> bool + Send + Sync + 'static) -> Self {
        Stop(std::sync::Arc::new(asks))
    }

    /// Whether the pass must stop at `point`.
    pub fn at(&self, point: StopPoint) -> bool {
        (self.0)(point)
    }
}

impl std::fmt::Debug for Stop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Stop(..)")
    }
}

/// Whether an optional [`Stop`] says to stop at `point`; no `Stop` never does.
pub fn stops_at(stop: Option<&Stop>, point: StopPoint) -> bool {
    stop.is_some_and(|stop| stop.at(point))
}

/// A mailbox to sync: the configured role and the name on the server.
///
/// The local directory and `.md` status the old struct carried are gone: the
/// ingest path has no filesystem destination. The role is a [`MailboxRole`]
/// rather than a bare string, so `--mailbox INBOX` and the configured inbox
/// are the same target and file their rows under one key (#0064).
///
/// `window` caps a `status-only` folder's download at its newest few messages
/// (#0141, [`crate::catalogue::STATUS_ONLY_WINDOW`]), below whatever limit the
/// pass runs at. A capped target never holds the account's prune gate shut by
/// what it left on the server: leaving the folder's history there is its
/// policy, not a short pass, and gating on it would suspend every prune for as
/// long as the folder holds more than the window.
#[derive(Debug, Clone)]
pub struct SyncTarget {
    pub role: MailboxRole,
    pub server_name: String,
    pub window: Option<usize>,
}

impl SyncTarget {
    /// A target downloading up to the pass's limit.
    pub fn new(role: MailboxRole, server_name: impl Into<String>) -> Self {
        SyncTarget {
            role,
            server_name: server_name.into(),
            window: None,
        }
    }

    /// How many new messages this target downloads in a pass whose limit is
    /// `limit`.
    pub fn limit(&self, limit: usize) -> usize {
        self.window.map_or(limit, |window| window.min(limit))
    }

    /// Whether a short download of this target counts against the pass's
    /// coverage; see the type's docs.
    pub fn short_download_counts(&self) -> bool {
        self.window.is_none()
    }
}

/// The address headers of a message a sync inserted as a new row, for the
/// contacts hook ([`crate::contacts::hooks::bump_after_sync`]) to fold in.
///
/// Only an inserted row is one: a row ingest updated or rebound to a new UID
/// is a message the index has already counted, once, exactly as a rebuild
/// counts each row once (PERSO-234).
#[derive(Debug, Clone)]
pub struct FreshObservation {
    /// When the observation was made, from the process-wide counter of
    /// [`crate::contacts::window`]: what tells a merge whether the rebuild
    /// whose index is on disk already counted the row (PERSO-234).
    pub sequence: u64,
    /// The mailbox the message was ingested into.
    pub role: MailboxRole,
    pub from: String,
    pub to: String,
    pub cc: Option<String>,
    pub bcc: Option<String>,
    /// RFC-2822 date header from the email, or empty if unavailable.
    pub date: String,
}

impl FreshObservation {
    /// The observation of `email`, just inserted under `role`: the headers a
    /// rebuild reads back from the row ingest wrote, numbered now. Make it
    /// right after the insert returns, never before: an observation numbered
    /// before its row was committed could be taken for one a rebuild's walk
    /// had seen when it had not.
    pub fn of(role: &MailboxRole, email: &crate::parse::FetchedEmail) -> FreshObservation {
        FreshObservation {
            sequence: crate::contacts::window::next_sequence(),
            role: role.clone(),
            from: email.from.clone(),
            to: email.to.clone(),
            cc: email.cc.clone(),
            bcc: email.bcc.clone(),
            date: email.date.clone(),
        }
    }
}

/// Where a pass records a [`FreshObservation`] the moment it inserts the row,
/// shared with the caller that handed it over (PERSO-234).
///
/// A [`SyncResult`] reaches its caller only when the pass returns `Ok`, and a
/// pass can fail after it has committed rows: a quick Graph tick cut off at
/// its ceiling, a cursor write that fails after an earlier mailbox was
/// ingested. Those rows are never inserted again, the next pass resumes past
/// them, so an observation carried only in the result was lost with it and
/// the correspondent stayed unsearchable until a rebuild. The sink is the
/// caller's own, so whatever the pass pushed is still there whatever it
/// returned. A clone shares the same list.
#[derive(Debug, Clone, Default)]
pub struct ObservationSink(std::sync::Arc<std::sync::Mutex<Vec<FreshObservation>>>);

impl ObservationSink {
    fn list(&self) -> std::sync::MutexGuard<'_, Vec<FreshObservation>> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Record the observation of a row just inserted.
    pub fn push(&self, observation: FreshObservation) {
        self.list().push(observation);
    }

    /// Everything recorded so far, leaving the sink empty.
    pub fn take(&self) -> Vec<FreshObservation> {
        std::mem::take(&mut *self.list())
    }

    pub fn len(&self) -> usize {
        self.list().len()
    }

    pub fn is_empty(&self) -> bool {
        self.list().is_empty()
    }
}

/// Results from a sync run.
#[derive(Debug, Default)]
pub struct SyncResult {
    /// Messages ingested as new rows.
    pub saved: usize,
    /// Messages the store already held (pass 1 hits).
    pub skipped: usize,
    /// Rows whose flags were updated from the server: read, answered or
    /// forwarded (#TKT-0051).
    pub flags_updated: usize,
    /// Rows deleted because the server no longer lists their UID (a message
    /// archived, moved or deleted in another client).
    pub pruned: usize,
    /// Rows this pass found vanished but did not delete, because some mailbox
    /// it touched came back short and the diff cannot be trusted until one
    /// does not (see [`crate::ingest::pass_may_prune`]).
    pub prunes_deferred: usize,
    /// Rows rebound to a new UID after a UIDVALIDITY reset.
    pub uid_rebound: usize,
    /// Mailboxes whose body fetch stopped at its deadline with new messages
    /// still to download (#0113). They resume on the next pass; the prune is
    /// suspended in the meantime like any other short pass.
    pub bodies_truncated: usize,
    /// Mailboxes whose server-side UIDVALIDITY no longer matched the stored
    /// cursor, and were therefore refetched in full.
    pub uidvalidity_resets: usize,
    /// Server names of the mailboxes that downloaded the same set of UIDs as
    /// the pass before them, twice or more in a row (#0115).
    ///
    /// A complete pass owes the next one nothing, so a repeat means something
    /// upstream is handing the same mail back every tick and paying for it
    /// every tick. The log line carries the detail; this is what the status
    /// line and `mp sync` report so it cannot pass for a clean sync.
    pub non_converging: Vec<String>,
    /// Server names of the mailboxes whose download `limit` cut short after
    /// a window that landed whole, so the next pass takes the next window
    /// (PERSO-172). Graph only: it downloads by id, so a quick pass drains a
    /// backlog one window at a time, while an IMAP quick window is positional
    /// and skips the backlog under it by design. The account runtime runs its
    /// next tick at once while this is not empty.
    pub backlogged: Vec<String>,
    /// Address observations from newly-inserted rows, ready to be merged into
    /// the contacts index by the caller. Empty on `dry_run`. The pass's
    /// [`ObservationSink`]: the one its caller handed it, so a caller holding
    /// a clone reads them whether or not the pass returned this result.
    pub fresh_observations: ObservationSink,
    /// Sender + subject of every genuinely new inbox message (#0009).
    pub new_inbox_mail: Vec<crate::notify::NewMailMeta>,
}

/// One message downloaded for ingest, with the identity the store keys on.
pub struct FetchedRaw {
    pub uid: u32,
    pub raw: Vec<u8>,
    /// What the server says has happened to it: read, answered, forwarded.
    pub flags: MessageFlags,
}

/// What the SELECT response said about the mailbox.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MailboxState {
    pub uid_validity: Option<u32>,
    pub uid_next: Option<u32>,
    pub exists: u32,
}

/// What one backend fetch of one mailbox brought back: everything the engine
/// needs to ingest, re-flag, prune and record a cursor, and nothing about how
/// it was obtained.
pub struct MailboxFetch {
    /// Messages the store does not hold yet, with their bodies.
    pub messages: Vec<FetchedRaw>,
    /// How many UIDs in the window the store already held.
    pub skipped: usize,
    /// The flags of those already-held UIDs, the only server-to-local channel
    /// for the second status axis (#0004, #TKT-0051): read, answered and
    /// forwarded all arrive here and nowhere else.
    pub known_flags: Vec<(u32, MessageFlags)>,
    /// What SELECT said about the mailbox.
    pub state: MailboxState,
    /// UIDs the store holds for this mailbox that the server no longer lists.
    /// See [`crate::imap_client::vanished_uids`].
    pub vanished: Vec<u32>,
    /// Every UID the server listed for this mailbox on this pass, ascending.
    ///
    /// The engine reads it as "what the server is currently holding", which is
    /// what decides whether ingest may move an existing row onto a new UID or
    /// owes the message a row of its own
    /// ([`crate::ingest::RebindPolicy`], #0112). An empty listing means the
    /// backend cannot answer, and the gate degrades to the unconditional
    /// rebind ingest always did.
    pub listed: Vec<u32>,
    /// True when the server's UIDVALIDITY no longer matches the stored one, so
    /// this fetch deliberately skipped nothing and redownloaded the window.
    pub uidvalidity_reset: bool,
    /// True when the enumeration [`vanished`] was computed from is the whole
    /// mailbox rather than a short answer.
    ///
    /// [`vanished`]: MailboxFetch::vanished
    pub enumeration_complete: bool,
    /// True when this pass did not ingest every message that arrived in the
    /// mailbox since the store last saw it: the `limit` window cut some off,
    /// a body did not come back, or the caller failed to ingest one. Backlog
    /// *older* than what the store already holds does not count.
    pub download_incomplete: bool,
    /// False when the body pass stopped at its deadline with new UIDs it never
    /// asked for (#0113), which is the one way this fetch is short *by design*
    /// rather than by accident.
    ///
    /// It is not a second spelling of [`download_incomplete`], which the
    /// coverage arithmetic already sets on such a pass: it names the reason, so
    /// the engine can refuse to record a modseq off it, count the mailbox for
    /// the status line, and leave everything else alone. Every path that ran to
    /// the end of its new UIDs is `true`, backends without a deadline included.
    ///
    /// [`download_incomplete`]: MailboxFetch::download_incomplete
    pub bodies_complete: bool,
    /// The arrival mark to persist for this mailbox: `Some(mark)` while an
    /// arrival above it is still missing, `None` once every arrival is in.
    /// Carried back in by the next fetch so the gate cannot open on a mark that
    /// this pass's own ingest raised (#0072).
    pub pending_arrival_mark: Option<u32>,
    /// The CONDSTORE `HIGHESTMODSEQ` to record as the next pass's resume point,
    /// or `None` for "this fetch cannot vouch for one", which the cursor UPSERT
    /// reads as "keep whatever is stored" (#0041).
    ///
    /// `None` is the answer for every backend and every path without CONDSTORE,
    /// and for a CONDSTORE pass whose window did not cover the whole mailbox:
    /// a modseq claims every flag in the mailbox was correct as of that point,
    /// and only a pass that looked at all of them may claim it.
    pub highest_modseq: Option<i64>,
}

/// The per-transport half of a sync: fetch the window of every target.
///
/// Everything else, ingest, arrival marks, ingest-failure bookkeeping, flags,
/// cursors and the deferred prune pass, is the engine's ([`run_sync`]), written
/// once and tested against a fake implementation of this trait.
///
/// The trait is deliberately one method wide. It is not the "list folders,
/// move, delete, set read" surface the ticket sketched: those ops already have
/// their own seam in [`crate::ops::run_op`], and widening this one would be
/// generality no consumer has asked for.
///
/// Two contracts a backend must honour:
///
/// - one result per target, in the order the targets were given, so the engine
///   can zip them and keep the #0072 prune ordering (inbox before archive
///   before sent) whatever order the fetches finished in;
/// - a per-target failure is an `Err` *element*, not an `Err` return: the
///   engine treats it as the strongest form of partial pass and keeps going,
///   whereas an `Err` return fails the whole sync.
///
/// `&mut self` is where a backend keeps what outlives one mailbox: a persistent
/// session and its `HIGHESTMODSEQ` (#0041), a `deltaLink` (#0042). Today's
/// [`crate::imap_client::ImapBackend`] keeps only its config.
pub trait SyncBackend {
    /// Fetch every target's window. `knowns` is parallel to `targets`: the
    /// store's skip list for each, already read under the single-writer
    /// discipline before any network call.
    fn fetch_targets(
        &mut self,
        targets: &[SyncTarget],
        limit: usize,
        knowns: Vec<KnownUids>,
    ) -> impl std::future::Future<Output = Vec<Result<MailboxFetch>>>;
}
