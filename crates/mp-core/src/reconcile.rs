//! The fold half of iMIP invite reconciliation (#0030, #0126 unit P5-U10b).
//!
//! An organizer's invitation is a `METHOD:REQUEST` message; an attendee's
//! answer is a `METHOD:REPLY` carrying that attendee's `PARTSTAT`. Both arrive
//! as ordinary mail and both are ingested as an ordinary row with an
//! `invite.ics` attachment blob. Reconciliation folds the replies onto the
//! invite so the organizer sees who responded, and so an attendee sees their
//! own answer on the invite they answered.
//!
//! This module is the half of that which reads nothing: it takes a slice of
//! [`InviteMessage`], already parsed out of the blobs, and answers with the
//! indices and the statuses. The half that opens a store and reads the blobs
//! (`load_invites`, `event_for_message`, `reconcile_account`) stays in the root
//! crate and re-exports everything here, so `crate::reconcile::…` resolves
//! unchanged on both sides of the seam.
//!
//! # Derived, not stored
//!
//! Nothing here writes. The pre-store build rewrote `event.attendees[].status`
//! into the invite's `.md` frontmatter, but the store is a cache in front of
//! the server (a schema mismatch drops the whole file), so a persisted fold
//! would be a second source of truth that can drift from the blobs it was
//! computed from, and it buys nothing that recomputing does not. The fold
//! therefore runs where the answer is displayed, over the same rows every time:
//!
//! - **Idempotent** by construction: there is no state to converge.
//! - **Multi-machine consistent** by construction: two machines holding the
//!   same messages compute the same statuses with no machine-to-machine sync.
//! - **Cheap**: an invite is a rare row, the query is one index-driven join,
//!   and each ics is a few kilobytes.
//!
//! `mp calendar rebuild` therefore reports what the fold resolves instead of
//! rewriting files, and [`ReconcileReport`] is a report rather than a diff.
//!
//! # Algorithm
//!
//! 1. `load_invites` (the root crate's half) reads every row of the account
//!    that carries an `invite.ics` blob and parses it. The ics is the only
//!    source: there is no frontmatter cache left to drift from it, which also
//!    removes the attachment-`.md` forgery surface TKT-0047 described (there is
//!    no `.md` on disk to walk, and an attachment blob is not a message row).
//! 2. [`fold_replies`] indexes the REPLYs by UID and attendee address
//!    (case-insensitive). The winner per address is the reply with the highest
//!    `(sequence, dtstamp)`: a newer sequence supersedes, ties break on the
//!    later `DTSTAMP`.
//! 3. [`apply_replies`] writes the winning `PARTSTAT`s onto an invite's
//!    in-memory attendee list, skipping replies older than the invite's own
//!    sequence. An address that was never invited is ignored: the attendee
//!    list belongs to the organizer's invitation.

use std::collections::{BTreeSet, HashMap};

use crate::calendar::ParsedEvent;
use crate::types::EventFrontmatter;

/// One stored message carrying an iMIP payload: its row identity and the
/// parsed contents of its `invite.ics` blob.
#[derive(Debug, Clone)]
pub struct InviteMessage {
    /// `messages.id` of the row the payload came from.
    pub row_id: i64,
    /// The mailbox the row sits in. `sent` is what makes us the organizer of
    /// a REQUEST, the way the Sent *directory* used to.
    pub mailbox: String,
    /// The row's UID, used only as the final identity tiebreak.
    pub uid: i64,
    /// The email subject, the agenda's fallback title when the event carries
    /// no `SUMMARY`.
    pub subject: Option<String>,
    /// The parsed ics. Authoritative for UID, SEQUENCE, DTSTAMP and every
    /// displayed field.
    pub parsed: ParsedEvent,
}

/// The mailbox role the account's own sent mail lives in.
const SENT_MAILBOX: &str = "sent";

/// Whether a row in `mailbox` is the account's own sent copy: a REQUEST
/// there is one the account sent, which is what makes it the organizer, on
/// the agenda and for the daemon's update and cancellation alike, and a
/// CANCEL there one it sent as the organizer. An invitation that names the
/// account as its organizer but arrived in any other mailbox is one it was
/// sent, which anyone can forge.
pub fn is_sent_copy(mailbox: &str) -> bool {
    mailbox == SENT_MAILBOX
}

