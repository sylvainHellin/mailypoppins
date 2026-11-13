use std::time::Duration;

use anyhow::{anyhow, Result};
use futures::TryStreamExt;
use log::{info, warn};

use super::{
    pool::ServerCaps,
    search::{build_imap_search_query, FetchCriteria},
    ImapSession,
};
use crate::ingest::{KnownUids, ModseqMark};
use crate::parse::{compress_uid_set, parse_rfc822_to_fetched_email, FetchedEmail};
use crate::server_search::{Fetched, MailboxStubs, SearchStub};
use crate::sync::{FetchedRaw, MailboxCoverage, MailboxListing, MailboxState};
use crate::timing::TimingSpan;
use crate::types::MessageFlags;

/// The status axes as the server states them: `\Seen`, `\Answered` and the
/// `$Forwarded` keyword (#TKT-0051), plus `\Flagged` (#0007), read off one
/// `FETCH FLAGS` response.
///
/// A keyword arrives as [`async_imap::types::Flag::Custom`], so it goes
/// through [`MessageFlags::parse`] rather than a match arm, which is also what
/// keeps the two spellings of the forwarded keyword in one place.
fn flags_of(msg: &async_imap::types::Fetch) -> MessageFlags {
    let mut out = MessageFlags::default();
    for flag in msg.flags() {
        match flag {
            async_imap::types::Flag::Seen => out.seen = true,
            async_imap::types::Flag::Answered => out.answered = true,
            async_imap::types::Flag::Flagged => out.flagged = true,
            async_imap::types::Flag::Custom(name) => {
                out.forwarded |= MessageFlags::parse(&name).forwarded;
            }
            _ => {}
        }
    }
    out
}

/// The first pass of a server search (PERSO-175): the newest `limit` UIDs
/// `mailbox` matches for a rendered `SEARCH`, or the oldest under
/// `SearchOrder::Oldest` (PERSO-273), each with only its `Date` and
/// `Message-ID` headers, and whether the server matched more.
///
/// This is the seam the unified search grammar (#0086a) lowers to: the caller
/// renders `imap_search` from the shared AST (`crate::search::to_imap` for a
/// plain server, `to_gmail_search_command` for a Gmail `X-GM-RAW` string), so
/// every grammar feature reaches the wire without this function knowing the
/// grammar. `message_id`, when set, tightens the server's substring `HEADER`
/// match to an exact one.
///
/// The caller merges every mailbox's stubs and cuts them to `limit` before
/// downloading anything, then fetches the survivors whole with
/// [`fetch_uids_on_session`] ([`crate::server_search`]). `INTERNALDATE` would
/// be cheaper still but is the arrival time, not the `Date` the hits sort by.
pub async fn search_stubs_on_session<T>(
    session: &mut async_imap::Session<T>,
    imap_search: &str,
    message_id: Option<&str>,
    mailbox: &str,
    limit: usize,
    order: crate::server_search::SearchOrder,
) -> Result<MailboxStubs<u32>>
where
    T: futures::io::AsyncRead + futures::io::AsyncWrite + Unpin + std::fmt::Debug + Send,
{
    session
        .select(mailbox)
        .await
        .map_err(|e| anyhow!("Failed to select mailbox '{}': {}", mailbox, e))?;

    let uids = super::search::uid_search_with_literals(session, imap_search)
        .await
        .map_err(|e| anyhow!("IMAP search failed: {}", e))?;

    let matched = uids.len();
    let mut uid_list: Vec<u32> = uids.into_iter().collect();
    uid_list.sort_unstable();
    // UIDs ascend with arrival, so the highest are the newest and the lowest
    // the oldest; the merge then orders the stubs by their `Date`.
    let selected_uids: Vec<u32> = match order {
        crate::server_search::SearchOrder::Oldest => uid_list.into_iter().take(limit).collect(),
        _ => uid_list.into_iter().rev().take(limit).collect(),
    };
    let more = matched > limit;
    if selected_uids.is_empty() {
        return Ok(MailboxStubs {
            stubs: Vec::new(),
            more,
            capped: false,
        });
    }

    let fetched: Vec<_> = session
        .uid_fetch(
            &compress_uid_set(&selected_uids),
            "(UID BODY.PEEK[HEADER.FIELDS (DATE MESSAGE-ID)])",
        )
        .await
        .map_err(|e| anyhow!("Failed to fetch headers: {}", e))?
        .try_collect()
        .await
        .map_err(|e| anyhow!("Failed to collect headers: {}", e))?;

    let wanted = message_id.map(|mid| super::search::normalize_message_id(mid).to_string());
    let mut stubs = Vec::new();
    for msg in fetched.iter() {
        let Some(uid) = msg.uid else { continue };
        // The same parse the full message gets, so `date` and `message_id`
        // are the values the output carries.
        let (date, mid) = match parse_rfc822_to_fetched_email(msg.header().unwrap_or_default()) {
            Some(email) => (email.date, email.message_id),
            None => ("(unknown date)".to_string(), None),
        };
        // `HEADER "Message-ID"` is a substring match on the server side, made
        // exact here.
        if let Some(wanted) = &wanted {
            let exact = mid.as_deref().is_some_and(|m| {
                super::search::normalize_message_id(m).eq_ignore_ascii_case(wanted)
            });
            if !exact {
                continue;
            }
        }
        stubs.push(SearchStub {
            key: uid,
            date,
            message_id: mid,
            folder: None,
        });
    }
    Ok(MailboxStubs {
        stubs,
        more,
        capped: false,
    })
}

/// The second pass of a server search (PERSO-175): the full messages behind
/// `uids` in `mailbox`, with their flags, in one `UID FETCH`.
///
/// A UID the answer does not carry, expunged since the first pass, and one
/// whose bytes do not parse come back in `missing` with why, so the search
/// reports them rather than listing fewer hits without a word.
pub async fn fetch_uids_on_session(
    session: &mut ImapSession,
    mailbox: &str,
    uids: &[u32],
) -> Result<Fetched<u32>> {
    if uids.is_empty() {
        return Ok(Fetched::default());
    }
    session
        .select(mailbox)
        .await
        .map_err(|e| anyhow!("Failed to select mailbox '{}': {}", mailbox, e))?;
    let fetched: Vec<_> = session
        .uid_fetch(&compress_uid_set(uids), "(BODY.PEEK[] FLAGS)")
        .await
        .map_err(|e| anyhow!("Failed to fetch emails: {}", e))?
        .try_collect()
        .await
        .map_err(|e| anyhow!("Failed to collect emails: {}", e))?;
    let mut answer = Fetched::default();
    let mut answered = std::collections::HashSet::new();
    for msg in fetched.iter() {
        let Some(uid) = msg.uid else { continue };
        answered.insert(uid);
        match parse_rfc822_to_fetched_email(msg.body().unwrap_or_default()) {
            Some(mut email) => {
                email.flags = flags_of(msg);
                answer.emails.push((uid, email));
            }
            None => answer.missing.push((uid, "did not parse".to_string())),
        }
    }
    answer.missing.extend(
        uids.iter()
            .filter(|uid| !answered.contains(uid))
            .map(|uid| (*uid, "no longer in the mailbox".to_string())),
    );
    Ok(answer)
}

/// Fetch one message's raw RFC822 bytes by Message-ID (#0104).
///
/// The search overlay's fetch-into-store action needs the UID the mailbox
/// holds the message under, which the plain search path discards, and the raw
/// bytes ingest wants for the `raw` blob and the threading headers. The
/// server's `HEADER "Message-ID"` match is a substring match, so the hits are
/// filtered to an exact one, the same guarantee [`search_stubs_on_session`]
/// gives.
pub async fn fetch_raw_by_message_id(
    session: &mut ImapSession,
    mailbox: &str,
    message_id: &str,
) -> Result<Option<(u32, Vec<u8>, FetchedEmail)>> {
    session
        .select(mailbox)
        .await
        .map_err(|e| anyhow!("Failed to select mailbox '{}': {}", mailbox, e))?;

    let criteria = FetchCriteria {
        message_id: Some(message_id.to_string()),
        ..Default::default()
    };
    let query = build_imap_search_query(&criteria);
    let uids = session
        .uid_search(&query)
        .await
        .map_err(|e| anyhow!("IMAP search failed: {}", e))?;

    let wanted = super::search::normalize_message_id(message_id).to_string();
    let mut uid_list: Vec<u32> = uids.into_iter().collect();
    uid_list.sort_unstable();
    for uid in uid_list {
        let fetched: Vec<_> = session
            .uid_fetch(&uid.to_string(), "(BODY.PEEK[] FLAGS)")
            .await
            .map_err(|e| anyhow!("Failed to fetch email: {}", e))?
            .try_collect()
            .await
            .map_err(|e| anyhow!("Failed to collect email: {}", e))?;
        for msg in fetched.iter() {
            let body_raw = msg.body().unwrap_or_default();
            let Some(mut email) = parse_rfc822_to_fetched_email(body_raw) else {
                continue;
            };
            let exact = email.message_id.as_deref().is_some_and(|m| {
                super::search::normalize_message_id(m).eq_ignore_ascii_case(&wanted)
            });
            if !exact {
                continue;
            }
            email.flags = flags_of(msg);
            return Ok(Some((uid, body_raw.to_vec(), email)));
        }
    }
    Ok(None)
}

// ---------------------------------------------------------------------------
// Store ingest fetch
// ---------------------------------------------------------------------------

// The store fetch's result types live in `crate::sync` since #0059: they are
// the engine's contract with every backend, not this module's private shape.
// See `MailboxListing`, `FetchedRaw` and `MailboxState`.

/// The UIDs the store holds that the server did not list, up to `ceiling`.
///
/// `listed` is the *whole* mailbox as `UID SEARCH ALL` returned it, not the
/// download window: the enumeration is complete even when the fetch is capped,
/// so a message archived, moved or deleted in another client is absent from it
/// whatever its UID. Diffing against the window instead was #0072, where
/// archiving the oldest mail elsewhere left rows below the window's bottom
/// that nothing could ever reach.
///
/// Two kinds of known UID are still exempt, both of them locally written rather
/// than server-issued:
///
/// - negative UIDs, the `-id` sentinel a local move parks a row on (see
///   [`crate::store::write`]), waiting for the destination's next sync to give
///   it a real UID;
/// - anything above `ceiling`, which the caller sets to `UIDNEXT - 1`. A Sent
///   copy appended without an `APPENDUID` is stored under
///   [`crate::ingest::graph_uid`], a 63-bit hash of the Message-ID that no
///   server would ever assign, and a row written optimistically ahead of the
///   server sits just above the same line. `UIDNEXT` is the right ceiling
///   rather than `max(listed)` because it does not drop when the newest
///   message is the one that was deleted.
///
/// Whether the result may be *applied* is a separate question, answered by the
/// coverage of [`MailboxListing`] and by [`crate::ingest::pass_may_prune`].
pub fn vanished_uids(
    known: &std::collections::HashSet<i64>,
    listed: &[u32],
    ceiling: u32,
) -> Vec<u32> {
    let listed: std::collections::HashSet<u32> = listed.iter().copied().collect();
    let mut out: Vec<u32> = known
        .iter()
        .filter(|&&uid| uid > 0 && uid <= ceiling as i64)
        .map(|&uid| uid as u32)
        .filter(|uid| !listed.contains(uid))
        .collect();
    out.sort_unstable();
    out
}

