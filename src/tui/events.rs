//! The daemon's events, applied to the model (P5-U8, #0124).
//!
//! The contract is P5-U7's (`src/tui/events_tests.rs`): [`Incoming`],
//! [`Subscription`], [`Applied`], [`drain`] and [`App::apply_event`], and
//! nothing else. What it replaces is two watcher threads per account and a
//! 100 ms `operation.status` poll per background action: the daemon watches the
//! server now, and it says when something moved.
//!
//! # The four things a client does with an event
//!
//! `docs/daemon-protocol.md` fixes them and this is where the TUI does them.
//! An event above the watermark is applied and moves it; one at or below it is
//! a duplicate the bootstrap snapshot already carries (the daemon queues from
//! the moment a connection registers, which is *before* it captures the
//! snapshot) and is dropped without a word; one from an instance this client
//! never bootstrapped against is refused whatever its number, because revisions
//! are only comparable inside the instance that issued them; and a
//! `state.resync_required` costs a fresh `state.bootstrap` rather than a
//! cleared flag, because a poisoned queue accepts no further domain event.
//!
//! The refusal is sticky. A client that kept applying past an instance change
//! would build a state nothing on the daemon's side corresponds to, so the only
//! way out is [`App::apply_bootstrap`], which is the one place a watermark is
//! set at all.
//!
//! # Why the connection's own state travels on the event channel
//!
//! A daemon killed mid-session and a daemon restarted afterwards are facts the
//! UI has to show, and they arrive where the events arrive: the session thread.
//! Two more variants on one stream keep them ordered against the events around
//! them, where a second channel would let a [`Incoming::Reconnected`] overtake
//! the last event of the dead instance and make the watermark arithmetic
//! disagree with what the client actually saw.
//!
//! # Why the drain's bound is not a new constant
//!
//! An event batch is drained in the same pre-draw pass as a terminal batch and
//! is held to the same [`MAX_COALESCED_EVENTS`] and [`COALESCE_BUDGET`]: a
//! first sync of a large mailbox publishing a row per message may not starve
//! the paint any more than a bracketed paste may.

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Instant;

use serde_json::json;

use mp_protocol::events::{Arrival, SyncCompleted, KIND_SYNC_COMPLETED};
use mp_protocol::state::Bootstrap;
use mp_protocol::EventEnvelope;

use super::app::{App, StatusLevel};
use super::queries::{MessageRowDelta, Queries};
use super::{COALESCE_BUDGET, MAX_COALESCED_EVENTS};

/// The `kind` a finished operation travels as.
const KIND_OPERATION_FINISHED: &str = "operation.finished";

/// The `kind` an invalidation travels as.
const KIND_INVALIDATE: &str = "state.invalidate";

/// One thing the session thread has to tell the UI thread.
#[derive(Debug)]
pub enum Incoming {
    /// One decoded `state.event`.
    Event(EventEnvelope),
    /// `state.resync_required`: this connection's queue was poisoned and
    /// accepts no further domain event until it bootstraps again.
    Resync {
        /// The instance that gave up on the queue.
        instance_id: String,
        /// Why, in the daemon's own word (`event_queue_overflow`).
        reason: String,
    },
    /// The daemon went away.
    Disconnected {
        /// What the transport said, for the log and the status line.
        reason: String,
    },
    /// A daemon answers again, and it may not be the same one.
    Reconnected {
        /// The instance the handshake reported.
        instance_id: String,
    },
}

/// The UI thread's end of the session's event stream.
///
/// A plain `Receiver`, which is what `run_loop` already holds three of and what
/// a `try_recv` drain reads without a second vocabulary. It is also what lets a
/// test feed the drain without a socket: whether the far end is a session
/// thread or a `Sender` in a test is not a property the drain may observe.
pub type Subscription = Receiver<Incoming>;