impl InviteMessage {
    /// Whether the row is the account's own sent copy ([`is_sent_copy`]).
    pub fn is_sent_copy(&self) -> bool {
        is_sent_copy(&self.mailbox)
    }

    /// The upper-cased `METHOD`, empty when the payload carried none.
    pub fn method(&self) -> String {
        self.parsed
            .method
            .as_deref()
            .unwrap_or_default()
            .to_uppercase()
    }

    /// The trimmed iCal UID, `None` when it is absent or empty. An invite
    /// without one is still a real event; it just cannot be deduped or
    /// matched to a reply.
    pub fn uid(&self) -> Option<&str> {
        self.parsed
            .uid
            .as_deref()
            .map(str::trim)
            .filter(|u| !u.is_empty())
    }

    /// `DTSTAMP` as a lexicographically comparable string, empty when unknown.
    pub fn dtstamp(&self) -> &str {
        self.parsed.dtstamp.as_deref().unwrap_or_default()
    }

    /// The normalised `RECURRENCE-ID`, `None` when the payload addresses the
    /// whole series (#0031).
    pub fn recurrence_id(&self) -> Option<&str> {
        self.parsed
            .recurrence_id
            .as_deref()
            .map(str::trim)
            .filter(|r| !r.is_empty())
    }

    /// The occurrence the payload names, keyed by its instant (or its date)
    /// rather than by the text of its `RECURRENCE-ID` ([`occurrence_key`]);
    /// `None` for the whole series.
    pub fn occurrence_key(&self) -> Option<String> {
        self.recurrence_id().map(occurrence_key)
    }
}

/// The key the fold gives the occurrence a `RECURRENCE-ID` in
/// [`ParsedEvent::recurrence_id`]'s form names: one per instant, so the UTC
/// form and the `TZID` form of one start meet (PERSO-302), one per date for
/// an all-day value, and a floating time as written
/// ([`crate::calendar_model::normalise_recurrence_id`]).
pub fn occurrence_key(recurrence_id: &str) -> String {
    crate::calendar_model::normalise_recurrence_id(recurrence_id)
}

/// A single REPLY observation: one attendee's `PARTSTAT` for one UID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplyObs {
    /// Lowercased attendee address.
    pub address: String,
    /// Our lowercase status vocabulary (`accepted`, `declined`, ...).
    pub status: String,
    pub sequence: u32,
    /// RFC3339 UTC `DTSTAMP`, or empty when the source omitted it. Compared
    /// lexicographically, which is chronological for RFC3339 UTC strings.
    pub dtstamp: String,
}

/// UID -> attendee address -> the winning reply for that attendee.
pub type ReplyIndex = HashMap<String, HashMap<String, ReplyObs>>;

/// What one reconciliation pass saw and resolved. A report, not a diff:
/// nothing is written, so there is no "updated" count to give.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ReconcileReport {
    /// REQUEST invites read from the store.
    pub invites_seen: usize,
    /// REPLY messages read from the store.
    pub replies_seen: usize,
    /// Attendee statuses the fold resolved onto an invite, counted once per
    /// (invite, attendee) pair.
    pub resolved: usize,
    /// Invites a `METHOD:CANCEL` tombstoned (#0031). They stay listed and
    /// readable; nothing is deleted.
    pub cancelled: usize,
}

/// Index the REPLYs of `invites` by UID and attendee, keeping the winner per
/// attendee (highest `(sequence, dtstamp)`).
///
/// A REPLY carries exactly the replying attendee, so the first attendee with a
/// usable address is the observation; a REPLY with no attendee at all is not
/// an observation and is skipped.
pub fn fold_replies(invites: &[InviteMessage]) -> ReplyIndex {
    let mut replies: ReplyIndex = HashMap::new();
    for invite in invites {
        if invite.method() != "REPLY" {
            continue;
        }
        let (Some(uid), Some(obs)) = (invite.uid(), reply_obs(invite)) else {
            continue;
        };
        let by_addr = replies.entry(uid.to_string()).or_default();
        match by_addr.get(&obs.address) {
            Some(existing) if !supersedes(&obs, existing) => {}
            _ => {
                by_addr.insert(obs.address.clone(), obs);
            }
        }
    }
    replies
}