/// The highest UID `known` holds that a server could have issued, i.e. the
/// mailbox's local high-water mark.
///
/// Placeholder rows are excluded by the same `ceiling` the prune uses, so a
/// `graph_uid` hash cannot push the mark to 2^62 and make every real arrival
/// look old.
fn high_water(known: &std::collections::HashSet<i64>, ceiling: u32) -> u32 {
    known
        .iter()
        .filter(|&&uid| uid > 0 && uid <= ceiling as i64)
        .map(|&uid| uid as u32)
        .max()
        .unwrap_or(0)
}

/// What one pass learned about the arrivals it was supposed to bring in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArrivalCoverage {
    /// True when at least one arrival above the mark is still not in the store
    /// after this pass. Suspends the prune (see [`crate::ingest::pass_may_prune`]).
    pub incomplete: bool,
    /// The mark to carry into the next pass, or `None` when every arrival is
    /// in and the next pass may derive its own mark from the store again.
    pub pending_mark: Option<u32>,
}

/// The line above which every listed UID must be in the store for the pass to
/// count as complete, or `None` when the mailbox has no line to stand on.
///
/// The floor is what the mailbox is known to have held: the store's own
/// high-water mark, or the top the last recorded pass saw (`sync_cursors.last_uid`,
/// `prior`) when the rows themselves are gone. A UID at or below it existed
/// before the last pass and was already passed over as backlog, so it is not an
/// arrival now. `prior` is clamped to the same `ceiling` the rest of the module
/// uses, which is what keeps a placeholder UID from lifting the floor to 2^62.
///
/// With a carried mark the answer is the lower of the two, which is the whole
/// point: a pass that ingested the top of the window raises the floor above the
/// arrivals it could *not* reach, and deriving the mark afresh would put them
/// below it and declare the pass complete (#0072 review note 1).
///
/// `None` is *first contact*: no carried mark, no cursor row, no rows. Nothing
/// about this mailbox is known yet, so nothing the server lists is an arrival
/// and there is no mark worth handing to the next pass. Persisting one there is
/// what turned a first capped sync of a large mailbox into a mark of `0` that
/// no positional window could ever meet, suspending the prune account-wide
/// until a manual full sync (#0072 sweep review B1).
fn arrival_mark(
    known: &std::collections::HashSet<i64>,
    ceiling: u32,
    carried: Option<u32>,
    prior: Option<u32>,
) -> Option<u32> {
    let prior = prior.map(|uid| uid.min(ceiling));
    let floor = high_water(known, ceiling).max(prior.unwrap_or(0));
    match carried {
        Some(mark) => Some(mark.min(floor)),
        // A cursor row is history even when it records a top of 0 and every
        // local row is gone: a mailbox emptied and then bulk-moved into still
        // has to defer, which is why the cursor is consulted rather than the
        // rows alone.
        None if prior.is_none() && floor == 0 => None,
        None => Some(floor),
    }
}

/// Whether any message that arrived since the mailbox's arrival mark is still
/// not in the store after this pass, and the mark the next pass must use.
///
/// "Arrival" is the load-bearing word. A quick sync downloads the last `limit`
/// UIDs and deliberately ignores the backlog below them, so measuring coverage
/// as `everything the folder holds` would report every capped pass as short and
/// suspend the prune forever on any mailbox bigger than the window. What the
/// prune actually depends on is that the copy of a message *moved into* this
/// mailbox landed, and a move issues a fresh UID at the top of the folder;
/// anything above the mark is such an arrival, and all of them must be in.
///
/// The mark is *persisted* (`sync_cursors.arrival_mark`) rather than recomputed,
/// because an unmet arrival outlives the pass that missed it: bulk-move 300
/// messages into a mailbox a 100-UID window can only take the top of, and pass 2
/// would otherwise stand on a high-water mark the 200 stragglers sit below, open
/// the gate, and prune the source rows of copies that were never ingested. The
/// mark therefore stays put until a pass actually reaches through it, which any
/// full sync does; it also clears when the stragglers stop being listed (deleted
/// on the server), so it cannot deadlock.
///
/// First contact ([`arrival_mark`] returning `None`) is the one case that hands
/// nothing on. The pass is still reported short whenever it could not take the
/// whole listing, which costs one conservative pass and nothing more, because
/// the cursor it writes gives the next pass a real floor to stand on.
pub(crate) fn arrival_coverage(
    listed: &[u32],
    known: &std::collections::HashSet<i64>,
    ceiling: u32,
    ingested: &[u32],
    carried: Option<u32>,
    prior: Option<u32>,
) -> ArrivalCoverage {
    let mark = arrival_mark(known, ceiling, carried, prior);
    let ingested: std::collections::HashSet<u32> = ingested.iter().copied().collect();
    let incomplete = listed
        .iter()
        .filter(|&&uid| uid > mark.unwrap_or(0))
        .filter(|&&uid| !known.contains(&(uid as i64)))
        .any(|uid| !ingested.contains(uid));
    ArrivalCoverage {
        incomplete,
        pending_mark: mark.filter(|_| incomplete),
    }
}

/// Whether the `UID SEARCH ALL` listing is the whole mailbox.
///
/// A listing shorter than the `EXISTS` the same SELECT announced is a partial
/// answer, and a partial answer reads exactly like a mass deletion. Two
/// independent server statements have to agree before anything is pruned, which
/// is what bounds the blast radius of the diff to something a single malformed
/// response cannot widen.
///
/// A listing *longer* than `EXISTS` is not a contradiction worth acting on:
/// messages can arrive between the two responses, and every extra UID can only
/// keep a row alive.
fn enumeration_complete(listed_len: usize, exists: u32) -> bool {
    listed_len >= exists as usize
}

/// The prune's top clamp, and the line that separates a server UID from a
/// locally written placeholder.
///
/// `UIDNEXT` is what SELECT promised and is preferred over `max(listed)`
/// because it does not drop when the newest message is the one that was
/// deleted. A server that withholds it leaves the highest UID it did list;
/// a server that withholds both leaves 0, which prunes nothing at all.
///
/// `listed` must be sorted ascending, as it is at every call site.
pub(crate) fn ceiling(uid_next: Option<u32>, listed: &[u32]) -> u32 {
    uid_next
        .map(|n| n.saturating_sub(1))
        .or_else(|| listed.last().copied())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// CONDSTORE flag deltas (#0041)
// ---------------------------------------------------------------------------

/// How pass 1 should ask the server for the window's flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FlagPass {
    /// `UID FETCH <window> (UID FLAGS)`: every flag in the window, restated.
    /// The only behaviour before #0041, and still the answer for every server
    /// without CONDSTORE, which includes Proton Bridge, the daily driver.
    Full,
    /// `UID FETCH <window> (UID FLAGS) (CHANGEDSINCE n)`: only what the server
    /// says has changed since mod-sequence `n`.
    ChangedSince(u64),
}

/// The RFC 7162 fetch-modifier form, confirmed against async-imap 0.11.2 in the
/// ticket's spike: `uid_fetch` interpolates the query verbatim after the UID
/// set, so the modifier is simply the tail of the query string.
///
/// A free function with a test rather than a `format!` at the call site,
/// because getting this string wrong is not a compile error and not a runtime
/// error either: a server that cannot parse it answers `BAD`, and one that
/// parses it differently answers with the wrong set of messages.
pub(crate) fn flag_query(pass: FlagPass) -> String {
    match pass {
        FlagPass::Full => "(UID FLAGS)".to_string(),
        FlagPass::ChangedSince(modseq) => format!("(UID FLAGS) (CHANGEDSINCE {modseq})"),
    }
}

/// Whether this pass may ask for a flag delta instead of the whole window.
///
/// Every condition is a reason the delta could *miss a flag change*, and a
/// missed flag change is #0004: a message marked read in webmail that never
/// comes back unread locally, silently, with no error anywhere. So the gate is
/// unanimous rather than best-effort:
///
/// - `condstore` is the strict CAPABILITY gate. Advertise is not correctness,
///   but not advertising is conclusive.
/// - `server` is the `HIGHESTMODSEQ` this SELECT reported. A server that
///   advertised CONDSTORE and then said nothing is not one to hand a
///   `CHANGEDSINCE` to.
/// - `stored` is the resume point. Absent means no previous pass could vouch
///   for one, so there is nothing to be a delta *from*.
/// - `uidvalidity_reset` invalidates the mailbox's whole numbering; the modseq
///   recorded under the old one describes a different mailbox.
/// - `server < stored` is the server having gone backwards (a restore from
///   backup, a rebuilt index). RFC 7162 requires the server to bump
///   UIDVALIDITY in that case, so seeing it means the server is not keeping the
///   contract that makes the delta safe.
pub(crate) fn flag_pass(
    condstore: bool,
    stored: Option<u64>,
    server: Option<u64>,
    uidvalidity_reset: bool,
) -> FlagPass {
    if !condstore || uidvalidity_reset {
        return FlagPass::Full;
    }
    match (stored, server) {
        (Some(stored), Some(server)) if stored > 0 && server >= stored => {
            FlagPass::ChangedSince(stored)
        }
        _ => FlagPass::Full,
    }
}

/// The `CHANGEDSINCE` half of a [`FlagPlan`]: every UID from `from` to `to`
/// whose flags changed after mod-sequence `since`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FlagDelta {
    pub since: u64,
    pub from: u32,
    pub to: u32,
}

impl FlagDelta {
    /// The UID set the delta's `UID FETCH` names.
    pub(crate) fn uid_set(&self) -> String {
        if self.from == self.to {
            self.from.to_string()
        } else {
            format!("{}:{}", self.from, self.to)
        }
    }
}