/// What one event did to the model.
#[derive(Debug, PartialEq)]
pub enum Applied {
    /// The open list moved.
    Rows,
    /// A sidebar count moved.
    Counts,
    /// A tick landed, and this is what it notified the user about: empty when
    /// it notified about nothing, whether because nothing arrived or because
    /// `notifications` is off.
    NewMail(Vec<Arrival>),
    /// The operation this client started, by id, finished.
    Operation(String),
    /// At or below the watermark, so the snapshot already carries it.
    Duplicate,
    /// From an instance this client never bootstrapped against.
    Refused,
    /// A kind no surface of this client reads.
    Ignored,
}

// ---------------------------------------------------------------------------
// The watermark, and the operations this client started
// ---------------------------------------------------------------------------

/// What an [`App`] remembers about the stream it is reading.
#[derive(Debug, Default)]
pub struct EventState {
    /// The instance whose revisions are comparable, `None` before the first
    /// bootstrap and after a refusal.
    instance: Option<String>,
    /// The highest revision applied, which the bootstrap sets and every
    /// applied event moves.
    revision: u64,
    /// The operations this client started and has not seen finish, by id.
    started: HashMap<String, Awaited>,
}

/// One operation this client is waiting for, and what its result means.
///
/// Kept per id rather than one slot, because a startup auto-fetch runs one per
/// account at once and because an `operation.finished` reaches every
/// bootstrapped connection: an id this table does not hold belongs to another
/// window.
#[derive(Clone, Debug)]
pub(super) enum Awaited {
    /// A quick pass, landing as [`BgResult::Fetch`](super::app::BgResult).
    Quick {
        /// The account it belongs to, for the status line and the health mark.
        account_index: usize,
        /// Its name, which the settled outcome is rendered against.
        account: String,
    },
    /// A full pass, landing as [`BgResult::Sync`](super::app::BgResult).
    Full {
        /// The account it belongs to.
        account_index: usize,
        /// Its name.
        account: String,
    },
    /// `send.approved`.
    SendApproved {
        /// The account whose drafts went.
        account_index: usize,
    },
    /// `calendar.rsvp`.
    Rsvp {
        /// The account the reply was sent from.
        account_index: usize,
    },
}

/// Whether one event may be applied, decided before its kind is looked at.
enum Admission {
    Apply,
    Duplicate,
    Refused,
}

impl EventState {
    /// Adopt an instance and a revision, which only a bootstrap may do.
    ///
    /// A different instance is a daemon that restarted, so everything this
    /// client was waiting for died with it: the table is emptied and its
    /// entries are reported as background work that ended, or the spinner would
    /// run for the rest of the session.
    pub(super) fn watermark(&mut self, instance_id: &str, revision: u64) -> usize {
        let restarted = self.instance.as_deref() != Some(instance_id);
        self.instance = Some(instance_id.to_string());
        self.revision = revision;
        if !restarted {
            return 0;
        }
        std::mem::take(&mut self.started).len()
    }

    /// Whether `event` may be applied, moving the watermark when it may.
    fn admit(&mut self, event: &EventEnvelope) -> Admission {
        match self.instance.as_deref() {
            None => Admission::Refused,
            Some(known) if known != event.instance_id => {
                // Sticky: only a fresh bootstrap admits anything again.
                self.instance = None;
                Admission::Refused
            }
            Some(_) if event.revision <= self.revision => Admission::Duplicate,
            Some(_) => {
                self.revision = event.revision;
                Admission::Apply
            }
        }
    }

    /// Remember an operation this client started.
    pub(super) fn started(&mut self, id: String, awaited: Awaited) {
        self.started.insert(id, awaited);
    }

    /// Whether a pass over `account` is one this client asked for.
    fn awaits_a_pass_on(&self, account: &str) -> bool {
        self.started.values().any(|awaited| match awaited {
            Awaited::Quick { account: name, .. } | Awaited::Full { account: name, .. } => {
                name == account
            }
            _ => false,
        })
    }
}

// ---------------------------------------------------------------------------
// One event
// ---------------------------------------------------------------------------