/// The replying attendee's `(address, status)` from a REPLY payload.
fn reply_obs(invite: &InviteMessage) -> Option<ReplyObs> {
    let att = invite
        .parsed
        .attendees
        .iter()
        .find(|a| !a.address.trim().is_empty())?;
    Some(ReplyObs {
        address: att.address.trim().to_lowercase(),
        status: att.status.clone(),
        sequence: invite.parsed.sequence,
        dtstamp: invite.dtstamp().to_string(),
    })
}

/// Whether reply `a` supersedes reply `b` for the same attendee and UID: a
/// newer sequence wins, and within a sequence the later `DTSTAMP` wins.
fn supersedes(a: &ReplyObs, b: &ReplyObs) -> bool {
    (a.sequence, a.dtstamp.as_str()) > (b.sequence, b.dtstamp.as_str())
}

/// Apply the winning replies for one UID onto an invite's attendee list, and
/// return how many statuses were resolved.
///
/// `sequence` is the invite's own: a reply for an older sequence answered a
/// version of the event that no longer exists and is dropped.
pub fn apply_replies(
    event: &mut EventFrontmatter,
    sequence: u32,
    by_addr: Option<&HashMap<String, ReplyObs>>,
) -> usize {
    let Some(by_addr) = by_addr else { return 0 };
    let mut resolved = 0;
    for attendee in &mut event.attendees {
        let key = attendee.address.trim().to_lowercase();
        let Some(obs) = by_addr.get(&key) else {
            continue;
        };
        if obs.sequence < sequence {
            continue;
        }
        attendee.status = obs.status.clone();
        resolved += 1;
    }
    resolved
}

/// The cancellations recorded for one iCal UID (#0031).
#[derive(Debug, Default, Clone)]
struct UidCancels {
    /// Each whole-series `METHOD:CANCEL` (no `RECURRENCE-ID`) seen for this
    /// UID.
    series: Vec<SeriesCancel>,
    /// [`occurrence_key`] -> the single-occurrence CANCEL of it with the
    /// highest `SEQUENCE`.
    instances: std::collections::BTreeMap<String, InstanceCancel>,
}

/// The cancellation of one occurrence.
#[derive(Debug, Clone)]
struct InstanceCancel {
    sequence: u32,
    /// Its `RECURRENCE-ID` as the CANCEL writes it, in
    /// [`ParsedEvent::recurrence_id`]'s form, the form a person reads.
    named: String,
}

/// One whole-series `METHOD:CANCEL`.
#[derive(Debug, Clone)]
struct SeriesCancel {
    sequence: u32,
    /// On the account's own sent copy, the attendees it names, lower-cased,
    /// the organizer left out; `None` on a cancellation the account received.
    told: Option<BTreeSet<String>>,
    /// Whether the account's own sent copy carries
    /// [`crate::calendar::UNINVITE_PROPERTY`], the mark of the cancellation
    /// an update sends the attendees it drops; `false` on a cancellation
    /// the account received, whatever it carries.
    uninvite: bool,
}