/// How pass 1 asks the server for flags, and what the answer will vouch for
/// (PERSO-202).
///
/// A stored [`ModseqMark`] covers the UIDs at or above its `from_uid`, so the
/// pass splits there: the window's UIDs below the mark get every flag, and
/// everything the server lists from the mark up gets a `CHANGEDSINCE`. The
/// delta reaches *below* the window when the mark does, which is the point:
/// it costs one line per changed message rather than one per message, so a
/// quick sync over the newest 50 still hears about a flag changed elsewhere on
/// a message from last year.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FlagPlan {
    /// The window's UIDs whose every flag is fetched, ascending.
    pub full: Vec<u32>,
    /// The `CHANGEDSINCE` fetch, when there is a mark to resume from and the
    /// server lists anything at or above it.
    pub delta: Option<FlagDelta>,
    /// The lowest UID at and above which the plan, run to the end, has seen
    /// every flag change the server made up to the SELECT that started it:
    /// the `from_uid` of the mark the pass may record.
    pub covered_from: u32,
}

/// The [`FlagPlan`] for a non-empty, ascending `window` whose server listing
/// tops out at `listed_top`, given the gate's decision `pass` and the stored
/// `mark` that decision was taken on.
///
/// Why `covered_from` is sound, case by case:
///
/// - No delta ([`FlagPass::Full`]): every flag in the window was fetched, and
///   the window is the top of the listing, so every listed UID at or above its
///   lowest is covered.
/// - A delta from mark `(m, L)`: the stored mark already vouched for every
///   flag at or above `L` as of `m`, and the delta fetched every change after
///   `m` from `L` to the top, so from `L` up the store is current. The window's
///   UIDs below `L` were fetched in full. Together that is everything from the
///   lower of `L` and the window's bottom.
///
/// A UID the server lists at or above `covered_from` that the store does not
/// hold is not a gap: it has no flags to be wrong, and whichever pass
/// downloads it reads its flags with its body.
pub(crate) fn flag_plan(
    window: &[u32],
    listed_top: u32,
    mark: Option<ModseqMark>,
    pass: FlagPass,
) -> FlagPlan {
    let bottom = window.first().copied().unwrap_or(1);
    let (FlagPass::ChangedSince(since), Some(mark)) = (pass, mark) else {
        return FlagPlan {
            full: window.to_vec(),
            delta: None,
            covered_from: bottom,
        };
    };
    let from = mark.from_uid.max(1);
    FlagPlan {
        full: window.iter().copied().filter(|&uid| uid < from).collect(),
        delta: (from <= listed_top).then_some(FlagDelta {
            since,
            from,
            to: listed_top,
        }),
        covered_from: bottom.min(from),
    }
}

/// The mark this pass may leave behind as the next one's resume point, or
/// `None` to keep whatever is stored (the cursor UPSERT carries it forward).
///
/// `covered_from` is the lowest UID the pass saw every flag change at or above
/// ([`FlagPlan::covered_from`]), and `None` for a pass that looked at no flag:
/// `mp sync -n 0` over a populated mailbox computes a whole vanished set and
/// returns before pass 1, and recording the server's HIGHESTMODSEQ off it made
/// the next `CHANGEDSINCE` skip every flag change in between (#0041 review
/// blocker). An empty mailbox has nothing to have missed and covers from 1.
///
/// The bound is what lets a capped pass record (PERSO-202). Until it existed a
/// mark claimed the whole mailbox, so only a pass that saw every message could
/// write one, and on a mailbox bigger than the quick-sync window the delta ran
/// only after a full sync and quick syncs consumed the resume point without
/// advancing it.
///
/// `enumeration_complete` is the second condition: if `UID SEARCH ALL` came
/// back short, the window and the listing's top are read off a listing already
/// known to be untrustworthy, and a UID it left out may sit inside the range.
///
/// `bodies_complete` is the third (#0113): a pass that stopped at its body
/// deadline looked at every flag in the window and at only part of the mail. A
/// truncated pass vouches for nothing, and the stored mark, still true, stays.
pub(crate) fn modseq_to_record(
    server: Option<u64>,
    covered_from: Option<u32>,
    enumeration_complete: bool,
    bodies_complete: bool,
) -> Option<ModseqMark> {
    if !enumeration_complete || !bodies_complete {
        return None;
    }
    let from_uid = covered_from?.max(1);
    let modseq = server.filter(|&m| m > 0 && i64::try_from(m).is_ok())?;
    Some(ModseqMark { modseq, from_uid })
}

/// Which UIDs in the window the store does not hold.
///
/// The full pass reads this off the `FETCH` response, which restates every
/// message. A delta cannot: it answers only for messages whose flags changed,
/// so an untouched message the store has never seen is simply absent from it,
/// and taking "absent" for "already held" would stop new mail from downloading.
/// The window and the skip list are both already in hand and say it exactly.
pub(crate) fn new_uids_in_window(
    window: &[u32],
    known: &std::collections::HashSet<i64>,
) -> Vec<u32> {
    window
        .iter()
        .copied()
        .filter(|uid| !known.contains(&(*uid as i64)))
        .collect()
}

/// How many UIDs one `UID FETCH ... BODY.PEEK[]` asks for (#0113).
///
/// Twenty is the Graph batch size and, on the mailbox that motivated the
/// ticket (34 MB over 100 messages), about 7 MB of mail: small enough that the
/// deadline is answered within a few seconds of expiring, large enough that the
/// per-command round trip stays a rounding error next to the download.
pub(crate) const BODY_CHUNK_SIZE: usize = 20;

/// The body pass's `UID FETCH` commands, newest chunk first.
///
/// `new_uids` is ascending (it is read off the ascending window), so the chunks
/// are taken off the *end*: a pass that stops early has downloaded the newest
/// mail and left the oldest, which is the half the user is least likely to be
/// waiting for and the half the next pass picks up from the same cursor. Taking
/// them off the front would leave the newest message undownloaded for as long
/// as the backlog takes to drain.
///
/// The short chunk is therefore the last one, holding the oldest UIDs.
pub(crate) fn body_chunks(new_uids: &[u32], chunk: usize) -> Vec<&[u32]> {
    new_uids.rchunks(chunk.max(1)).collect()
}

/// Whether a sync pass must stop before asking for another chunk of bodies,
/// having asked for `asked` chunks so far and spent `spent` of a `budget` on
/// them, every mailbox of the pass counted.
///
/// The budget is the pass's, not each mailbox's (PERSO-300): the mailboxes
/// download one after another, so a budget per mailbox let a tick catching up
/// six of them spend six budgets while every other account waited behind it.
/// A mailbox the budget cuts short, and every mailbox after it, closes short
/// and resumes on the next pass.
///
/// Two rules, and both are load-bearing. The pass's first chunk always goes
/// out, so a budget that is already spent still makes progress rather than
/// turning the pass into a no-op that repeats forever. And the check sits
/// *between* chunks: wrapping a `UID FETCH` in a timeout would abandon a
/// command mid-stream and leave unread response bytes on a session that goes
/// back into the pool ([`super::pool`]), so the next borrower reads this
/// fetch's mail as the answer to its own command. A chunk that has been asked
/// for is always collected in full, which is why a pass can overrun its budget
/// by one chunk.
///
/// The budget is spent, not elapsed: the engine ingests each chunk before it
/// asks for the next, so the wall clock would charge the download for the
/// ingest too.
pub(crate) fn stop_before_chunk(asked: usize, budget: Option<Duration>, spent: Duration) -> bool {
    asked > 0 && budget.is_some_and(|budget| spent >= budget)
}

/// What one mailbox's body pass still has to download, from the listing that
/// planned it to the last chunk the engine asks for (PERSO-300).
pub(crate) struct BodyPlan {
    /// The mailbox as the server names it, which the body pass `SELECT`s again
    /// on whichever pooled session it borrows.
    pub(crate) mailbox: String,
    /// What the listing's `SELECT` reported: a body pass that finds another
    /// one has lost the numbering its UIDs were planned under.
    pub(crate) uid_validity: Option<u32>,
    /// The `UID FETCH` commands still to send, newest first
    /// ([`body_chunks`]).
    pub(crate) chunks: std::collections::VecDeque<Vec<u32>>,
    /// How many new UIDs the listing found, for the deadline's log line.
    pub(crate) new: usize,
    /// How many bodies the pass has handed over so far.
    pub(crate) handed: usize,
}