impl App {
    /// Apply one event, or say why it was not applied.
    ///
    /// On [`App`] and not on the drain because applying one event is the
    /// model's business and nothing else's; the drain is the loop's, because it
    /// is bounded and because it re-bootstraps, which needs a door.
    pub fn apply_event(&mut self, event: &EventEnvelope) -> Applied {
        match self.events.admit(event) {
            Admission::Refused => {
                log::debug!(
                    "[events] refused a {} from {}: this client bootstrapped against another \
                     instance",
                    event.kind,
                    event.instance_id
                );
                Applied::Refused
            }
            Admission::Duplicate => Applied::Duplicate,
            Admission::Apply => self.apply_admitted(event),
        }
    }

    /// One event whose instance and revision have already been checked.
    fn apply_admitted(&mut self, event: &EventEnvelope) -> Applied {
        match event.kind.as_str() {
            KIND_SYNC_COMPLETED => self.apply_tick(event),
            KIND_OPERATION_FINISHED => self.apply_finished(event),
            // The counts scope is the sidebar's and not the list's: a hundred
            // count changes for one mailbox may not each refetch the open list,
            // which is why `MessageRowDelta::decode` returns `None` for it.
            KIND_INVALIDATE if event.payload["scope"]["query"] == json!("counts") => {
                self.apply_counts(event)
            }
            _ => match MessageRowDelta::decode(event) {
                Some(delta) => {
                    super::bg::apply_row_delta(self, &delta);
                    Applied::Rows
                }
                // A kind this client has no surface for is ignored rather than
                // guessed at: treating an unknown kind as a reason to reload
                // would turn every future protocol addition into a refetch
                // storm.
                None => Applied::Ignored,
            },
        }
    }

    /// A finished tick: the status line, the account's health mark, the
    /// desktop notification and the refresh the rows owe.
    ///
    /// A pass *this* client asked for is not landed here. Its
    /// `operation.finished` says the same thing to the one client that wants
    /// it, and the daemon publishes the tick to everyone first, so landing
    /// both would notify twice and reload twice.
    fn apply_tick(&mut self, event: &EventEnvelope) -> Applied {
        let outcome: SyncCompleted = match serde_json::from_value(event.payload.clone()) {
            Ok(outcome) => outcome,
            Err(e) => {
                log::warn!("[events] a sync.completed did not decode: {e}");
                return Applied::Ignored;
            }
        };
        let Some(index) = self.account_index(&outcome.account) else {
            return Applied::Ignored;
        };
        if self.events.awaits_a_pass_on(&outcome.account) {
            return Applied::Ignored;
        }
        let arrivals = outcome.new_inbox_mail.clone();
        let notified = if self.global_config.notifications {
            arrivals.clone()
        } else {
            Vec::new()
        };
        let result = match &outcome.error {
            Some(error) => Err(error.clone()),
            None => Ok(mp_client::format::sync_status_line(&outcome)),
        };
        super::bg::land_sync(self, index, result, arrivals_as_meta(&arrivals));
        Applied::NewMail(notified)
    }

    /// An operation finished. This client's own land where its poll's answer
    /// landed; another window's are ignored, because a line about a pass the
    /// user did not ask *this* client for has no business on its status line.
    fn apply_finished(&mut self, event: &EventEnvelope) -> Applied {
        let Some(id) = event.payload["operation_id"].as_str() else {
            return Applied::Ignored;
        };
        let Some(awaited) = self.events.started.remove(id) else {
            return Applied::Ignored;
        };
        let id = id.to_string();
        // The same handler the polled answer posted into, so every line, every
        // health mark and every refresh is the one that path produced.
        super::bg::handle_bg_result(self, super::commands::settled(&awaited, &event.payload));
        Applied::Operation(id)
    }