impl SeriesCancel {
    /// Whether this cancellation, which the account sent as the organizer,
    /// only uninvites some attendees of a version that invites `roster`
    /// (RFC 5546 3.2.5, the organizer tells the attendees it removes with a
    /// CANCEL): it carries the uninvitation mark, or, a cancellation sent
    /// before mp marked them, it names a strict subset of them. Such a
    /// cancellation ends the meeting for those it names, never on the
    /// organizer's agenda, whatever its `SEQUENCE` against the version filed
    /// beside it. The mark settles the case the subset cannot: an update
    /// that replaces every attendee and waits in the outbox, its own copy
    /// not filed yet, leaves a cancellation naming every attendee of the
    /// version the agenda still holds. A whole cancellation is never marked.
    ///
    /// The subset fallback misreads a whole cancellation built on a version
    /// that a later-filed one at its `SEQUENCE` or below extends (an update
    /// adding an attendee still queued when the cancellation was built).
    /// mp's own `event.update` and `event.cancel` do not produce that on one
    /// daemon: both are refused while an earlier change of the `UID` waits in
    /// the outbox (`invite::unsettled_change` in the root crate), checked
    /// when the change is planned and again under the account's lock just
    /// before its first message is enqueued, which also refuses one whose
    /// version was filed since its plan; and a discarded row some attendee
    /// may hold files its copy first, locally at once and then in the
    /// server's Sent folder by a later pass, where a store rebuilt from the
    /// server finds it (`outbox::discard`, PERSO-304). So a cancellation
    /// they build is above every version filed or sent. It stays reachable
    /// for a hand-made `mp send --invite --sequence`, another client sending
    /// from the same mailbox, copies sent before the guard existed, and a
    /// store rebuilt while a discarded version is only in the local store:
    /// before a later pass filed it in Sent, on an account that APPENDs no
    /// copy of its own (`save_to_sent = "never"`, or a server that files its
    /// own and did not take this one), and after a discarded row's copy was
    /// given up (the server refused it a week or more after the discard,
    /// which its outbox row says until the user discards it). That is why
    /// the fallback is kept.
    fn only_uninvites(&self, roster: &BTreeSet<String>) -> bool {
        self.told.as_ref().is_some_and(|told| {
            !told.is_empty()
                && (self.uninvite || (told.len() < roster.len() && told.is_subset(roster)))
        })
    }
}

/// The addresses of `attendees` but `organizer`, trimmed and lower-cased.
fn roster<'a>(
    attendees: impl Iterator<Item = &'a str>,
    organizer: Option<&str>,
) -> BTreeSet<String> {
    let organizer = organizer.map(|o| o.trim().to_lowercase());
    attendees
        .map(|a| a.trim().to_lowercase())
        .filter(|a| !a.is_empty() && Some(a) != organizer.as_ref())
        .collect()
}

/// The account-wide `(UID, RECURRENCE-ID)` version chain: which identities were
/// cancelled, and which copy of each is the current one (#0031).
///
/// Derived like everything else here: computed from the ics blobs on every
/// pass, never stored. Arrival order does not matter -- a CANCEL that reaches
/// the mailbox before its REQUEST is folded exactly like one that follows it,
/// because both are just rows when the fold runs.
#[derive(Debug, Default, Clone)]
pub struct StatusIndex {
    /// UID -> its cancellations.
    cancels: HashMap<String, UidCancels>,
    /// `(UID, occurrence_key or "")` -> the highest `(sequence, dtstamp)`
    /// seen among the REQUESTs for that identity.
    latest: HashMap<(String, String), (u32, String)>,
}

/// Fold every CANCEL and REQUEST of `invites` into a [`StatusIndex`].
///
/// Payloads with no usable UID are skipped: without one there is no identity
/// to cancel or supersede against, and an unrelated event must never be
/// tombstoned by a UID-less CANCEL.
pub fn fold_status(invites: &[InviteMessage]) -> StatusIndex {
    let mut index = StatusIndex::default();
    for invite in invites {
        let Some(uid) = invite.uid() else { continue };
        let seq = invite.parsed.sequence;
        match invite.method().as_str() {
            "CANCEL" => {
                let entry = index.cancels.entry(uid.to_string()).or_default();
                match invite.recurrence_id() {
                    // A `RECURRENCE-ID` scopes the cancellation to that one
                    // occurrence; the rest of the series lives on.
                    Some(rid) => {
                        let cancel = InstanceCancel {
                            sequence: seq,
                            named: rid.to_string(),
                        };
                        let slot = entry
                            .instances
                            .entry(occurrence_key(rid))
                            .or_insert_with(|| cancel.clone());
                        // The highest SEQUENCE wins, and the form it is
                        // named in, whatever the order of the rows.
                        if (cancel.sequence, std::cmp::Reverse(&cancel.named))
                            > (slot.sequence, std::cmp::Reverse(&slot.named))
                        {
                            *slot = cancel;
                        }
                    }
                    None => entry.series.push(SeriesCancel {
                        sequence: seq,
                        told: invite.is_sent_copy().then(|| {
                            roster(
                                invite.parsed.attendees.iter().map(|a| a.address.as_str()),
                                invite.parsed.organizer.as_deref(),
                            )
                        }),
                        uninvite: invite.is_sent_copy() && invite.parsed.uninvite,
                    }),
                }
            }
            "REQUEST" => {
                let key = (uid.to_string(), invite.occurrence_key().unwrap_or_default());
                let rank = (seq, invite.dtstamp().to_string());
                let slot = index.latest.entry(key).or_insert_with(|| rank.clone());
                if rank > *slot {
                    *slot = rank;
                }
            }
            _ => {}
        }
    }
    index
}