/// The listing half of the store ingest fetch: `SELECT`, `UID SEARCH ALL`,
/// the flags of the window, and the plan of the body pass (PERSO-300).
///
/// Pass 1 fetches `UID FLAGS` over the whole window; the plan it returns
/// names the UIDs pass 2 downloads with `BODY.PEEK[]`, only those the store
/// does not hold yet, a chunk at a time through [`fetch_body_chunk_on_session`]
/// as the engine asks for them. Identity is the UID, so pass 1 no longer needs
/// the `Message-ID` header the `.md` era compared against a directory scan.
///
/// Pass 1 always covers the full `limit` window even when nothing is new: the
/// `\Seen` flags it collects are the only server-to-local read-status channel
/// (ticket #0004), so shrinking the window silently drops flag changes made in
/// other clients. A plan with no chunk skips pass 2 entirely.
///
/// The skip list is resolved against the server's UIDVALIDITY *after* SELECT
/// (see [`KnownUids::resolve`]): a renumbering hands recycled UIDs to different
/// messages, so carrying the stored list across one would make pass 2 skip
/// bodies that were never downloaded.
///
/// The listing's [`MailboxListing::coverage`] keeps what the coverage
/// arithmetic needs, so the engine asks it once the body pass has ended,
/// whichever way it ended: run to the end, cut at the budget, stopped or
/// failed.
///
/// On a CONDSTORE server with a stored mark, pass 1 is the [`FlagPlan`] rather
/// than the whole window (#0041, PERSO-202). Generic over the transport so a
/// test can drive it against a scripted server.
pub(crate) async fn list_mailbox_on_session<T>(
    session: &mut async_imap::Session<T>,
    mailbox: &str,
    limit: Option<usize>,
    known: KnownUids,
    caps: ServerCaps,
) -> Result<(MailboxListing, BodyPlan)>
where
    T: futures::io::AsyncRead + futures::io::AsyncWrite + Unpin + std::fmt::Debug + Send,
{
    let mut span = TimingSpan::with_context("list_mailbox", mailbox.to_string());

    // `SELECT (CONDSTORE)` only where the server advertised CONDSTORE: it is
    // the command that makes the server report HIGHESTMODSEQ, and a server that
    // never heard of it answers BAD. Proton Bridge takes the plain SELECT it
    // always took.
    let imap_mailbox = if caps.condstore {
        session.select_condstore(mailbox).await
    } else {
        session.select(mailbox).await
    }
    .map_err(|e| anyhow!("Failed to select mailbox '{}': {}", mailbox, e))?;
    let server_modseq = imap_mailbox.highest_modseq;
    span.mark("select");
    let state = MailboxState {
        uid_validity: imap_mailbox.uid_validity,
        uid_next: imap_mailbox.uid_next,
        exists: imap_mailbox.exists,
    };

    let stored_uidvalidity = known.uidvalidity;
    let stored_arrival_mark = known.arrival_mark;
    let stored_prior_high_water = known.prior_high_water;
    let stored_modseq = known.highest_modseq;
    let (known_uids, uidvalidity_reset) = known.resolve(state.uid_validity);
    // A mark and a recorded top are both UIDs, so a renumbering makes them
    // meaningless: they are dropped with the skip list they travelled with.
    let (carried_mark, prior_high_water) = if uidvalidity_reset {
        (None, None)
    } else {
        (stored_arrival_mark, stored_prior_high_water)
    };
    // ...and so is a mod-sequence, for the same reason; the engine clears the
    // stored column on the same branch.
    let known_modseq = if uidvalidity_reset {
        None
    } else {
        stored_modseq
    };
    if uidvalidity_reset {
        warn!(
            "UIDVALIDITY for '{}' changed from {:?} to {:?}: refetching the whole window, \
             the rows are rebound through their Message-IDs",
            mailbox, stored_uidvalidity, state.uid_validity
        );
    }
    let uids = session
        .uid_search("ALL")
        .await
        .map_err(|e| anyhow!("IMAP search failed: {}", e))?;
    span.mark("uid_search");

    let mut listed: Vec<u32> = uids.into_iter().collect();
    listed.sort_unstable();

    let ceiling = ceiling(state.uid_next, &listed);
    let enumeration_complete = enumeration_complete(listed.len(), state.exists);
    if !enumeration_complete {
        warn!(
            "'{}' listed {} UID(s) but announced EXISTS {}: treating the enumeration as short, \
             nothing will be pruned this pass",
            mailbox,
            listed.len(),
            state.exists
        );
    }
    let vanished = if enumeration_complete {
        vanished_uids(&known_uids, &listed, ceiling)
    } else {
        Vec::new()
    };
    if !vanished.is_empty() {
        info!(
            "Store fetch for '{}': {} row(s) are no longer on the server",
            mailbox,
            vanished.len()
        );
    }

    // The delta gate, decided before anything is fetched so the reason is one
    // readable expression rather than a condition spread over the two passes.
    let pass = flag_pass(
        caps.condstore,
        known_modseq.map(|mark| mark.modseq),
        server_modseq,
        uidvalidity_reset,
    );

    let mut window: Vec<u32> = match limit {
        Some(n) => listed.iter().rev().take(n).copied().collect(),
        None => listed.clone(),
    };
    window.sort_unstable();

    // Pass 1: flags, every one over the part of the window no mark covers
    // (~40 bytes per message) and only the changed ones from the mark up. An
    // empty window, an empty mailbox or `mp sync -n 0`, asks for none and has
    // nothing new; it still answers the coverage question below, because
    // `mp sync -n 0` computes a whole vanished set and hardcoding "complete"
    // would force the gate open on a prune-only pass (#0072 review note 3).
    let mut new_uids: Vec<u32> = Vec::new();
    let mut known_flags: Vec<(u32, MessageFlags)> = Vec::new();
    let mut skipped = 0;
    // The lowest UID this pass saw every flag change at or above, `None` when
    // it looked at no flag. An empty window covers the mailbox only when the
    // mailbox is itself empty: `mp sync -n 0` over a populated mailbox fetched
    // no flags and may not advance the resume point (#0041 review).
    let mut covered_from = listed.is_empty().then_some(1);
    if !window.is_empty() {
        let listed_top = listed.last().copied().unwrap_or(0);
        let flags = flag_plan(&window, listed_top, known_modseq, pass);
        let mut flagged = Vec::new();
        if !flags.full.is_empty() {
            flagged.extend(
                session
                    .uid_fetch(compress_uid_set(&flags.full), flag_query(FlagPass::Full))
                    .await
                    .map_err(|e| anyhow!("Failed to fetch flags: {}", e))?
                    .try_collect::<Vec<_>>()
                    .await
                    .map_err(|e| anyhow!("Failed to collect flags: {}", e))?,
            );
        }
        if let Some(delta) = flags.delta {
            flagged.extend(
                session
                    .uid_fetch(
                        delta.uid_set(),
                        flag_query(FlagPass::ChangedSince(delta.since)),
                    )
                    .await
                    .map_err(|e| anyhow!("Failed to fetch changed flags: {}", e))?
                    .try_collect::<Vec<_>>()
                    .await
                    .map_err(|e| anyhow!("Failed to collect changed flags: {}", e))?,
            );
        }
        span.mark("pass1_flags");

        for msg in flagged.iter() {
            let Some(uid) = msg.uid else { continue };
            if known_uids.contains(&(uid as i64)) {
                known_flags.push((uid, flags_of(msg)));
            } else {
                new_uids.push(uid);
            }
        }
        skipped = if let Some(delta) = flags.delta {
            // A delta answers only for messages whose flags changed, so what
            // came back says nothing about which UIDs are new: an unchanged
            // message the store has never seen would look like an absence, and
            // one it answers for below the window is not this pass's to
            // download. The window and the skip list, both of which this pass
            // already holds, say it exactly. Getting this backwards would
            // silently stop downloading new mail.
            new_uids = new_uids_in_window(&window, &known_uids);
            info!(
                "Store fetch for '{}': CONDSTORE delta since {} over {} returned {} changed \
                 flag(s), {} UID(s) fetched in full",
                mailbox,
                delta.since,
                delta.uid_set(),
                known_flags.len(),
                flags.full.len()
            );
            window.len() - new_uids.len()
        } else {
            known_flags.len()
        };
        covered_from = Some(flags.covered_from);
    }
    if !new_uids.is_empty() {
        info!(
            "Store fetch for '{}': {} new, {} already ingested",
            mailbox,
            new_uids.len(),
            skipped
        );
    }
    let plan = BodyPlan {
        mailbox: mailbox.to_string(),
        uid_validity: state.uid_validity,
        chunks: body_chunks(&new_uids, BODY_CHUNK_SIZE)
            .into_iter()
            .map(<[u32]>::to_vec)
            .collect(),
        new: new_uids.len(),
        handed: 0,
    };
    let listing = MailboxListing {
        skipped,
        known_flags,
        state,
        vanished,
        listed: listed.clone(),
        uidvalidity_reset,
        enumeration_complete,
        coverage: Box::new(move |downloaded: &[u32], bodies_complete: bool| {
            let coverage = arrival_coverage(
                &listed,
                &known_uids,
                ceiling,
                downloaded,
                carried_mark,
                prior_high_water,
            );
            MailboxCoverage {
                download_incomplete: coverage.incomplete,
                pending_arrival_mark: coverage.pending_mark,
                highest_modseq: modseq_to_record(
                    server_modseq,
                    covered_from,
                    enumeration_complete,
                    bodies_complete,
                ),
            }
        }),
    };
    Ok((listing, plan))
}