    /// A mailbox's counts moved, so the sidebar is read again from the daemon.
    ///
    /// One `mailbox.list` for the account, not one per mailbox: the answer
    /// carries every count, and the events coalesce by resource so a tick that
    /// touched four mailboxes costs four cheap reads of one small answer.
    fn apply_counts(&mut self, event: &EventEnvelope) -> Applied {
        let account = event.payload["resource"]
            .as_str()
            .and_then(|resource| resource.strip_prefix("mailbox:"))
            .and_then(|rest| rest.split_once('/'))
            .map(|(account, _)| account.to_string());
        if account.as_deref() == Some(self.account_config.name.as_str()) {
            self.recount_all_mailboxes();
        }
        Applied::Counts
    }

    /// The index of a configured account by name.
    fn account_index(&self, account: &str) -> Option<usize> {
        self.accounts
            .iter()
            .position(|state| state.account_config.name == account)
    }
}

/// The notifier's shape for the arrivals a tick reported.
fn arrivals_as_meta(arrivals: &[Arrival]) -> Vec<crate::notify::NewMailMeta> {
    arrivals
        .iter()
        .map(|arrival| crate::notify::NewMailMeta::new(&arrival.from, &arrival.subject))
        .collect()
}

// ---------------------------------------------------------------------------
// The pre-draw drain
// ---------------------------------------------------------------------------

/// Drain what the session thread has posted, bounded twice, and answer how
/// many items were taken.
///
/// The door is an argument rather than read off `app.session` for the reason
/// `commands::dispatch` takes one: this needs `&mut App`, and a borrow of the
/// session inside it would collide. An empty stream costs one `try_recv` and
/// nothing else, because this runs on every loop iteration.
pub fn drain(app: &mut App, door: &dyn Queries, events: &Subscription) -> usize {
    let started = Instant::now();
    let mut handled = 0usize;
    while handled < MAX_COALESCED_EVENTS {
        match events.try_recv() {
            Ok(incoming) => {
                handle(app, door, incoming);
                handled += 1;
            }
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
        }
        if started.elapsed() >= COALESCE_BUDGET {
            break;
        }
    }
    handled
}

/// One item off the stream.
fn handle(app: &mut App, door: &dyn Queries, incoming: Incoming) {
    match incoming {
        Incoming::Event(event) => {
            app.apply_event(&event);
        }
        Incoming::Resync {
            instance_id,
            reason,
        } => {
            log::info!("[events] {instance_id} asked for a resync: {reason}");
            rebootstrap(app, door);
        }
        Incoming::Reconnected { instance_id } => {
            log::info!("[events] reconnected to {instance_id}");
            watching(app, true);
            app.set_status("Reconnected to the daemon".to_string());
            // The instance and the revision come from the bootstrap's answer
            // rather than from this notice: the notice is the client's guess,
            // the answer is the daemon's word.
            rebootstrap(app, door);
        }
        Incoming::Disconnected { reason } => {
            log::warn!("[events] the daemon session was lost: {reason}");
            watching(app, false);
            app.set_status_level(
                "The daemon is not reachable; reconnecting...".to_string(),
                StatusLevel::Warning,
            );
        }
    }
}

/// Whether this client is being told about new mail at all, which is what the
/// sidebar's watcher indicator means since the watch moved into the daemon.
fn watching(app: &mut App, live: bool) {
    for account in &mut app.accounts {
        account.watcher_active = live;
    }
    app.watcher_active = live;
}

/// Ask for the whole state again and apply it.
///
/// The one answer to a poisoned stream and to a daemon that is not the one this
/// client bootstrapped against. A failure is logged and left for the next
/// notice: a client with no snapshot refuses every event it is sent, which is
/// visible without being fatal.
fn rebootstrap(app: &mut App, door: &dyn Queries) {
    match door.call("state.bootstrap", json!({})) {
        Ok(answer) => match serde_json::from_value::<Bootstrap>(answer) {
            Ok(bootstrap) => {
                log::info!(
                    "[events] re-bootstrapped at revision {} against {}",
                    bootstrap.revision,
                    bootstrap.instance_id
                );
                app.apply_bootstrap(&bootstrap);
            }
            Err(e) => log::warn!("[events] the bootstrap did not decode: {e}"),
        },
        Err(e) => log::warn!("[events] the bootstrap failed: {e:#}"),
    }
}