impl StatusIndex {
    /// Mark one event with what the account-wide fold knows about it:
    /// `cancelled`, `superseded`, and the individually cancelled occurrences
    /// of a series.
    ///
    /// `dtstamp` is the copy's own, the tiebreak within one `SEQUENCE`.
    ///
    /// Sequence rules (iTIP, RFC 5546 §3.2):
    /// - a CANCEL cancels a copy whose `SEQUENCE` is at or below its own; a
    ///   *stale* CANCEL (lower sequence than the surviving REQUEST) is a
    ///   cancellation of a version that was already replaced, and is ignored;
    /// - a CANCEL of one occurrence counts on the series while its
    ///   `SEQUENCE` is at or above the series' own, and not once a REQUEST
    ///   for that occurrence re-issued it above it
    ///   ([`StatusIndex::cancelled_occurrences`]);
    /// - a whole-event CANCEL the account sent as an update's uninvitation
    ///   (marked, or naming a strict subset of the copy's attendees)
    ///   uninvites the attendees it names and leaves the copy live
    ///   ([`SeriesCancel::only_uninvites`]), whatever its `SEQUENCE`;
    /// - a REQUEST is superseded only by a strictly greater
    ///   `(SEQUENCE, DTSTAMP)`, so a re-delivered or replayed copy at an equal
    ///   or lower version never displaces the newer state.
    pub fn apply(&self, event: &mut EventFrontmatter, dtstamp: &str) {
        let Some(uid) = event
            .uid
            .as_deref()
            .map(str::trim)
            .filter(|u| !u.is_empty())
        else {
            return;
        };
        let rid = event
            .recurrence_id
            .as_deref()
            .map(str::trim)
            .filter(|r| !r.is_empty())
            .map(occurrence_key);
        let seq = event.sequence;
        if let Some(cancels) = self.cancels.get(uid) {
            let invited = roster(
                event.attendees.iter().map(|a| a.address.as_str()),
                event.organizer.as_deref(),
            );
            let series_cancelled = cancels
                .series
                .iter()
                .any(|c| c.sequence >= seq && !c.only_uninvites(&invited));
            let instance_cancelled = rid
                .as_deref()
                .and_then(|r| cancels.instances.get(r))
                .is_some_and(|c| c.sequence >= seq);
            event.cancelled = series_cancelled || instance_cancelled;
            // Only the series row reports per-occurrence cancellations; an
            // occurrence payload reports its own state in `cancelled`.
            if rid.is_none() {
                event.cancelled_instances = self.cancelled_occurrences(uid, seq);
            }
        }
        let key = (uid.to_string(), rid.unwrap_or_default());
        event.superseded = self
            .latest
            .get(&key)
            .is_some_and(|latest| *latest > (seq, dtstamp.to_string()));
    }