/// One chunk of a body pass: the bodies and flags of `uids`, in one
/// `UID FETCH`, on a session that has `SELECT`ed their mailbox (PERSO-300).
///
/// Each response is turned into a [`FetchedRaw`] as it is read, so the chunk
/// is held once and not twice, and a UID the answer does not carry is simply
/// absent: the coverage arithmetic counts it as not downloaded. Ascending, the
/// order ingest saw when the whole window was one command.
pub(crate) async fn fetch_body_chunk_on_session(
    session: &mut ImapSession,
    uids: &[u32],
) -> Result<Vec<FetchedRaw>> {
    let mut out: Vec<FetchedRaw> = session
        .uid_fetch(compress_uid_set(uids), "(UID BODY.PEEK[] FLAGS)")
        .await
        .map_err(|e| anyhow!("Failed to fetch emails: {}", e))?
        .try_filter_map(|msg| async move {
            Ok(match (msg.uid, msg.body()) {
                (Some(uid), Some(body)) => {
                    Some(FetchedRaw::new(uid, body.to_vec(), flags_of(&msg)))
                }
                _ => None,
            })
        })
        .try_collect()
        .await
        .map_err(|e| anyhow!("Failed to collect emails: {}", e))?;
    out.sort_by_key(|m| m.uid);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// A UID the server no longer lists is gone.
    #[test]
    fn a_uid_missing_from_the_listing_is_vanished() {
        let known = HashSet::from([10, 11, 12, 13]);
        assert_eq!(vanished_uids(&known, &[10, 12, 13], 13), vec![11]);
    }

    /// #0072: the reported bug. The oldest inbox message is archived in another
    /// client, so it is below every UID the server still lists. The old
    /// window-range clamp could not see it; the full listing can.
    #[test]
    fn the_oldest_uid_archived_elsewhere_is_vanished() {
        let known: HashSet<i64> = (1..=83).collect();
        let listed: Vec<u32> = (2..=83).collect();
        assert_eq!(vanished_uids(&known, &listed, 83), vec![1]);
    }

    /// A capped download does not cap the diff: `UID SEARCH ALL` enumerated the
    /// whole mailbox even when only its last 50 UIDs were fetched, so the rows
    /// below the window are proved present, not merely unexamined.
    #[test]
    fn uids_below_the_download_window_are_kept_when_the_server_still_lists_them() {
        let known: HashSet<i64> = (1..=100).collect();
        let listed: Vec<u32> = (1..=100).collect();
        assert_eq!(vanished_uids(&known, &listed, 100), Vec::<u32>::new());
    }

    /// The one clamp that survives: above `UIDNEXT - 1` sit the locally written
    /// placeholders (a `graph_uid` hash, a row written ahead of the server),
    /// which the server was never asked about.
    #[test]
    fn uids_above_the_ceiling_survive() {
        let known = HashSet::from([5, 6, 7, 4_611_686_018_427_387_904]);
        assert_eq!(vanished_uids(&known, &[5, 7], 7), vec![6]);
    }

    /// A UIDVALIDITY reset hands `resolve` an empty known set, which must
    /// produce an empty prune: a renumbering says nothing about what is gone.
    #[test]
    fn an_empty_known_set_prunes_nothing() {
        assert_eq!(
            vanished_uids(&HashSet::new(), &[1, 2, 3], 3),
            Vec::<u32>::new()
        );
    }

    /// A mailbox the server lists as empty holds no rows either. The caller
    /// only reaches this with `EXISTS 0` agreeing with the empty listing.
    #[test]
    fn an_empty_listing_prunes_every_known_row() {
        let known = HashSet::from([1, 2, 3]);
        assert_eq!(vanished_uids(&known, &[], 3), vec![1, 2, 3]);
    }

    /// The negative sentinel of a locally moved row is not a server UID and
    /// must never be pruned.
    #[test]
    fn a_locally_moved_rows_sentinel_uid_is_never_pruned() {
        let known = HashSet::from([-7, 4, 5]);
        assert_eq!(vanished_uids(&known, &[4, 5], 5), Vec::<u32>::new());
    }

    /// The high-water mark ignores the placeholder above the ceiling, which
    /// would otherwise make every real arrival look older than the store.
    #[test]
    fn the_high_water_mark_ignores_placeholders() {
        let known = HashSet::from([-3, 4, 9, 4_611_686_018_427_387_904]);
        assert_eq!(high_water(&known, 20), 9);
    }

    /// Backlog below the high-water mark is not an arrival: a quick sync never
    /// promised to fetch it, so it must not suspend the prune.
    #[test]
    fn old_backlog_the_window_skipped_is_not_a_missed_arrival() {
        let known = HashSet::from([50, 51]);
        let listed: Vec<u32> = (1..=51).collect();
        let coverage = arrival_coverage(&listed, &known, 51, &[], None, Some(51));
        assert!(!coverage.incomplete);
        assert_eq!(coverage.pending_mark, None);
    }

    /// A message that arrived above the mark and was not downloaded is: the
    /// bulk-move case where the destination window could not hold every copy.
    #[test]
    fn an_arrival_the_window_cut_off_is_a_missed_arrival() {
        let known = HashSet::from([50]);
        assert!(arrival_coverage(&[50, 51, 52], &known, 52, &[52], None, Some(50)).incomplete);
        assert!(!arrival_coverage(&[50, 51, 52], &known, 52, &[51, 52], None, Some(50)).incomplete);
    }

    /// A first sync of a mailbox bigger than the window has no mark to stand
    /// on, so every listed UID is an arrival and the pass is short. Nothing to
    /// prune then anyway, and the flag keeps it that way.
    #[test]
    fn a_first_sync_of_a_capped_mailbox_is_incomplete() {
        let known = HashSet::new();
        assert!(arrival_coverage(&[1, 2, 3], &known, 3, &[2, 3], None, None).incomplete);
    }

    /// ...and it hands *no* mark to the next pass, which is the difference
    /// between one conservative pass and a permanently shut gate (#0072 sweep
    /// review B1).
    ///
    /// A capped first sync of a large mailbox used to persist a mark of 0: the
    /// carried mark is combined with `min`, so it could never rise again, and a
    /// mark of 0 demands the whole mailbox be in the store before any pass
    /// counts as complete, which a positional window never achieves. Because
    /// `pass_may_prune` needs *every* target complete, one such mailbox
    /// suspended the prune for the whole account until a manual full sync, and
    /// the schema v5 rebuild put every user in exactly that state.
    #[test]
    fn a_first_capped_sync_of_a_large_mailbox_hands_on_no_mark() {
        let listed: Vec<u32> = (1..=8000).collect();
        let window: Vec<u32> = (7951..=8000).collect();
        let nothing_known = HashSet::new();

        let first = arrival_coverage(&listed, &nothing_known, 8000, &window, None, None);
        assert!(
            first.incomplete,
            "one pass of conservatism is fine: the store has nothing to prune yet anyway"
        );
        assert_eq!(
            first.pending_mark, None,
            "the forbidden pre-fix answer is Some(0), a mark no window can ever meet"
        );

        // The next pass stands on the cursor the first one wrote (top of the
        // window) plus the rows it ingested, and prunes normally.
        let known: HashSet<i64> = (7951..=8000).map(i64::from).collect();
        let second = arrival_coverage(&listed, &known, 8000, &[], None, Some(8000));
        assert!(
            !second.incomplete,
            "the backlog below the window is not an arrival"
        );
        assert_eq!(second.pending_mark, None);
    }

    /// The edge that keeps first contact from being "the store holds no rows":
    /// a mailbox emptied in another client and then bulk-moved into has no rows
    /// left, but its cursor still records the top it reached. That recorded top
    /// is the floor, so the 200 copies the window could not take still hold the
    /// gate shut.
    #[test]
    fn an_emptied_then_refilled_mailbox_defers_on_its_recorded_top() {
        let listed: Vec<u32> = (101..=400).collect();
        let window: Vec<u32> = (301..=400).collect();
        let nothing_known = HashSet::new();

        let coverage = arrival_coverage(&listed, &nothing_known, 400, &window, None, Some(100));
        assert!(
            coverage.incomplete,
            "101..=300 arrived above the recorded top and are not in the store"
        );
        assert_eq!(coverage.pending_mark, Some(100));
    }

    /// A pass that comes up short leaves the mark it stood on behind, so the
    /// next one can be held to the same line.
    #[test]
    fn a_short_pass_hands_its_mark_to_the_next_one() {
        let known = HashSet::from([50]);
        let coverage = arrival_coverage(&[50, 51, 52], &known, 52, &[52], None, Some(50));
        assert!(coverage.incomplete);
        assert_eq!(coverage.pending_mark, Some(50));
    }

    /// #0072 review note 1, the two-pass bulk move: 300 messages land in a
    /// mailbox whose quick sync only takes the top 100.
    ///
    /// Pass 1 defers, which the shipped gate already did. Pass 2 is where it
    /// used to fail: its own ingest raised `max(known)` to 400, which put the
    /// 200 stragglers *below* a freshly derived mark and opened the gate on
    /// rows whose copies were never fetched. The carried mark is what keeps it
    /// shut.
    #[test]
    fn the_pass_after_a_bulk_move_still_defers_while_arrivals_are_missing() {
        let listed: Vec<u32> = (1..=400).collect();
        let window: Vec<u32> = (301..=400).collect();

        // Pass 1: the store holds 1..=100, the move added 101..=400.
        let known_before: HashSet<i64> = (1..=100).collect();
        let pass1 = arrival_coverage(&listed, &known_before, 400, &window, None, Some(100));
        assert!(
            pass1.incomplete,
            "pass 1 could not reach 101..=300 and must defer"
        );
        assert_eq!(pass1.pending_mark, Some(100));

        // Pass 2: the store now also holds what pass 1 downloaded, and the
        // window has nothing new in it.
        let mut known_after = known_before.clone();
        known_after.extend((301..=400).map(i64::from));
        let derived = arrival_coverage(&listed, &known_after, 400, &[], None, Some(400));
        assert!(
            !derived.incomplete,
            "the pre-fix behaviour this test exists to forbid: a mark of 400 sees no arrival"
        );
        let carried = arrival_coverage(
            &listed,
            &known_after,
            400,
            &[],
            pass1.pending_mark,
            Some(400),
        );
        assert!(
            carried.incomplete,
            "101..=300 are still not in the store, so pass 2 must defer too"
        );
        assert_eq!(carried.pending_mark, Some(100));

        // A full sync brings the stragglers in and the gate opens.
        let everything: Vec<u32> = (101..=300).collect();
        let opened = arrival_coverage(
            &listed,
            &known_after,
            400,
            &everything,
            carried.pending_mark,
            Some(400),
        );
        assert!(!opened.incomplete);
        assert_eq!(opened.pending_mark, None, "the mark clears once it is met");
    }

    /// No mark can get stuck, including the lowest one there is. A mark of 0 is
    /// still reachable after the fix, and legitimately so: a mailbox that had
    /// never held a message when it was last synced records a top of 0, and a
    /// bulk move into it makes every copy an arrival. Ingesting through it
    /// clears it, which is the property that makes the whole mechanism safe to
    /// persist.
    #[test]
    fn any_persisted_mark_clears_once_a_pass_reaches_through_it() {
        let listed: Vec<u32> = (1..=300).collect();
        let known: HashSet<i64> = (201..=300).map(i64::from).collect();

        let stuck = arrival_coverage(&listed, &known, 300, &[], Some(0), Some(300));
        assert!(stuck.incomplete);
        assert_eq!(
            stuck.pending_mark,
            Some(0),
            "a mark of 0 holds the gate shut"
        );

        let backlog: Vec<u32> = (1..=200).collect();
        let opened = arrival_coverage(&listed, &known, 300, &backlog, Some(0), Some(300));
        assert!(!opened.incomplete, "a full sync reaches through any mark");
        assert_eq!(opened.pending_mark, None);
    }

    /// The other way out of a carried mark: the arrivals that were never
    /// fetched are deleted on the server, so nothing owes the store anything
    /// and the gate opens without a full sync. The mark cannot deadlock.
    #[test]
    fn a_carried_mark_clears_when_the_missing_arrivals_stop_being_listed() {
        let known: HashSet<i64> = HashSet::from([1, 2, 3]);
        let coverage = arrival_coverage(&[1, 2, 3], &known, 10, &[], Some(1), Some(3));
        assert!(!coverage.incomplete);
        assert_eq!(coverage.pending_mark, None);
    }

    /// A carried mark never rises with the store: it is the lower of the two,
    /// which is the whole mechanism.
    #[test]
    fn the_carried_mark_wins_over_a_higher_high_water() {
        let known: HashSet<i64> = HashSet::from([10, 90]);
        assert_eq!(arrival_mark(&known, 100, Some(10), Some(90)), Some(10));
        assert_eq!(arrival_mark(&known, 100, None, Some(90)), Some(90));
        // A stale mark above the store's own high-water mark cannot loosen it.
        assert_eq!(arrival_mark(&known, 100, Some(95), Some(90)), Some(90));
    }

    /// The floor a pass stands on: the higher of what the store holds and what
    /// the cursor recorded, and `None` only when there is neither.
    #[test]
    fn the_mark_is_none_only_at_first_contact() {
        let nothing = HashSet::new();
        assert_eq!(arrival_mark(&nothing, 100, None, None), None);
        // A cursor row is history, even one that recorded no UID.
        assert_eq!(arrival_mark(&nothing, 100, None, Some(0)), Some(0));
        assert_eq!(arrival_mark(&nothing, 100, None, Some(40)), Some(40));
        // Rows without a cursor row still count: the store knows something.
        let known: HashSet<i64> = HashSet::from([40]);
        assert_eq!(arrival_mark(&known, 100, None, None), Some(40));
        // The recorded top only ever raises the floor, and is clamped to the
        // same ceiling that keeps a placeholder out of the high-water mark.
        assert_eq!(arrival_mark(&known, 100, None, Some(10)), Some(40));
        assert_eq!(arrival_mark(&known, 100, None, Some(4_000_000)), Some(100));
    }

    /// `mp sync -n 0` computes a full vanished set and downloads nothing. It
    /// must not report itself complete when the mailbox holds rows the store
    /// has never seen (#0072 review note 3).
    #[test]
    fn a_pass_that_downloads_nothing_is_not_complete() {
        let known: HashSet<i64> = HashSet::from([1, 2]);
        assert!(arrival_coverage(&[1, 2, 3], &known, 3, &[], None, Some(2)).incomplete);
    }

    /// ...and stays complete when the mailbox is genuinely empty, which is the
    /// one case the empty-listing return has to keep prunable.
    #[test]
    fn an_empty_listing_is_a_complete_pass() {
        let known: HashSet<i64> = HashSet::from([1, 2]);
        let coverage = arrival_coverage(&[], &known, 2, &[], None, Some(2));
        assert!(!coverage.incomplete);
        assert_eq!(coverage.pending_mark, None);
    }

    /// The enumeration gate: `UID SEARCH ALL` must account for every message
    /// `SELECT` announced before a single row is pruned.
    #[test]
    fn a_listing_shorter_than_exists_is_an_incomplete_enumeration() {
        assert!(!enumeration_complete(3, 4));
        assert!(enumeration_complete(4, 4));
        // A message that arrived between the two responses is not a short
        // answer, and can only keep rows alive.
        assert!(enumeration_complete(5, 4));
    }

    /// `EXISTS 0` with an empty listing is the one case where a complete
    /// enumeration prunes a whole mailbox, so it is pinned deliberately.
    #[test]
    fn an_empty_mailbox_enumerates_completely() {
        assert!(enumeration_complete(0, 0));
    }

    /// The ceiling comes from `UIDNEXT`, which does not drop when the newest
    /// message is the one that was deleted.
    #[test]
    fn the_ceiling_is_uidnext_minus_one() {
        assert_eq!(ceiling(Some(84), &[1, 2, 3]), 83);
    }

    // -----------------------------------------------------------------------
    // The CONDSTORE gate (#0041)
    // -----------------------------------------------------------------------

    /// The wire form the ticket's spike pinned. Wrong here is not a compile
    /// error and not a runtime error: a server that cannot parse it answers
    /// `BAD`, and one that parses it differently answers about the wrong
    /// messages.
    #[test]
    fn the_changedsince_modifier_is_the_tail_of_the_fetch_query() {
        assert_eq!(flag_query(FlagPass::Full), "(UID FLAGS)");
        assert_eq!(
            flag_query(FlagPass::ChangedSince(90_060_115_205_545_359)),
            "(UID FLAGS) (CHANGEDSINCE 90060115205545359)"
        );
        // `uid_fetch` interpolates as `UID FETCH <set> <query>`, so this is the
        // command that goes out:
        assert_eq!(
            format!(
                "UID FETCH {} {}",
                "1:50",
                flag_query(FlagPass::ChangedSince(7))
            ),
            "UID FETCH 1:50 (UID FLAGS) (CHANGEDSINCE 7)"
        );
    }

    /// The capability gate, strictly. Everything that is not an unambiguous
    /// yes is the full window, because the cost of a needless full window is
    /// latency and the cost of a wrong delta is #0004: a read/unread change
    /// made in webmail that never arrives, with no error anywhere.
    #[test]
    fn the_delta_needs_every_condition_and_falls_back_to_the_full_window() {
        let stored = Some(1_000u64);
        let server = Some(1_500u64);

        assert_eq!(
            flag_pass(true, stored, server, false),
            FlagPass::ChangedSince(1_000),
            "the only case that may skip the full window"
        );

        // No CAPABILITY: Proton Bridge and Outlook-IMAP live here, and the
        // heuristic + full-window path must stay exactly what it was.
        assert_eq!(flag_pass(false, stored, server, false), FlagPass::Full);
        // Advertised, but this SELECT reported no HIGHESTMODSEQ: advertise is
        // not correctness, and a silent server is not one to hand a resume
        // point to.
        assert_eq!(flag_pass(true, stored, None, false), FlagPass::Full);
        // No stored resume point: the first CONDSTORE pass is a full one.
        assert_eq!(flag_pass(true, None, server, false), FlagPass::Full);
        // A stored 0 is a column that was never written, not modseq zero.
        assert_eq!(flag_pass(true, Some(0), server, false), FlagPass::Full);
        // The renumbering invalidates the modseq with everything else.
        assert_eq!(flag_pass(true, stored, server, true), FlagPass::Full);
        // The server went backwards without bumping UIDVALIDITY (a restore
        // from backup, a rebuilt index): it is not keeping the contract the
        // delta rests on, so nothing is assumed about what changed.
        assert_eq!(
            flag_pass(true, Some(1_500), Some(1_000), false),
            FlagPass::Full
        );
        // Equal is fine and is the common case: nothing has changed since.
        assert_eq!(
            flag_pass(true, Some(1_500), Some(1_500), false),
            FlagPass::ChangedSince(1_500)
        );
    }

    /// A CONDSTORE mark: `modseq`, vouching for every UID from `from_uid` up.
    fn mark(modseq: u64, from_uid: u32) -> ModseqMark {
        ModseqMark { modseq, from_uid }
    }

    /// What a pass may leave behind. A mark claims "every flag at or above
    /// `from_uid` was correct as of `modseq`", so a pass records the lowest UID
    /// it covered and no lower: a quick sync that saw the last 50 of 8000 and
    /// claimed all of them would make the next full sync skip exactly the
    /// old-message flag changes it exists to catch (#0004 again). A capped
    /// pass does record, for what it covered (PERSO-202).
    #[test]
    fn a_pass_records_its_modseq_for_the_uids_it_covered_and_no_lower() {
        let server = Some(90_060_115_205_545_359u64);
        assert_eq!(
            modseq_to_record(server, Some(1), true, true),
            Some(mark(90_060_115_205_545_359, 1)),
            "a full pass over a completely enumerated mailbox"
        );
        assert_eq!(
            modseq_to_record(server, Some(7_951), true, true),
            Some(mark(90_060_115_205_545_359, 7_951)),
            "a capped window vouches for itself and not for the messages below it"
        );
        assert_eq!(
            modseq_to_record(server, None, true, true),
            None,
            "a pass that looked at no flag vouches for nothing"
        );
        assert_eq!(
            modseq_to_record(server, Some(1), false, true),
            None,
            "nor may a pass whose own enumeration came back short"
        );
        assert_eq!(
            modseq_to_record(server, Some(1), true, false),
            None,
            "nor may a pass that stopped at its body deadline (#0113)"
        );
        // Nothing to record is not a failure; the cursor UPSERT carries the
        // stored value forward on None.
        assert_eq!(modseq_to_record(None, Some(1), true, true), None);
        assert_eq!(modseq_to_record(Some(0), Some(1), true, true), None);
        // A modseq beyond i64 is a server the store cannot represent; it gets
        // the full window forever rather than a truncated resume point.
        assert_eq!(modseq_to_record(Some(u64::MAX), Some(1), true, true), None);
    }

    /// PERSO-202: how a pass splits around a stored mark.
    #[test]
    fn the_flag_plan_splits_the_window_at_the_marks_lower_bound() {
        let window: Vec<u32> = (7_951..=8_000).collect();

        // No mark (or no CONDSTORE): every flag in the window, and the window
        // is what the next mark may claim.
        let plan = flag_plan(&window, 8_000, None, FlagPass::Full);
        assert_eq!(plan.full, window);
        assert_eq!(plan.delta, None);
        assert_eq!(plan.covered_from, 7_951);

        // A mark over the whole mailbox: the quick sync is a delta from UID 1
        // up, below its own window, and fetches no flag in full. This is the
        // common path the ticket is about.
        let whole = Some(mark(1_000, 1));
        let plan = flag_plan(&window, 8_000, whole, FlagPass::ChangedSince(1_000));
        assert!(plan.full.is_empty());
        assert_eq!(
            plan.delta,
            Some(FlagDelta {
                since: 1_000,
                from: 1,
                to: 8_000
            })
        );
        assert_eq!(plan.delta.unwrap().uid_set(), "1:8000");
        assert_eq!(
            plan.covered_from, 1,
            "so the next mark still covers everything"
        );

        // A mark left by an earlier capped pass, above this window's bottom
        // (new mail moved nothing, the window grew, or this is a full sync):
        // what the mark does not cover is fetched in full.
        let capped = Some(mark(1_000, 7_990));
        let plan = flag_plan(&window, 8_000, capped, FlagPass::ChangedSince(1_000));
        assert_eq!(plan.full, (7_951..7_990).collect::<Vec<u32>>());
        assert_eq!(plan.delta.unwrap().uid_set(), "7990:8000");
        assert_eq!(plan.covered_from, 7_951);

        // A mark above everything the server lists (its messages were all
        // expunged since): nothing to ask a delta about.
        let above = Some(mark(1_000, 9_000));
        let plan = flag_plan(&window, 8_000, above, FlagPass::ChangedSince(1_000));
        assert_eq!(plan.full, window);
        assert_eq!(plan.delta, None);
        assert_eq!(plan.covered_from, 7_951);

        // The gate said no (a UIDVALIDITY reset, a server gone backwards):
        // the mark is ignored entirely.
        let plan = flag_plan(&window, 8_000, whole, FlagPass::Full);
        assert_eq!(plan.full, window);
        assert_eq!(plan.delta, None);
        assert_eq!(plan.covered_from, 7_951);

        // A one-UID delta names the UID, not a range.
        let plan = flag_plan(
            &[8_000],
            8_000,
            Some(mark(5, 8_000)),
            FlagPass::ChangedSince(5),
        );
        assert_eq!(plan.delta.unwrap().uid_set(), "8000");
    }

    /// A mailbox on a CONDSTORE server, as the delta sees it: each message's
    /// flag and the mod-sequence of its last change.
    #[derive(Default)]
    struct ModelServer {
        messages: std::collections::BTreeMap<u32, (bool, u64)>,
        highest_modseq: u64,
        uid_next: u32,
    }

    impl ModelServer {
        fn bump(&mut self) -> u64 {
            self.highest_modseq += 1;
            self.highest_modseq
        }

        fn arrive(&mut self, seen: bool) {
            self.uid_next += 1;
            let modseq = self.bump();
            self.messages.insert(self.uid_next, (seen, modseq));
        }

        /// One change made in another client: a flag toggled, a message
        /// arrived or one expunged.
        fn change(&mut self, rng: &mut impl rand::Rng) {
            let roll = rng.gen_range(0..10);
            if self.messages.is_empty() || roll < 2 {
                self.arrive(rng.gen_bool(0.5));
                return;
            }
            let uids: Vec<u32> = self.messages.keys().copied().collect();
            let uid = uids[rng.gen_range(0..uids.len())];
            if roll < 3 {
                self.messages.remove(&uid);
                self.bump();
            } else {
                let modseq = self.bump();
                let entry = self.messages.get_mut(&uid).unwrap();
                *entry = (!entry.0, modseq);
            }
        }
    }

    /// One pass of `list_mailbox_on_session` plus the engine's writes, against
    /// the model: the same gate, plan and record the real pass runs, with the
    /// server answering the full fetch with every flag and the delta with only
    /// what changed since. `meanwhile` happen after the SELECT and before the
    /// flag fetch, which is the race a mark taken at SELECT has to survive.
    fn model_pass(
        server: &mut ModelServer,
        store: &mut std::collections::BTreeMap<u32, bool>,
        stored: &mut Option<ModseqMark>,
        limit: Option<usize>,
        meanwhile: &mut dyn FnMut(&mut ModelServer),
    ) {
        let at_select = server.highest_modseq;
        let listed: Vec<u32> = server.messages.keys().copied().collect();
        let known: HashSet<i64> = store.keys().map(|&uid| uid as i64).collect();
        // A change between the SELECT and the flag fetch, which the fetch sees.
        meanwhile(server);
        // Vanished rows go (the listing is complete in the model).
        store.retain(|uid, _| listed.contains(uid));
        let mut window: Vec<u32> = match limit {
            Some(n) => listed.iter().rev().take(n).copied().collect(),
            None => listed.clone(),
        };
        window.sort_unstable();
        if window.is_empty() {
            if listed.is_empty() {
                *stored = modseq_to_record(Some(at_select), Some(1), true, true).or(*stored);
            }
            return;
        }
        let pass = flag_pass(true, stored.map(|m| m.modseq), Some(at_select), false);
        let plan = flag_plan(&window, *listed.last().unwrap(), *stored, pass);
        for uid in &plan.full {
            if let (Some(row), Some((seen, _))) = (store.get_mut(uid), server.messages.get(uid)) {
                *row = *seen;
            }
        }
        if let Some(delta) = plan.delta {
            for (uid, (seen, modseq)) in server.messages.range(delta.from..=delta.to) {
                if *modseq > delta.since {
                    if let Some(row) = store.get_mut(uid) {
                        *row = *seen;
                    }
                }
            }
        }
        // And one after the flag fetch, which it does not.
        meanwhile(server);
        for uid in new_uids_in_window(&window, &known) {
            if let Some((seen, _)) = server.messages.get(&uid) {
                store.insert(uid, *seen);
            }
        }
        if let Some(mark) = modseq_to_record(Some(at_select), Some(plan.covered_from), true, true) {
            *stored = Some(mark);
        }
    }

    /// PERSO-202, the property the mark exists to keep: no flag change made
    /// elsewhere is ever lost.
    ///
    /// A row may disagree with the server only where the next pass is still
    /// bound to look: below the mark's lower bound, which a pass that reaches
    /// there fetches in full, or on a message whose mod-sequence is above the
    /// mark's, which the next `CHANGEDSINCE` returns. A row that disagrees
    /// anywhere else is a flag the delta will skip for good, which is #0004.
    /// Capped quick syncs, full syncs and changes landing mid-pass are mixed
    /// at random over many seeds, and a full sync at the end has to leave the
    /// store equal to the server.
    ///
    /// Recording a capped pass's mark from UID 1, or running its delta over the
    /// window alone while claiming the mark's bound, both fail this test.
    #[test]
    fn no_flag_change_made_elsewhere_is_lost_across_capped_and_full_passes() {
        use rand::{Rng, SeedableRng};

        for seed in 0..200u64 {
            let mut rng = rand::rngs::StdRng::seed_from_u64(seed);
            let mut server = ModelServer::default();
            for _ in 0..rng.gen_range(0..40) {
                server.arrive(rng.gen_bool(0.5));
            }
            // A store an earlier build filled, with no mark and flags that
            // may have changed since: what a server gaining CONDSTORE, or a
            // mark the gate refused, leaves for the next pass.
            let mut store = std::collections::BTreeMap::new();
            for &uid in server.messages.keys() {
                if rng.gen_bool(0.7) {
                    store.insert(uid, rng.gen_bool(0.5));
                }
            }
            let mut stored: Option<ModseqMark> = None;

            for step in 0..60 {
                for _ in 0..rng.gen_range(0..4) {
                    server.change(&mut rng);
                }
                if rng.gen_bool(0.05) {
                    stored = None;
                }
                let limit = if rng.gen_bool(0.2) {
                    None
                } else {
                    Some(rng.gen_range(0..8))
                };
                let racing = rng.gen_bool(0.3);
                let mut meanwhile_rng = rand::rngs::StdRng::seed_from_u64(seed * 1_000 + step);
                model_pass(&mut server, &mut store, &mut stored, limit, &mut |s| {
                    if racing {
                        s.change(&mut meanwhile_rng);
                    }
                });

                if let Some(mark) = stored {
                    for (uid, seen) in &store {
                        let Some((truth, modseq)) = server.messages.get(uid) else {
                            continue;
                        };
                        assert!(
                            seen == truth || *uid < mark.from_uid || *modseq > mark.modseq,
                            "seed {seed} step {step}: UID {uid} reads {seen} and the server \
                             holds {truth} since modseq {modseq}, which mark {mark:?} will \
                             never ask about again"
                        );
                    }
                }
            }

            model_pass(&mut server, &mut store, &mut stored, None, &mut |_| {});
            for (uid, seen) in &store {
                assert_eq!(
                    Some(*seen),
                    server.messages.get(uid).map(|(truth, _)| *truth),
                    "seed {seed}: a full sync leaves UID {uid} as the server holds it"
                );
            }
            assert_eq!(store.len(), server.messages.len());
        }
    }

    /// #0113: the body pass is cut into commands, and the cut is from the top.
    ///
    /// Forty-five new UIDs are three `UID FETCH`es rather than one, and the
    /// first one asks for the newest twenty, so a pass that stops after it has
    /// the newest mail and owes only the backlog. The short chunk is the oldest.
    #[test]
    fn the_body_pass_is_chunked_newest_first() {
        let new_uids: Vec<u32> = (1..=45).collect();

        let chunks = body_chunks(&new_uids, BODY_CHUNK_SIZE);

        assert_eq!(
            chunks.len(),
            3,
            "45 new UIDs are three FETCH commands, not one"
        );
        assert_eq!(
            chunks[0],
            &(26..=45).collect::<Vec<u32>>()[..],
            "the newest 20 go first"
        );
        assert_eq!(chunks[1], &(6..=25).collect::<Vec<u32>>()[..]);
        assert_eq!(
            chunks[2],
            &(1..=5).collect::<Vec<u32>>()[..],
            "the oldest chunk is the short one"
        );
        // Every UID is asked for exactly once: a planner that dropped or
        // duplicated one would leave a message undownloaded forever or fetch it
        // twice, and neither is visible in the chunk count alone.
        let mut flat: Vec<u32> = chunks.concat();
        flat.sort_unstable();
        assert_eq!(flat, new_uids);
        // A window smaller than one chunk is still one command.
        assert_eq!(body_chunks(&[7, 8], BODY_CHUNK_SIZE).len(), 1);
    }

    /// #0113: where the deadline is answered, and where it is not.
    ///
    /// A sync pass's first chunk always goes out even on a budget already
    /// spent, or a pass would download nothing and never converge. From the
    /// second chunk on, whichever mailbox it belongs to, a spent budget stops
    /// the pass (PERSO-300). The check is between chunks by construction:
    /// there is no point at which a `UID FETCH` in flight can be abandoned, so
    /// a half-read response can never be left on a pooled session.
    #[test]
    fn the_deadline_stops_the_next_chunk_and_never_the_first() {
        let budget = Some(Duration::from_secs(30));
        let spent = Duration::from_secs(31);
        let left = Duration::from_secs(29);

        assert!(
            !stop_before_chunk(0, budget, spent),
            "a budget already spent still buys one chunk, so the pass makes progress"
        );
        assert!(
            stop_before_chunk(1, budget, spent),
            "and stops the pass at the next one"
        );
        assert!(
            !stop_before_chunk(1, budget, left),
            "a live budget fetches on"
        );
        assert!(
            !stop_before_chunk(9, None, spent),
            "no budget never stops (`mp sync`)"
        );
    }

    /// The #0041 review blocker, scripted at the only level
    /// `list_mailbox_on_session` is testable without a live socket: `mp sync
    /// -n 0` against a CONDSTORE server lists an empty window, fetching no flags
    /// and must leave the stored resume point alone. Recording the server's
    /// HIGHESTMODSEQ there would make the next `CHANGEDSINCE` start past every
    /// flag change made in between and lose them silently (#0004's failure).
    #[test]
    fn an_n0_pass_over_a_populated_mailbox_may_not_advance_the_modseq() {
        let stored = Some(1_000u64);
        let server = Some(2_000u64);
        // The mailbox has 12 messages; `-n 0` gives `limit == Some(0)`, so the
        // window is empty and the pass returns before pass 1.
        let listed_len = 12usize;
        let window_len = 0usize;

        // The delta is engaged (this is the dangerous configuration): a
        // CONDSTORE server, a prior recorded modseq, no UIDVALIDITY reset.
        assert_eq!(
            flag_pass(true, stored, server, false),
            FlagPass::ChangedSince(1_000),
            "a CONDSTORE server with a stored resume point runs the delta"
        );

        // The pass fetched nothing, so it vouches for nothing: the empty
        // return covers from 1 only when the listing is itself empty.
        assert_eq!(window_len, 0);
        let covered = (listed_len == 0).then_some(1);
        assert_eq!(
            modseq_to_record(server, covered, true, true),
            None,
            "`mp sync -n 0` must leave the stored modseq where it is"
        );

        // A genuinely empty mailbox still may advance: nothing was missed.
        assert_eq!(
            modseq_to_record(server, Some(1), true, true),
            Some(mark(2_000, 1)),
            "an empty mailbox has no flags to have skipped"
        );
    }

    /// The delta's other half, and the one that breaks new mail rather than
    /// flags if it is wrong: a CHANGEDSINCE response says nothing about
    /// messages whose flags did not change, so which UIDs are new has to come
    /// from the window and the skip list.
    #[test]
    fn new_uids_come_from_the_window_and_not_from_a_delta_response() {
        let known: HashSet<i64> = HashSet::from([10, 11, 12]);
        assert_eq!(
            new_uids_in_window(&[10, 11, 12, 13, 14], &known),
            vec![13, 14]
        );
        // The case that matters: a brand-new message nobody touched, so it is
        // absent from the delta response entirely. It is still new.
        assert_eq!(new_uids_in_window(&[13], &known), vec![13]);
        assert!(new_uids_in_window(&[10, 11], &known).is_empty());
        // A UIDVALIDITY reset empties the skip list, so everything is new.
        assert_eq!(new_uids_in_window(&[10, 11], &HashSet::new()), vec![10, 11]);
    }

    /// A server that withholds `UIDNEXT` leaves the highest UID it did list,
    /// and one that lists neither leaves 0, which prunes nothing and makes
    /// every listed UID a placeholder rather than a prune candidate.
    #[test]
    fn the_ceiling_falls_back_to_the_listing_then_to_zero() {
        assert_eq!(ceiling(None, &[1, 2, 9]), 9);
        assert_eq!(ceiling(None, &[]), 0);
        assert_eq!(ceiling(Some(0), &[1, 2, 9]), 0);
    }

    /// An IMAP server played from a script, since the repository has no
    /// in-process one: each read that finds the current reply used up hands
    /// out the next, and records what the client wrote since the previous
    /// one, so a test sees which bytes went out before which answer came back.
    #[derive(Debug)]
    struct ScriptedServer {
        replies: std::collections::VecDeque<&'static str>,
        reply: Vec<u8>,
        written: Vec<u8>,
        turns: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl futures::io::AsyncRead for ScriptedServer {
        fn poll_read(
            mut self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            buf: &mut [u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            if self.reply.is_empty() {
                let sent = String::from_utf8_lossy(&std::mem::take(&mut self.written)).into_owned();
                self.turns.lock().unwrap().push(sent);
                let Some(next) = self.replies.pop_front() else {
                    return std::task::Poll::Ready(Ok(0));
                };
                self.reply = next.as_bytes().to_vec();
            }
            let n = self.reply.len().min(buf.len());
            buf[..n].copy_from_slice(&self.reply[..n]);
            self.reply.drain(..n);
            std::task::Poll::Ready(Ok(n))
        }
    }

    impl futures::io::AsyncWrite for ScriptedServer {
        fn poll_write(
            mut self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
            buf: &[u8],
        ) -> std::task::Poll<std::io::Result<usize>> {
            self.written.extend_from_slice(buf);
            std::task::Poll::Ready(Ok(buf.len()))
        }

        fn poll_flush(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }

        fn poll_close(
            self: std::pin::Pin<&mut Self>,
            _: &mut std::task::Context<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            std::task::Poll::Ready(Ok(()))
        }
    }

    /// One store listing of `INBOX` over a CONDSTORE server played from
    /// `replies` (the first answers the `LOGIN`), the store holding UIDs 1 to
    /// 5 under UIDVALIDITY 7 and `stored` as its mark: the listing, its body
    /// plan, and what the client wrote before each read, the `LOGIN` turn
    /// dropped.
    async fn condstore_fetch(
        limit: usize,
        stored: Option<ModseqMark>,
        replies: &[&'static str],
    ) -> (MailboxListing, BodyPlan, Vec<String>) {
        let turns = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let server = ScriptedServer {
            replies: replies.iter().copied().collect(),
            reply: Vec::new(),
            written: Vec::new(),
            turns: turns.clone(),
        };
        let mut session = async_imap::Client::new(server)
            .login("user", "secret")
            .await
            .map_err(|e| e.0)
            .unwrap();
        let known = KnownUids {
            uids: (1..=5).collect(),
            uidvalidity: Some(7),
            arrival_mark: None,
            prior_high_water: Some(5),
            highest_modseq: stored,
        };
        let caps = ServerCaps {
            condstore: true,
            ..ServerCaps::default()
        };
        let (listing, plan) =
            list_mailbox_on_session(&mut session, "INBOX", Some(limit), known, caps)
                .await
                .unwrap();
        let turns = turns.lock().unwrap().split_off(1);
        (listing, plan, turns)
    }

    /// The mark a listing leaves once a body pass with nothing to download
    /// has run to its end.
    fn mark_left(listing: MailboxListing) -> Option<ModseqMark> {
        (listing.coverage)(&[], true).highest_modseq
    }

    /// What a CONDSTORE server answers to `SELECT "INBOX" (CONDSTORE)` for a
    /// mailbox of UIDs 1 to 5 at HIGHESTMODSEQ 1200.
    const CONDSTORE_SELECT: &str = "* 5 EXISTS\r\n* OK [UIDVALIDITY 7] ok\r\n\
         * OK [UIDNEXT 6] ok\r\n* OK [HIGHESTMODSEQ 1200] ok\r\n\
         A0002 OK [READ-WRITE] selected\r\n";

    /// PERSO-202, the common path: a quick sync whose window is the newest two
    /// messages, over a mailbox a mark covers whole, asks the server what
    /// changed from UID 1 up and nothing else. A flag changed elsewhere on UID
    /// 2, below the window, arrives on this pass, and the pass records a mark
    /// that still covers the whole mailbox at the newer modseq.
    #[tokio::test]
    async fn a_capped_pass_hears_of_a_flag_change_below_its_window_and_advances_the_mark() {
        let (fetched, plan, turns) = condstore_fetch(
            2,
            Some(mark(1_000, 1)),
            &[
                "A0001 OK logged in\r\n",
                CONDSTORE_SELECT,
                "* SEARCH 1 2 3 4 5\r\nA0003 OK done\r\n",
                "* 2 FETCH (UID 2 FLAGS (\\Seen) MODSEQ (1100))\r\nA0004 OK done\r\n",
            ],
        )
        .await;

        assert_eq!(
            turns,
            vec![
                "A0002 SELECT \"INBOX\" (CONDSTORE)\r\n".to_string(),
                "A0003 UID SEARCH ALL\r\n".to_string(),
                "A0004 UID FETCH 1:5 (UID FLAGS) (CHANGEDSINCE 1000)\r\n".to_string(),
            ]
        );
        assert_eq!(fetched.known_flags.len(), 1);
        assert_eq!(fetched.known_flags[0].0, 2);
        assert!(fetched.known_flags[0].1.seen);
        assert!(plan.chunks.is_empty());
        assert_eq!(fetched.skipped, 2);
        assert_eq!(mark_left(fetched), Some(mark(1_200, 1)));
    }

    /// The first CONDSTORE pass has no mark to resume from: it fetches every
    /// flag in its window, and records a mark for the window and no lower,
    /// which the next pass resumes from instead of waiting for a full sync.
    #[tokio::test]
    async fn a_capped_pass_without_a_mark_records_one_for_its_window() {
        let (fetched, _, turns) = condstore_fetch(
            2,
            None,
            &[
                "A0001 OK logged in\r\n",
                CONDSTORE_SELECT,
                "* SEARCH 1 2 3 4 5\r\nA0003 OK done\r\n",
                "* 4 FETCH (UID 4 FLAGS () MODSEQ (900))\r\n\
                 * 5 FETCH (UID 5 FLAGS (\\Seen) MODSEQ (1200))\r\nA0004 OK done\r\n",
            ],
        )
        .await;

        assert_eq!(turns[2], "A0004 UID FETCH 4:5 (UID FLAGS)\r\n");
        assert_eq!(fetched.known_flags.len(), 2);
        assert_eq!(mark_left(fetched), Some(mark(1_200, 4)));
    }

    /// A window that reaches below the stored mark: the part below it is
    /// fetched in full, the part from it up as a delta, and the new mark
    /// covers from the window's bottom.
    #[tokio::test]
    async fn a_window_below_the_mark_fetches_that_part_in_full() {
        let (fetched, _, turns) = condstore_fetch(
            3,
            Some(mark(1_000, 5)),
            &[
                "A0001 OK logged in\r\n",
                CONDSTORE_SELECT,
                "* SEARCH 1 2 3 4 5\r\nA0003 OK done\r\n",
                "* 3 FETCH (UID 3 FLAGS () MODSEQ (10))\r\n\
                 * 4 FETCH (UID 4 FLAGS (\\Seen) MODSEQ (20))\r\nA0004 OK done\r\n",
                "A0005 OK done\r\n",
            ],
        )
        .await;

        assert_eq!(
            turns[2..],
            [
                "A0004 UID FETCH 3:4 (UID FLAGS)\r\n".to_string(),
                "A0005 UID FETCH 5 (UID FLAGS) (CHANGEDSINCE 1000)\r\n".to_string(),
            ]
        );
        assert_eq!(fetched.known_flags.len(), 2);
        assert_eq!(mark_left(fetched), Some(mark(1_200, 3)));
    }

    /// The search pass of `mp search <query> --server` on a plain IMAP server,
    /// against `replies` (the first answers the `LOGIN`): its result, and
    /// what the client wrote before each read, the `LOGIN` turn dropped.
    async fn server_search(
        query: &str,
        replies: &[&'static str],
    ) -> (Result<MailboxStubs<u32>>, Vec<String>) {
        let turns = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let server = ScriptedServer {
            replies: replies.iter().copied().collect(),
            reply: Vec::new(),
            written: Vec::new(),
            turns: turns.clone(),
        };
        let mut session = async_imap::Client::new(server)
            .login("user", "secret")
            .await
            .map_err(|e| e.0)
            .unwrap();
        let search = crate::search::to_imap(&crate::search::parse(query).unwrap())
            .unwrap()
            .search;
        let result = search_stubs_on_session(
            &mut session,
            &search,
            None,
            "INBOX",
            10,
            crate::server_search::SearchOrder::Newest,
        )
        .await;
        let turns = turns.lock().unwrap().split_off(1);
        (result, turns)
    }

    /// PERSO-214: RFC 3501 allows only 7-bit text in a quoted string, so a
    /// non-ASCII search value goes as a literal under `CHARSET UTF-8`, and
    /// the literal's bytes wait for the server's continuation.
    #[tokio::test]
    async fn a_non_ascii_search_value_is_a_utf8_literal_sent_after_the_continuation() {
        let (result, turns) = server_search(
            "Grüße",
            &[
                "A0001 OK logged in\r\n",
                "* 0 EXISTS\r\nA0002 OK [READ-WRITE] selected\r\n",
                "+ go ahead\r\n",
                "* SEARCH\r\nA0003 OK search done\r\n",
            ],
        )
        .await;
        assert_eq!(
            turns,
            vec![
                "A0002 SELECT \"INBOX\"\r\n".to_string(),
                "A0003 UID SEARCH CHARSET UTF-8 TEXT {7}\r\n".to_string(),
                "Grüße\r\n".to_string(),
            ]
        );
        assert!(result.unwrap().stubs.is_empty());
    }

    /// A server without UTF-8 search answers `NO [BADCHARSET]` (RFC 3501
    /// section 6.4.4) instead of the continuation: the literal is never sent,
    /// and the error says the server cannot search for that text.
    #[tokio::test]
    async fn a_server_without_utf8_search_refuses_with_badcharset_before_the_literal() {
        let (result, turns) = server_search(
            "Grüße",
            &[
                "A0001 OK logged in\r\n",
                "* 0 EXISTS\r\nA0002 OK [READ-WRITE] selected\r\n",
                "A0003 NO [BADCHARSET (US-ASCII)] unsupported charset\r\n",
            ],
        )
        .await;
        assert_eq!(
            turns,
            vec![
                "A0002 SELECT \"INBOX\"\r\n".to_string(),
                "A0003 UID SEARCH CHARSET UTF-8 TEXT {7}\r\n".to_string(),
            ]
        );
        let error = result.unwrap_err().to_string();
        assert!(
            error.contains("cannot search for non-ASCII text") && error.contains("US-ASCII"),
            "{error}"
        );
    }
}