    /// The occurrences of the series `uid`, at version `sequence`, that a
    /// CANCEL at that `SEQUENCE` or above names and no REQUEST for that
    /// occurrence re-issued above the CANCEL's `SEQUENCE` since, in
    /// [`ParsedEvent::recurrence_id`]'s form.
    ///
    /// A version of the whole series above the CANCEL is its full current
    /// state and brings the occurrence back unless it excludes it with an
    /// `EXDATE`, RFC 5546 4.4.7: "Note that since this is an entire
    /// rescheduling of the event, any instance-specific information will be
    /// lost, unless explicitly included with the update "REQUEST"."
    /// Thunderbird, Evolution and sabre/vobject read it so, and Outlook and
    /// Google restore deleted occurrences on a change of the series' time
    /// or pattern and send no `EXDATE` (PERSO-303, research note
    /// `.agents/research/2026-11-13-imip-series-update-after-occurrence-cancel.md`).
    /// A REQUEST for the occurrence itself obsoletes the CANCEL when its
    /// `SEQUENCE` is higher, as RFC 5546 2.1.5 orders the components of one
    /// `UID` and `RECURRENCE-ID`.
    pub fn cancelled_occurrences(&self, uid: &str, sequence: u32) -> Vec<String> {
        let Some(cancels) = self.cancels.get(uid.trim()) else {
            return Vec::new();
        };
        cancels
            .instances
            .iter()
            .filter(|(_, cancel)| cancel.sequence >= sequence)
            .filter(|(key, cancel)| {
                let key = (uid.trim().to_string(), (*key).clone());
                self.latest
                    .get(&key)
                    .is_none_or(|(reissued, _)| *reissued <= cancel.sequence)
            })
            .map(|(_, cancel)| cancel.named.clone())
            .collect()
    }
}

/// The `SEQUENCE` the organizer's next version of `uid` goes out with: one
/// above every REQUEST and CANCEL of it the account holds and above `floor`,
/// the version being changed (PERSO-204).
///
/// Every version after the first is a strictly higher one, never a newer
/// `DTSTAMP` at the same sequence: an attendee's reply to the old version
/// would then still look current, and [`StatusIndex::apply`] would not mark
/// the old copy superseded on a store whose clocks disagree.
/// A REPLY echoes the version it answered and names none of its own.
pub fn next_sequence(invites: &[InviteMessage], uid: &str, floor: u32) -> u32 {
    invites
        .iter()
        .filter(|invite| invite.uid() == Some(uid))
        .filter(|invite| matches!(invite.method().as_str(), "REQUEST" | "CANCEL"))
        .map(|invite| invite.parsed.sequence)
        .fold(floor, u32::max)
        .saturating_add(1)
}

/// Our own answer to an invite: the winning REPLY we sent for it, falling back
/// to whatever `PARTSTAT` the organizer's own copy already carries for us.
///
/// The fallback matters because the two are the same fact from two directions:
/// once the organizer has processed our reply their next REQUEST carries it,
/// and before we have replied at all it reads `needs-action`, which is exactly
/// the pre-store default. Our own sent reply lands in the store during the
/// send itself (`crate::outbox::ingest_sent_copy` runs from the append), so
/// this answers correctly without waiting for a sync.
pub fn own_rsvp(
    event: &EventFrontmatter,
    self_address: &str,
    by_addr: Option<&HashMap<String, ReplyObs>>,
) -> String {
    let key = self_address.trim().to_lowercase();
    if !key.is_empty() {
        if let Some(obs) = by_addr.and_then(|m| m.get(&key)) {
            return obs.status.clone();
        }
        if let Some(att) = event
            .attendees
            .iter()
            .find(|a| a.address.trim().eq_ignore_ascii_case(&key))
        {
            return att.status.clone();
        }
    }
    "needs-action".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(row_id: i64, method: &str, uid: &str, sequence: u32) -> InviteMessage {
        let ics = format!(
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nMETHOD:{method}\r\nBEGIN:VEVENT\r\nUID:{uid}\r\n\
             SEQUENCE:{sequence}\r\nDTSTAMP:20261201T100000Z\r\nDTSTART:20261214T090000Z\r\n\
             END:VEVENT\r\nEND:VCALENDAR\r\n"
        );
        InviteMessage {
            row_id,
            mailbox: "sent".to_string(),
            uid: row_id,
            subject: None,
            parsed: crate::calendar::parse_ics(ics.as_bytes()).unwrap(),
        }
    }

    /// The organizer's next version is above every version of the UID the
    /// account holds, a cancellation included, and above the one it changes;
    /// an attendee's REPLY echoes a version and names none of its own.
    #[test]
    fn the_next_sequence_is_above_every_version_held() {
        let held = [
            payload(1, "REQUEST", "a@x", 0),
            payload(2, "REQUEST", "a@x", 2),
            payload(3, "CANCEL", "a@x", 4),
            payload(4, "REPLY", "a@x", 9),
            payload(5, "REQUEST", "b@x", 7),
        ];
        assert_eq!(next_sequence(&held, "a@x", 0), 5);
        assert_eq!(next_sequence(&held, "a@x", 6), 7);
        assert_eq!(next_sequence(&held, "c@x", 0), 1);
    }

    /// A copy of `method` for `uid` at `sequence` in `mailbox`, organised by
    /// me@x and naming `attendees` (the organizer among them, as Outlook
    /// lists it).
    fn told(
        row_id: i64,
        mailbox: &str,
        method: &str,
        sequence: u32,
        attendees: &[&str],
    ) -> InviteMessage {
        let attendees: String = attendees
            .iter()
            .map(|a| format!("ATTENDEE:mailto:{a}\r\n"))
            .collect();
        let ics = format!(
            "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nMETHOD:{method}\r\nBEGIN:VEVENT\r\nUID:m@x\r\n\
             SEQUENCE:{sequence}\r\nDTSTAMP:20261201T100000Z\r\nDTSTART:20261214T090000Z\r\n\
             ORGANIZER:mailto:me@x\r\n{attendees}END:VEVENT\r\nEND:VCALENDAR\r\n"
        );
        InviteMessage {
            row_id,
            mailbox: mailbox.to_string(),
            uid: row_id,
            subject: None,
            parsed: crate::calendar::parse_ics(ics.as_bytes()).unwrap(),
        }
    }

    /// Whether the fold marks the first of `held` cancelled.
    fn cancelled(held: &[InviteMessage]) -> bool {
        let mut event = crate::calendar::event_frontmatter(&held[0].parsed);
        fold_status(held).apply(&mut event, held[0].dtstamp());
        event.cancelled
    }

    /// A cancellation the organizer sent to some of the attendees uninvites
    /// them: its own agenda keeps the meeting, whatever the cancellation's
    /// sequence, while the attendee it names and a cancellation of everyone
    /// still end it.
    #[test]
    fn an_organizer_cancelling_some_attendees_uninvites_them() {
        let request = |mailbox| told(1, mailbox, "REQUEST", 0, &["me@x", "ada@x", "bob@x"]);
        let to_bob = |mailbox| told(2, mailbox, "CANCEL", 1, &["BOB@x"]);
        assert!(
            !cancelled(&[request("sent"), to_bob("sent")]),
            "the organizer's meeting stays live"
        );
        assert!(
            cancelled(&[request("inbox"), to_bob("inbox")]),
            "Bob's copy is cancelled"
        );
        let everyone = told(2, "sent", "CANCEL", 1, &["ada@x", "bob@x"]);
        assert!(cancelled(&[request("sent"), everyone]));
        let nobody = told(2, "sent", "CANCEL", 1, &[]);
        assert!(cancelled(&[request("sent"), nobody]));
    }

    /// The uninvitation an update that replaces every attendee sends names
    /// all those of the version the organizer's agenda holds while the
    /// update waits in the outbox; its mark keeps that meeting live, while
    /// the attendees' copies, and a mark on a cancellation received, still
    /// end it.
    #[test]
    fn a_marked_uninvitation_of_every_attendee_keeps_the_organizers_meeting() {
        let request = |mailbox| told(1, mailbox, "REQUEST", 0, &["me@x", "ada@x", "bob@x"]);
        let uninvite = |mailbox| {
            let mut cancel = told(2, mailbox, "CANCEL", 1, &["ada@x", "bob@x"]);
            cancel.parsed.uninvite = true;
            cancel
        };
        assert!(
            !cancelled(&[request("sent"), uninvite("sent")]),
            "the organizer's meeting stays live"
        );
        assert!(
            cancelled(&[request("inbox"), uninvite("inbox")]),
            "the attendee's copy is cancelled"
        );
        let mut nobody = told(2, "sent", "CANCEL", 1, &[]);
        nobody.parsed.uninvite = true;
        assert!(cancelled(&[request("sent"), nobody]));
    }
}
